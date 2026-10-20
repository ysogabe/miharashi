//! 各画面の見本(仮データ)を、固定刻みの仮想時刻で動画に書き出す(`demo-video`)と、
//! それをつなぐ紹介動画(`intro-video`)。本番のキオスクは使わない。
//! 時刻は `Instant`(単調時刻)も壁時計も、フレーム番号から決める仮想のもの。実時計は読まない。
pub mod demo;
pub mod intro;
pub mod photos;

use chrono::TimeZone;
use miharashi_vrm::record;
use std::path::Path;

/// 仮想の壁時計の開始(2026-10-16 14:35:00)。いつ書き出しても同じ日付・時刻の絵になる。
pub fn wall_start() -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .with_ymd_and_hms(2026, 10, 16, 14, 35, 0)
        .single()
        .expect("固定の壁時計")
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
        vaapi_device: "/dev/dri/renderD128".into(),
        extra_args: args.map(String::from).into(),
    }
}
