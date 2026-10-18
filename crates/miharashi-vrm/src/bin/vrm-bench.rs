//! `vrm-bench`: VRM 1.0 モデルを内蔵 GPU に surfaceless で描き、フレーム時間などを測る(Issue #71 A)。
//! 画面には出さない。モデルは利用条件を確認して各自で用意する(リポジトリには置かない)。
//! `--synthetic` なら、コードから組み立てた小さなモデルで動く。
use miharashi_vrm::synth;
use miharashi_vrm::{Model, Renderer, attach_depth, detach_depth};
use std::path::{Path, PathBuf};
use std::time::Instant;

const USAGE: &str = "使い方: vrm-bench (--model <path.vrm> | --synthetic) [--pci 0000:10:00.0] [--size 1280x720] \
[--frames 600 | --seconds 30] [--fps 30] [--expression blink] [--still] [--png out.png] [--segments 48] [--texture 1024]";

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
    synth: synth::Opts,
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
        synth: synth::Opts::default(),
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
    // ウォームアップ(シェーダのコンパイル・最初の転送を計測に入れない)
    for _ in 0..5 {
        r.update(gl);
        r.render(gl, size);
    }
    unsafe { gl.finish() };
    let (cpu_start, wall_start) = (cpu_seconds(), Instant::now());
    let (mut total, mut morph) = (Vec::new(), Vec::new());
    let mut i = 0u32;
    loop {
        match a.seconds {
            Some(s) if wall_start.elapsed().as_secs_f64() >= s => break,
            None if i >= a.frames => break,
            _ => {}
        }
        let t = i as f32 / 30.0;
        // 2 秒周期で 0 → 1 → 0
        let w = if a.still {
            1.0
        } else {
            0.5 - 0.5 * (t * std::f32::consts::PI).cos()
        };
        let f0 = Instant::now();
        r.set_expression(expr, w)?;
        r.update(gl);
        let f1 = Instant::now();
        r.render(gl, size);
        unsafe { gl.finish() };
        total.push(f0.elapsed().as_secs_f64() * 1e3);
        morph.push((f1 - f0).as_secs_f64() * 1e3);
        i += 1;
        if let Some(fps) = a.fps {
            // 次のフレームの時刻まで待つ(遅れていれば待たない)
            let next = wall_start + std::time::Duration::from_secs_f64(i as f64 / fps);
            if let Some(d) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
        }
    }
    let wall = wall_start.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu_start;
    if total.is_empty() {
        return Err("計測したフレームがない".into());
    }
    let (tm, tp) = mean_p95(&mut total);
    let (mm, mp) = mean_p95(&mut morph);
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
    println!("morph_cpu_ms: mean={mm:.3} p95={mp:.3}(set_expression + 頂点の合成・転送)");
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
        let mut r = match Renderer::new(gl, model) {
            Ok(r) => r,
            Err(e) => {
                detach_depth(gl, depth);
                return Err(e);
            }
        };
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
