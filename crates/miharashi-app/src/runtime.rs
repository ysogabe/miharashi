//! キオスクの実行ループ。画面ロジックとは `screen::App` だけでつながり、具体的な型は知らない。
//! seat/VT の有効・無効、SIGINT/SIGTERM、入力、描画、画面の電源(DPMS)、状態ログを扱う。
//! 状態ログは共通部分(t / rss / fds / frames / worst_frame)に `App::stats()` の `k=v` を続けて出す。
use crate::config::Config;
use crate::panel::{DisplayControl, DisplayStatus, Panel};
use crate::screen::{App, Composite, KeyPress, PhotoData, PhotoRef};
use crate::screen_power::{Mode as PowerMode, ScreenPower};
use miharashi_gpu::{Frame, GlesDisplay, PhotoLayer, PhotoSource, Placement, Radar, SourceImage};
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::input::{Action, Keys};
use miharashi_linux::kms::Display;
use miharashi_linux::seat::{SeatMode, Session, SessionEvent};
use miharashi_linux::vt::VtGuard;
use miharashi_render::Canvas;
use miharashi_render::aircraft::{self, BlipKind, RadarGeom};
use miharashi_render::photo_layout::{self, Mode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / 1024 / 1024)
}

fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

/// 状態ログ 1 行を作る。共通部分のあとに画面ロジックの `k=v` を並べる(tools/soak_summary.py が読む)。
fn format_stats(
    t: u64,
    rss_mb: u64,
    fds: usize,
    frames: u64,
    worst_ms: u128,
    extra: &[(&'static str, String)],
) -> String {
    let mut line =
        format!("stats: t={t}s rss={rss_mb}MB fds={fds} frames={frames} worst_frame={worst_ms}ms");
    for (k, v) in extra {
        line.push_str(&format!(" {k}={v}"));
    }
    line
}

/// 区間内のフレーム時間 [ms] を "平均/p95" にする(PoC-005 のベースライン用)。空なら "-"。
fn summarize_ms(samples: &mut Vec<f32>) -> String {
    if samples.is_empty() {
        return "-".into();
    }
    samples.sort_by(f32::total_cmp);
    let avg = samples.iter().sum::<f32>() / samples.len() as f32;
    let p95 = samples[((samples.len() as f32 * 0.95).ceil() as usize).clamp(1, samples.len()) - 1];
    samples.clear();
    format!("{avg:.1}/{p95:.1}")
}

/// 画面ロジックの写真を GPU の合成器の形にする。`screen_h` は画面の高さ(影の間隔の倍率に使う)。
pub fn photo_layer(p: PhotoRef<'_>, screen_h: u32) -> PhotoLayer<'_> {
    let src = match p.data {
        PhotoData::Frame(f) => PhotoSource::Frame(f.data()),
        PhotoData::Source { image, layout } => PhotoSource::Image(SourceImage {
            pixels: image.as_bytes(),
            size: (image.width(), image.height()),
            channels: image.color().channel_count() as u32,
            place: Placement {
                cover: layout.mode == Mode::Cover,
                rect: (
                    layout.photo.x,
                    layout.photo.y,
                    layout.photo.w,
                    layout.photo.h,
                ),
                scale: photo_layout::scale(screen_h),
            },
        }),
        PhotoData::Resident => PhotoSource::Resident,
    };
    PhotoLayer { id: p.id, src }
}

/// レーダーの点を GPU の合成器の形 (x, y, 種類) にする。
pub fn radar_blips(g: &RadarGeom) -> Vec<[f32; 3]> {
    g.blips
        .iter()
        .map(|b| {
            let kind = match b.kind {
                BlipKind::Other => 0.0,
                BlipKind::Focus => 1.0,
                BlipKind::Ground => 2.0,
            };
            [b.x, b.y, kind]
        })
        .collect()
}

/// 航空機の画面のレーダーを GPU の合成器の形にする。`blips` は `radar_blips` の結果。
pub fn radar_layer<'a>(
    g: &RadarGeom,
    sweep: f32,
    trail: f32,
    screen_h: u32,
    blips: &'a [[f32; 3]],
) -> Radar<'a> {
    Radar {
        center: g.center,
        radius: g.radius,
        sweep,
        trail,
        scale: photo_layout::scale(screen_h),
        blips,
    }
}

/// 表示経路。CPU は Canvas を dumb buffer へ変換コピーし、GLES は表示カードの GPU で
/// テクスチャとして描いて page flip する(PoC-005 PR2。見た目は同じ)。
// 1 つしか作らないので、variant の大きさの差(glow のコンテキストが約 6KB)は気にしない
#[allow(clippy::large_enum_variant)]
enum Presenter {
    Cpu(Display),
    Gles(GlesDisplay),
}

impl Presenter {
    /// `renderer` は auto / cpu / gles。auto で GLES が使えなければ理由をログに出して CPU に戻る。
    fn open(
        fd: &std::os::fd::OwnedFd,
        renderer: &str,
        connector: &str,
        mode: ModeSel,
    ) -> Result<Self, String> {
        let dup = || fd.try_clone().map_err(|e| e.to_string());
        if renderer != "cpu" {
            match GlesDisplay::open(dup()?, connector, mode) {
                Ok(d) => {
                    eprintln!("renderer: gles ({})", d.describe());
                    return Ok(Self::Gles(d));
                }
                Err(e) if renderer == "auto" => {
                    eprintln!("renderer: GLES が使えない: {e}(cpu に戻る)")
                }
                Err(e) => return Err(format!("renderer=gles: {e}")),
            }
        }
        eprintln!("renderer: cpu (dumb buffer)");
        Ok(Self::Cpu(Display::open(dup()?, connector, mode)?))
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Cpu(d) => d.size(),
            Self::Gles(d) => d.size(),
        }
    }

    fn refresh(&self) -> u32 {
        match self {
            Self::Cpu(d) => d.refresh(),
            Self::Gles(d) => d.refresh(),
        }
    }

    /// GLES なら、GPU で扱える写真の最大の幅と高さ(写真の合成を GPU に任せる)。
    fn gpu_max_texture(&self) -> Option<u32> {
        match self {
            Self::Cpu(_) => None,
            Self::Gles(d) => Some(d.max_texture()),
        }
    }

    /// 前回から今までに写真を GPU で作った時間 [ms]。CPU 経路は空。
    fn take_photo_build_ms(&mut self) -> Vec<f32> {
        match self {
            Self::Cpu(_) => Vec::new(),
            Self::Gles(d) => d.take_build_ms(),
        }
    }

    fn suspend(&mut self) {
        match self {
            Self::Cpu(d) => d.suspend(),
            Self::Gles(d) => d.suspend(),
        }
    }

    fn resume(&mut self) -> Result<(), String> {
        match self {
            Self::Cpu(d) => d.resume(),
            Self::Gles(d) => d.resume(),
        }
    }

    fn set_power(&mut self, on: bool) -> Result<(), String> {
        match self {
            Self::Cpu(d) => d.set_power(on),
            Self::Gles(d) => d.set_power(on),
        }
    }

    /// 1 枚描いて表示する。戻り値は (描画, 転送) の時間 [ms]。
    /// CPU は Canvas に完成画面を描いて変換コピーする。GLES は写真の合成を GPU に任せ(PoC-005 PR3)、
    /// Canvas には重ね絵だけを、変わったときだけ描く。
    fn draw(&mut self, app: &mut dyn App, canvas: &mut Canvas) -> Result<(f32, f32), String> {
        let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f32() * 1000.0;
        let t0 = Instant::now();
        match self {
            Self::Cpu(d) => {
                app.render(canvas);
                let t1 = Instant::now();
                d.present(|buf, pitch| canvas.write_xrgb8888(buf, pitch))?;
                Ok((ms(t0, t1), ms(t1, Instant::now())))
            }
            Self::Gles(d) => {
                let h = canvas.height();
                let comp = app.render_composite(canvas);
                let t1 = Instant::now();
                let blips = match &comp {
                    Composite::Radar { geom, .. } => radar_blips(geom),
                    _ => Vec::new(),
                };
                let frame = match comp {
                    Composite::Canvas => Frame::Image(canvas.pixmap.data()),
                    Composite::Photo {
                        photo,
                        old,
                        overlay_changed,
                    } => Frame::Photo {
                        photo: photo_layer(photo, h),
                        old: old.map(|(p, t)| (photo_layer(p, h), t)),
                        overlay: overlay_changed.then(|| canvas.pixmap.data()),
                    },
                    Composite::Radar {
                        background_changed,
                        geom,
                        sweep,
                    } => Frame::Radar {
                        background: background_changed.then(|| canvas.pixmap.data()),
                        radar: radar_layer(&geom, sweep, aircraft::TRAIL, h, &blips),
                    },
                };
                d.present_frame(&frame)?;
                Ok((ms(t0, t1), ms(t1, Instant::now())))
            }
        }
    }
}

/// 画面ロジックを作るために必要な、表示先が決まったあとの情報。
/// 操作パネルの待ち受けに失敗したとき、試し直すまでの間隔。
const PANEL_RETRY: Duration = Duration::from_secs(30);
/// キーボードが 1 台も無いとき、挿されていないか見直す間隔。
const KEYS_RETRY: Duration = Duration::from_secs(10);

/// 操作パネルに見せるモニターの状態。
fn display_status(screen_on: bool, power: Option<&ScreenPower>) -> DisplayStatus {
    DisplayStatus {
        on: screen_on,
        mode: power.and_then(ScreenPower::mode).map(PowerMode::as_str),
    }
}

pub struct Output {
    pub size: (u32, u32),
    /// GLES なら Some(GPU で扱える写真の最大の幅と高さ)。auto で CPU に戻ったときは None
    pub gpu_max_texture: Option<u32>,
}

/// 実行ループ。`make_app` には確定した表示サイズが渡される。
pub fn run(cfg: &Config, make_app: impl FnOnce(&Output) -> Box<dyn App>) -> Result<(), String> {
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }

    let seat_mode = SeatMode::parse(&cfg.runtime.seat)?;
    let mut session = Session::open(seat_mode)?;
    eprintln!(
        "seat: {} ({})",
        session.name(),
        if session.is_libseat() {
            "libseat"
        } else {
            "直接オープン"
        }
    );

    // 表示先のカードとコネクターを決める。/dev/dri/card0 には固定しない。
    let cards = drm_select::scan();
    let card = drm_select::choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let connector = drm_select::choose_connector(card, &cfg.display.connector)?;
    eprintln!(
        "display: {} driver={} pci={} connector={}",
        card.path.display(),
        card.driver,
        card.pci.as_deref().unwrap_or("-"),
        connector.name
    );
    let drm_dev = session.open_device(&card.path)?;
    let mode = ModeSel::parse(&cfg.display.mode)?;
    let mut display = Presenter::open(&drm_dev.fd, &cfg.display.renderer, &connector.name, mode)?;
    let (w, h) = display.size();
    eprintln!("mode: {w}x{h}@{}Hz", display.refresh());

    let vt = VtGuard::enter();
    eprintln!(
        "vt: {}",
        if vt.is_vt() {
            "KD_GRAPHICS"
        } else {
            "標準入力は VT ではない"
        }
    );
    // 操作パネルがあればキーボードは無くてもよい(後から挿されたら KEYS_RETRY ごとに拾う)
    let mut keys = if cfg.panel.enabled {
        Keys::open_any(&mut session, &cfg.input.devices)
    } else {
        Keys::open(&mut session, &cfg.input.devices)?
    };
    if keys.is_empty() {
        eprintln!("input: キーボードが無い(操作パネルで操作する)");
    } else {
        eprintln!("input: {:?}", keys.device_names());
    }
    let mut keys_retry = Instant::now() + KEYS_RETRY;

    let mut app = make_app(&Output {
        size: (w, h),
        gpu_max_texture: display.gpu_max_texture(),
    });
    let mut canvas = Canvas::new(w, h);

    let start = Instant::now();
    let max = (cfg.runtime.max_seconds > 0).then(|| Duration::from_secs(cfg.runtime.max_seconds));
    let stats_every = Duration::from_secs(cfg.runtime.stats_interval_secs.max(1));
    let mut next_stats = Instant::now() + stats_every;
    let (mut worst_ms, mut frames) = (0u128, 0u64);
    // 区間ごとの CPU 使用率と、描画(app.render)・転送(CPU: dumb buffer への変換コピー、GLES: テクスチャ転送と page flip の完了待ち)の時間
    let mut self_cpu = miharashi_linux::sysinfo::SelfCpu::default();
    self_cpu.poll(Instant::now());
    let (mut render_ms, mut present_ms) = (Vec::new(), Vec::new());
    // 写真 1 枚ごとの、GPU で完成フレームを作った CPU 時間(PoC-005 PR3b)
    let mut photo_ms = Vec::new();
    let mut redraw = true;
    let mut resume_pending = false;
    // 画面の電源。消している間は描かない(GLES は page flip が終わらないことがある)。
    // `power_tried` は最後に切り替えを試みた値。失敗しても同じ値では 20ms ごとに再試行しない
    // 操作パネルのスレッドからも手動オーバーライドを切り替えるので共有する
    let power = ScreenPower::start(&cfg.home_assistant, &cfg.screen_power).map(Arc::new);
    let mut screen_on = true;
    let mut power_tried = true;
    let mut published_display = DisplayStatus::default();
    // 手元の操作パネルの API。待ち受けられなくてもキオスクは動かし続け(状態ログは panel=error)、
    // PANEL_RETRY ごとに試し直す(起動直後で LAN の IP がまだ無いときなど)。同じ失敗は 1 回だけログに出す
    let mut panel: Option<Panel> = None;
    let mut panel_err: Option<String> = None;
    let mut panel_retry = Instant::now();

    let reason = loop {
        if term.load(Ordering::Relaxed) {
            break "シグナルを受信";
        }
        if max.is_some_and(|m| start.elapsed() >= m) {
            break "max_seconds に到達";
        }

        for ev in session.dispatch() {
            match ev {
                SessionEvent::Disabled => {
                    // VT が非アクティブ: 描画・入力・DRM master を手放してから応答する。
                    keys.close(&mut session);
                    display.suspend();
                    session.ack_disable();
                    eprintln!("seat: 無効化(描画を停止)");
                }
                SessionEvent::Enabled => {
                    let n = keys.reopen(&mut session);
                    eprintln!("input: {n} デバイスを開き直した");
                    resume_pending = true;
                    eprintln!("seat: 有効化(復帰処理)");
                }
            }
        }
        if !session.is_enabled() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if resume_pending {
            match display.resume() {
                Ok(()) => {
                    resume_pending = false;
                    redraw = true;
                    eprintln!("seat: 表示を復元");
                    // CRTC を設定し直すと画面が点くので、消しておくべきなら消し直す
                    if !screen_on {
                        screen_on = true;
                        power_tried = true;
                    }
                }
                Err(e) => {
                    eprintln!("{e}(再試行)");
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }

        if keys.is_empty() && Instant::now() >= keys_retry {
            keys_retry = Instant::now() + KEYS_RETRY;
            if keys.reopen(&mut session) > 0 {
                eprintln!("input: {:?}", keys.device_names());
            }
        }
        let mut quit = false;
        for a in keys.poll() {
            let key = match a {
                Action::Quit => {
                    quit = true;
                    break;
                }
                Action::Prev => KeyPress::Prev,
                Action::Switch => KeyPress::Switch,
                Action::Next => KeyPress::Next,
            };
            eprintln!("key: {a:?}");
            if let Some(p) = &power {
                p.on_key();
            }
            // 消えている画面へのキーは点けるだけにする(見えないまま画面を切り替えない)
            if screen_on {
                redraw |= app.on_key(key);
            }
        }
        if quit {
            break "キー操作(Esc/q)";
        }
        if cfg.panel.enabled && panel.is_none() && Instant::now() >= panel_retry {
            panel_retry = Instant::now() + PANEL_RETRY;
            // モニターの切り替えは、HA の手動オーバーライド(mode エンティティ)があるときだけ
            let control = power
                .clone()
                .filter(|p| p.mode().is_some())
                .map(|p| p as Arc<dyn DisplayControl>);
            match Panel::start(&cfg.panel, control) {
                Ok(p) => {
                    eprintln!("panel: {} で待ち受け", p.addr());
                    published_display = display_status(screen_on, power.as_deref());
                    p.publish(app.panel_view(), published_display);
                    panel = Some(p);
                    panel_err = None;
                }
                Err(e) => {
                    if panel_err.as_ref() != Some(&e) {
                        eprintln!("panel: 起動できない(パネルなしで続け、再試行する): {e}");
                    }
                    panel_err = Some(e);
                }
            }
        }
        // パネルのキーもキーボードと同じに扱う(消えている画面では点けるだけ)
        for key in panel.iter().flat_map(Panel::keys) {
            eprintln!("key: panel {key:?}");
            if let Some(p) = &power {
                p.on_key();
            }
            if screen_on {
                redraw |= app.on_key(key);
            }
        }
        redraw |= app.tick(Instant::now());

        // アラートは画面を消していても点けて見せる(tick で届いたものを同じ周期で反映する)
        let want_on = app.wants_screen() || power.as_ref().is_none_or(|p| p.want_on());
        if want_on == screen_on {
            power_tried = screen_on;
        } else if want_on != power_tried {
            power_tried = want_on;
            match display.set_power(want_on) {
                Ok(()) => {
                    screen_on = want_on;
                    redraw |= want_on;
                    eprintln!("display: {}", if want_on { "点灯" } else { "消灯" });
                }
                Err(e) => eprintln!("display: 電源を切り替えられない: {e}"),
            }
        }

        // 画面を消していてもパネルの表示は追従させる(モニターの点灯・消灯も知らせる)
        if let Some(p) = &panel {
            let ds = display_status(screen_on, power.as_deref());
            if redraw || ds != published_display {
                p.publish(app.panel_view(), ds);
                published_display = ds;
            }
        }

        if redraw && screen_on {
            let t0 = Instant::now();
            let (r, p) = display.draw(app.as_mut(), &mut canvas)?;
            render_ms.push(r);
            present_ms.push(p);
            photo_ms.extend(display.take_photo_build_ms());
            redraw = false;
            frames += 1;
            worst_ms = worst_ms.max(t0.elapsed().as_millis());
        }

        if Instant::now() >= next_stats {
            // 画面ロジックの値のあとに、PoC-005 の計測値を足す(既存の並びは変えない)
            let mut extra = app.stats();
            extra.push((
                "cpu_self",
                self_cpu
                    .poll(Instant::now())
                    .map_or("-".into(), |v| format!("{v:.1}")),
            ));
            extra.push(("render_ms", summarize_ms(&mut render_ms)));
            extra.push(("present_ms", summarize_ms(&mut present_ms)));
            extra.push(("photo_gpu_ms", summarize_ms(&mut photo_ms)));
            if cfg.panel.enabled {
                extra.push(("panel", panel.as_ref().map_or("error", Panel::stat).into()));
            }
            eprintln!(
                "{}",
                format_stats(
                    start.elapsed().as_secs(),
                    rss_mb(),
                    fd_count(),
                    frames,
                    worst_ms,
                    &extra
                )
            );
            worst_ms = 0;
            next_stats += stats_every;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    eprintln!("終了: {reason}");
    // 消したまま終わると、元のコンソールも見えなくなる
    if !screen_on && session.is_enabled() {
        let _ = display.set_power(true);
    }
    // 表示(元の CRTC を復元)→ 入力 → デバイスの順に解放する。VT は VtGuard が TEXT に戻す。
    drop(display);
    keys.close(&mut session);
    session.close_device(drm_dev);
    drop(vt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn extra(ticks: u64, cpu: &str) -> Vec<(&'static str, String)> {
        vec![
            ("ticks", ticks.to_string()),
            ("keys", "0".into()),
            ("screen", "Status(Overview)".into()),
            ("photo", "1".into()),
            ("feed", "online".into()),
            ("cpu", cpu.into()),
            ("gpu", "Some(45.0)".into()),
        ]
    }

    #[test]
    fn stats_line_matches_legacy_format() {
        let line = format_stats(60, 81, 26, 62, 19, &extra(61, "Some(13.0)"));
        assert_eq!(
            line,
            "stats: t=60s rss=81MB fds=26 frames=62 worst_frame=19ms ticks=61 keys=0 screen=Status(Overview) photo=1 feed=online cpu=Some(13.0) gpu=Some(45.0)"
        );
        eprintln!("{line}");
    }

    #[test]
    fn summarize_ms_gives_avg_and_p95() {
        let mut v: Vec<f32> = (1..=20).map(|x| x as f32).collect();
        assert_eq!(summarize_ms(&mut v), "10.5/19.0");
        assert!(v.is_empty(), "区間ごとに空にする");
        assert_eq!(summarize_ms(&mut v), "-");
        assert_eq!(summarize_ms(&mut vec![4.0]), "4.0/4.0");
    }

    #[test]
    fn stats_lines_are_readable_by_soak_summary() {
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/soak_summary.py");
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("python3 がないので skip");
            return;
        }
        let prefix = "2026-10-13T08:00:00+09:00 host miharashi[1]: ";
        let mut text = String::new();
        for i in 0..3u64 {
            // 今の並び: 画面ロジックの値(aircraft を含む)+ gpus + PoC-005 の計測値 + パネル(有効なときだけ)
            let mut e = extra(61 + 60 * i, "Some(13.0)");
            e.insert(4, ("aircraft", "false".to_string()));
            e.extend([
                ("gpus", "GPU0:0,GPU1:0".to_string()),
                ("cpu_self", format!("{}.5", i + 1)),
                ("render_ms", "12.0/20.0".to_string()),
                ("present_ms", "3.0/4.0".to_string()),
                ("panel", "idle".to_string()),
            ]);
            let line = format_stats(60 * (i + 1), 81, 26, 62 + i, 19, &e);
            text.push_str(&format!("{prefix}{line}\n"));
        }
        let path = std::env::temp_dir().join(format!("miharashi-soak-{}.log", std::process::id()));
        std::fs::write(&path, text).unwrap();
        let out = Command::new("python3")
            .arg(&script)
            .stdin(Stdio::from(std::fs::File::open(&path).unwrap()))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("stats 3 行"), "{stdout}");
        assert!(stdout.contains("CPU(自プロセス, %)"), "{stdout}");
    }
}
