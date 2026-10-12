//! Miharashi: Linux キオスク・ダッシュボード。`run`(実機)、`png`(ヘッドレス)、`kms-info`。
mod app;
mod config;
mod data;
mod photos;
#[cfg(target_os = "linux")]
mod runtime;

use config::Config;
use miharashi_render::{Canvas, screens};
use photos::{Catalog, Source};
use std::path::PathBuf;

#[derive(Default)]
struct Cli {
    config: Option<PathBuf>,
    out: Option<PathBuf>,
    size: Option<(u32, u32)>,
    photo: Option<PathBuf>,
    caption: Option<String>,
    seconds: Option<u64>,
    screen: Option<String>,
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
            "--screen" => c.screen = Some(v()?),
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
    runtime::run(&cfg, |out| {
        app::KioskApp::new(&cfg, out.size, src, data, snap)
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
    let item = Catalog::open(&source(&cfg, &cli), size)?.current()?;
    let mut text = app::text_engine(&cfg.ui.font);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut c = Canvas::new(size.0, size.1);
    screens::status(&mut c, &mut text, &snap, screens::StatusPage::Overview);
    c.save_png(&out.join("status.png"))?;
    screens::status(&mut c, &mut text, &snap, screens::StatusPage::Details);
    c.save_png(&out.join("status-details.png"))?;
    screens::photo_frame(
        &mut c,
        &mut text,
        &snap,
        Some((&item.frame, &item.layout)),
        Some(&item.info),
        None,
        None,
    );
    c.save_png(&out.join("photo.png"))?;
    println!(
        "wrote {}/{{status,status-details,photo}}.png ({}x{})",
        out.display(),
        size.0,
        size.1
    );
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
        "kms-info" => kms_info(cli),
        "check-config" => load_config(&cli).map(|_| println!("OK")),
        other => Err(format!(
            "unknown command {other}(run / png / kms-info / check-config)"
        )),
    }
}
