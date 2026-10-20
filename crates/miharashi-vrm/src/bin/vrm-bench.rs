//! `vrm-bench`: VRM 1.0 モデルを内蔵 GPU に surfaceless で描き、フレーム時間などを測る(Issue #71 A)。
//! 画面には出さない。モデルは利用条件を確認して各自で用意する(リポジトリには置かない)。
//! `--synthetic` なら、コードから組み立てた小さなモデルで動く。
use miharashi_vrm::demo::{self, FaceSet};
use miharashi_vrm::synth;
use miharashi_vrm::{
    Framing, Level, Model, Renderer, StageRegion, VrmLayer, attach_depth, detach_depth,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

const USAGE: &str = "使い方: vrm-bench (--model <path.vrm> | --synthetic) [--pci 0000:10:00.0] [--size 1280x720] \
[--frames 600 | --seconds 30] [--fps 30] [--feature-level bake|skin|pose|mtoon|spring] [--outline-scale 1] \
[--expression blink] [--still] [--png out.png] [--segments 48] [--texture 1024] \
[--frame full|waist|bust] [--region left|center|right|full] [--gesture] [--lipsync] [--start 0]";

struct Args {
    model: Option<PathBuf>,
    pci: String,
    size: (u32, u32),
    frames: u32,
    /// 指定すると、フレーム数ではなくこの秒数だけ描き続ける
    seconds: Option<f64>,
    /// 指定すると、各フレームのあと次の時刻まで sleep してこの間隔を保つ
    fps: Option<f64>,
    expression: Option<String>,
    still: bool,
    png: Option<PathBuf>,
    level: Level,
    outline_scale: f32,
    synth: synth::Opts,
    /// 構図と、描く領域(`--size` の画面の中)。full / full 以外は透明な FBO に描いて重ねる
    frame: Framing,
    region: StageRegion,
    gesture: bool,
    lipsync: bool,
    /// 計測の最初の時刻 [秒](途中の身振りや口の形の PNG を見るため)
    start: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        model: None,
        pci: "0000:10:00.0".into(),
        size: (1280, 720),
        frames: 600,
        seconds: None,
        fps: None,
        expression: None,
        still: false,
        png: None,
        level: Level::Spring,
        outline_scale: 1.0,
        synth: synth::Opts::default(),
        frame: Framing::Full,
        region: StageRegion::Full,
        gesture: false,
        lipsync: false,
        start: 0.0,
    };
    let mut synthetic = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} に値がない\n{USAGE}"));
        match flag.as_str() {
            "--model" => a.model = Some(value()?.into()),
            "--synthetic" => synthetic = true,
            "--pci" => a.pci = value()?,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size は 1280x720 の形")?;
                a.size = (
                    w.parse().map_err(|_| "--size の幅が数でない")?,
                    h.parse().map_err(|_| "--size の高さが数でない")?,
                );
            }
            "--frames" => a.frames = value()?.parse().map_err(|_| "--frames が数でない")?,
            "--seconds" => a.seconds = Some(value()?.parse().map_err(|_| "--seconds が数でない")?),
            "--fps" => a.fps = Some(value()?.parse().map_err(|_| "--fps が数でない")?),
            "--expression" => a.expression = Some(value()?),
            "--still" => a.still = true,
            "--png" => a.png = Some(value()?.into()),
            "--frame" => a.frame = Framing::parse(&value()?)?,
            "--region" => a.region = StageRegion::parse(&value()?)?,
            "--gesture" => a.gesture = true,
            "--lipsync" => a.lipsync = true,
            "--start" => a.start = value()?.parse().map_err(|_| "--start が数でない")?,
            "--feature-level" => a.level = Level::parse(&value()?)?,
            "--outline-scale" => {
                a.outline_scale = value()?.parse().map_err(|_| "--outline-scale が数でない")?
            }
            "--segments" => {
                a.synth.segments = value()?.parse().map_err(|_| "--segments が数でない")?
            }
            "--texture" => {
                a.synth.texture_size = value()?.parse().map_err(|_| "--texture が数でない")?
            }
            _ => return Err(format!("不明な引数 {flag}\n{USAGE}")),
        }
    }
    if a.model.is_none() && !synthetic {
        return Err(format!("--model か --synthetic が要る\n{USAGE}"));
    }
    if a.frames == 0 || a.size.0 == 0 || a.size.1 == 0 {
        return Err("--frames と --size は 1 以上".into());
    }
    if a.seconds
        .is_some_and(|s| s.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
        || a.fps
            .is_some_and(|f| f.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
    {
        return Err("--seconds と --fps は 0 より大きい数".into());
    }
    Ok(a)
}

fn sysfs_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn mib(v: Option<u64>) -> String {
    v.map_or("-".into(), |b| format!("{}MB", b >> 20))
}

/// このプロセスの CPU 時間(ユーザー + システム)の秒数。
fn cpu_seconds() -> f64 {
    let mut u = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    t(u.ru_utime) + t(u.ru_stime)
}

fn mean_p95(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (mean, v[((v.len() - 1) as f64 * 0.95).round() as usize])
}

#[allow(clippy::too_many_arguments)]
fn measure(
    a: &Args,
    gl: &glow::Context,
    r: &mut Renderer,
    expr: &str,
    t0: Instant,
    load_cpu: std::time::Duration,
    (vram0, gtt0): (Option<u64>, Option<u64>),
    vram: &dyn Fn() -> Option<u64>,
    gtt: &dyn Fn() -> Option<u64>,
) -> Result<(), String> {
    use glow::HasContext;
    let size = a.size;
    unsafe { gl.finish() };
    let upload = t0.elapsed();
    println!(
        "load: file+parse={:.0}ms gpu_upload={:.0}ms total={:.0}ms uploaded={}MB",
        load_cpu.as_secs_f64() * 1e3,
        upload.as_secs_f64() * 1e3,
        (load_cpu + upload).as_secs_f64() * 1e3,
        r.uploaded_bytes >> 20
    );
    // 描く領域。full / full 以外は、領域と同じ大きさの透明な FBO に描いて画面(この FBO)へ重ねる
    let (rx, ry, rw, rh) = a.region.rect(size);
    let use_layer = a.region != StageRegion::Full || a.frame != Framing::Full;
    let layer = if use_layer {
        use glow::HasContext;
        let target = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
        let l = VrmLayer::new(gl, (rw, rh))?;
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, target) };
        r.clear_color = [0.0; 4];
        Some(l)
    } else {
        None
    };
    let target = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
    let cam = r.framing_camera(a.frame, (rw, rh), 0.0);
    let draw = |gl: &glow::Context, r: &Renderer| {
        use glow::HasContext;
        match &layer {
            Some(l) => {
                // 背景(写真の代わりの単色)を塗り、領域に重ねる
                unsafe {
                    gl.viewport(0, 0, size.0 as i32, size.1 as i32);
                    gl.clear_color(0.22, 0.24, 0.30, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
                l.bind(gl);
                r.render_camera(gl, (rw, rh), &cam.0, cam.1);
                l.composite(gl, target, (rx as i32, ry as i32));
            }
            None => r.render(gl, size),
        }
    };
    let faces = FaceSet::of(&r.model().vrm);
    let mut list: Vec<(&'static str, f32)> = Vec::new();
    // ウォームアップ(シェーダのコンパイル・最初の転送を計測に入れない)
    for k in 0..5 {
        let t = a.start + k as f32 / 30.0;
        r.animate(t, 1.0 / 30.0);
        r.update(gl);
        draw(gl, r);
    }
    unsafe { gl.finish() };
    let (cpu_start, wall_start) = (cpu_seconds(), Instant::now());
    let (mut total, mut morph, mut pose, mut spring) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut gpu = Vec::new();
    let mut i = 0u32;
    loop {
        match a.seconds {
            Some(s) if wall_start.elapsed().as_secs_f64() >= s => break,
            None if i >= a.frames => break,
            _ => {}
        }
        let t = a.start + i as f32 / 30.0;
        let f0 = Instant::now();
        r.animate(t, 1.0 / 30.0);
        let f_anim = Instant::now();
        if a.lipsync {
            // 口の母音・まばたき・笑顔を同時に合成する
            demo::face(t, &faces, &mut list);
            r.set_expressions(&list)?;
        } else {
            // 2 秒周期で 0 → 1 → 0
            let w = if a.still {
                1.0
            } else {
                0.5 - 0.5 * (t * std::f32::consts::PI).cos()
            };
            r.set_expression(expr, w)?;
        }
        let times = r.update(gl);
        let f1 = Instant::now();
        draw(gl, r);
        unsafe { gl.finish() };
        total.push(f0.elapsed().as_secs_f64() * 1e3);
        // set_expression + update のうち、骨の分(UBO 転送込み)を除いたものが morph
        morph.push((f1 - f_anim).as_secs_f64() * 1e3 - times.pose_ms);
        pose.push(times.pose_ms);
        spring.push(times.spring_ms);
        // GPU: 描画コマンドの発行から glFinish の完了まで
        gpu.push(f1.elapsed().as_secs_f64() * 1e3);
        i += 1;
        if let Some(fps) = a.fps {
            // 次のフレームの時刻まで待つ(遅れていれば待たない)
            let next = wall_start + std::time::Duration::from_secs_f64(i as f64 / fps);
            if let Some(d) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
        }
    }
    if let Some(l) = layer {
        l.destroy(gl);
        // destroy は FRAMEBUFFER を外すので、あとの detach_depth のために描画先へ戻す
        unsafe { glow::HasContext::bind_framebuffer(gl, glow::FRAMEBUFFER, target) };
    }
    let wall = wall_start.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu_start;
    if total.is_empty() {
        return Err("計測したフレームがない".into());
    }
    let (tm, tp) = mean_p95(&mut total);
    let (mm, mp) = mean_p95(&mut morph);
    let (pm, pp) = mean_p95(&mut pose);
    let (sm, sp) = mean_p95(&mut spring);
    let (gm, gp) = mean_p95(&mut gpu);
    println!(
        "expression: {expr}{}",
        if a.still {
            " (weight 1 固定)"
        } else {
            " (weight を時間で動かす)"
        }
    );
    println!(
        "frames: {i} size={}x{} fps_limit={}",
        size.0,
        size.1,
        a.fps.map_or("なし(連続描画)".into(), |f| format!("{f}"))
    );
    println!(
        "frame_ms: mean={tm:.2} p95={tp:.2}(glFinish まで待つ。実 FPS 換算 {:.0} fps)",
        1e3 / tm
    );
    println!("feature_level: {}", r.level().name());
    println!(
        "composition: frame={} region={} ({rw}x{rh} at {rx},{ry}) gesture={} lipsync={} start={:.1}s{}",
        a.frame.name(),
        a.region.name(),
        a.gesture,
        a.lipsync,
        a.start,
        if use_layer {
            "(透明な FBO に描いて重ねる)"
        } else {
            "(画面全体に直接)"
        }
    );
    println!("gpu_ms(描画の発行 + glFinish): mean={gm:.2} p95={gp:.2}");
    println!("pose_cpu_ms: mean={pm:.3} p95={pp:.3}(ノード階層の評価 + 関節行列 + UBO 転送)");
    println!("spring_cpu_ms: mean={sm:.3} p95={sp:.3}");
    println!(
        "morph_cpu_ms: mean={mm:.3} p95={mp:.3}(表情合成: set_expression(s) + 頂点の合成・転送{})",
        if a.lipsync {
            "、口パク + まばたき + 笑顔"
        } else {
            ""
        }
    );
    println!(
        "process_cpu: {:.1}% (cpu {:.2}s / wall {:.2}s、{})",
        cpu / wall * 100.0,
        cpu,
        wall,
        if a.fps.is_some() {
            "fps 制限あり、glFinish の待ちを含む"
        } else {
            "glFinish の待ちを含む連続描画"
        }
    );
    println!(
        "gpu_mem(終了前): vram {} -> {} gtt {} -> {}",
        mib(vram0),
        mib(vram()),
        mib(gtt0),
        mib(gtt())
    );
    Ok(())
}

fn run(a: &Args) -> Result<(), String> {
    let dev = PathBuf::from(format!("/sys/bus/pci/devices/{}", a.pci));
    let vram = || sysfs_u64(&dev.join("mem_info_vram_used"));
    let gtt = || sysfs_u64(&dev.join("mem_info_gtt_used"));

    let node = miharashi_gpu::render_node_for_pci(&a.pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("render node: {}", node.display());
    println!("egl: {}", off.describe());

    let (vram0, gtt0) = (vram(), gtt());
    let t_load = Instant::now();
    let model = match &a.model {
        Some(p) => Model::from_path(p)?,
        None => Model::from_glb(&synth::build_vrm_glb(&a.synth))?,
    };
    let load_cpu = t_load.elapsed();
    let st = model.stats();
    println!(
        "model: {} (VRM {})",
        a.model
            .as_deref()
            .map_or("synthetic".into(), |p| p.display().to_string()),
        model.vrm.spec_version
    );
    println!(
        "  meshes={} primitives={} vertices={} triangles={} materials={} morph_targets={}",
        st.meshes, st.primitives, st.vertices, st.triangles, st.materials, st.morph_targets
    );
    let tex: Vec<String> = st.images.iter().map(|(w, h)| format!("{w}x{h}")).collect();
    println!("  textures={} [{}]", st.images.len(), tex.join(" "));
    let names: Vec<&str> = model
        .vrm
        .expressions
        .iter()
        .filter(|e| !e.binds.is_empty())
        .map(|e| e.name.as_str())
        .collect();
    println!("  expressions(morph あり)={names:?}");
    let expr = match &a.expression {
        Some(n) => n.clone(),
        None => model
            .vrm
            .default_expression()
            .ok_or("morph bind を持つ expression がない(--expression で指定)")?
            .to_string(),
    };
    if model.vrm.expression(&expr).is_none() {
        return Err(format!("expression {expr} がない"));
    }

    let size = a.size;
    let (r, img) = off.with_fbo(size, |gl| -> Result<(), String> {
        use glow::HasContext;
        unsafe {
            println!(
                "gl: GL_RENDERER={} GL_VERSION={}",
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION)
            );
        }
        let depth = attach_depth(gl, size)?;
        let t0 = Instant::now();
        let mut r = match Renderer::with_level(gl, model, a.level) {
            Ok(r) => r,
            Err(e) => {
                detach_depth(gl, depth);
                return Err(e);
            }
        };
        r.outline_scale = a.outline_scale;
        let mut setup = || -> Result<(), String> {
            if a.gesture {
                r.set_gesture(true)?;
                let (arms, fingers) = r.rig().gesture_bones();
                println!("gesture: 腕 {arms} 本、指の関節 {fingers} 個");
            }
            if a.lipsync {
                let f = FaceSet::of(&r.model().vrm);
                if f.no_vowels() {
                    return Err("口パクに使う表情(aa/ih/ou/ee/oh)がモデルにない".into());
                }
                println!(
                    "lipsync: 母音 {:?} blink={} happy={}",
                    demo::VOWELS
                        .iter()
                        .zip(f.vowels)
                        .filter(|(_, ok)| *ok)
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>(),
                    f.blink,
                    f.happy
                );
            }
            Ok(())
        };
        if let Err(e) = setup() {
            r.destroy(gl);
            detach_depth(gl, depth);
            return Err(e);
        }
        // 計測の途中でエラーになっても、あとで destroy / detach_depth を通る
        let measured = measure(
            a,
            gl,
            &mut r,
            &expr,
            t0,
            load_cpu,
            (vram0, gtt0),
            &vram,
            &gtt,
        );
        r.destroy(gl);
        detach_depth(gl, depth);
        unsafe { gl.finish() };
        measured
    })?;
    r?;
    println!("gpu_mem(解放後): vram {} gtt {}", mib(vram()), mib(gtt()));
    if let Some(path) = &a.png {
        image::RgbaImage::from_raw(a.size.0, a.size.1, img)
            .ok_or("画像の大きさが合わない")?
            .save(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("png: {}", path.display());
    }
    Ok(())
}

fn main() {
    let result = parse_args().and_then(|a| run(&a));
    if let Err(e) = result {
        eprintln!("vrm-bench: {e}");
        std::process::exit(1);
    }
}
