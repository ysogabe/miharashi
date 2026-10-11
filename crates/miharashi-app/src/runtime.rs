//! キオスクの実行ループ。画面ロジック(`KioskApp`)とは `Loop` の関数だけでつながる。
//! seat/VT の有効・無効、SIGINT/SIGTERM、入力、描画、状態ログを扱う。
use crate::app::{Key, KioskApp};
use crate::config::Config;
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::input::{Action, Keys};
use miharashi_linux::kms::Display;
use miharashi_linux::seat::{SeatMode, Session, SessionEvent};
use miharashi_linux::vt::VtGuard;
use miharashi_render::Canvas;
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

/// 画面ロジックを作るために必要な、表示先が決まったあとの情報。
pub struct Output {
    pub size: (u32, u32),
}

/// 実行ループ。`make_app` には確定した表示サイズが渡される。
pub fn run(cfg: &Config, make_app: impl FnOnce(&Output) -> KioskApp) -> Result<(), String> {
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
    let drm_fd = drm_dev.fd.try_clone().map_err(|e| e.to_string())?;
    let mode = ModeSel::parse(&cfg.display.mode)?;
    let mut display = Display::open(drm_fd, &connector.name, mode)?;
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
    let mut keys = Keys::open(&mut session, &cfg.input.devices)?;
    eprintln!("input: {:?}", keys.device_names());

    let mut app = make_app(&Output { size: (w, h) });
    let mut canvas = Canvas::new(w, h);

    let start = Instant::now();
    let max = (cfg.runtime.max_seconds > 0).then(|| Duration::from_secs(cfg.runtime.max_seconds));
    let stats_every = Duration::from_secs(cfg.runtime.stats_interval_secs.max(1));
    let mut next_stats = Instant::now() + stats_every;
    let (mut worst_ms, mut frames) = (0u128, 0u64);
    let mut redraw = true;
    let mut resume_pending = false;

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
                }
                Err(e) => {
                    eprintln!("{e}(再試行)");
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }

        let mut quit = false;
        for a in keys.poll() {
            let key = match a {
                Action::Quit => {
                    quit = true;
                    break;
                }
                Action::Prev => Key::Prev,
                Action::Switch => Key::Switch,
                Action::Next => Key::Next,
            };
            eprintln!("key: {a:?}");
            redraw |= app.on_key(key);
        }
        if quit {
            break "キー操作(Esc/q)";
        }
        redraw |= app.tick(Instant::now());

        if redraw {
            let t0 = Instant::now();
            app.render(&mut canvas);
            display.present(|buf, pitch| canvas.write_xrgb8888(buf, pitch))?;
            redraw = false;
            frames += 1;
            worst_ms = worst_ms.max(t0.elapsed().as_millis());
        }

        if Instant::now() >= next_stats {
            let s = app.snapshot();
            eprintln!(
                "stats: t={}s rss={}MB fds={} frames={} worst_frame={}ms ticks={} keys={} screen={:?} photo={} feed={} cpu={:?} gpu={:?}",
                start.elapsed().as_secs(),
                rss_mb(),
                fd_count(),
                frames,
                worst_ms,
                app.ticks,
                app.keys,
                app.screen(),
                app.photo_position(),
                app.feed_notice().unwrap_or("online"),
                s.cpu.map(|v| v.round()),
                s.gpu.map(|v| v.round()),
            );
            worst_ms = 0;
            next_stats += stats_every;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    eprintln!("終了: {reason}");
    // 表示(元の CRTC を復元)→ 入力 → デバイスの順に解放する。VT は VtGuard が TEXT に戻す。
    drop(display);
    keys.close(&mut session);
    session.close_device(drm_dev);
    drop(vt);
    Ok(())
}
