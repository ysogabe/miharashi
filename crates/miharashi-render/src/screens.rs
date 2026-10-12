use crate::canvas::rgba;
use crate::{Canvas, TextEngine};
use tiny_skia::Pixmap;

/// 画面に渡すデータ。取得できない項目は None(画面には "--" と表示する)。
#[derive(Default)]
pub struct Snapshot {
    pub date: String,
    pub time: String,
    pub cpu: Option<f32>,
    pub cpu_temp: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub mem_used: Option<f32>,
    pub mem_total: Option<f32>,
    pub ssd_temps: Vec<(String, f32)>,
    pub fans: Vec<(String, String)>,
    pub cpu_history: Vec<f32>,
    pub gpu_history: Vec<f32>,

    // --- 電力(OVERVIEW)。docs/poc/DESIGN-next-screens.md 3.1 ---
    /// CPU パッケージの消費電力 [W](RAPL による推定)
    pub cpu_power_w: Option<f32>,
    /// GPU の消費電力 [W](NVML)
    pub gpu_power_w: Option<f32>,
    /// 今日(ローカル時刻 0 時から)の電力量 [Wh]
    pub cpu_energy_today_wh: Option<f64>,
    pub gpu_energy_today_wh: Option<f64>,
    /// 累計の電力量 [Wh](CPU+GPU、再起動をまたいで保存)
    pub total_energy_wh: Option<f64>,
    /// 電気代の単価 [円/kWh]。None なら電気代を表示しない
    pub price_per_kwh: Option<f32>,
    /// 直近の電力の時系列 [W](cpu_history と同じ長さ・間隔)
    pub cpu_power_history: Vec<f32>,
    pub gpu_power_history: Vec<f32>,

    // --- 詳細(DETAILS)。docs/poc/DESIGN-next-screens.md 3.2 ---
    /// PSI の some avg10 [%]
    pub psi_cpu: Option<f32>,
    pub psi_mem: Option<f32>,
    pub psi_io: Option<f32>,
    pub swap_used_gb: Option<f32>,
    pub swap_total_gb: Option<f32>,
    /// 1/5/15 分のロードアベレージ
    pub load_avg: Option<[f32; 3]>,
    pub vram_used_mib: Option<u32>,
    pub vram_total_mib: Option<u32>,
    pub gpu_clock_mhz: Option<u32>,
    pub gpu_clock_max_mhz: Option<u32>,
    /// "P0".."P12"
    pub gpu_pstate: Option<String>,
    /// (プロセス名, PID, 使用 VRAM [MiB])
    pub gpu_processes: Vec<(String, u32, u32)>,
    /// 既定経路のインターフェース名
    pub net_iface: Option<String>,
    pub net_rx_mbps: Option<f32>,
    pub net_tx_mbps: Option<f32>,
    pub net_rx_history: Vec<f32>,
    pub net_tx_history: Vec<f32>,
    pub psi_cpu_history: Vec<f32>,
    pub psi_mem_history: Vec<f32>,
    /// ルートファイルシステムの使用率 [%] と (使用, 全体) [GB]
    pub disk_root_used_pct: Option<f32>,
    pub disk_root_used_total_gb: Option<(f32, f32)>,
    /// (デバイス名, 読み込み MB/s, 書き込み MB/s)
    pub disk_io: Vec<(String, f32, f32)>,
    /// systemd の failed ユニット数
    pub failed_units: Option<u32>,
    /// 写真取得の状態(画面ロジックが設定する。"online" や再接続中の文言)
    pub feed_status: Option<String>,
    /// 今日のピーク: (項目名, 値の表示文字列, 時刻 "HH:MM")
    pub peaks_today: Vec<(String, String, String)>,
}

/// フォトフレームに表示する写真の情報。
pub struct PhotoInfo {
    pub place: String,
    pub taken: String,
    /// (現在位置 1 始まり, 総数)
    pub position: (usize, usize),
}

pub use crate::status::{CPU_COLOR, GPU_COLOR, StatusPage, status};

const DIM: [u8; 4] = [140, 160, 180, 255];

/// 写真の下に敷く半透明の情報帯(日時・場所・撮影日時・位置)。
fn photo_band(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, info: Option<&PhotoInfo>) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let band = 130.0;
    c.fill_rect(0.0, h - band, w, band, rgba(0, 0, 0, 150));
    t.draw(c, &s.time, 40.0, h - band + 8.0, 60.0, [255, 255, 255, 255]);
    t.draw(
        c,
        &s.date,
        40.0,
        h - band + 80.0,
        30.0,
        [215, 222, 230, 255],
    );
    if let Some(i) = info {
        t.draw_right(
            c,
            &i.place,
            w - 40.0,
            h - band + 14.0,
            38.0,
            [255, 255, 255, 255],
        );
        t.draw_right(
            c,
            &i.taken,
            w - 40.0,
            h - band + 66.0,
            28.0,
            [215, 222, 230, 255],
        );
        let pos = format!("{} / {}", i.position.0, i.position.1);
        t.draw_right(
            c,
            &pos,
            w - 40.0,
            h - band + 98.0,
            22.0,
            [160, 170, 182, 255],
        );
    }
}

/// 画面2: 全画面写真 + 半透明の情報帯。`fade` は切替中の (旧写真, 新写真の不透明度)。
/// `notice` は写真の取得状態の案内(例: 再接続中)。写真が 1 枚もないときは中央に大きく出す。
pub fn photo_frame(
    c: &mut Canvas,
    t: &mut TextEngine,
    s: &Snapshot,
    photo: Option<&Pixmap>,
    info: Option<&PhotoInfo>,
    fade: Option<(&Pixmap, f32)>,
    notice: Option<&str>,
) {
    c.clear(rgba(0, 0, 0, 255));
    match (photo, fade) {
        (Some(new), Some((old, alpha))) => {
            c.draw_pixmap(old, 0, 0);
            c.draw_pixmap_opacity(new, 0, 0, alpha);
        }
        (Some(new), None) => c.draw_pixmap(new, 0, 0),
        _ => {
            let msg = notice.unwrap_or("写真を読み込み中…");
            let (w, h) = (c.width() as f32, c.height() as f32);
            let mw = t.measure(msg, 36.0);
            t.draw(c, msg, (w - mw) / 2.0, h / 2.0 - 20.0, 36.0, DIM);
        }
    }
    photo_band(c, t, s, info);
    if let (Some(n), Some(_)) = (notice, photo) {
        // 最後の写真を表示したまま、右上に小さく状態を出す。
        let w = c.width() as f32;
        let tw = t.measure(n, 24.0);
        c.fill_rect(w - tw - 56.0, 24.0, tw + 32.0, 44.0, rgba(0, 0, 0, 150));
        t.draw(c, n, w - tw - 40.0, 32.0, 24.0, [255, 200, 120, 255]);
    }
}
