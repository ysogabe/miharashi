//! `vrm-view`: VRM 1.0 モデルを実際のモニター(DRM/KMS)に表示する PoC(Issue #71 B の下調べ)。
//! `--photos` で、写真画面と同じ GPU 経路の写真を背景に描き、その上の領域(`--region`)にバストアップなど
//! (`--frame`)のキャラクターを重ねて、身振り(`--gesture`)と口パク(`--lipsync`)の負荷を測れる。
//! キオスク(miharashi.service)とは DRM master を取り合うので、止めてから実行する。libseat は使わず、
//! カードを直接 open する。終了(`--seconds` の経過、SIGINT/SIGTERM)で元の画面に戻る。
//! モデルは利用条件を確認して各自で用意する(リポジトリには置かない)。`--synthetic` なら合成モデル。
use glow::HasContext;
use miharashi_gpu::GlesDisplay;
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::sysinfo::SelfCpu;
use miharashi_vrm::demo::FaceSet;
use miharashi_vrm::photo_stage::{self, Slideshow};
use miharashi_vrm::synth;
use miharashi_vrm::{Framing, Level, Model, Region, Renderer, VrmLayer, demo};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const USAGE: &str = "使い方: vrm-view (--model <path.vrm> | --synthetic) [--card driver:amdgpu] [--connector auto] \
[--mode preferred] [--seconds 60] [--expression blink] [--fps 30] [--feature-level spring] [--outline-scale 1] \
[--frame full|waist|bust] [--region left|center|full] [--gesture] [--lipsync] \
[--photos <dir|synthetic>] [--photo-interval 10] [--no-vrm]";

/// fps などを標準エラーに出す間隔
const STATS_EVERY: Duration = Duration::from_secs(5);

struct Args {
    model: Option<PathBuf>,
    card: String,
    connector: String,
    mode: String,
    seconds: f64,
    expression: String,
    /// 指定すると、フレームの間隔をこの fps に保つ(既定は vsync のまま)
    fps: Option<f64>,
    level: Level,
    outline_scale: f32,
    frame: Framing,
    region: Region,
    gesture: bool,
    lipsync: bool,
    /// 背景に写真を描く。`synthetic` か、JPEG の入ったディレクトリ(なければ合成写真)
    photos: Option<String>,
    photo_interval: f64,
    /// キャラクターを描かず、写真だけを描く(VRM の上乗せを出すための基準)
    no_vrm: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        model: None,
        card: "driver:amdgpu".into(),
        connector: "auto".into(),
        mode: "preferred".into(),
        seconds: 60.0,
        expression: "blink".into(),
        fps: None,
        level: Level::Spring,
        outline_scale: 1.0,
        frame: Framing::Full,
        region: Region::Full,
        gesture: false,
        lipsync: false,
        photos: None,
        photo_interval: 10.0,
        no_vrm: false,
    };
    let mut synthetic = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} に値がない\n{USAGE}"));
        match flag.as_str() {
            "--model" => a.model = Some(value()?.into()),
            "--synthetic" => synthetic = true,
            "--card" => a.card = value()?,
            "--connector" => a.connector = value()?,
            "--mode" => a.mode = value()?,
            "--seconds" => a.seconds = value()?.parse().map_err(|_| "--seconds が数でない")?,
            "--expression" => a.expression = value()?,
            "--fps" => a.fps = Some(value()?.parse().map_err(|_| "--fps が数でない")?),
            "--feature-level" => a.level = Level::parse(&value()?)?,
            "--outline-scale" => {
                a.outline_scale = value()?.parse().map_err(|_| "--outline-scale が数でない")?
            }
            "--frame" => a.frame = Framing::parse(&value()?)?,
            "--region" => a.region = Region::parse(&value()?)?,
            "--gesture" => a.gesture = true,
            "--lipsync" => a.lipsync = true,
            "--photos" => a.photos = Some(value()?),
            "--photo-interval" => {
                a.photo_interval = value()?
                    .parse()
                    .map_err(|_| "--photo-interval が数でない")?
            }
            "--no-vrm" => a.no_vrm = true,
            _ => return Err(format!("不明な引数 {flag}\n{USAGE}")),
        }
    }
    if a.no_vrm && a.photos.is_none() {
        return Err("--no-vrm は --photos と一緒に使う(写真だけを描く)".into());
    }
    if a.photo_interval.partial_cmp(&1.0).is_none_or(|o| o.is_lt()) {
        return Err("--photo-interval は 1 以上の数".into());
    }
    if a.model.is_none() && !synthetic && !a.no_vrm {
        return Err(format!("--model か --synthetic が要る\n{USAGE}"));
    }
    if a.seconds.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err("--seconds は 0 より大きい数".into());
    }
    if a.fps
        .is_some_and(|f| f.partial_cmp(&1.0).is_none_or(|o| o.is_lt()))
    {
        return Err("--fps は 1 以上の数".into());
    }
    Ok(a)
}

/// CPU 時間の内訳 [ms]の合計(窓ごと・全体)。
#[derive(Default, Clone)]
struct Sum {
    frames: u32,
    frame: Duration,
    pose: f64,
    spring: f64,
    /// 表情の合成(`set_expressions` + 頂点の合成・転送)
    face: f64,
    /// 写真の合成(`Compositor::draw` の命令の発行まで。1 枚目の完成フレーム作りを含む)
    photo_draw: f64,
    /// VRM の描画命令の発行(層への描画と合成)
    vrm_draw: f64,
    /// 写真 1 枚ごとの「完成フレームを作る」CPU 時間
    builds: Vec<f32>,
}

impl Sum {
    fn add(&mut self, o: &Sum) {
        self.frames += o.frames;
        self.frame += o.frame;
        self.pose += o.pose;
        self.spring += o.spring;
        self.face += o.face;
        self.photo_draw += o.photo_draw;
        self.vrm_draw += o.vrm_draw;
        self.builds.extend_from_slice(&o.builds);
    }

    fn line(&self, secs: f64, cpu: Option<f32>) -> String {
        let n = self.frames.max(1) as f64;
        let build = if self.builds.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{}枚 平均{:.1}/最大{:.1}ms",
                self.builds.len(),
                self.builds.iter().sum::<f32>() / self.builds.len() as f32,
                self.builds.iter().copied().fold(0.0, f32::max)
            )
        };
        format!(
            "{:.1} fps  frame {:.2}ms  cpu {}  | CPU/フレーム ms: pose {:.3} spring {:.3} face {:.3} photo {:.3} vrm_draw {:.3}  | 写真の準備 {build}",
            self.frames as f64 / secs,
            self.frame.as_secs_f64() * 1e3 / n,
            cpu.map_or("-".into(), |c| format!("{c:.1}%")),
            self.pose / n,
            self.spring / n,
            self.face / n,
            self.photo_draw / n,
            self.vrm_draw / n,
        )
    }
}

/// VRM を描くのに要るもの(`--no-vrm` では持たない)。
struct Vrm {
    renderer: Renderer,
    layer: VrmLayer,
    faces: FaceSet,
}

fn run(a: &Args) -> Result<(), String> {
    let term = Arc::new(AtomicBool::new(false));
    // 1 回目はフラグで正常終了(画面を元に戻す)、2 回目は即終了。条件付きの登録を先にすること
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register_conditional_shutdown(sig, 128 + sig, term.clone())
            .map_err(|e| format!("signal: {e}"))?;
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }

    // モデルと写真は画面を取る前に読む(読めなければキオスクの画面を触らずに終わる)
    let model = if a.no_vrm {
        None
    } else {
        let t_load = Instant::now();
        let model = match &a.model {
            Some(p) => Model::from_path(p)?,
            None => Model::from_glb(&synth::build_vrm_glb(&synth::Opts::default()))?,
        };
        let st = model.stats();
        eprintln!(
            "model: {} (VRM {}) meshes={} vertices={} triangles={} textures={} morph_targets={} ({:.0}ms)",
            a.model
                .as_deref()
                .map_or("synthetic".into(), |p| p.display().to_string()),
            model.vrm.spec_version,
            st.meshes,
            st.vertices,
            st.triangles,
            st.images.len(),
            st.morph_targets,
            t_load.elapsed().as_secs_f64() * 1e3
        );
        if model.vrm.expression(&a.expression).is_none() {
            return Err(format!("expression {} がない", a.expression));
        }
        if a.lipsync && FaceSet::of(&model.vrm).no_vowels() {
            return Err("口パクに使う表情(aa/ih/ou/ee/oh)がモデルにない".into());
        }
        Some(model)
    };
    let photos = match &a.photos {
        Some(p) => {
            let t = Instant::now();
            let dir = (p != "synthetic").then(|| Path::new(p));
            let (imgs, jpegs) = photo_stage::load(dir)?;
            eprintln!(
                "photos: {}枚({}) {:.0}ms",
                imgs.len(),
                if jpegs == 0 {
                    "合成写真".to_string()
                } else {
                    format!("JPEG {jpegs} 枚")
                },
                t.elapsed().as_secs_f64() * 1e3
            );
            Some(imgs)
        }
        None => None,
    };

    // 表示先を決めて、カードを直接 open する
    let cards = drm_select::scan();
    let card = drm_select::choose_card(&cards, &CardSel::parse(&a.card))?;
    let connector = drm_select::choose_connector(card, &a.connector)?;
    eprintln!(
        "display: {} driver={} pci={} connector={}",
        card.path.display(),
        card.driver,
        card.pci.as_deref().unwrap_or("-"),
        connector.name
    );
    let mode = ModeSel::parse(&a.mode)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&card.path)
        .map_err(|e| format!("{}: {e}", card.path.display()))?;
    let mut display = GlesDisplay::open(file.into(), &connector.name, mode)?;
    let size = display.size();
    eprintln!(
        "mode: {}x{}@{}Hz  gles: {}",
        size.0,
        size.1,
        display.refresh(),
        display.describe()
    );
    let (rx, ry, rw, rh) = a.region.rect(size);
    eprintln!(
        "composition: frame={} region={} ({rw}x{rh} at {rx},{ry}) gesture={} lipsync={} photos={} vrm={}",
        a.frame.name(),
        a.region.name(),
        a.gesture,
        a.lipsync,
        a.photos.as_deref().unwrap_or("なし"),
        if a.no_vrm { "なし" } else { "あり" },
    );

    // GL の初期化(Renderer と描画先)
    let init = display.with_gl(|gl, _| -> Result<Option<Vrm>, String> {
        unsafe {
            eprintln!(
                "gl: GL_RENDERER={} GL_VERSION={}",
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION)
            );
        }
        let Some(model) = model else { return Ok(None) };
        let faces = FaceSet::of(&model.vrm);
        let layer = VrmLayer::new(gl, (rw, rh))?;
        match Renderer::with_level(gl, model, a.level) {
            Ok(mut r) => {
                r.outline_scale = a.outline_scale;
                // 透明な層に描いて、写真(または暗い背景)に重ねる
                r.clear_color = [0.0; 4];
                if a.gesture {
                    if let Err(e) = r.set_gesture(true) {
                        r.destroy(gl);
                        layer.destroy(gl);
                        return Err(e);
                    }
                    let (arms, fingers) = r.rig().gesture_bones();
                    eprintln!("gesture: 腕 {arms} 本、指の関節 {fingers} 個");
                }
                Ok(Some(Vrm {
                    renderer: r,
                    layer,
                    faces,
                }))
            }
            Err(e) => {
                layer.destroy(gl);
                Err(e)
            }
        }
    });
    let mut vrm = init?;
    let mut show = photos.map(|imgs| {
        Slideshow::new(
            imgs,
            size,
            Duration::from_secs_f64(a.photo_interval),
            Instant::now(),
        )
    });

    // 描画ループ。エラーでも後始末(destroy → GlesDisplay の drop)を通るよう、結果を持ち越す
    let result = view_loop(a, &mut display, vrm.as_mut(), show.as_mut(), &term);

    display.with_gl(|gl, _| {
        if let Some(v) = vrm {
            v.renderer.destroy(gl);
            v.layer.destroy(gl);
        }
        unsafe { gl.finish() };
    });
    // display の drop で元の CRTC に戻る
    drop(display);
    result
}

fn view_loop(
    a: &Args,
    display: &mut GlesDisplay,
    mut vrm: Option<&mut Vrm>,
    mut show: Option<&mut Slideshow>,
    term: &AtomicBool,
) -> Result<(), String> {
    let size = display.size();
    let (rx, ry, rw, rh) = a.region.rect(size);
    // 全身・全画面・写真なし・身振りなしのときだけ、従来どおりカメラを左右に振る
    let sway =
        a.frame == Framing::Full && a.region == Region::Full && !a.gesture && a.photos.is_none();
    let start = Instant::now();
    let run_for = Duration::from_secs_f64(a.seconds);
    let mut cpu = SelfCpu::default();
    let mut cpu_total = SelfCpu::default();
    cpu.poll(start);
    cpu_total.poll(start);
    let mut next_stats = start + STATS_EVERY;
    let (mut win, mut total) = (Sum::default(), Sum::default());
    let mut last = Instant::now();
    let mut total_frames = 0u64;
    let mut prev_t = 0.0f32;
    let mut face: Vec<(&'static str, f32)> = Vec::new();
    while !term.load(Ordering::Relaxed) && start.elapsed() < run_for {
        if let Some(fps) = a.fps {
            // 次のフレームの時刻まで待つ(遅れていれば待たない)。そのあと present が vsync を待つ
            let next = start + Duration::from_secs_f64(total_frames as f64 / fps);
            // SIGTERM に反応できるよう、100ms ずつに分けて待つ
            while !term.load(Ordering::Relaxed) {
                let Some(d) = next.checked_duration_since(Instant::now()) else {
                    break;
                };
                std::thread::sleep(d.min(Duration::from_millis(100)));
            }
        }
        let now = Instant::now();
        let t = (now - start).as_secs_f32();
        let t0 = now;
        let mut sum = Sum::default();
        if let Some(v) = vrm.as_deref_mut() {
            v.renderer.animate(t, t - prev_t);
            let f0 = Instant::now();
            if a.lipsync {
                // 口の母音・まばたき・笑顔を同時に合成する
                demo::face(t, &v.faces, &mut face);
                v.renderer.set_expressions(&face)?;
            } else {
                v.renderer
                    .set_expression(&a.expression, demo::expression_weight(&a.expression, t))?;
            }
            sum.face = f0.elapsed().as_secs_f64() * 1e3;
        }
        prev_t = t;
        if let Some(s) = show.as_deref_mut() {
            s.update(now);
        }
        let has_vrm = vrm.is_some();
        let yaw = if sway { demo::camera_yaw(t) } else { 0.0 };
        let enter = Instant::now();
        // 層へ描いて、写真(または暗い背景)の上の領域に重ねる。GL を借りた中で測る
        let mut draw_vrm = |gl: &glow::Context, sum: &mut Sum, with_bg: bool| {
            let Some(v) = vrm.as_deref_mut() else { return };
            let t1 = Instant::now();
            let times = v.renderer.update(gl);
            if with_bg {
                unsafe {
                    gl.clear_color(0.08, 0.09, 0.12, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
            }
            let cam = v.renderer.framing_camera(a.frame, (rw, rh), yaw);
            v.layer.bind(gl);
            v.renderer.render_camera(gl, (rw, rh), &cam.0, cam.1);
            v.layer.composite(gl, None, (rx as i32, ry as i32));
            sum.pose = times.pose_ms;
            sum.spring = times.spring_ms;
            sum.face += times.morph_ms;
            sum.vrm_draw = t1.elapsed().as_secs_f64() * 1e3 - times.pose_ms - times.morph_ms;
        };
        // present が page flip の完了まで待つので、フレームの間隔は vsync でとれる
        match show.as_deref() {
            Some(s) => {
                let frame = s.frame(now);
                if has_vrm {
                    display.present_frame_with(&frame, |gl, _| {
                        sum.photo_draw = enter.elapsed().as_secs_f64() * 1e3;
                        draw_vrm(gl, &mut sum, false);
                    })?;
                } else {
                    display.present_frame(&frame)?;
                    sum.photo_draw = enter.elapsed().as_secs_f64() * 1e3;
                }
            }
            None => {
                display.present_with(|gl, _| draw_vrm(gl, &mut sum, true))?;
            }
        }
        if let Some(s) = show.as_deref_mut() {
            s.mark_sent();
        }
        sum.builds = display.take_build_ms();
        sum.frame = t0.elapsed();
        sum.frames = 1;
        win.add(&sum);
        total_frames += 1;
        let now = Instant::now();
        if now >= next_stats {
            let dt = now.duration_since(last).as_secs_f64();
            eprintln!("{}", win.line(dt, cpu.poll(now)));
            total.add(&win);
            win = Sum::default();
            last = now;
            next_stats = now + STATS_EVERY;
        }
    }
    total.add(&win);
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "終了: {total_frames} フレーム / {secs:.1}秒{}",
        if term.load(Ordering::Relaxed) {
            "(シグナル)"
        } else {
            ""
        }
    );
    eprintln!(
        "全体の平均: {}",
        total.line(secs, cpu_total.poll(Instant::now()))
    );
    Ok(())
}

fn main() {
    let result = parse_args().and_then(|a| run(&a));
    if let Err(e) = result {
        eprintln!("vrm-view: {e}");
        std::process::exit(1);
    }
}
