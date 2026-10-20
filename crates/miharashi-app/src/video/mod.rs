//! 各画面の見本(仮データ)を、固定刻みの仮想時刻で動画に書き出す(`demo-video`)と、
//! それをつなぐ紹介動画(`intro-video`)。本番のキオスクは使わない。
//! 時刻は `Instant`(単調時刻)も壁時計も、フレーム番号から決める仮想のもの。実時計は読まない。
pub mod agents_intro;
pub mod demo;
pub mod intro;
pub mod photos;
pub mod status_data;

use chrono::TimeZone;
use miharashi_vrm::record;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// 仮想の壁時計の開始(2026-10-16 14:35:00)。いつ書き出しても同じ日付・時刻の絵になる。
pub fn wall_start() -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .with_ymd_and_hms(2026, 10, 16, 14, 35, 0)
        .single()
        .expect("固定の壁時計")
}

/// SIGINT / SIGTERM で立つ終了フラグ。書き出しはフレームごとにこれを見て、通常のエラー経路で抜ける
/// (`Sink` の Drop が ffmpeg と一時ファイルを片付ける。完了前に中断した出力先は作らない)。
pub fn stop_flag() -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, flag.clone()).map_err(|e| format!("signal: {e}"))?;
    }
    Ok(flag)
}

/// 中断が要求されていたら、エラーにする。
pub fn check_stop(stop: &AtomicBool) -> Result<(), String> {
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        Err("中断した(書き出し途中の動画は残さない)".into())
    } else {
        Ok(())
    }
}

/// 出力先の親ディレクトリを(無ければ)作る。新しい checkout でも、手順のとおりに書き出せるように。
pub fn ensure_parent(out: &Path) -> Result<(), String> {
    match out.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => {
            std::fs::create_dir_all(p).map_err(|e| format!("{} を作れない: {e}", p.display()))
        }
        None => Ok(()),
    }
}

/// 動画の書き出しの設定(libx264、yuv420p、BT.709、音声なし、faststart。同じ入力から同じバイト列にする)。
pub fn record_settings(
    out: &Path,
    size: (u32, u32),
    fps: u32,
    overwrite: bool,
) -> record::Settings {
    let args = [
        "-fflags",
        "+bitexact",
        "-flags:v",
        "+bitexact",
        "-map_metadata",
        "-1",
        "-vf",
        "scale=out_color_matrix=bt709:out_range=tv",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-color_range",
        "tv",
    ];
    record::Settings {
        out: out.to_path_buf(),
        size,
        fps: f64::from(fps),
        codec: record::Codec::X264,
        overwrite,
        overwrite_flag: "--overwrite".into(),
        vaapi_device: "/dev/dri/renderD128".into(),
        extra_args: args.map(String::from).into(),
    }
}
