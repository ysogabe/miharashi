//! PoC-001 ヘッドレス実行: 2 画面を PNG へ出力する(P1/P2)。
use miharashi_render::{photo, screens, Canvas, TextEngine};
use std::path::{Path, PathBuf};

struct Args {
    out: PathBuf,
    size: (u32, u32),
    photo: Option<PathBuf>,
    geoframe: Option<String>,
    cache: PathBuf,
    seconds: u64,
    connector: Option<String>,
    font: Option<PathBuf>,
    dummy: bool,
    live: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut a = Args {
        out: "work/out".into(),
        size: (1920, 1080),
        photo: None,
        geoframe: None,
        cache: "work/cache/photo.jpg".into(),
        seconds: 10,
        connector: None,
        font: None,
        dummy: false,
        live: false,
    };
    let mut it = argv.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} requires a value"));
        match k.as_str() {
            "--out" => a.out = v()?.into(),
            "--photo" => a.photo = Some(v()?.into()),
            "--geoframe" => a.geoframe = Some(v()?),
            "--cache" => a.cache = v()?.into(),
            "--seconds" => a.seconds = v()?.parse().map_err(|_| "bad --seconds")?,
            "--connector" => a.connector = Some(v()?),
            "--font" => a.font = Some(v()?.into()),
            "--dummy" => a.dummy = true,
            "--live" => a.live = true,
            "--size" => {
                let s = v()?;
                let (w, h) = s.split_once('x').ok_or("--size WxH")?;
                a.size = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
            }
            _ => return Err(format!("unknown option {k}")),
        }
    }
    Ok(a)
}

/// GEOFRAME から写真を 1 枚取得する。quality_recommended を優先する。
fn fetch_geoframe(base: &str, cache: &Path) -> Result<(Vec<u8>, String), String> {
    let base = base.trim_end_matches('/');
    let body = ureq::get(&format!("{base}/api/photos"))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let list: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let arr = list.as_array().ok_or("unexpected /api/photos")?;
    let pick = arr.iter().find(|p| p["quality_recommended"] == true).or(arr.first()).ok_or("no photos")?;
    let file = pick["file"].as_str().ok_or("no file")?;
    let caption = format!(
        "{} {}　{}",
        pick["pref"].as_str().unwrap_or(""),
        pick["title"].as_str().unwrap_or(""),
        pick["time"].as_str().unwrap_or("")
    );
    let bytes = ureq::get(&format!("{base}/{file}"))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    if let Some(dir) = cache.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(cache, &bytes).map_err(|e| e.to_string())?;
    Ok((bytes, caption))
}

/// --font があればヒープへ読み込み、なければシステムフォントを使う。
fn text_engine(args: &Args) -> Result<TextEngine, String> {
    match &args.font {
        Some(p) => TextEngine::from_font_file("Noto Sans CJK JP", p).map_err(|e| format!("font {}: {e}", p.display())),
        None => Ok(TextEngine::new("Noto Sans CJK JP")),
    }
}

fn kms_info() {
    for card in miharashi_linux::kms::probe() {
        match card {
            Err(e) => println!("ERROR {e}"),
            Ok(c) => {
                println!("{} driver={}", c.path.display(), c.driver);
                for k in c.connectors {
                    println!("  {} {}", k.name, if k.connected { "connected" } else { "disconnected" });
                    for m in k.modes.iter().take(5) {
                        println!("    {} @{}Hz{}", m.name, m.refresh, if m.preferred { " (preferred)" } else { "" });
                    }
                }
            }
        }
    }
}

fn main() -> Result<(), String> {
    let mut rest: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if rest.first().is_some_and(|c| !c.starts_with("--")) { rest.remove(0) } else { "png".into() };
    match cmd.as_str() {
        "kms-info" => {
            kms_info();
            Ok(())
        }
        "kms-show" => kms_show(parse_args(rest)?),
        "png" => png(parse_args(rest)?),
        other => Err(format!("unknown command {other}")),
    }
}

struct Scene {
    snap: screens::Snapshot,
    left: Option<miharashi_render::Pixmap>,
    full: Option<miharashi_render::Pixmap>,
}

fn load_scene(args: &Args, (w, h): (u32, u32)) -> Result<Scene, String> {
    let (photo_bytes, caption) = if let Some(p) = &args.photo {
        (Some(std::fs::read(p).map_err(|e| e.to_string())?), p.display().to_string())
    } else if let Some(url) = &args.geoframe {
        let (b, c) = fetch_geoframe(url, &args.cache)?;
        (Some(b), c)
    } else if let Ok(b) = std::fs::read(&args.cache) {
        (Some(b), "cached".to_string())
    } else {
        (None, String::new())
    };

    let (head, foot) = (72u32, 64u32);
    let left = match &photo_bytes {
        Some(b) => Some(photo::load_cover(b, w / 2, h - head - foot - 1)?),
        None => None,
    };
    let full = match &photo_bytes {
        Some(b) => Some(photo::load_cover(b, w, h)?),
        None => None,
    };

    let history: Vec<f32> = (0..120)
        .map(|i| 45.0 + 25.0 * (i as f32 * 0.21).sin() + 12.0 * (i as f32 * 0.67).sin())
        .collect();
    let snap = screens::Snapshot {
        clock: "2026/10/11 23:00".into(),
        cpu: Some(32.0),
        cpu_temp: Some(58.0),
        gpu: Some(78.0),
        gpu_temp: Some(65.0),
        mem_used: Some(24.0),
        mem_total: Some(64.0),
        cpu_history: history,
        photo_caption: caption,
    };
    Ok(Scene { snap, left, full })
}

#[cfg(target_os = "linux")]
fn kms_show(args: Args) -> Result<(), String> {
    use miharashi_linux::input::{Action, Keys};
    use miharashi_linux::kms::Display;
    use std::time::{Duration, Instant};

    let mut keys = Keys::open()?;
    println!("input: {:?}", keys.device_names());
    let mut disp = Display::open(std::path::Path::new("/dev/dri/card0"), args.connector.as_deref())?;
    let (w, h) = disp.size();
    println!("KMS {w}x{h}, run {}s (4/6 切替, Esc/q 終了)", args.seconds);
    let mut scene = load_scene(&args, (w, h))?;
    let mut text = text_engine(&args)?;
    let mut c = Canvas::new(w, h);

    let start = Instant::now();
    let (mut screen, mut presses, mut frames) = (0usize, 0u64, 0u64);
    let mut next_tick = Instant::now();
    let mut phase = 0.0f32;
    let sampler = (!args.dummy).then(|| miharashi_linux::metrics::Sampler::spawn(Duration::from_secs(1)));
    if sampler.is_some() {
        scene.snap.cpu_history = vec![0.0; 120]; // 実測の履歴は空から貯める
    }
    loop {
        for a in keys.poll() {
            presses += 1;
            eprintln!("key: {a:?}");
            match a {
                Action::Next => screen = (screen + 1) % 2,
                Action::Prev => screen = (screen + 1) % 2,
                Action::Details => {}
                Action::Quit => return Ok(()),
            }
            next_tick = Instant::now(); // 切替は即時反映
        }
        if start.elapsed() >= Duration::from_secs(args.seconds) {
            return Ok(()); // Drop で CRTC を復元
        }
        if Instant::now() >= next_tick {
            let t0 = Instant::now();
            phase += 1.0;
            match &sampler {
                Some(sp) => apply_sample(&mut scene.snap, &sp.latest()),
                None => update_snapshot(&mut scene.snap, phase),
            }
            match screen {
                0 => screens::dashboard(&mut c, &mut text, &scene.snap, scene.left.as_ref()),
                _ => screens::photo_frame(&mut c, &mut text, &scene.snap, scene.full.as_ref()),
            }
            disp.present(|buf, pitch| c.write_xrgb8888(buf, pitch))?;
            frames += 1;
            if frames % 5 == 0 {
                eprintln!(
                    "t={:>4}s frame={:>2}ms rss={}MB frames={} keys={} cpu={:?} gpu={:?} mem={:?}",
                    start.elapsed().as_secs(),
                    t0.elapsed().as_millis(),
                    rss_mb(),
                    frames,
                    presses,
                    scene.snap.cpu.map(|v| v.round()),
                    scene.snap.gpu.map(|v| v.round()),
                    scene.snap.mem_used.map(|v| (v * 10.0).round() / 10.0)
                );
            }
            next_tick += Duration::from_secs(1);
            if next_tick < Instant::now() {
                next_tick = Instant::now() + Duration::from_secs(1);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 実測値を反映する。CPU 使用率が取れた回だけ履歴へ追加する。
#[cfg(target_os = "linux")]
fn apply_sample(s: &mut screens::Snapshot, m: &miharashi_linux::metrics::Sample) {
    s.cpu = m.cpu_percent;
    s.cpu_temp = m.cpu_temp;
    s.gpu = m.gpu_percent;
    s.gpu_temp = m.gpu_temp;
    s.mem_used = m.mem_used_gb;
    s.mem_total = m.mem_total_gb;
    if let Some(v) = m.cpu_percent {
        s.cpu_history.remove(0);
        s.cpu_history.push(v);
    }
    s.clock = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
}

/// ダミーデータを 1 ステップ進める。
fn update_snapshot(s: &mut screens::Snapshot, phase: f32) {
    let v = 45.0 + 25.0 * (phase * 0.21).sin() + 12.0 * (phase * 0.67).sin();
    s.cpu_history.remove(0);
    s.cpu_history.push(v);
    let gpu = 60.0 + 20.0 * (phase * 0.13).sin();
    s.cpu = Some(v);
    s.gpu = Some(gpu);
    s.cpu_temp = Some(50.0 + v * 0.15);
    s.gpu_temp = Some(55.0 + gpu * 0.15);
    s.clock = chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string();
}

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / 1024 / 1024)
}

#[cfg(not(target_os = "linux"))]
fn kms_show(_: Args) -> Result<(), String> {
    Err("KMS is Linux only".into())
}

fn png(args: Args) -> Result<(), String> {
    let (w, h) = args.size;
    let Scene { mut snap, left, full } = load_scene(&args, args.size)?;
    #[cfg(target_os = "linux")]
    if args.live {
        let sp = miharashi_linux::metrics::Sampler::spawn(std::time::Duration::from_millis(500));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let m = sp.latest();
        snap.cpu_history = vec![0.0; 120];
        println!("live sample: {m:?}");
        apply_sample(&mut snap, &m);
    }
    let mut text = text_engine(&args)?;
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;

    let mut c = Canvas::new(w, h);
    screens::dashboard(&mut c, &mut text, &snap, left.as_ref());
    c.save_png(&args.out.join("dashboard.png"))?;
    screens::photo_frame(&mut c, &mut text, &snap, full.as_ref());
    c.save_png(&args.out.join("photo.png"))?;
    println!("wrote {}/{{dashboard,photo}}.png ({w}x{h})", args.out.display());
    Ok(())
}
