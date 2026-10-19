//! `miharashi bench-gpu`: 将来の重いエフェクトに向けた GPU のベンチマーク(PoC-005 PR4)。
//! B1 コネクトーム: MaleCNS 全体級(ニューロン 16 万、結合 300 万本)の合成グラフを、表示カードの
//! render node に surfaceless で描く。表示中の miharashi を止めずに測れる(KMS には出さない)。
//! 1 秒ごとに fps、GPU の時間、CPU 使用率、iGPU の使用率、CPU パッケージの電力を出す。
//! `miharashi bench-scene`: キューブ(3D 画面)の 1 フレームの GPU 時間を、層を挟む `draw_scene` と
//! 層なしの直接描画で比べる(Issue #74 T4)。同じく surfaceless で、KMS には出さない。
use crate::app::screens::cube::{CubeScene, CubeScreen, THIRD_BAND, THIRD_BASE};
use glow::HasContext;
use miharashi_glscene::{GlScene, GlTarget, ObjectCount};
use miharashi_gpu::connectome::{self, FrameTimes, Lines, RunOpts, Scene};
use miharashi_gpu::{Frame, SceneFrame, SceneOutcome, SceneTop};
use miharashi_linux::sysinfo::{RaplTracker, SelfCpu};
use miharashi_render::Canvas;
use miharashi_render::canvas::rgba;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub struct Opts {
    /// 表示カードの PCI アドレス(この GPU の render node に描く)
    pub pci: String,
    pub size: (u32, u32),
    pub duration: Duration,
    pub fps: u32,
    pub neurons: u32,
    pub synapses: u32,
    pub stim: usize,
    pub gain: f32,
    pub lines: Lines,
    pub rapl_helper: PathBuf,
    /// 最後のフレームを PNG で保存する先
    pub out: Option<PathBuf>,
}

/// 1 秒ごとと全体の集計に使う値。
#[derive(Default)]
struct Window {
    frames: u32,
    step: Vec<f32>,
    draw: Vec<f32>,
    total: Vec<f32>,
    submit: Vec<f32>,
    wait: Vec<f32>,
}

impl Window {
    fn push(&mut self, t: &FrameTimes) {
        self.frames += 1;
        self.submit.push(t.submit);
        self.wait.push(t.wait);
        if let (Some(s), Some(d)) = (t.gpu_step, t.gpu_draw) {
            self.step.push(s);
            self.draw.push(d);
            self.total.push(s + d);
        }
    }
}

/// 平均と p95。空なら None。
fn mean_p95(v: &mut [f32]) -> Option<(f32, f32)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let p95 = v[((v.len() - 1) as f32 * 0.95).round() as usize];
    Some((mean, p95))
}

fn fmt_ms(v: &mut [f32]) -> String {
    mean_p95(v).map_or("-".into(), |(m, p)| format!("{m:.2}/{p:.2}"))
}

fn fmt_opt(v: Option<f32>, digits: usize) -> String {
    v.map_or("-".into(), |x| format!("{x:.digits$}"))
}

fn sysfs_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn mib(v: Option<u64>) -> String {
    v.map_or("-".into(), |b| format!("{}MB", b >> 20))
}

pub fn run(o: &Opts) -> Result<(), String> {
    let dev = PathBuf::from(format!("/sys/bus/pci/devices/{}", o.pci));
    let vram = || sysfs_u64(&dev.join("mem_info_vram_used"));
    let gtt = || sysfs_u64(&dev.join("mem_info_gtt_used"));
    let busy = || sysfs_u64(&dev.join("gpu_busy_percent")).map(|v| v as f32);

    let node = miharashi_gpu::render_node_for_pci(&o.pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("render node: {}", node.display());

    let t0 = Instant::now();
    let graph = connectome::synth(o.neurons, o.synapses, 1);
    println!(
        "graph: neurons={} synapses={} synth={:.0}ms",
        o.neurons,
        o.synapses,
        t0.elapsed().as_secs_f32() * 1000.0
    );
    let (vram0, gtt0) = (vram(), gtt());

    let opts = RunOpts {
        size: o.size,
        duration: o.duration,
        fps: o.fps,
        stim: o.stim,
        seed: 1,
    };
    let (result, img) = off.with_fbo(o.size, |gl| -> Result<f32, String> {
        println!("gl: {}", connectome::caps(gl));
        let t0 = Instant::now();
        let mut scene = Scene::new(gl, &graph)?;
        scene.gain = o.gain;
        scene.lines = o.lines;
        println!(
            "upload: {:.0}ms / vram {} → {} / gtt {} → {}",
            t0.elapsed().as_secs_f32() * 1000.0,
            mib(vram0),
            mib(vram()),
            mib(gtt0),
            mib(gtt())
        );
        println!(
            "run: {}x{} fps={} stim={} gain={} lines={:?} {}s",
            o.size.0,
            o.size.1,
            if o.fps == 0 { "max".into() } else { o.fps.to_string() },
            o.stim,
            o.gain,
            o.lines,
            o.duration.as_secs()
        );
        let mut cpu = SelfCpu::default();
        let mut rapl = RaplTracker::new(&o.rapl_helper);
        let now = Instant::now();
        cpu.poll(now);
        rapl.poll(now);
        let mut cpu_total = SelfCpu::default();
        cpu_total.poll(now);
        let (mut sec, mut all) = (Window::default(), Window::default());
        let (mut busy_s, mut watts_s, mut cpu_s) = (Vec::new(), Vec::new(), Vec::new());
        let mut next = Duration::from_secs(1);
        connectome::run(gl, &mut scene, &opts, |_, elapsed, t| {
            sec.push(t);
            all.push(t);
            if elapsed < next {
                return;
            }
            next += Duration::from_secs(1);
            let now = Instant::now();
            let (c, w, b) = (cpu.poll(now), rapl.poll(now), busy());
            cpu_s.extend(c);
            watts_s.extend(w);
            busy_s.extend(b);
            println!(
                "t={:>4}s frames={:>3} gpu={} (step {} / draw {}) submit={} wait={} cpu={}% busy={}% W={}",
                elapsed.as_secs(),
                sec.frames,
                fmt_ms(&mut sec.total),
                fmt_ms(&mut sec.step),
                fmt_ms(&mut sec.draw),
                fmt_ms(&mut sec.submit),
                fmt_ms(&mut sec.wait),
                fmt_opt(c, 1),
                fmt_opt(b, 0),
                fmt_opt(w, 1),
            );
            sec = Window::default();
        })?;
        let cpu_run = cpu_total.poll(Instant::now());
        let active = scene.active_fraction(gl);
        let lit = scene.active_lines(gl);
        scene.destroy(gl);
        let avg = |v: &[f32]| (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32);
        let fps = all.frames as f32 / o.duration.as_secs_f32();
        // GPU が埋まっている割合(タイマーで測った時間 × fps)。gpu_busy_percent はこの GPU では当てにならない
        let gpu_load = avg(&all.total).map(|ms| ms * fps / 10.0);
        println!(
            "summary: frames={} fps={:.1} gpu={} gpu_load={}% step={} draw={} submit={} cpu={}% busy={}% W={} active={:.2}% lit_lines={}",
            all.frames,
            fps,
            fmt_ms(&mut all.total),
            fmt_opt(gpu_load, 0),
            fmt_ms(&mut all.step),
            fmt_ms(&mut all.draw),
            fmt_ms(&mut all.submit),
            fmt_opt(cpu_run, 2),
            fmt_opt(avg(&busy_s), 0),
            fmt_opt(avg(&watts_s), 1),
            active * 100.0,
            lit,
        );
        println!("        (ms は 平均/p95。cpu は計測区間全体の自プロセス、1 コア = 100%。busy は gpu_busy_percent の 1 秒ごとの平均)");
        Ok(active)
    })?;
    result?;
    save_png(o.out.as_deref(), o.size, img)
}

fn save_png(path: Option<&Path>, size: (u32, u32), img: Vec<u8>) -> Result<(), String> {
    if let Some(path) = path {
        image::RgbaImage::from_raw(size.0, size.1, img)
            .ok_or("画像の大きさが合わない")?
            .save(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("png: {}", path.display());
    }
    Ok(())
}

/// B2 の条件。
#[cfg(feature = "video")]
pub struct VideoOpts {
    pub pci: String,
    pub file: PathBuf,
    pub size: (u32, u32),
    pub duration: Duration,
    /// 1 秒あたりのフレーム数。0 なら動画のフレームレートに合わせる
    pub fps: u32,
    pub detections: usize,
    pub font: String,
    pub rapl_helper: PathBuf,
    pub out: Option<PathBuf>,
}

/// 検出枠のラベルの文字を、1 行に 1 個ずつ描いたアトラスを作る(CPU で 1 回だけ)。
#[cfg(feature = "video")]
fn label_atlas(font: &str, n: usize) -> (Vec<u8>, u32, u32, Vec<f32>) {
    const CLASSES: [&str; 6] = ["person", "car", "bicycle", "dog", "bus", "bag"];
    const CELL: u32 = 26;
    const WIDTH: u32 = 256;
    let mut text = crate::app::text_engine(font);
    let mut canvas = miharashi_render::Canvas::new(WIDTH, CELL * n as u32);
    let mut widths = Vec::with_capacity(n);
    for i in 0..n {
        let label = format!(
            "{} #{i:02} 0.{}",
            CLASSES[i % CLASSES.len()],
            70 + (i * 7) % 30
        );
        widths.push(text.measure(&label, 18.0).min(WIDTH as f32));
        text.draw(
            &mut canvas,
            &label,
            0.0,
            (i as u32 * CELL) as f32 + 2.0,
            18.0,
            [255; 4],
        );
    }
    (canvas.pixmap.data().to_vec(), WIDTH, CELL, widths)
}

/// B2: 動画(VA-API でデコード)と検出枠の重ね合わせを `o.duration` の間描き、1 秒ごとと最後に計測値を出す。
#[cfg(feature = "video")]
pub fn run_video(o: &VideoOpts) -> Result<(), String> {
    use miharashi_gpu::connectome::Timer;
    use miharashi_gpu::video::{self, Tracks, VideoScene};
    use std::collections::VecDeque;

    let dev = PathBuf::from(format!("/sys/bus/pci/devices/{}", o.pci));
    let busy = || sysfs_u64(&dev.join("gpu_busy_percent")).map(|v| v as f32);
    let node = miharashi_gpu::render_node_for_pci(&o.pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    let dmabuf = off.dmabuf()?;
    let mut dec = miharashi_video::Decoder::open(&o.file, &node)?;
    let (vw, vh) = (dec.width, dec.height);
    println!(
        "video: {} {}x{} {:.2}fps codec={} (VA-API {})",
        o.file.display(),
        vw,
        vh,
        dec.fps,
        dec.codec_name,
        node.display()
    );
    let fps = if o.fps > 0 {
        o.fps as f64
    } else {
        dec.fps.max(1.0)
    };
    // 画面の中央に、縦横比を保って収める(左右は説明や入出力の表示に使う想定)
    let s = (o.size.0 as f32 / vw as f32).min(o.size.1 as f32 / vh as f32);
    let rect = [
        (o.size.0 as f32 - vw as f32 * s) / 2.0,
        (o.size.1 as f32 - vh as f32 * s) / 2.0,
        vw as f32 * s,
        vh as f32 * s,
    ];
    let (atlas_rgba, atlas_w, cell, widths) = label_atlas(&o.font, o.detections);

    let (result, img) = off.with_fbo(o.size, |gl| -> Result<(), String> {
        println!("gl: {}", miharashi_gpu::connectome::caps(gl));
        let scene = VideoScene::new(
            gl,
            &video::Atlas {
                rgba: &atlas_rgba,
                width: atlas_w,
                cell,
                widths: &widths,
            },
        )?;
        let mut timer = Timer::new(gl)?;
        let mut tracks = Tracks::new(o.detections, (vw, vh), 1);
        // GPU が読み終えるまで(Timer は 2 フレーム前の完了を待つ)サーフェスと EGLImage を持っておく
        let mut held: VecDeque<(miharashi_video::Frame, [video::Image; 2])> = VecDeque::new();
        println!(
            "run: {}x{} fps={fps:.2} detections={} {}s",
            o.size.0,
            o.size.1,
            o.detections,
            o.duration.as_secs()
        );
        let mut cpu = SelfCpu::default();
        let mut cpu_total = SelfCpu::default();
        let mut rapl = RaplTracker::new(&o.rapl_helper);
        let now = Instant::now();
        cpu.poll(now);
        cpu_total.poll(now);
        rapl.poll(now);
        let (mut sec, mut all) = (Window::default(), Window::default());
        let (mut dec_s, mut dec_all, mut imp_s, mut imp_all) = (vec![], vec![], vec![], vec![]);
        let (mut busy_s, mut watts_s) = (Vec::new(), Vec::new());
        let period = Duration::from_secs_f64(1.0 / fps);
        let start = Instant::now();
        let (mut next, mut next_print) = (start, Duration::from_secs(1));
        while start.elapsed() < o.duration {
            let t0 = Instant::now();
            let frame = dec.next_frame()?;
            let t1 = Instant::now();
            let layers = frame.layers();
            if layers.len() != 2 {
                return Err(format!("NV12 の 2 枚(Y、UV)で受け取れない: {layers:?}"));
            }
            let conv = |l: &miharashi_video::Layer| video::Layer {
                fd: l.fd,
                fourcc: l.fourcc,
                width: l.width,
                height: l.height,
                offset: l.offset,
                pitch: l.pitch,
                modifier: l.modifier,
            };
            let images = scene.import(gl, &dmabuf, &conv(&layers[0]), &conv(&layers[1]))?;
            let t2 = Instant::now();
            let dets = tracks.step(period.as_secs_f32());
            timer.begin(gl, 0);
            scene.draw_video(gl, o.size, rect);
            timer.end(gl);
            timer.begin(gl, 1);
            scene.draw_overlay(gl, o.size, rect, (vw, vh), &dets);
            timer.end(gl);
            let (gpu_step, gpu_draw, wait) = timer.finish(gl);
            held.push_back((frame, images));
            while held.len() > 3 {
                let (f, imgs) = held.pop_front().unwrap();
                for i in imgs {
                    dmabuf.release(i);
                }
                drop(f);
            }
            let t = FrameTimes {
                gpu_step,
                gpu_draw,
                submit: (t0.elapsed() - wait).as_secs_f32() * 1000.0,
                wait: wait.as_secs_f32() * 1000.0,
            };
            let (d_ms, i_ms) = (
                (t1 - t0).as_secs_f32() * 1000.0,
                (t2 - t1).as_secs_f32() * 1000.0,
            );
            sec.push(&t);
            all.push(&t);
            dec_s.push(d_ms);
            dec_all.push(d_ms);
            imp_s.push(i_ms);
            imp_all.push(i_ms);
            let elapsed = start.elapsed();
            if elapsed >= next_print {
                next_print += Duration::from_secs(1);
                let now = Instant::now();
                let (c, w, b) = (cpu.poll(now), rapl.poll(now), busy());
                watts_s.extend(w);
                busy_s.extend(b);
                println!(
                    "t={:>4}s frames={:>3} gpu={} (video {} / overlay {}) decode={} import={} submit={} cpu={}% busy={}% W={}",
                    elapsed.as_secs(),
                    sec.frames,
                    fmt_ms(&mut sec.total),
                    fmt_ms(&mut sec.step),
                    fmt_ms(&mut sec.draw),
                    fmt_ms(&mut dec_s),
                    fmt_ms(&mut imp_s),
                    fmt_ms(&mut sec.submit),
                    fmt_opt(c, 1),
                    fmt_opt(b, 0),
                    fmt_opt(w, 1),
                );
                sec = Window::default();
                dec_s.clear();
                imp_s.clear();
            }
            next += period;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        let cpu_run = cpu_total.poll(Instant::now());
        // GPU が読み終えてから捨てる
        timer.destroy(gl);
        for (f, imgs) in held.drain(..) {
            for i in imgs {
                dmabuf.release(i);
            }
            drop(f);
        }
        scene.destroy(gl);
        let avg = |v: &[f32]| (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32);
        let fps_run = all.frames as f32 / o.duration.as_secs_f32();
        let gpu_load = avg(&all.total).map(|ms| ms * fps_run / 10.0);
        println!(
            "summary: frames={} fps={:.1} gpu={} gpu_load={}% video={} overlay={} decode={} import={} submit={} cpu={}% busy={}% W={} loops={}",
            all.frames,
            fps_run,
            fmt_ms(&mut all.total),
            fmt_opt(gpu_load, 0),
            fmt_ms(&mut all.step),
            fmt_ms(&mut all.draw),
            fmt_ms(&mut dec_all),
            fmt_ms(&mut imp_all),
            fmt_ms(&mut all.submit),
            fmt_opt(cpu_run, 2),
            fmt_opt(avg(&busy_s), 0),
            fmt_opt(avg(&watts_s), 1),
            dec.loops,
        );
        println!(
            "        (ms は 平均/p95。decode はデコーダーから 1 枚受け取るまで、import は dmabuf を EGLImage にしてテクスチャに結びつけるまで。submit は両方を含む CPU 側の時間)"
        );
        Ok(())
    })?;
    result?;
    save_png(o.out.as_deref(), o.size, img)
}

/// `bench-scene` の条件。
pub struct SceneOpts {
    pub pci: String,
    pub sizes: Vec<(u32, u32)>,
    /// 連続で描くフレーム数(ウォームアップを除く)
    pub frames: usize,
    /// 0 なら連続だけ。0 より大きいと、そのあとに 1 秒あたりこの数のフレームで `seconds` 秒も測る
    pub fps: u32,
    pub seconds: u64,
}

/// `--size` が無いときに測る 3 つの大きさ。
pub const SCENE_SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (2560, 1080)];

pub fn scene_sizes(opt: Option<(u32, u32)>) -> Vec<(u32, u32)> {
    opt.map_or_else(|| SCENE_SIZES.to_vec(), |s| vec![s])
}

/// 最初の数フレーム(シェーダーのコンパイル、GPU のクロックの立ち上がり)は集計しない。
const WARMUP: usize = 30;
const WARMUP_PACED: usize = 10;

/// 1 フレームの計測値。
struct Sample {
    /// timer query の GPU 時間 [ms]。無いとき、時計が飛んだときは None
    gpu: Option<f32>,
    /// 命令を出し始めてから `glFinish` が返るまで [ms]
    wall: f32,
    /// 計測の外で先にした CPU の仕事(下地の作成)[ms]。無ければ None
    pre: Option<f32>,
}

#[derive(Default)]
struct Samples {
    gpu: Vec<f32>,
    wall: Vec<f32>,
    pre: Vec<f32>,
    /// pre(無ければ 0)+ wall
    total: Vec<f32>,
}

impl Samples {
    fn push(&mut self, s: &Sample) {
        self.gpu.extend(s.gpu);
        self.wall.push(s.wall);
        self.pre.extend(s.pre);
        self.total.push(s.pre.unwrap_or(0.0) + s.wall);
    }
}

/// 1 フレームを `glFinish` まで測る。timer query があれば GPU 時間も取る(フレームごとに待つので、重ならない 1 枚分)。
struct Probe {
    query: Option<glow::Query>,
}

impl Probe {
    fn new(gl: &glow::Context) -> Self {
        let query = gl
            .supported_extensions()
            .contains("GL_EXT_disjoint_timer_query")
            .then(|| unsafe { gl.create_query().ok() })
            .flatten();
        Self { query }
    }

    fn method(&self) -> &'static str {
        if self.query.is_some() {
            "timer-query(GL_EXT_disjoint_timer_query)+wall(glFinish)"
        } else {
            "wall(glFinish)"
        }
    }

    fn run(&self, gl: &glow::Context, pre: Option<f32>, f: impl FnOnce()) -> Sample {
        unsafe {
            if self.query.is_some() {
                // 前の回に立った「時計が飛んだ」を持ち越さない
                gl.get_parameter_i32(0x8FBB);
            }
            let t0 = Instant::now();
            if let Some(q) = self.query {
                gl.begin_query(0x88BF, q);
            }
            f();
            if self.query.is_some() {
                gl.end_query(0x88BF);
            }
            gl.finish();
            let wall = t0.elapsed().as_secs_f32() * 1000.0;
            let gpu = self.query.and_then(|q| {
                let ok = gl.get_query_parameter_u32(q, glow::QUERY_RESULT_AVAILABLE) != 0
                    && gl.get_parameter_i32(0x8FBB) == 0;
                ok.then(|| gl.get_query_parameter_u64(q, glow::QUERY_RESULT) as f32 / 1e6)
            });
            Sample { gpu, wall, pre }
        }
    }

    fn destroy(self, gl: &glow::Context) {
        if let Some(q) = self.query {
            unsafe { gl.delete_query(q) };
        }
    }
}

/// `step(i)` を `warmup + frames` 回呼び、ウォームアップを除いて集める。`period` があればその間隔で呼ぶ。
fn measure(
    frames: usize,
    warmup: usize,
    period: Option<Duration>,
    mut step: impl FnMut(usize) -> Sample,
) -> Samples {
    let mut out = Samples::default();
    let mut next = Instant::now();
    for i in 0..warmup + frames {
        let s = step(i);
        if i >= warmup {
            out.push(&s);
        }
        if let Some(p) = period {
            next += p;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    }
    out
}

fn report_line(size: (u32, u32), case: &str, pace: &str, s: &mut Samples, method: &str) -> String {
    let mut line = format!(
        "{}x{} {case:<34} {pace:<10} gpu={} wall={}",
        size.0,
        size.1,
        fmt_ms(&mut s.gpu),
        fmt_ms(&mut s.wall),
    );
    if !s.pre.is_empty() {
        line += &format!(
            " +下地作成(CPU)={} → 合計wall={}",
            fmt_ms(&mut s.pre),
            fmt_ms(&mut s.total)
        );
    }
    format!("{line} ms 平均/p95 ({} フレーム) [{method}]", s.wall.len())
}

/// 色と深度つきの FBO(層を挟まず直接描く比較用)。
struct DirectFbo {
    fbo: glow::Framebuffer,
    color: glow::Renderbuffer,
    depth: glow::Renderbuffer,
    size: (u32, u32),
    /// 画面の大きさの色だけの FBO。`draw_scene` が毎フレームする全画面の消去に合わせるために消す
    screen: (glow::Framebuffer, glow::Renderbuffer),
    screen_size: (u32, u32),
}

impl DirectFbo {
    fn new(gl: &glow::Context, size: (u32, u32), screen_size: (u32, u32)) -> Result<Self, String> {
        unsafe {
            let color = gl.create_renderbuffer()?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::RGBA8,
                size.0 as i32,
                size.1 as i32,
            );
            let depth = gl.create_renderbuffer()?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::DEPTH24_STENCIL8,
                size.0 as i32,
                size.1 as i32,
            );
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(color),
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_STENCIL_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(depth),
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            let screen_color = gl.create_renderbuffer()?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(screen_color));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::RGBA8,
                screen_size.0 as i32,
                screen_size.1 as i32,
            );
            let screen_fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(screen_fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(screen_color),
            );
            let screen_status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_renderbuffer(glow::RENDERBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE || screen_status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!(
                    "直接描く FBO が不完全: 0x{status:x} / 画面側 0x{screen_status:x}"
                ));
            }
            Ok(Self {
                fbo,
                color,
                depth,
                size,
                screen: (screen_fbo, screen_color),
                screen_size,
            })
        }
    }

    /// 画面全体を黒で消してから、消去(色・深度・ステンシル)して場面を描く(層の FBO と同じ中身。
    /// `reset_gl_state` は挟まない)。画面の消去は `draw_scene` の「下地なし」の黒の消去に合わせた。
    fn draw(&self, gl: &glow::Context, scene: &mut dyn GlScene) -> Result<(), String> {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.screen.0));
            gl.viewport(0, 0, self.screen_size.0 as i32, self.screen_size.1 as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.viewport(0, 0, self.size.0 as i32, self.size.1 as i32);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear_depth_f32(1.0);
            gl.clear_stencil(0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT);
        }
        scene.draw(
            gl,
            &GlTarget {
                fbo: self.fbo,
                size: self.size,
            },
        )
    }

    fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(self.fbo);
            gl.delete_renderbuffer(self.color);
            gl.delete_renderbuffer(self.depth);
            gl.delete_framebuffer(self.screen.0);
            gl.delete_renderbuffer(self.screen.1);
        }
    }
}

/// `bench-scene`: キューブ(`CubeScene`)の 1 フレームの GPU 時間を、`draw_scene`(下地 → 層 → 合成)と、
/// 層を挟まず直接描く場合とで測る(Issue #74 T4)。表示カードの render node に surfaceless で描く(KMS には出さない)。
pub fn run_scene(o: &SceneOpts) -> Result<(), String> {
    let node = miharashi_gpu::render_node_for_pci(&o.pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("render node: {}", node.display());
    for &size in &o.sizes {
        let (r, _) = off.render_with_compositor(size, |gl, comp, target| {
            scene_size(gl, comp, target, size, o)
        })?;
        r?;
    }
    println!(
        "(gpu = timer query の GPU 時間。wall = 命令を出し始めてから glFinish が返るまで(CPU の壁時計、GPU 待ちを含む)。\
         連続 = 待たずに続けて描く(GPU のクロックが上がった状態)、{}fps = 一定の間隔で描く(クロックが下がった状態)。\
         ウォームアップ {WARMUP}/{WARMUP_PACED} フレームは除く)",
        o.fps
    );
    Ok(())
}

fn scene_size(
    gl: &glow::Context,
    comp: &mut miharashi_gpu::compose::Compositor,
    target: Option<glow::Framebuffer>,
    size: (u32, u32),
    o: &SceneOpts,
) -> Result<(), String> {
    let probe = Probe::new(gl);
    let method = probe.method();
    if size == o.sizes[0] {
        println!("gl: {}", connectome::caps(gl));
        println!("測り方: {method}");
    }
    let count = Rc::new(ObjectCount::default());
    let mut cube = CubeScene::new(count);
    let full = CubeScreen::region(false, size);
    let third = CubeScreen::region(true, size);
    let mut canvas = Canvas::new(size.0, size.1);
    let mut paces = vec![("連続".to_string(), None, o.frames, WARMUP)];
    if o.fps > 0 {
        let frames = (o.fps as u64 * o.seconds).max(1) as usize;
        let period = Duration::from_secs_f64(1.0 / f64::from(o.fps));
        paces.push((format!("{}fps", o.fps), Some(period), frames, WARMUP_PACED));
    }
    let mut result = Ok(());
    let mut flip = false;
    for (pace, period, frames, warmup) in paces {
        let line = |case: &str, mut s: Samples| {
            println!("{}", report_line(size, case, &pace, &mut s, method));
        };
        // 何も描かない glFinish だけ(計測の床)
        line(
            "noop(glFinish のみ)",
            measure(frames, warmup, period, |_| probe.run(gl, None, || {})),
        );
        // draw_scene が呼ぶ reset_gl_state(下地、層、合成の 3 回)だけ
        line(
            "reset_gl_state x3",
            measure(frames, warmup, period, |_| {
                probe.run(gl, None, || {
                    for _ in 0..3 {
                        miharashi_gpu::compose::Compositor::reset_gl_state(gl, size, target);
                    }
                })
            }),
        );
        for (name, region) in [("全面", full), ("1/3 幅", third)] {
            let layered = |name: &str,
                           base: bool,
                           comp: &mut miharashi_gpu::compose::Compositor,
                           cube: &mut CubeScene,
                           canvas: &mut Canvas,
                           r: &mut Result<(), String>|
             -> Samples {
                measure(frames, warmup, period, |i| {
                    cube.set_secs(i as f32 / 30.0);
                    let t0 = Instant::now();
                    if base {
                        canvas.clear(rgba(
                            THIRD_BASE[0],
                            THIRD_BASE[1],
                            THIRD_BASE[2],
                            THIRD_BASE[3],
                        ));
                        canvas.fill_rect(
                            region.x as f32,
                            region.y as f32,
                            region.w as f32,
                            region.h as f32,
                            rgba(THIRD_BAND[0], THIRD_BAND[1], THIRD_BAND[2], THIRD_BAND[3]),
                        );
                    }
                    let pre = base.then(|| t0.elapsed().as_secs_f32() * 1000.0);
                    let base = base.then(|| Frame::Image(canvas.pixmap.data()));
                    probe.run(gl, pre, || {
                        let mut frame = SceneFrame {
                            base,
                            scene: cube,
                            region,
                            top: SceneTop::Off,
                        };
                        match comp.draw_scene(gl, target, &mut frame) {
                            Ok(SceneOutcome::Drawn) => {}
                            Ok(SceneOutcome::SceneFailed(m)) => {
                                *r = Err(format!("{name}: 3D の失敗: {m}"))
                            }
                            Err(e) => *r = Err(format!("{name}: {e}")),
                        }
                    })
                })
            };
            let direct = DirectFbo::new(gl, (region.w, region.h), size)?;
            let run_direct = |cube: &mut CubeScene, r: &mut Result<(), String>| {
                measure(frames, warmup, period, |i| {
                    cube.set_secs(i as f32 / 30.0);
                    probe.run(gl, None, || {
                        if let Err(e) = direct.draw(gl, cube) {
                            *r = Err(format!("{name} 直接: {e}"));
                        }
                    })
                })
            };
            // 測る順番の偏り(GPU のクロックや温度)を減らすため、直接を先に測るか後に測るかを交互にする
            let direct_first = flip;
            flip = !flip;
            let mut direct_s = direct_first.then(|| run_direct(&mut cube, &mut result));
            line(
                &format!("{name} draw_scene(下地なし=黒)"),
                layered(name, false, comp, &mut cube, &mut canvas, &mut result),
            );
            if region.w != size.0 {
                line(
                    &format!("{name} draw_scene(下地=CubeScreen)"),
                    layered(name, true, comp, &mut cube, &mut canvas, &mut result),
                );
            }
            let s = direct_s
                .take()
                .unwrap_or_else(|| run_direct(&mut cube, &mut result));
            direct.destroy(gl);
            line(
                &format!("{name} 直接(層なし、色+深度の FBO + 全画面の消去)"),
                s,
            );
            result.clone()?;
        }
    }
    cube.release(gl);
    probe.destroy(gl);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use miharashi_gpu::Offscreen;

    #[test]
    fn mean_p95_of_known_values() {
        let mut v: Vec<f32> = (1..=100).map(|x| x as f32).collect();
        let (m, p) = mean_p95(&mut v).unwrap();
        assert!((m - 50.5).abs() < 1e-4);
        assert_eq!(p, 95.0);
        assert!(mean_p95(&mut []).is_none());
    }

    #[test]
    fn scene_sizes_defaults_to_three_and_honors_size() {
        assert_eq!(
            scene_sizes(None),
            vec![(1280, 720), (1920, 1080), (2560, 1080)]
        );
        assert_eq!(scene_sizes(Some((640, 360))), vec![(640, 360)]);
    }

    #[test]
    fn samples_keep_gpu_separate_and_add_pre_to_total() {
        let mut s = Samples::default();
        s.push(&Sample {
            gpu: Some(1.0),
            wall: 2.0,
            pre: Some(0.5),
        });
        s.push(&Sample {
            gpu: None,
            wall: 4.0,
            pre: Some(0.5),
        });
        assert_eq!(s.gpu, vec![1.0]);
        assert_eq!(s.wall, vec![2.0, 4.0]);
        assert_eq!(s.total, vec![2.5, 4.5]);
    }

    #[test]
    fn measure_skips_warmup_and_report_line_shows_method() {
        let mut calls = 0;
        let mut s = measure(5, 3, None, |_| {
            calls += 1;
            Sample {
                gpu: None,
                wall: 1.0,
                pre: None,
            }
        });
        assert_eq!((calls, s.wall.len()), (8, 5));
        let line = report_line((1920, 1080), "case", "連続", &mut s, "wall(glFinish)");
        assert!(line.starts_with("1920x1080 case"), "{line}");
        assert!(line.contains("gpu=- wall=1.00/1.00"), "{line}");
        assert!(line.ends_with("(5 フレーム) [wall(glFinish)]"), "{line}");
        assert!(!line.contains("下地作成"), "{line}");
    }

    #[test]
    fn scene_bench_runs_on_a_small_size() {
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let o = SceneOpts {
            pci: String::new(),
            sizes: vec![(96, 54)],
            frames: 3,
            fps: 0,
            seconds: 1,
        };
        let (r, _) = off
            .render_with_compositor((96, 54), |gl, comp, target| {
                scene_size(gl, comp, target, (96, 54), &o)
            })
            .unwrap();
        r.unwrap();
    }
}
