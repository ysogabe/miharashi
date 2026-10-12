use crate::canvas::rgba;
use crate::chart::{self, Series};
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

const FG: [u8; 4] = [225, 235, 245, 255];
const DIM: [u8; 4] = [140, 160, 180, 255];
pub const CPU_COLOR: [u8; 4] = [64, 200, 255, 255];
pub const GPU_COLOR: [u8; 4] = [255, 170, 60, 255];
fn line() -> tiny_skia::Color {
    rgba(60, 80, 105, 255)
}

fn temp_color(v: f32) -> [u8; 4] {
    if v >= 90.0 {
        [255, 90, 80, 255]
    } else if v >= 80.0 {
        [255, 170, 60, 255]
    } else {
        FG
    }
}

fn pct(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.0}%"))
}

fn deg(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.0}°C"))
}

fn tile(c: &mut Canvas, t: &mut TextEngine, x: f32, y: f32, w: f32, h: f32, label: &str) {
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, 1.0, line());
    t.draw(c, label, x + 20.0, y + 14.0, 24.0, DIM);
}

/// 画面1: PC の状況(数値タイル + CPU/GPU 使用率グラフ)。
pub fn status(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    c.clear(rgba(8, 12, 20, 255));
    let (head, foot) = (72.0, 64.0);

    t.draw(c, "SERVER STATUS", 32.0, 16.0, 36.0, FG);
    t.draw_right(
        c,
        &format!("{}  {}", s.date, s.time),
        w - 32.0,
        22.0,
        30.0,
        DIM,
    );
    c.hline(0.0, head, w, line());

    let (gap, pad) = (20.0, 32.0);
    let tw = (w - pad * 2.0 - gap * 4.0) / 5.0;
    let (ty, th) = (head + 24.0, 220.0);
    let tx = |i: usize| pad + (tw + gap) * i as f32;

    // CPU / GPU: 使用率 + 温度
    for (i, (name, usage, temp, color)) in [
        ("CPU", s.cpu, s.cpu_temp, CPU_COLOR),
        ("GPU", s.gpu, s.gpu_temp, GPU_COLOR),
    ]
    .into_iter()
    .enumerate()
    {
        tile(c, t, tx(i), ty, tw, th, name);
        c.fill_rect(tx(i), ty, 6.0, th, rgba(color[0], color[1], color[2], 255));
        t.draw(c, &pct(usage), tx(i) + 20.0, ty + 56.0, 72.0, FG);
        t.draw(
            c,
            &deg(temp),
            tx(i) + 20.0,
            ty + 150.0,
            36.0,
            temp.map_or(DIM, temp_color),
        );
    }

    // MEMORY
    tile(c, t, tx(2), ty, tw, th, "MEMORY");
    let used = s.mem_used.map_or("--".to_string(), |v| format!("{v:.1}"));
    t.draw(c, &used, tx(2) + 20.0, ty + 56.0, 72.0, FG);
    let total = s
        .mem_total
        .map_or("--".to_string(), |v| format!("/ {v:.0} GB"));
    t.draw(c, &total, tx(2) + 20.0, ty + 150.0, 36.0, DIM);

    // SSD: ドライブごとの温度
    tile(c, t, tx(3), ty, tw, th, "SSD TEMP");
    if s.ssd_temps.is_empty() {
        t.draw(c, "--", tx(3) + 20.0, ty + 70.0, 48.0, DIM);
    }
    for (i, (name, temp)) in s.ssd_temps.iter().take(3).enumerate() {
        let ry = ty + 60.0 + i as f32 * 50.0;
        t.draw(c, name, tx(3) + 20.0, ry + 6.0, 26.0, DIM);
        t.draw_right(
            c,
            &format!("{temp:.0}°C"),
            tx(3) + tw - 20.0,
            ry,
            38.0,
            temp_color(*temp),
        );
    }

    // FAN
    tile(c, t, tx(4), ty, tw, th, "FAN");
    if s.fans.is_empty() {
        t.draw(c, "--", tx(4) + 20.0, ty + 70.0, 48.0, DIM);
    }
    for (i, (name, val)) in s.fans.iter().take(3).enumerate() {
        let ry = ty + 60.0 + i as f32 * 50.0;
        t.draw(c, name, tx(4) + 20.0, ry + 6.0, 26.0, DIM);
        t.draw_right(c, val, tx(4) + tw - 20.0, ry, 34.0, FG);
    }

    let cy = ty + th + 24.0;
    chart::line_chart(
        c,
        t,
        pad,
        cy,
        w - pad * 2.0,
        h - foot - 24.0 - cy,
        "USAGE (直近2分)",
        &[
            Series {
                name: "CPU",
                data: &s.cpu_history,
                color: CPU_COLOR,
            },
            Series {
                name: "GPU",
                data: &s.gpu_history,
                color: GPU_COLOR,
            },
        ],
        100.0,
    );

    let fy = h - foot;
    c.hline(0.0, fy, w, line());
    let label = "5 / 6: PHOTO FRAME";
    let lw = t.measure(label, 28.0);
    t.draw(c, label, (w - lw) / 2.0, fy + 16.0, 28.0, DIM);
}

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
