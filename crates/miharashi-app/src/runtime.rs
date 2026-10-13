//! キオスクの実行ループ。画面ロジックとは `screen::App` だけでつながり、具体的な型は知らない。
//! seat/VT の有効・無効、SIGINT/SIGTERM、入力、描画、状態ログを扱う。
//! 状態ログは共通部分(t / rss / fds / frames / worst_frame)に `App::stats()` の `k=v` を続けて出す。
use crate::config::Config;
use crate::screen::{App, KeyPress};
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

/// 画面ロジックを作るために必要な、表示先が決まったあとの情報。
pub struct Output {
    pub size: (u32, u32),
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
    // 区間ごとの CPU 使用率と、描画(app.render)・転送(dumb buffer への変換コピー)の時間
    let mut self_cpu = miharashi_linux::sysinfo::SelfCpu::default();
    self_cpu.poll(Instant::now());
    let (mut render_ms, mut present_ms) = (Vec::new(), Vec::new());
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
                Action::Prev => KeyPress::Prev,
                Action::Switch => KeyPress::Switch,
                Action::Next => KeyPress::Next,
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
            let t1 = Instant::now();
            display.present(|buf, pitch| canvas.write_xrgb8888(buf, pitch))?;
            render_ms.push(t1.duration_since(t0).as_secs_f32() * 1000.0);
            present_ms.push(t1.elapsed().as_secs_f32() * 1000.0);
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
            // 今の並び: 画面ロジックの値 + gpus + PoC-005 の計測値
            let mut e = extra(61 + 60 * i, "Some(13.0)");
            e.extend([
                ("gpus", "GPU0:0,GPU1:0".to_string()),
                ("cpu_self", format!("{}.5", i + 1)),
                ("render_ms", "12.0/20.0".to_string()),
                ("present_ms", "3.0/4.0".to_string()),
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
