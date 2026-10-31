//! Miharashi: Linux キオスク・ダッシュボード。`run`(実機)、`png`(ヘッドレス)、`agents-demo`、`kms-info`、
//! `bench`(CPU 描画のベースライン)、`bench-gpu`(重い描画の GPU ベンチマーク)。
mod agent_demo;
mod agents;
mod aircraft;
mod album;
mod app;
mod bench;
mod config;
mod data;
mod disaster;
mod env;
#[cfg(target_os = "linux")]
mod gpu_bench;
mod home;
mod panel;
mod photos;
mod quake;
#[cfg(target_os = "linux")]
mod runtime;
mod scene_check;
mod screen;
mod screen_power;
mod video;
mod weather;

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
    // bench-gpu
    fps: Option<u32>,
    neurons: Option<u32>,
    synapses: Option<u32>,
    stim: Option<usize>,
    gain: Option<f32>,
    lines: Option<String>,
    video: Option<PathBuf>,
    detections: Option<usize>,
    // demo-video / intro-video / agents-intro-video
    overwrite: bool,
    version: Option<String>,
    out_dir: Option<PathBuf>,
    edit: Option<PathBuf>,
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
            "--fps" => c.fps = Some(v()?.parse().map_err(|_| "bad --fps")?),
            "--neurons" => c.neurons = Some(v()?.parse().map_err(|_| "bad --neurons")?),
            "--synapses" => c.synapses = Some(v()?.parse().map_err(|_| "bad --synapses")?),
            "--stim" => c.stim = Some(v()?.parse().map_err(|_| "bad --stim")?),
            "--gain" => c.gain = Some(v()?.parse().map_err(|_| "bad --gain")?),
            "--lines" => c.lines = Some(v()?),
            "--video" => c.video = Some(v()?.into()),
            "--detections" => c.detections = Some(v()?.parse().map_err(|_| "bad --detections")?),
            "--live" => c.live = true,
            "--overwrite" => c.overwrite = true,
            "--version" => c.version = Some(v()?),
            "--out-dir" => c.out_dir = Some(v()?.into()),
            "--edit" => c.edit = Some(v()?.into()),
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

/// 設定ファイルに管理ページの上書きを重ねて読む。`consume_marker` は `run` だけが true(起動の印を消す)。
/// CLI の `--seconds` / `--screen` / `--renderer` は effective だけに掛ける。
fn load_config(cli: &Cli, consume_marker: bool) -> Result<config::LoadedConfig, String> {
    let mut loaded = Config::discover_with_overlay(cli.config.as_deref(), consume_marker)?;
    match &loaded.base_path {
        Some(p) => eprintln!("config: {}", p.display()),
        None => eprintln!("config: 既定値(設定ファイルなし)"),
    }
    // CLI 引数の検査は印を書く前に済ませる(不正な引数で終了したときに、次の起動の上書きを外させない)
    if let Some(r) = &cli.renderer {
        if !matches!(r.as_str(), "auto" | "cpu" | "gles") {
            return Err("--renderer は auto / cpu / gles".into());
        }
    }
    if consume_marker {
        // 表示・app・DRM・GL の初期化より前に印を書く(上書きで有効にした画面の初期化で落ちても次の起動で外せる)。
        // 書けなければ effective が base に戻るので、CLI の反映はこのあとにする
        loaded.arm_boot_marker();
    }
    let cfg = &mut loaded.effective;
    if let Some(s) = cli.seconds {
        cfg.runtime.max_seconds = s;
    }
    if let Some(s) = &cli.screen {
        cfg.ui.start_screen = s.clone();
    }
    if let Some(r) = &cli.renderer {
        cfg.display.renderer = r.clone();
    }
    Ok(loaded)
}

/// `check-config` の出力。(stdout, stderr)。上書きが効いていれば件数と値を、無視したときは理由を stderr に出す。
fn check_config_output(report: &config::OverlayReport) -> (String, Option<String>) {
    let mut out = String::from("OK");
    if let Some(t) = report.applied_text() {
        out.push('\n');
        out.push_str(&t);
    }
    (out, report.ignored_text())
}

/// `config-get` の失敗。終了コードは UnknownKey = 1、Unreadable = 2。
#[derive(Debug, PartialEq)]
enum ConfigGetError {
    UnknownKey(String),
    Unreadable(String),
}

/// `config-get <key>` の本体。設定ファイル + 上書き(起動時と同じ規則。印は消さない)の effective の値。
fn config_get(config: Option<&std::path::Path>, key: &str) -> Result<String, ConfigGetError> {
    let loaded =
        Config::discover_with_overlay(config, false).map_err(ConfigGetError::Unreadable)?;
    loaded
        .effective
        .value(key)
        .ok_or_else(|| ConfigGetError::UnknownKey(format!("知らないキー: {key}")))
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
    // 起動の印を書く前に登録する(登録前に SIGTERM が来ると既定の動作で死んで印が残る)
    let term = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }
    let loaded = load_config(&cli, true)?;
    if let Some(t) = loaded.report.applied_text() {
        eprintln!("{t}");
    }
    if let Some(t) = loaded.report.ignored_text() {
        eprintln!("{t}");
    }
    let cfg = &loaded.effective;
    let src = source(cfg, &cli);
    let (data, snap) = data_source(cfg, cli.dummy)?;
    let (alert_tx, alerts) = std::sync::mpsc::channel();
    quake::start(&cfg.alerts, &cfg.home_assistant, alert_tx.clone());
    let warnings = disaster::start(&cfg.alerts, &cfg.home_assistant, alert_tx);
    // 警報と航空機の取得は、画面の有無とは別に動く。管理ページが状態を読めるよう、参照を渡しておく
    let admin_disaster = app::disaster_source(cfg, warnings.clone());
    let room = room_feeds(cfg, warnings);
    let (planes, plane_health) = match aircraft::start(&cfg.aircraft) {
        Some((shared, health)) => (Some(shared), Some(health)),
        None => (None, None),
    };
    let admin_aircraft = app::aircraft_source(cfg, plane_health);
    runtime::run(&loaded, term, |out| {
        Box::new(
            app::kiosk(
                cfg,
                out.size,
                out.gpu_max_texture,
                src,
                data,
                snap,
                room,
                planes,
            )
            .with_admin(|a| {
                a.disaster = admin_disaster;
                a.aircraft = admin_aircraft;
            })
            .with_alerts(Some(alerts)),
        )
    })
}

/// 室内環境と天気の取得を始める。画面を出さない設定(無効、または HA も予報の地域も無い)なら None。
fn room_feeds(
    cfg: &Config,
    warnings: Option<disaster::HomeShared>,
) -> Option<app::screens::room::RoomFeeds> {
    if !cfg.room.enabled {
        return None;
    }
    let feeds = app::screens::room::RoomFeeds {
        env: env::start(&cfg.home_assistant, &cfg.room),
        weather: weather::start(&cfg.home_assistant, &cfg.room),
        warnings,
    };
    if feeds.env.is_none() && feeds.weather.is_none() {
        eprintln!(
            "room: HA の url も room.forecast_area も無いので、室内環境と天気の画面は出さない"
        );
        return None;
    }
    Some(feeds)
}

#[cfg(not(target_os = "linux"))]
fn run(_: Cli) -> Result<(), String> {
    Err("run は Linux 専用".into())
}

/// `png` の fixture の壁時計(いつ作っても同じ日付・時刻の絵にする)。
#[cfg(target_os = "linux")]
fn fixture_wall() -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local
        .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
        .single()
        .expect("固定の壁時計")
}

fn png(cli: Cli) -> Result<(), String> {
    png_with(cli, &open_gles_checker)
}

/// `png` の本体。GPU の検出と初期化(`open`)を差し替えられる(GPU のない環境の試験用)。
fn png_with(cli: Cli, open: &OpenGles) -> Result<(), String> {
    let cfg = load_config(&cli, false)?.effective;
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
    let mut gles = gles_checker(&cfg, cli.renderer.is_some(), open)?;
    // GLES の検証では写真をデコードしただけで受け取って GPU で合成し、CPU 版の完成フレームは別に作る
    let src = source(&cfg, &cli);
    let grouping = Grouping {
        home: match src {
            Source::Geoframe(_) => HomeLookup {
                ha: cfg.home_assistant.clone(),
                radius_km: cfg.geoframe.home_radius_km,
            }
            .resolve(),
            Source::File(..) | Source::Demo { .. } => None,
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
    // メニュー(0)を状況画面の上に開いたところ。2 つ目(写真)を選んでいる
    screens::status(&mut c, &mut text, &snap, screens::StatusPage::Overview);
    miharashi_render::menu::menu(
        &mut c,
        &mut text,
        &["STATUS", "PHOTO FRAME", "ROOM & WEATHER", "AIRCRAFT"],
        0,
        1,
    );
    save(&c, Layers::Image, "menu")?;
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
        (
            catalog.sample_card(false).unwrap_or_else(album::demo_card),
            "trip-card",
        ),
        (
            catalog.sample_card(true).unwrap_or_else(album::demo_card),
            "everyday-card",
        ),
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
    // 8/2 で 3 番目に近い機体に注目したところと、半径内に機体がいないところ
    let focused = air::AircraftView {
        focus: 2,
        ..view.clone()
    };
    air::aircraft_screen(&mut c, &mut text, &focused);
    save(&c, Layers::Image, "aircraft-focus")?;
    let empty = air::AircraftView {
        planes: Vec::new(),
        ..view.clone()
    };
    air::aircraft_screen(&mut c, &mut text, &empty);
    save(&c, Layers::Image, "aircraft-empty")?;
    // 室内環境と天気の画面。見本と、HA・気象庁に届かないとき(最後の値と取得時刻)と、まだ何も取れていないとき
    let room = miharashi_render::room::demo_view();
    miharashi_render::room::room_screen(&mut c, &mut text, &room);
    save(&c, Layers::Image, "room")?;
    let offline = app::screens::room::offline_demo(&room);
    miharashi_render::room::room_screen(&mut c, &mut text, &offline);
    save(&c, Layers::Image, "room-offline")?;
    let first = app::screens::room::first_demo();
    miharashi_render::room::room_screen(&mut c, &mut text, &first);
    save(&c, Layers::Image, "room-empty")?;
    if cli.live {
        // --live では実際の HA と気象庁から取って描く(取れるか、失敗が分かるまで最大 40 秒待つ)
        let (tx, _alerts) = std::sync::mpsc::channel();
        let warnings = disaster::start(&cfg.alerts, &cfg.home_assistant, tx);
        if let Some(feeds) = room_feeds(&cfg, warnings) {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(40);
            while std::time::Instant::now() < end && !app::screens::room::settled(&feeds) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let screen = app::screens::room::RoomScreen::new(cfg.room.tiles.clone(), feeds);
            let v = screen.view(&chrono::Local::now());
            miharashi_render::room::room_screen(&mut c, &mut text, &v);
            save(&c, Layers::Image, "room-live")?;
            println!(
                "room-live: {} / {} / {}",
                v.env.text, v.weather_feed.text, v.warn_feed.text
            );
        }
    }
    // 焼き付き対策の演出(グリッチが強まる途中と、再表示の途中)
    air::aircraft_screen(&mut c, &mut text, &view);
    let clean = Canvas {
        pixmap: c.pixmap.clone(),
    };
    for (name, t) in [("refresh-glitch", 0.45), ("refresh-return", 1.35)] {
        miharashi_render::glitch::refresh_frame(&mut c, &clean, t, 3);
        save(&c, Layers::Image, name)?;
    }
    agent_png_fixtures(&out, size, &mut text)?;
    // 3D だけの画面(試験用の場面)。GL が使えない(renderer = cpu や GPU なし)ときは案内の 1 枚になる
    let mut notice = scene_manager(&cfg, true, scene_check::Base::None, false);
    notice.render(&mut c);
    c.save_png(&out.join("scene-notice.png"))?;
    // GLES 経路では、3D の画面を ScreenManager → `Composite::Gl` → `present_scene` の同じ経路で描く
    #[cfg(target_os = "linux")]
    if let Some(check) = gles.as_mut() {
        scene_gles_fixtures(check, &cfg, size, &out)?;
        cube_gles_fixtures(check, size, &out)?;
        vrm_gles_fixtures(check, &cfg, &cli, size, &out, fixture_wall())?;
    }
    println!(
        "wrote {}/{{status,status-details,menu,photo,alert-quake,alert-disaster,trip-card,everyday-card,aircraft,aircraft-trail-short,aircraft-trail-long,aircraft-focus,aircraft-empty,room,room-offline,room-empty,refresh-glitch,refresh-return,scene-notice,agents-overview,agents-busy,agents-new-*,agents-changed-*,agents-stale,agents-empty}}.png ({}x{})",
        out.display(),
        size.0,
        size.1
    );
    #[cfg(target_os = "linux")]
    if gles.is_some() {
        println!(
            "wrote {}/{{cube-gles,cube-third-gles,photo-vrm-gles,agents-vrm-gles}}.png",
            out.display()
        );
    }
    Ok(())
}

/// `demo-video`: 1 画面の見本(仮データ)を、固定刻みの仮想時刻で動画に書き出す。
fn demo_video(cli: Cli) -> Result<(), String> {
    let screen = video::demo::DemoScreen::parse(
        cli.screen
            .as_deref()
            .ok_or("--screen が要る(status / photo / trip / room / aircraft / agents)")?,
    )?;
    let out = cli.out.ok_or("--out <file.mp4> が要る")?;
    let size = cli.size.unwrap_or((2560, 1080));
    let fps = cli.fps.unwrap_or(30);
    let seconds = u32::try_from(cli.seconds.unwrap_or(10)).map_err(|_| "--seconds が大きすぎる")?;
    let stop = video::stop_flag()?;
    let frames = video::demo::write(screen, size, fps, seconds, &out, cli.overwrite, &stop)?;
    println!(
        "wrote {} ({frames} frames, {}x{}, {fps} fps, {seconds}s)",
        out.display(),
        size.0,
        size.1
    );
    Ok(())
}

/// `intro-video`: 10 / 30 / 60 秒の紹介動画を、各画面の見本から合成して書き出す。
fn intro_video(cli: Cli) -> Result<(), String> {
    let version = cli.version.as_deref().unwrap_or("all");
    if !matches!(version, "10" | "30" | "60" | "all") {
        return Err("--version は 10 / 30 / 60 / all".into());
    }
    let out_dir = cli.out_dir.ok_or("--out-dir <dir> が要る")?;
    let stop = video::stop_flag()?;
    video::intro::write(version, &out_dir, cli.edit.as_deref(), cli.overwrite, &stop)?;
    Ok(())
}

/// Agent 専用紹介動画。設定探索・実接続は行わない。
fn agents_intro_video(cli: Cli) -> Result<(), String> {
    let version = cli.version.as_deref().unwrap_or("all");
    let out_dir = cli.out_dir.ok_or("--out-dir <dir> が要る")?;
    let stop = video::stop_flag()?;
    video::agents_intro::write(version, &out_dir, cli.edit.as_deref(), cli.overwrite, &stop)?;
    Ok(())
}

fn agents_demo(cli: Cli) -> Result<(), String> {
    // Synthetic data only: this command deliberately does not discover settings or contact T3.
    let cfg = Config::default();
    let size = cli.size.unwrap_or((1920, 1080));
    let out = cli.out.unwrap_or_else(|| "work/agents-demo".into());
    let seconds = cli.seconds.unwrap_or(45);
    let fps = cli.fps.unwrap_or(15);
    let frames = agent_demo::render_sequence(&cfg, size, &out, seconds, fps)?;
    println!(
        "wrote {frames} AgentScreen frames to {} ({}x{}, {fps} fps, {seconds}s)",
        out.display(),
        size.0,
        size.1
    );
    println!(
        "ffmpeg: ffmpeg -framerate {fps} -i {}/frame-%06d.png -pix_fmt yuv420p {}/agents-demo.mp4",
        out.display(),
        out.display()
    );
    Ok(())
}

/// Deterministic, network-free review frames from the same AgentScreen state machine used at runtime.
fn agent_png_fixtures(
    out: &std::path::Path,
    size: (u32, u32),
    text: &mut miharashi_render::TextEngine,
) -> Result<(), String> {
    agent_demo::write_fixtures(out, size, text, 30)
}

/// 試験用の 3D 画面(`scene_check::CheckScreen`)だけを登録した ScreenManager。`png` の fixture 用。
/// `gl` は GL が使えるものとして扱うか。`gl_only` は 3D だけの画面(GL が使えないときは案内)にするか。
fn scene_manager(
    cfg: &Config,
    gl_only: bool,
    base: scene_check::Base,
    gl: bool,
) -> app::manager::ScreenManager {
    let screen = scene_check::CheckScreen::new("check", gl_only, base, Default::default());
    app::manager::ScreenManager::new(
        vec![Box::new(screen)],
        "check",
        Box::new(data::DummyData::new()),
        data::dummy_snapshot(0.0),
        app::text_engine(&cfg.ui.font),
        std::time::Duration::from_secs(60),
    )
    .with_gl_available(gl)
}

/// `png --renderer gles|auto`: キューブ画面を GLES で描いた `cube-gles.png`(全面)と `cube-third-gles.png`(中央 1/3 幅)。
/// 回転の時間は 0(表示してから時間が経っていない)なので、いつ作っても同じ絵になる。
#[cfg(target_os = "linux")]
fn cube_gles_fixtures(
    check: &mut GlesCheck,
    size: (u32, u32),
    out: &std::path::Path,
) -> Result<(), String> {
    use crate::screen::Composite;
    for (name, third) in [("cube", false), ("cube-third", true)] {
        let screen = app::screens::cube::CubeScreen::new(third, 30);
        let mut mgr = app::manager::ScreenManager::new(
            vec![Box::new(screen)],
            name,
            Box::new(data::DummyData::new()),
            data::dummy_snapshot(0.0),
            app::text_engine(""),
            std::time::Duration::from_secs(60),
        )
        .with_gl_available(true);
        let mut c = Canvas::new(size.0, size.1);
        let comp = mgr.render_composite(&mut c);
        let mut parts = runtime::FrameParts::default();
        parts.fill(&comp);
        let Composite::Gl(g) = comp else {
            return Err("キューブ画面が Composite::Gl を返さない".into());
        };
        let frame = runtime::scene_frame(g, &c, &parts);
        check(
            &c,
            Layers::Scene(frame),
            &out.join(format!("{name}-gles.png")),
        )?;
    }
    Ok(())
}

/// `png --renderer gles|auto`: 写真画面と Agent 画面に VRM を載せた `photo-vrm-gles.png` と `agents-vrm-gles.png`
/// (ScreenManager → `WithScene` → `Composite::Gl` → `present_scene` の本番と同じ経路)。
/// モデルは `[vrm]` が無効なら合成モデル(fixture。いつ作っても同じ絵になる)、有効なら `vrm.model`
/// (実際のモデルでの目視用。リポジトリには入れない)。時刻は固定で、写真は表示から 6 秒、Agent は新しいスレッドのカードが
/// 中央に止まっているところ(3 秒余り)。壁時計(写真の日付・時刻、Agent 画面の見出しの時刻と最終取得)も
/// `wall` に固定するので、同じ入力から作った PNG は画素まで一致する。
#[cfg(target_os = "linux")]
fn vrm_gles_fixtures(
    check: &mut GlesCheck,
    cfg: &Config,
    cli: &Cli,
    size: (u32, u32),
    out: &std::path::Path,
    wall: chrono::DateTime<chrono::Local>,
) -> Result<(), String> {
    use crate::screen::{App, Composite, Provider, World};
    use app::screens::{agents::AgentScreen, photo::PhotoScreen, with_scene::WithScene};
    use std::time::{Duration, Instant, SystemTime};

    /// Agent 画面に渡す一覧を、途中で差し替えられる Provider
    struct Feed(std::rc::Rc<std::cell::RefCell<agents::AgentSnapshot>>);
    impl Provider for Feed {
        fn update(&mut self, world: &mut World) {
            world.agents = self.0.borrow().clone();
        }
    }

    let scene = || -> Result<miharashi_vrm::VrmScene, String> {
        let count = std::rc::Rc::new(miharashi_glscene::ObjectCount::default());
        if !cfg.vrm.enabled {
            let glb = miharashi_vrm::synth::build_vrm_glb(&Default::default());
            let model = miharashi_vrm::Model::from_glb(&glb)?;
            return Ok(miharashi_vrm::VrmScene::from_model(model, count));
        }
        let mut scene = miharashi_vrm::VrmScene::from_path(cfg.vrm.model.clone().into(), count);
        let end = Instant::now() + Duration::from_secs(60);
        while scene.state() == miharashi_vrm::LoadState::Loading && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            scene.poll();
        }
        match scene.state() {
            miharashi_vrm::LoadState::Ready => Ok(scene),
            _ => Err(format!(
                "vrm.model を読めない: {}",
                scene.error().unwrap_or("読み込みが終わらない")
            )),
        }
    };
    let manager = |screen: Box<dyn crate::screen::Screen>, id: &str| {
        app::manager::ScreenManager::new(
            vec![screen],
            id,
            Box::new(data::DummyData::new()),
            data::dummy_snapshot(0.0),
            app::text_engine(&cfg.ui.font),
            Duration::from_secs(60),
        )
        .with_gl_available(true)
        .with_fixed_clock(wall)
    };
    let region = cfg.vrm.stage_region();
    let mut save = |mgr: &mut app::manager::ScreenManager, name: &str| -> Result<(), String> {
        let mut c = Canvas::new(size.0, size.1);
        let comp = mgr.render_composite(&mut c);
        let mut parts = runtime::FrameParts::default();
        parts.fill(&comp);
        let Composite::Gl(g) = comp else {
            return Err(format!("{name}: Composite::Gl を返さない"));
        };
        let frame = runtime::scene_frame(g, &c, &parts);
        check(
            &c,
            Layers::Scene(frame),
            &out.join(format!("{name}-gles.png")),
        )
    };

    // 写真 + VRM。写真が届くまでは時刻を進めない(時刻は表示してからの経過で決まる)
    let t0 = Instant::now();
    let photo = PhotoScreen::new(cfg, size, Some(u32::MAX), source(cfg, cli));
    let feed = photo.feed_provider();
    let screen = WithScene::new(photo, scene()?, region, cfg.vrm.fps);
    let mut mgr = manager(Box::new(screen), "photo").with_provider(Box::new(feed));
    let end = Instant::now() + Duration::from_secs(10);
    while mgr.panel_view().and_then(|v| v.photo).is_none() {
        if Instant::now() >= end {
            return Err("vrm: 写真が届かない".into());
        }
        mgr.tick(t0);
        std::thread::sleep(Duration::from_millis(10));
    }
    mgr.tick(t0 + Duration::from_secs(6));
    save(&mut mgr, "photo-vrm")?;

    // Agent + VRM。新しいスレッドのカードが中央に拡大して止まるまで、0.1 秒ずつ進める(頭の向きの重みが出るまで)
    let wall_sys = SystemTime::from(wall);
    let (initial, fresh, _) = agent_demo::fixture_snapshots(wall_sys, t0);
    let shared = std::rc::Rc::new(std::cell::RefCell::new(initial));
    let screen = WithScene::new(
        AgentScreen::new(size, true, cfg.agents.stale_secs).with_fixed_wall(wall_sys),
        scene()?,
        region,
        cfg.vrm.fps,
    );
    let mut mgr = manager(Box::new(screen), "agents").with_provider(Box::new(Feed(shared.clone())));
    mgr.tick(t0);
    *shared.borrow_mut() = fresh;
    let mut c = Canvas::new(size.0, size.1);
    for k in 10..=32 {
        mgr.tick(t0 + Duration::from_millis(100 * k));
        // 頭の向きを進めるため、描画と同じ呼び出しを毎回通す
        let _ = mgr.render_composite(&mut c);
    }
    // 保存は新しい Compositor で描くので、下地(背景)も描き直させる(GL の可否を切り替えると、現在の画面を invalidate する)
    let mut mgr = mgr.with_gl_available(false).with_gl_available(true);
    save(&mut mgr, "agents-vrm")
}

/// `png --renderer gles|auto`: 3D の画面を GLES で描いた `scene-gles.png`(3D だけ)、`scene-base-gles.png`
/// (下地つき)、`scene-alert-gles.png`(下地つきでアラートを 3D の上に重ねたもの)を保存する。
/// 3D は CPU の画像と画素比較できない(CPU 版がない)ので、差は出さない。
#[cfg(target_os = "linux")]
fn scene_gles_fixtures(
    check: &mut GlesCheck,
    cfg: &Config,
    size: (u32, u32),
    out: &std::path::Path,
) -> Result<(), String> {
    use crate::screen::Composite;
    use scene_check::Base;
    for (name, gl_only, base, alert) in [
        ("scene", true, Base::None, false),
        ("scene-base", false, Base::Canvas, false),
        ("scene-alert", false, Base::Canvas, true),
    ] {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut mgr = scene_manager(cfg, gl_only, base, true).with_alerts(Some(rx));
        if alert {
            tx.send(miharashi_render::alert::AlertView::Quake(quake::demo_view()))
                .map_err(|e| e.to_string())?;
            mgr.tick(std::time::Instant::now());
        }
        let mut c = Canvas::new(size.0, size.1);
        let comp = mgr.render_composite(&mut c);
        let mut parts = runtime::FrameParts::default();
        parts.fill(&comp);
        let Composite::Gl(g) = comp else {
            return Err("試験用の 3D 画面が Composite::Gl を返さない".into());
        };
        let frame = runtime::scene_frame(g, &c, &parts);
        check(
            &c,
            Layers::Scene(frame),
            &out.join(format!("{name}-gles.png")),
        )?;
    }
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
    /// 3D の画面(下地 → 3D → アラート)。CPU の画像がないので、画素比較はしない
    #[cfg(target_os = "linux")]
    Scene(miharashi_gpu::SceneFrame<'a>),
}

/// GPU の検出と初期化(表示カードの選択・PCI・render node を開く)をまとめたもの。
type OpenGles = dyn Fn(&Config) -> Result<GlesCheck, String>;

/// `--renderer` が指定されたときの GLES 検証の準備。`auto` は GPU が使えなければ理由をログに出して
/// None(GPU なし。CPU の画像と案内だけ保存する)、`gles` は明示なのでエラーにする。
fn gles_checker(cfg: &Config, asked: bool, open: &OpenGles) -> Result<Option<GlesCheck>, String> {
    if !asked || cfg.display.renderer == "cpu" {
        return Ok(None);
    }
    match open(cfg) {
        Ok(check) => Ok(Some(check)),
        Err(e) if cfg.display.renderer == "auto" => {
            eprintln!("gles: GPU を使えない({e})。CPU の画像と案内だけ保存する");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "linux")]
fn open_gles_checker(cfg: &Config) -> Result<GlesCheck, String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, scan};
    let cards = scan();
    let card = choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let pci = card
        .pci
        .as_deref()
        .ok_or("表示カードの PCI アドレスが分からない")?;
    let node = miharashi_gpu::render_node_for_pci(pci);
    let off = miharashi_gpu::Offscreen::open(&node)?;
    println!("gles: {} ({})", node.display(), off.describe());
    Ok(Box::new(
        move |c: &Canvas, layers, path: &std::path::Path| {
            use miharashi_gpu::{Frame, SceneOutcome};
            let (w, h) = (c.width(), c.height());
            let layers = match layers {
                Layers::Scene(mut frame) => {
                    let (outcome, got) = off.render_scene(&mut frame, (w, h))?;
                    if let SceneOutcome::SceneFailed(msg) = outcome {
                        return Err(format!("{}: 3D の描画に失敗: {msg}", path.display()));
                    }
                    image::RgbaImage::from_raw(w, h, got)
                        .ok_or("読み戻した画像の大きさが違う")?
                        .save(path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    println!("gles: {} 3D を描いた", path.display());
                    return Ok(());
                }
                other => other,
            };
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
                Layers::Scene(_) => unreachable!("上で扱った"),
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
    ))
}

#[cfg(not(target_os = "linux"))]
fn open_gles_checker(_: &Config) -> Result<GlesCheck, String> {
    Err("--renderer は Linux 専用".into())
}

/// CPU 描画のベースライン。`--size` を省くと 1920x1080 と 2560x1080 の両方を測る。
/// `--photo` を省くと 4032x3024 の合成画像を使う(ネットワークに依存しない)。
fn bench(cli: Cli) -> Result<(), String> {
    let cfg = load_config(&cli, false)?.effective;
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

/// `bench-metrics [--frames N]`: 監視スレッドの読み取りを 1 種類ずつ、1 秒ごとに N 回(既定 20)測る。
#[cfg(target_os = "linux")]
fn bench_metrics(cli: Cli) -> Result<(), String> {
    use miharashi_linux::metrics::{GpuSel, SamplerOptions, profile};
    let cfg = load_config(&cli, false)?.effective;
    let gpu = GpuSel::parse(&cfg.metrics.gpu)?;
    let opts = SamplerOptions {
        state_file: None,
        rapl_helper_file: cfg.power.rapl_helper_file.clone().into(),
    };
    let rounds = cli.frames.unwrap_or(20) as u32;
    let interval = std::time::Duration::from_secs(cfg.metrics.interval_secs.max(1));
    let rd = |f: &str| {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu0/cpufreq/{f}"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "-".into())
    };
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    println!(
        "bench-metrics: {rounds} 回、{}s ごと(1 回あたりの平均)。cpu0 {}kHz({})、loadavg {}",
        interval.as_secs(),
        rd("scaling_cur_freq"),
        rd("scaling_governor"),
        load.split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let costs = profile(rounds, interval, &gpu, &opts);
    let (mut wall, mut cpu) = (0.0, 0.0);
    println!("{:<40} {:>10} {:>10}", "読み取り", "経過 [µs]", "CPU [µs]");
    for c in &costs {
        println!("{:<40} {:>10.1} {:>10.1}", c.name, c.wall_us, c.cpu_us);
        if !c.name.starts_with("init:") {
            wall += c.wall_us;
            cpu += c.cpu_us;
        }
    }
    println!(
        "{:<40} {:>10.1} {:>10.1}  (1 秒ごとなら CPU {:.2}%)",
        "合計(init を除く)",
        wall,
        cpu,
        cpu / interval.as_secs_f64() / 1e4
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn bench_metrics(_: Cli) -> Result<(), String> {
    Err("bench-metrics は Linux 専用".into())
}

/// `bench-gpu [--size WxH] [--seconds N] [--fps N(0 = 上限)] [--neurons N] [--synapses N]
/// [--stim N] [--gain X] [--lines none|all|active] [--out FILE.png]`(B1 コネクトーム)
/// `bench-gpu --video FILE [--detections N] [--size WxH] [--seconds N] [--fps N] [--out FILE.png]`
/// (B2 動画と検出枠。`--features video` でビルドしたときだけ)
#[cfg(target_os = "linux")]
fn bench_gpu(cli: Cli) -> Result<(), String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, scan};
    let cfg = load_config(&cli, false)?.effective;
    let cards = scan();
    let card = choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let pci = card
        .pci
        .clone()
        .ok_or("表示カードの PCI アドレスが分からない")?;
    if let Some(file) = cli.video.clone() {
        #[cfg(feature = "video")]
        return gpu_bench::run_video(&gpu_bench::VideoOpts {
            pci,
            file,
            size: cli.size.unwrap_or((2560, 1080)),
            duration: std::time::Duration::from_secs(cli.seconds.unwrap_or(60)),
            fps: cli.fps.unwrap_or(0),
            detections: cli.detections.unwrap_or(50),
            font: cfg.ui.font.clone(),
            rapl_helper: cfg.power.rapl_helper_file.clone().into(),
            out: cli.out.clone(),
        });
        #[cfg(not(feature = "video"))]
        return Err(format!(
            "--video {} は `cargo build --release --features video` でビルドしたときだけ使える",
            file.display()
        ));
    }
    gpu_bench::run(&gpu_bench::Opts {
        pci,
        size: cli.size.unwrap_or((2560, 1080)),
        duration: std::time::Duration::from_secs(cli.seconds.unwrap_or(60)),
        fps: cli.fps.unwrap_or(30),
        neurons: cli.neurons.unwrap_or(160_000),
        synapses: cli.synapses.unwrap_or(3_000_000),
        stim: cli.stim.unwrap_or(4),
        gain: cli.gain.unwrap_or(miharashi_gpu::connectome::GAIN),
        lines: miharashi_gpu::connectome::Lines::parse(cli.lines.as_deref().unwrap_or("active"))?,
        rapl_helper: cfg.power.rapl_helper_file.clone().into(),
        out: cli.out.clone(),
    })
}

#[cfg(not(target_os = "linux"))]
fn bench_gpu(_: Cli) -> Result<(), String> {
    Err("bench-gpu は Linux 専用".into())
}

/// `bench-scene [--size WxH(既定は 1280x720・1920x1080・2560x1080 の全部)] [--frames N(連続、既定 600)]
/// [--fps N(0 = 連続だけ、既定 30)] [--seconds N(--fps の間隔で描く秒数、既定 5)]`(キューブの GPU 時間)
#[cfg(target_os = "linux")]
fn bench_scene(cli: Cli) -> Result<(), String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, scan};
    let cfg = load_config(&cli, false)?.effective;
    let cards = scan();
    let card = choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let pci = card
        .pci
        .clone()
        .ok_or("表示カードの PCI アドレスが分からない")?;
    gpu_bench::run_scene(&gpu_bench::SceneOpts {
        pci,
        sizes: gpu_bench::scene_sizes(cli.size),
        frames: cli.frames.unwrap_or(600),
        fps: cli.fps.unwrap_or(30),
        seconds: cli.seconds.unwrap_or(5),
    })
}

#[cfg(not(target_os = "linux"))]
fn bench_scene(_: Cli) -> Result<(), String> {
    Err("bench-scene は Linux 専用".into())
}

#[cfg(target_os = "linux")]
fn kms_info(cli: Cli) -> Result<(), String> {
    use miharashi_linux::drm_select::{CardSel, choose_card, choose_connector, scan};
    let cfg = load_config(&cli, false)?.effective;
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
    // config-get は最初の引数がキー(位置引数)
    let key = (cmd == "config-get" && rest.first().is_some_and(|k| !k.starts_with("--")))
        .then(|| rest.remove(0));
    let cli = parse_cli(rest)?;
    match cmd.as_str() {
        "run" => run(cli),
        "png" => png(cli),
        "agents-demo" => agents_demo(cli),
        "demo-video" => demo_video(cli),
        "intro-video" => intro_video(cli),
        "agents-intro-video" => agents_intro_video(cli),
        "bench" => bench(cli),
        "bench-gpu" => bench_gpu(cli),
        "bench-scene" => bench_scene(cli),
        "bench-metrics" => bench_metrics(cli),
        "kms-info" => kms_info(cli),
        "check-config" => load_config(&cli, false).map(|loaded| {
            let (out, err) = check_config_output(&loaded.report);
            println!("{out}");
            if let Some(e) = err {
                eprintln!("{e}");
            }
        }),
        "config-get" => {
            let Some(key) = key else {
                eprintln!("使い方: miharashi config-get <key> [--config FILE]");
                std::process::exit(2);
            };
            match config_get(cli.config.as_deref(), &key) {
                Ok(v) => {
                    println!("{v}");
                    Ok(())
                }
                Err(ConfigGetError::UnknownKey(e)) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
                Err(ConfigGetError::Unreadable(e)) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            }
        }
        other => Err(format!(
            "unknown command {other}(run / png / agents-demo / demo-video / intro-video / agents-intro-video / bench / bench-gpu / bench-scene / bench-metrics / kms-info / check-config / config-get)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_for(out: &std::path::Path, photo: &std::path::Path, renderer: &str) -> Cli {
        Cli {
            config: Some("/dev/null".into()),
            out: Some(out.into()),
            size: Some((96, 54)),
            photo: Some(photo.into()),
            renderer: Some(renderer.into()),
            ..Cli::default()
        }
    }

    /// 不正な CLI 引数(`--renderer`)は、起動の印を書く前にエラーにする(次の起動で上書きが外れない)。
    #[test]
    fn invalid_renderer_fails_before_the_boot_marker_is_written() {
        let dir =
            std::env::temp_dir().join(format!("miharashi-marker-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let overlay = dir.join("admin.toml");
        std::fs::write(&overlay, "[room]\nenabled = false\n").unwrap();
        let base = dir.join("config.toml");
        std::fs::write(
            &base,
            format!("[admin]\noverlay_file = \"{}\"\n", overlay.display()),
        )
        .unwrap();
        let mut cli = Cli {
            config: Some(base),
            renderer: Some("invalid".into()),
            ..Cli::default()
        };
        assert!(load_config(&cli, true).is_err());
        assert!(!dir.join("admin.boot").exists(), "印は書かれない");
        // 正しい引数なら印を書き、CLI の値は effective に残る
        cli.renderer = Some("cpu".into());
        cli.seconds = Some(7);
        let l = load_config(&cli, true).unwrap();
        assert!(dir.join("admin.boot").exists());
        assert_eq!(l.effective.display.renderer, "cpu");
        assert_eq!(l.effective.runtime.max_seconds, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn no_gpu(_: &Config) -> Result<GlesCheck, String> {
        Err("GPU が見えない(試験)".into())
    }

    /// GPU が使えない環境の `png --renderer auto` は、CPU の画像と案内を保存して成功し、
    /// `*-gles.png` は作らない。明示した `gles` はエラーのまま。
    #[test]
    fn png_auto_without_gpu_saves_the_notice_but_gles_fails() {
        let dir = std::env::temp_dir().join(format!("miharashi-png-nogpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("in.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 200]))
            .save(&photo)
            .unwrap();

        let out = dir.join("auto");
        png_with(cli_for(&out, &photo, "auto"), &no_gpu).expect("auto は GPU なしで続ける");
        assert!(out.join("scene-notice.png").exists());
        assert!(out.join("status.png").exists());
        let gles: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with("-gles.png"))
            .collect();
        assert!(gles.is_empty(), "{gles:?}");

        let out = dir.join("gles");
        let err = png_with(cli_for(&out, &photo, "gles"), &no_gpu).unwrap_err();
        assert!(err.contains("GPU が見えない"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写真 + VRM と Agent + VRM の PNG は、壁時計が進んでも(写真の時計・Agent の見出しと最終取得)
    /// 同じ入力から画素まで同じになる。GPU がなければ見ない。
    #[cfg(target_os = "linux")]
    #[test]
    fn vrm_png_fixtures_do_not_depend_on_the_wall_clock() {
        let dir = std::env::temp_dir().join(format!("miharashi-png-vrm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("in.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 200]))
            .save(&photo)
            .unwrap();
        let names = ["photo-vrm-gles.png", "agents-vrm-gles.png"];
        let (a, b) = (dir.join("a"), dir.join("b"));
        // skip は GLES を開けないときだけ。開けたあとの描画・保存の失敗はテストの失敗にする。
        let opened = std::rc::Rc::new(std::cell::Cell::new(false));
        let open = {
            let opened = opened.clone();
            move |cfg: &Config| {
                let r = open_gles_checker(cfg);
                opened.set(r.is_ok());
                r
            }
        };
        png_with(cli_for(&a, &photo, "auto"), &open).unwrap();
        if !opened.get() {
            eprintln!("skip: GLES を開けない");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        for n in names {
            assert!(a.join(n).exists(), "{n} がない");
        }
        // 時計の秒が必ず変わるだけ待つ
        std::thread::sleep(std::time::Duration::from_millis(1100));
        png_with(cli_for(&b, &photo, "auto"), &open_gles_checker).unwrap();
        for n in names {
            let (x, y) = (
                std::fs::read(a.join(n)).unwrap(),
                std::fs::read(b.join(n)).unwrap(),
            );
            assert!(x == y, "{n}: 時刻が変わると絵が変わる");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    fn report(applied: Vec<(&'static str, String)>, error: Option<&str>) -> config::OverlayReport {
        config::OverlayReport {
            path: "/var/lib/miharashi/admin.toml".into(),
            exists: true,
            generation: "0123456789abcdef".into(),
            applied,
            error: error.map(str::to_string),
            error_kind: None,
            boot_failed_generation: None,
        }
    }

    /// AC14: check-config の出力は、効いている・無い・無視した、の 3 通り。
    #[test]
    fn check_config_output_for_applied_none_and_ignored() {
        let applied = report(
            vec![
                ("aircraft.enabled", "false".into()),
                ("agents.url", "http://192.0.2.30:3773".into()),
            ],
            None,
        );
        assert_eq!(
            check_config_output(&applied),
            (
                "OK\n上書き: /var/lib/miharashi/admin.toml(2 項目)\n  aircraft.enabled = false\n  agents.url = http://192.0.2.30:3773".to_string(),
                None
            )
        );
        assert_eq!(
            check_config_output(&report(vec![], None)),
            ("OK".to_string(), None)
        );
        assert_eq!(
            check_config_output(&report(vec![], Some("知らないキーがある"))),
            (
                "OK".to_string(),
                Some(
                    "上書き: /var/lib/miharashi/admin.toml を無視した: 知らないキーがある"
                        .to_string()
                )
            )
        );
    }

    /// 一時ディレクトリに base と overlay を置き、base のパスを返す。
    fn config_dir(name: &str, base_extra: &str, overlay: Option<&str>) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("miharashi-cfgget-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("config.toml");
        std::fs::write(
            &base,
            format!(
                "[admin]\noverlay_file = \"{}\"\n{base_extra}",
                dir.join("admin.toml").display()
            ),
        )
        .unwrap();
        if let Some(o) = overlay {
            std::fs::write(dir.join("admin.toml"), o).unwrap();
        }
        (dir, base)
    }

    /// AC17: config-get は起動時と同じ規則の値を出す。印には触らない。
    #[test]
    fn config_get_follows_the_startup_rules() {
        let file_url = "http://192.0.2.30:3773";
        let base_text = format!("[agents]\nurl = \"{file_url}\"\n");
        // 上書きなし → 設定ファイルの値
        let (dir, base) = config_dir("none", &base_text, None);
        assert_eq!(
            config_get(Some(&base), "agents.url"),
            Ok(file_url.to_string())
        );
        assert_eq!(
            config_get(Some(&base), "aircraft.enabled"),
            Ok("true".to_string())
        );
        // 上書きあり → 上書きの値(同じ origin、許可ホスト)
        std::fs::write(
            dir.join("admin.toml"),
            "[agents]\nurl = \"http://192.0.2.30:3773/\"\n[aircraft]\nenabled = false\n[geoframe]\nurl = \"http://192.0.2.20:8080\"\n",
        )
        .unwrap();
        assert_eq!(
            config_get(Some(&base), "aircraft.enabled"),
            Ok("false".to_string())
        );
        assert_eq!(
            config_get(Some(&base), "geoframe.url"),
            Ok("http://192.0.2.20:8080".to_string())
        );
        assert_eq!(
            config_get(Some(&base), "agents.url"),
            Ok("http://192.0.2.30:3773/".to_string())
        );
        // 許されないホスト → 上書き全体を無視して base の値
        std::fs::write(
            dir.join("admin.toml"),
            "[agents]\nurl = \"http://192.0.2.31:3773\"\n[aircraft]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(
            config_get(Some(&base), "agents.url"),
            Ok(file_url.to_string())
        );
        assert_eq!(
            config_get(Some(&base), "aircraft.enabled"),
            Ok("true".to_string())
        );
        // 壊れた上書き・知らないキーのある上書き → base の値
        for broken in [
            "[agents\nurl = ",
            "[agents]\ntoken_file = \"/x\"\n[aircraft]\nenabled = false\n",
        ] {
            std::fs::write(dir.join("admin.toml"), broken).unwrap();
            assert_eq!(
                config_get(Some(&base), "aircraft.enabled"),
                Ok("true".to_string()),
                "{broken}"
            );
            assert_eq!(
                config_get(Some(&base), "agents.url"),
                Ok(file_url.to_string())
            );
        }
        // 前回の起動が早く終わった印 → 上書きを無視し、印は消さない
        std::fs::write(dir.join("admin.toml"), "[aircraft]\nenabled = false\n").unwrap();
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let marker = dir.join("admin.boot");
        std::fs::write(
            &marker,
            format!("{{\"pid\":2000000000,\"boot_id\":\"{}\",\"generation\":\"x\",\"started_at\":\"\"}}\n", boot.trim()),
        )
        .unwrap();
        if !boot.trim().is_empty() {
            assert_eq!(
                config_get(Some(&base), "aircraft.enabled"),
                Ok("true".to_string())
            );
        }
        assert!(marker.exists(), "config-get は印を消さない");
        // 知らないキー → UnknownKey(終了コード 1)、設定が読めない → Unreadable(終了コード 2)
        assert!(matches!(
            config_get(Some(&base), "panel.token_file"),
            Err(ConfigGetError::UnknownKey(_))
        ));
        assert!(matches!(
            config_get(Some(&base), "nope"),
            Err(ConfigGetError::UnknownKey(_))
        ));
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "nope = 1\n").unwrap();
        assert!(matches!(
            config_get(Some(&bad), "agents.url"),
            Err(ConfigGetError::Unreadable(_))
        ));
        assert!(matches!(
            config_get(Some(&dir.join("missing.toml")), "agents.url"),
            Err(ConfigGetError::Unreadable(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
