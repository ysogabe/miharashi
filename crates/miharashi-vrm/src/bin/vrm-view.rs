//! `vrm-view`: VRM 1.0 モデルを実際のモニター(DRM/KMS)に表示する PoC(Issue #71 B の下調べ)。
//! キオスク(miharashi.service)とは DRM master を取り合うので、止めてから実行する。libseat は使わず、
//! カードを直接 open する。終了(`--seconds` の経過、SIGINT/SIGTERM)で元の画面に戻る。
//! モデルは利用条件を確認して各自で用意する(リポジトリには置かない)。`--synthetic` なら合成モデル。
use glow::HasContext;
use miharashi_gpu::GlesDisplay;
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::sysinfo::SelfCpu;
use miharashi_vrm::synth;
use miharashi_vrm::{Model, Renderer, attach_depth, demo};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const USAGE: &str = "使い方: vrm-view (--model <path.vrm> | --synthetic) [--card driver:amdgpu] [--connector auto] \
[--mode preferred] [--seconds 60] [--expression blink]";

/// fps などを標準エラーに出す間隔
const STATS_EVERY: Duration = Duration::from_secs(5);

struct Args {
    model: Option<PathBuf>,
    card: String,
    connector: String,
    mode: String,
    seconds: f64,
    expression: String,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        model: None,
        card: "driver:amdgpu".into(),
        connector: "auto".into(),
        mode: "preferred".into(),
        seconds: 60.0,
        expression: "blink".into(),
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
            _ => return Err(format!("不明な引数 {flag}\n{USAGE}")),
        }
    }
    if a.model.is_none() && !synthetic {
        return Err(format!("--model か --synthetic が要る\n{USAGE}"));
    }
    if a.seconds.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err("--seconds は 0 より大きい数".into());
    }
    Ok(a)
}

/// 深度付きの描画先。既定の framebuffer には深度が無いので、ここに描いて blit で画面へ写す。
struct Target {
    fbo: glow::Framebuffer,
    color: glow::Renderbuffer,
    depth: glow::Renderbuffer,
    size: (u32, u32),
}

impl Target {
    fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self, String> {
        unsafe {
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            let color = match gl.create_renderbuffer() {
                Ok(c) => c,
                Err(e) => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.delete_framebuffer(fbo);
                    return Err(e);
                }
            };
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::RGBA8,
                size.0 as i32,
                size.1 as i32,
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(color),
            );
            // 深度は attach_depth(DEPTH24_STENCIL8)。不完全なら attach_depth がエラーを返す
            match attach_depth(gl, size) {
                Ok(depth) => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    Ok(Self {
                        fbo,
                        color,
                        depth,
                        size,
                    })
                }
                Err(e) => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.delete_renderbuffer(color);
                    gl.delete_framebuffer(fbo);
                    Err(e)
                }
            }
        }
    }

    fn bind(&self, gl: &glow::Context) {
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo)) };
    }

    /// 描いた内容を既定の framebuffer(画面)へ写す。
    fn blit_to_screen(&self, gl: &glow::Context) {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }

    fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_renderbuffer(self.depth);
            gl.delete_renderbuffer(self.color);
            gl.delete_framebuffer(self.fbo);
        }
    }
}

fn run(a: &Args) -> Result<(), String> {
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }

    // モデルは画面を取る前に読む(読めなければキオスクの画面を触らずに終わる)
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

    // GL の初期化(Renderer と描画先)
    let init = display.with_gl(|gl, size| -> Result<(Renderer, Target), String> {
        unsafe {
            eprintln!(
                "gl: GL_RENDERER={} GL_VERSION={}",
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION)
            );
        }
        let target = Target::new(gl, size)?;
        match Renderer::new(gl, model) {
            Ok(r) => Ok((r, target)),
            Err(e) => {
                target.destroy(gl);
                Err(e)
            }
        }
    });
    let (mut renderer, target) = init?;

    // 描画ループ。エラーでも後始末(destroy → GlesDisplay の drop)を通るよう、結果を持ち越す
    let result = view_loop(a, &mut display, &mut renderer, &target, &term);

    display.with_gl(|gl, _| {
        renderer.destroy(gl);
        target.destroy(gl);
        unsafe { gl.finish() };
    });
    // display の drop で元の CRTC に戻る
    drop(display);
    result
}

fn view_loop(
    a: &Args,
    display: &mut GlesDisplay,
    renderer: &mut Renderer,
    target: &Target,
    term: &AtomicBool,
) -> Result<(), String> {
    let start = Instant::now();
    let run_for = Duration::from_secs_f64(a.seconds);
    let mut cpu = SelfCpu::default();
    cpu.poll(start);
    let mut next_stats = start + STATS_EVERY;
    let (mut frames, mut busy) = (0u32, Duration::ZERO);
    let mut last = Instant::now();
    let mut total_frames = 0u64;
    while !term.load(Ordering::Relaxed) && start.elapsed() < run_for {
        let t = start.elapsed().as_secs_f32();
        let t0 = Instant::now();
        renderer.set_expression(&a.expression, demo::expression_weight(&a.expression, t))?;
        let yaw = demo::camera_yaw(t);
        // present_with が page flip の完了まで待つので、フレームの間隔は vsync でとれる
        display.present_with(|gl, _| {
            renderer.update(gl);
            target.bind(gl);
            renderer.render_yaw(gl, target.size, yaw);
            target.blit_to_screen(gl);
        })?;
        busy += t0.elapsed();
        frames += 1;
        total_frames += 1;
        let now = Instant::now();
        if now >= next_stats {
            let dt = now.duration_since(last).as_secs_f64();
            eprintln!(
                "{:.1} fps  frame {:.2}ms  cpu {}",
                frames as f64 / dt,
                busy.as_secs_f64() * 1e3 / frames as f64,
                cpu.poll(now).map_or("-".into(), |c| format!("{c:.1}%"))
            );
            (frames, busy, last) = (0, Duration::ZERO, now);
            next_stats = now + STATS_EVERY;
        }
    }
    eprintln!(
        "終了: {total_frames} フレーム / {:.1}秒{}",
        start.elapsed().as_secs_f64(),
        if term.load(Ordering::Relaxed) {
            "(シグナル)"
        } else {
            ""
        }
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
