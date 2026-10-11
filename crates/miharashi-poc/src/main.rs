//! PoC-001: PC の状況表示とフォトフレームの 2 画面。`png`(ヘッドレス)と `kms-show`(実機)。
mod photos;

use miharashi_render::{Canvas, Pixmap, TextEngine, screens};
use photos::{Catalog, Feed, Source};
use std::path::PathBuf;

struct Args {
    out: PathBuf,
    size: (u32, u32),
    photo: Option<PathBuf>,
    geoframe: String,
    seconds: u64,
    connector: Option<String>,
    font: Option<PathBuf>,
    dummy: bool,
    live: bool,
    interval: u64,
    start_photo: bool,
    caption: Option<String>,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut a = Args {
        out: "work/out".into(),
        size: (1920, 1080),
        photo: None,
        geoframe: "http://127.0.0.1:8080".into(),
        seconds: 10,
        connector: None,
        font: None,
        dummy: false,
        live: false,
        interval: 30,
        start_photo: false,
        caption: None,
    };
    let mut it = argv.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} requires a value"));
        match k.as_str() {
            "--out" => a.out = v()?.into(),
            "--photo" => a.photo = Some(v()?.into()),
            "--geoframe" => a.geoframe = v()?,
            "--seconds" => a.seconds = v()?.parse().map_err(|_| "bad --seconds")?,
            "--interval" => a.interval = v()?.parse().map_err(|_| "bad --interval")?,
            "--connector" => a.connector = Some(v()?),
            "--font" => a.font = Some(v()?.into()),
            "--dummy" => a.dummy = true,
            "--caption" => a.caption = Some(v()?),
            "--screen" => a.start_photo = v()? == "photo",
            "--live" => a.live = true,
            "--size" => {
                let s = v()?;
                let (w, h) = s.split_once('x').ok_or("--size WxH")?;
                a.size = (
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                );
            }
            _ => return Err(format!("unknown option {k}")),
        }
    }
    Ok(a)
}

fn source(args: &Args) -> Source {
    match &args.photo {
        Some(p) => Source::File(p.clone(), args.caption.clone()),
        None => Source::Geoframe(args.geoframe.clone()),
    }
}

/// --font があればヒープへ読み込み、なければシステムフォントを使う。
fn text_engine(args: &Args) -> Result<TextEngine, String> {
    match &args.font {
        Some(p) => TextEngine::from_font_file("Noto Sans CJK JP", p)
            .map_err(|e| format!("font {}: {e}", p.display())),
        None => Ok(TextEngine::new("Noto Sans CJK JP")),
    }
}

fn set_clock(s: &mut screens::Snapshot) {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let wd =
        ["月", "火", "水", "木", "金", "土", "日"][now.weekday().num_days_from_monday() as usize];
    s.date = format!("{} ({wd})", now.format("%Y/%m/%d"));
    s.time = now.format("%H:%M:%S").to_string();
}

/// ダミーの時系列(実測がない場合の初期値・--dummy 用)。
fn dummy_history(phase: f32, n: usize, base: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let p = phase + i as f32;
            base + 25.0 * (p * 0.21).sin() + 12.0 * (p * 0.67).sin()
        })
        .collect()
}

fn dummy_snapshot(phase: f32) -> screens::Snapshot {
    let cpu_history = dummy_history(phase, 120, 45.0);
    let gpu_history = dummy_history(phase + 40.0, 120, 55.0);
    let (cpu, gpu) = (*cpu_history.last().unwrap(), *gpu_history.last().unwrap());
    let mut s = screens::Snapshot {
        cpu: Some(cpu),
        cpu_temp: Some(50.0 + cpu * 0.15),
        gpu: Some(gpu),
        gpu_temp: Some(55.0 + gpu * 0.15),
        mem_used: Some(24.0),
        mem_total: Some(64.0),
        ssd_temps: vec![("SSD-A".into(), 41.0), ("SSD-B".into(), 39.0)],
        fans: vec![
            ("CPU".into(), "1200 rpm".into()),
            ("GPU".into(), "32%".into()),
        ],
        cpu_history,
        gpu_history,
        ..Default::default()
    };
    set_clock(&mut s);
    s
}

#[cfg(target_os = "linux")]
fn apply_sample(s: &mut screens::Snapshot, m: &miharashi_linux::metrics::Sample) {
    s.cpu = m.cpu_percent;
    s.cpu_temp = m.cpu_temp;
    s.gpu = m.gpu_percent;
    s.gpu_temp = m.gpu_temp;
    s.mem_used = m.mem_used_gb;
    s.mem_total = m.mem_total_gb;
    s.ssd_temps = m.ssd_temps.clone();
    s.fans = m.fans.clone();
    // マザーボードのファンが読めない場合は、その旨を表示する。
    if !s.fans.iter().any(|(n, _)| n != "GPU") {
        s.fans.insert(0, ("CPU/CASE".into(), "--".into()));
    }
    if let Some(v) = m.cpu_percent {
        s.cpu_history.remove(0);
        s.cpu_history.push(v);
    }
    if let Some(v) = m.gpu_percent {
        s.gpu_history.remove(0);
        s.gpu_history.push(v);
    }
    set_clock(s);
}

fn kms_info() {
    for card in miharashi_linux::kms::probe() {
        match card {
            Err(e) => println!("ERROR {e}"),
            Ok(c) => {
                println!("{} driver={}", c.path.display(), c.driver);
                for k in c.connectors {
                    println!(
                        "  {} {}",
                        k.name,
                        if k.connected {
                            "connected"
                        } else {
                            "disconnected"
                        }
                    );
                    for m in k.modes.iter().take(5) {
                        println!(
                            "    {} @{}Hz{}",
                            m.name,
                            m.refresh,
                            if m.preferred { " (preferred)" } else { "" }
                        );
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Status,
    Photo,
}

#[cfg(target_os = "linux")]
fn kms_show(args: Args) -> Result<(), String> {
    use miharashi_linux::input::{Action, Keys};
    use miharashi_linux::kms::Display;
    use std::time::{Duration, Instant};

    let mut keys = Keys::open()?;
    println!("input: {:?}", keys.device_names());
    let mut disp = Display::open(
        std::path::Path::new("/dev/dri/card0"),
        args.connector.as_deref(),
    )?;
    let (w, h) = disp.size();
    println!(
        "KMS {w}x{h}, run {}s, photo interval {}s (4/6 前後, 5 画面切替, Esc/q 終了)",
        args.seconds, args.interval
    );

    let mut text = text_engine(&args)?;
    let mut c = Canvas::new(w, h);
    let feed = Feed::spawn(source(&args), (w, h));
    let sampler =
        (!args.dummy).then(|| miharashi_linux::metrics::Sampler::spawn(Duration::from_secs(1)));
    let mut snap = if sampler.is_some() {
        let mut s = dummy_snapshot(0.0);
        s.cpu_history = vec![0.0; 120]; // 実測の履歴は空から貯める
        s.gpu_history = vec![0.0; 120];
        s
    } else {
        dummy_snapshot(0.0)
    };

    let interval = Duration::from_secs(args.interval);
    let fade_len = Duration::from_millis(800);
    let start = Instant::now();
    let mut screen = if args.start_photo {
        Screen::Photo
    } else {
        Screen::Status
    };
    let mut current: Option<photos::PhotoItem> = None;
    let mut fade: Option<(Pixmap, Instant)> = None;
    let mut last_photo_change = Instant::now();
    let (mut presses, mut frames, mut phase) = (0u64, 0u64, 0.0f32);
    let mut next_tick = Instant::now();
    let mut redraw = true;

    loop {
        for a in keys.poll() {
            presses += 1;
            eprintln!("key: {a:?}");
            match (a, screen) {
                (Action::Quit, _) => return Ok(()),
                (Action::Switch, Screen::Status) | (Action::Next, Screen::Status) => {
                    screen = Screen::Photo
                }
                (Action::Switch, Screen::Photo) => screen = Screen::Status,
                (Action::Next, Screen::Photo) => {
                    feed.next();
                    last_photo_change = Instant::now();
                }
                (Action::Prev, Screen::Photo) => {
                    feed.prev();
                    last_photo_change = Instant::now();
                }
                (Action::Prev, Screen::Status) => {}
            }
            redraw = true;
        }
        match feed.try_recv() {
            Some(Ok(item)) => {
                if let Some(old) = current.take() {
                    fade = Some((old.pix, Instant::now()));
                }
                current = Some(item);
                last_photo_change = Instant::now();
                redraw = true;
            }
            Some(Err(e)) => eprintln!("photo: {e}"),
            None => {}
        }
        if screen == Screen::Photo && last_photo_change.elapsed() >= interval {
            feed.next();
            last_photo_change = Instant::now();
        }
        if start.elapsed() >= Duration::from_secs(args.seconds) {
            return Ok(()); // Drop で CRTC を復元
        }
        let fade_alpha = fade
            .as_ref()
            .map(|(_, t0)| t0.elapsed().as_secs_f32() / fade_len.as_secs_f32());
        if fade_alpha.is_some_and(|a| a >= 1.0) {
            fade = None;
        }
        let ticking = Instant::now() >= next_tick;
        if ticking {
            phase += 1.0;
            match &sampler {
                Some(sp) => apply_sample(&mut snap, &sp.latest()),
                None => snap = dummy_snapshot(phase),
            }
            next_tick += Duration::from_secs(1);
            if next_tick < Instant::now() {
                next_tick = Instant::now() + Duration::from_secs(1);
            }
        }
        if ticking || redraw || (screen == Screen::Photo && fade.is_some()) {
            let t0 = Instant::now();
            match screen {
                Screen::Status => screens::status(&mut c, &mut text, &snap),
                Screen::Photo => {
                    let fade_arg = fade.as_ref().map(|(old, t0)| {
                        (
                            old,
                            (t0.elapsed().as_secs_f32() / fade_len.as_secs_f32()).min(1.0),
                        )
                    });
                    screens::photo_frame(
                        &mut c,
                        &mut text,
                        &snap,
                        current.as_ref().map(|p| &p.pix),
                        current.as_ref().map(|p| &p.info),
                        fade_arg,
                    )
                }
            }
            disp.present(|buf, pitch| c.write_xrgb8888(buf, pitch))?;
            redraw = false;
            if ticking {
                frames += 1;
                if frames % 5 == 0 {
                    eprintln!(
                        "t={:>4}s frame={:>2}ms rss={}MB ticks={} keys={} cpu={:?} gpu={:?} mem={:?} screen={} photo={}",
                        start.elapsed().as_secs(),
                        t0.elapsed().as_millis(),
                        rss_mb(),
                        frames,
                        presses,
                        snap.cpu.map(|v| v.round()),
                        snap.gpu.map(|v| v.round()),
                        snap.mem_used.map(|v| (v * 10.0).round() / 10.0),
                        if screen == Screen::Status {
                            "status"
                        } else {
                            "photo"
                        },
                        current.as_ref().map_or(0, |p| p.info.position.0),
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(target_os = "linux"))]
fn kms_show(_: Args) -> Result<(), String> {
    Err("KMS is Linux only".into())
}

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / 1024 / 1024)
}

fn png(args: Args) -> Result<(), String> {
    let (w, h) = args.size;
    let mut snap = dummy_snapshot(0.0);
    #[cfg(target_os = "linux")]
    if args.live {
        // --seconds 秒間、1 秒ごとに実測して時系列グラフを貯めてから描く。
        let sp = miharashi_linux::metrics::Sampler::spawn(std::time::Duration::from_secs(1));
        snap.cpu_history = vec![0.0; 120];
        snap.gpu_history = vec![0.0; 120];
        for _ in 0..args.seconds.max(2) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            apply_sample(&mut snap, &sp.latest());
        }
        println!(
            "live: cpu={:?} gpu={:?} ssd={:?} fans={:?}",
            snap.cpu, snap.gpu, snap.ssd_temps, snap.fans
        );
    }
    let item = Catalog::open(&source(&args), args.size)?.current()?;
    let mut text = text_engine(&args)?;
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
    let mut c = Canvas::new(w, h);
    screens::status(&mut c, &mut text, &snap);
    c.save_png(&args.out.join("status.png"))?;
    screens::photo_frame(
        &mut c,
        &mut text,
        &snap,
        Some(&item.pix),
        Some(&item.info),
        None,
    );
    c.save_png(&args.out.join("photo.png"))?;
    println!(
        "wrote {}/{{status,photo}}.png ({w}x{h})",
        args.out.display()
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let mut rest: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if rest.first().is_some_and(|c| !c.starts_with("--")) {
        rest.remove(0)
    } else {
        "png".into()
    };
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
