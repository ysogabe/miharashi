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
        cpu: 32.0,
        cpu_temp: 58.0,
        gpu: 78.0,
        gpu_temp: 65.0,
        mem_used: 24.0,
        mem_total: 64.0,
        cpu_history: history,
        photo_caption: caption,
    };
    Ok(Scene { snap, left, full })
}

#[cfg(target_os = "linux")]
fn kms_show(args: Args) -> Result<(), String> {
    use miharashi_linux::kms::Display;
    let mut disp = Display::open(std::path::Path::new("/dev/dri/card0"), args.connector.as_deref())?;
    let (w, h) = disp.size();
    println!("KMS {w}x{h}, showing {}s", args.seconds);
    let scene = load_scene(&args, (w, h))?;
    let mut text = TextEngine::new("Noto Sans CJK JP");
    let mut c = Canvas::new(w, h);
    screens::dashboard(&mut c, &mut text, &scene.snap, scene.left.as_ref());
    disp.present(|buf, pitch| c.write_xrgb8888(buf, pitch))?;
    std::thread::sleep(std::time::Duration::from_secs(args.seconds));
    Ok(()) // Drop で CRTC を復元
}

#[cfg(not(target_os = "linux"))]
fn kms_show(_: Args) -> Result<(), String> {
    Err("KMS is Linux only".into())
}

fn png(args: Args) -> Result<(), String> {
    let (w, h) = args.size;
    let Scene { snap, left, full } = load_scene(&args, args.size)?;
    let mut text = TextEngine::new("Noto Sans CJK JP");
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;

    let mut c = Canvas::new(w, h);
    screens::dashboard(&mut c, &mut text, &snap, left.as_ref());
    c.save_png(&args.out.join("dashboard.png"))?;
    screens::photo_frame(&mut c, &mut text, &snap, full.as_ref());
    c.save_png(&args.out.join("photo.png"))?;
    println!("wrote {}/{{dashboard,photo}}.png ({w}x{h})", args.out.display());
    Ok(())
}
