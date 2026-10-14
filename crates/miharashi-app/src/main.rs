//! Miharashi: Linux キオスク・ダッシュボード。`run`(実機)、`png`(ヘッドレス)、`kms-info`、
//! `bench`(CPU 描画のベースライン)。
mod aircraft;
mod album;
mod app;
mod bench;
mod config;
mod data;
mod disaster;
mod panel;
mod photos;
mod quake;
#[cfg(target_os = "linux")]
mod runtime;
mod screen;
mod screen_power;

use config::Config;
use miharashi_render::aircraft as air;
use miharashi_render::{Canvas, photo, screens};
use photos::{Catalog, Grouping, HomeLookup, PhotoItem, PhotoPixels, Source};
use std::path::PathBuf;

#[derive(Default)]
struct Cli {
    config: Option<PathBuf>,
    out: Option<PathBuf>,
    size: Option<(u32, u32)>,
    photo: Option<PathBuf>,
    caption: Option<String>,
    seconds: Option<u64>,
    frames: Option<usize>,
    screen: Option<String>,
    renderer: Option<String>,
    dummy: bool,
    live: bool,
}

fn parse_cli(argv: Vec<String>) -> Result<Cli, String> {
    let mut c = Cli::default();
    let mut it = argv.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} requires a value"));
        match k.as_str() {
            "--config" => c.config = Some(v()?.into()),
            "--out" => c.out = Some(v()?.into()),
            "--photo" => c.photo = Some(v()?.into()),
            "--caption" => c.caption = Some(v()?),
            "--seconds" => c.seconds = Some(v()?.parse().map_err(|_| "bad --seconds")?),
            "--frames" => c.frames = Some(v()?.parse().map_err(|_| "bad --frames")?),
            "--screen" => c.screen = Some(v()?),
            "--renderer" => c.renderer = Some(v()?),
            "--dummy" => c.dummy = true,
            "--live" => c.live = true,
            "--size" => {
                let s = v()?;
                let (w, h) = s.split_once('x').ok_or("--size WxH")?;
                c.size = Some((
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                ));
            }
            _ => return Err(format!("unknown option {k}")),
        }
    }
    Ok(c)
}

fn load_config(cli: &Cli) -> Result<Config, String> {
    let (mut cfg, path) = Config::discover(cli.config.as_deref())?;
    match &path {
        Some(p) => eprintln!("config: {}", p.display()),
        None => eprintln!("config: 既定値(設定ファイルなし)"),
    }
    if let Some(s) = cli.seconds {
        cfg.runtime.max_seconds = s;
    }
    if let Some(s) = &cli.screen {
        cfg.ui.start_screen = s.clone();
    }
    if let Some(r) = &cli.renderer {
        if !matches!(r.as_str(), "auto" | "cpu" | "gles") {
            return Err("--renderer は auto / cpu / gles".into());
        }
        cfg.display.renderer = r.clone();
    }
    Ok(cfg)
}

fn source(cfg: &Config, cli: &Cli) -> Source {
    match &cli.photo {
        Some(p) => Source::File(p.clone(), cli.caption.clone()),
        None => Source::Geoframe(cfg.geoframe.url.clone()),
    }
}

#[cfg(target_os = "linux")]
fn data_source(
    cfg: &Config,
    dummy: bool,
) -> Result<(Box<dyn app::DataSource>, screens::Snapshot), String> {
    if dummy {
        return Ok((Box::new(data::DummyData::new()), data::dummy_snapshot(0.0)));
    }
    let gpu = miharashi_linux::metrics::GpuSel::parse(&cfg.metrics.gpu)?;
    let live = data::LiveData::new(
        std::time::Duration::from_secs(cfg.metrics.interval_secs),
        gpu,
        &cfg.power,
    );
    Ok((Box::new(live), data::live_snapshot()))
}

#[cfg(target_os = "linux")]
fn run(cli: Cli) -> Result<(), String> {
    let cfg = load_config(&cli)?;
    let src = source(&cfg, &cli);
    let (data, snap) = data_source(&cfg, cli.dummy)?;
    let (alert_tx, alerts) = std::sync::mpsc::channel();
    quake::start(&cfg.alerts, &cfg.home_assistant, alert_tx.clone());
    disaster::start(&cfg.alerts, &cfg.home_assistant, alert_tx);
    let planes = aircraft::start(&cfg.aircraft);
    runtime::run(&cfg, |out| {
        Box::new(
            app::KioskApp::new(&cfg, out.size, out.gpu_max_texture, src, data, snap)
                .with_alerts(Some(alerts))
                .with_aircraft(planes),
        )
    })
}

#[cfg(not(target_os = "linux"))]
fn run(_: Cli) -> Result<(), String> {
    Err("run は Linux 専用".into())
}

fn png(cli: Cli) -> Result<(), String> {
    let cfg = load_config(&cli)?;
    let size = cli.size.unwrap_or((1920, 1080));
    let out = cli.out.clone().unwrap_or_else(|| "work/out".into());
    let mut snap = data::dummy_snapshot(0.0);
    #[cfg(target_os = "linux")]
    if cli.live {
        // --seconds 秒間、1 秒ごとに実測して時系列グラフを貯めてから描く。
        let (mut data, mut s) = data_source(&cfg, false)?;
        for _ in 0..cli.seconds.unwrap_or(2).max(2) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            data.update(&mut s);
        }
        app::set_clock(&mut s);
        snap = s;
    }
    let mut gles = gles_checker(&cfg, cli.renderer.is_some())?;
    // GLES の検証では写真をデコードしただけで受け取って GPU で合成し、CPU 版の完成フレームは別に作る
    let src = source(&cfg, &cli);
    let grouping = Grouping {
        home: match src {
            Source::Geoframe(_) => HomeLookup {
                ha: cfg.home_assistant.clone(),
                radius_km: cfg.geoframe.home_radius_km,
            }
            .resolve(),
            Source::File(..) => None,
        },
        radius_km: cfg.geoframe.home_radius_km,
    };
    let mut catalog = Catalog::open(&src, size, &grouping)?;
    catalog.gpu_max = gles.is_some().then_some(u32::MAX);
    let item = catalog.current_photo()?;
    let frame = match &item.pixels {
        PhotoPixels::Source(img) => photo::compose_image(img, size.0, size.1, &item.layout)?,
        PhotoPixels::Frame(f) => f.clone(),
        PhotoPixels::Resident => return Err("写真の画素がない".into()),
    };
    let mut text = app::text_engine(&cfg.ui.font);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut c = Canvas::new(size.0, size.1);
    let mut save = |c: &Canvas, layers: Layers<'_>, name: &str| {
        c.save_png(&out.join(format!("{name}.png")))?;
        if let Some(check) = gles.as_mut() {
            check(c, layers, &out.join(format!("{name}-gles.png")))?;
        }
        Ok::<(), String>(())
    };
    screens::status(&mut c, &mut text, &snap, screens::StatusPage::Overview);
    save(&c, Layers::Image, "status")?;
    screens::status(&mut c, &mut text, &snap, screens::StatusPage::Details);
    save(&c, Layers::Image, "status-details")?;
    screens::photo_frame(
        &mut c,
        &mut text,
        &snap,
        Some((&frame, &item.layout)),
        Some(&item.info),
        None,
        None,
    );
    // GPU 経路は写真と重ね絵(透明背景)に分けて GPU で合成する(PoC-005 PR3)
    let mut overlay = Canvas::new(size.0, size.1);
    overlay.clear(miharashi_render::canvas::rgba(0, 0, 0, 0));
    screens::photo_overlay(
        &mut overlay,
        &mut text,
        &snap,
        &item.layout,
        Some(&item.info),
        None,
    );
    save(&c, Layers::Photo(&item, &overlay), "photo")?;
    // 地震アラート(テスト表示)を写真画面の重ね絵に足したもの
    miharashi_render::alert::quake_overlay(&mut overlay, &mut text, &quake::demo_view());
    miharashi_render::alert::quake_overlay(&mut c, &mut text, &quake::demo_view());
    save(&c, Layers::Photo(&item, &overlay), "alert-quake")?;
    // 災害アラート(テスト表示)。不透明に近い幕で覆うので地震の上に重ねてよい
    let dv = disaster::demo_view();
    overlay.clear(miharashi_render::canvas::rgba(0, 0, 0, 0));
    screens::photo_overlay(
        &mut overlay,
        &mut text,
        &snap,
        &item.layout,
        Some(&item.info),
        None,
    );
    miharashi_render::alert::disaster_overlay(&mut overlay, &mut text, &dv);
    miharashi_render::alert::disaster_overlay(&mut c, &mut text, &dv);
    save(&c, Layers::Photo(&item, &overlay), "alert-disaster")?;
    // 旅のカード(一覧に旅があればそのうち最大のもの、なければ見本)
    std::fs::write(out.join("groups.txt"), catalog.group_summary()).map_err(|e| e.to_string())?;
    let cards = [
        (catalog.sample_card(false).unwrap_or_else(album::demo_card), "trip-card"),
        (catalog.sample_card(true).unwrap_or_else(album::demo_card), "everyday-card"),
    ];
    for (card, name) in cards {
        miharashi_render::trip_card::trip_card(&mut overlay, &mut text, &card);
        miharashi_render::trip_card::trip_card(&mut c, &mut text, &card);
        save(&c, Layers::Photo(&item, &overlay), name)?;
    }
    // 航空機の画面。GPU 経路は背景だけを CPU で描き、回る線と点はシェーダーで描く。残光の長さを変えた見本も出す
    let view = aircraft::demo_view();
    let geom = air::radar_geometry(&view, size);
    let mut bg = Canvas::new(size.0, size.1);
    air::aircraft_background(&mut bg, &mut text, &view);
    for (name, trail) in [
        ("aircraft", air::TRAIL),
        ("aircraft-trail-short", air::TRAIL / 2.0),
        ("aircraft-trail-long", air::TRAIL * 2.0),
    ] {
        air::aircraft_background(&mut c, &mut text, &view);
        air::sweep_cpu(&mut c, &geom, air::STILL_SWEEP, trail);
        save(&c, Layers::Radar(&bg, &geom, trail), name)?;
    }
    println!(
        "wrote {}/{{status,status-details,photo,alert-quake,alert-disaster,trip-card,everyday-card,aircraft,aircraft-trail-short,aircraft-trail-long}}.png ({}x{})",
        out.display(),
        size.0,
        size.1
    );
    Ok(())
}

/// `png --renderer gles|auto` の検証。GLES 経路(表示と同じ合成器)で表示カードの render node に
/// 描いて読み戻し、`*-gles.png` に保存して CPU の画像との差を出す。`layers` が Some なら
/// (写真のフレーム, 重ね絵)を GPU で合成し、None なら CPU の完成画面をそのまま描く。
type GlesCheck = Box<dyn FnMut(&Canvas, Layers<'_>, &std::path::Path) -> Result<(), String>>;

/// `png --renderer gles` で GPU に渡す内容。
enum Layers<'a> {
    /// CPU の完成画面をそのまま描く
    Image,
    /// (写真のフレーム, 重ね絵)を GPU で合成する
    Photo(&'a PhotoItem, &'a Canvas),
    /// (背景, レーダー, 残光の長さ)。回る線は CPU と同じ `STILL_SWEEP` の角度で描く。
    /// 線と点は CPU と描き方が違う(GPU は光のにじみを計算する)ので、差は大きく出る
    Radar(&'a Canvas, &'a air::RadarGeom, f32),
}

#[cfg(target_os = "linux")]
fn gles_checker(cfg: &Config, asked: bool) -> Result<Option<GlesCheck>, String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, scan};
    if !asked || cfg.display.renderer == "cpu" {
        return Ok(None);
    }
    let cards = scan();
    let card = choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let pci = card
        .pci
        .as_deref()
        .ok_or("表示カードの PCI アドレスが分からない")?;
    let node = miharashi_gpu::render_node_for_pci(pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("gles: {} ({})", node.display(), off.describe());
    Ok(Some(Box::new(
        move |c: &Canvas, layers, path: &std::path::Path| {
            use miharashi_gpu::Frame;
            let (w, h) = (c.width(), c.height());
            let blips = match &layers {
                Layers::Radar(_, g, _) => runtime::radar_blips(g),
                _ => Vec::new(),
            };
            let frame = match layers {
                Layers::Photo(item, overlay) => Frame::Photo {
                    photo: runtime::photo_layer(item.photo_ref(1), h),
                    old: None,
                    overlay: Some(overlay.pixmap.data()),
                },
                Layers::Image => Frame::Image(c.pixmap.data()),
                Layers::Radar(bg, g, trail) => Frame::Radar {
                    background: Some(bg.pixmap.data()),
                    radar: runtime::radar_layer(g, air::STILL_SWEEP, trail, h, &blips),
                },
            };
            let got = off.render_frames(&[frame], (w, h))?;
            let (over, max) = miharashi_gpu::diff_rgb(c.pixmap.data(), &got, 0);
            image::RgbaImage::from_raw(w, h, got)
                .ok_or("読み戻した画像の大きさが違う")?
                .save(path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            println!(
                "gles: {} 差のある画素 {over} / {}(最大差 {max})",
                path.display(),
                w as usize * h as usize
            );
            Ok(())
        },
    )))
}

#[cfg(not(target_os = "linux"))]
fn gles_checker(_: &Config, asked: bool) -> Result<Option<GlesCheck>, String> {
    if asked {
        return Err("--renderer は Linux 専用".into());
    }
    Ok(None)
}

/// CPU 描画のベースライン。`--size` を省くと 1920x1080 と 2560x1080 の両方を測る。
/// `--photo` を省くと 4032x3024 の合成画像を使う(ネットワークに依存しない)。
fn bench(cli: Cli) -> Result<(), String> {
    let cfg = load_config(&cli)?;
    let sizes = match cli.size {
        Some(s) => vec![s],
        None => vec![(1920, 1080), (2560, 1080)],
    };
    let photo = match &cli.photo {
        Some(p) => Some(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?),
        None => None,
    };
    let mut text = app::text_engine(&cfg.ui.font);
    let rows = bench::run(&mut text, &sizes, cli.frames.unwrap_or(30), photo)?;
    print!("{}", bench::format(&rows));
    Ok(())
}

#[cfg(target_os = "linux")]
fn kms_info(cli: Cli) -> Result<(), String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, choose_connector, scan};
    let cfg = load_config(&cli)?;
    let cards = scan();
    for c in &cards {
        println!(
            "{} driver={} pci={} vendor={} boot_vga={}",
            c.path.display(),
            c.driver,
            c.pci.as_deref().unwrap_or("-"),
            c.vendor_id.map_or("-".to_string(), |v| format!("{v:04x}")),
            c.boot_vga
        );
        for k in &c.connectors {
            println!(
                "  {} {} {}",
                k.name,
                if k.connected {
                    "connected"
                } else {
                    "disconnected"
                },
                k.modes
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    match choose_card(&cards, &CardSel::parse(&cfg.display.card))
        .and_then(|c| choose_connector(c, &cfg.display.connector).map(|k| (c, k)))
    {
        Ok((c, k)) => println!("=> 設定で選ばれる表示先: {} / {}", c.path.display(), k.name),
        Err(e) => println!("=> 表示先を選べない: {e}"),
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn kms_info(_: Cli) -> Result<(), String> {
    Err("kms-info は Linux 専用".into())
}

fn main() -> Result<(), String> {
    let mut rest: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if rest.first().is_some_and(|c| !c.starts_with("--")) {
        rest.remove(0)
    } else {
        "run".into()
    };
    let cli = parse_cli(rest)?;
    match cmd.as_str() {
        "run" => run(cli),
        "png" => png(cli),
        "bench" => bench(cli),
        "kms-info" => kms_info(cli),
        "check-config" => load_config(&cli).map(|_| println!("OK")),
        other => Err(format!(
            "unknown command {other}(run / png / bench / kms-info / check-config)"
        )),
    }
}
