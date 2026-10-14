//! `miharashi bench-gpu`: 将来の重いエフェクトに向けた GPU のベンチマーク(PoC-005 PR4)。
//! B1 コネクトーム: MaleCNS 全体級(ニューロン 16 万、結合 300 万本)の合成グラフを、表示カードの
//! render node に surfaceless で描く。表示中の miharashi を止めずに測れる(KMS には出さない)。
//! 1 秒ごとに fps、GPU の時間、CPU 使用率、iGPU の使用率、CPU パッケージの電力を出す。
use miharashi_gpu::connectome::{self, FrameTimes, Lines, RunOpts, Scene};
use miharashi_linux::sysinfo::{RaplTracker, SelfCpu};
use std::path::{Path, PathBuf};
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
