pub use crate::photo_screen::{PhotoInfo, photo_frame, photo_overlay};
pub use crate::status::{CPU_COLOR, GPU_COLOR, StatusPage, gpu_color, status};

/// GPU 1 台分の負荷。ラベルは "GPU0" のように番号付き(NVML の番号順、その後ろに内蔵 GPU)。
#[derive(Default, Clone, Debug)]
pub struct GpuLoad {
    pub label: String,
    /// 画面に出す名前。空なら `label` を出す
    pub name: String,
    pub usage: Option<f32>,
    pub temp: Option<f32>,
    /// 使用率の時系列 [%](cpu_history と同じ長さ・間隔)
    pub history: Vec<f32>,
    /// 使用中のメモリ [MiB]
    pub mem_used_mib: Option<u32>,
    /// 使えるメモリの全体 [MiB]
    pub mem_total_mib: Option<u32>,
    /// メモリ使用率の時系列 [%](cpu_history と同じ長さ・間隔)
    pub mem_history: Vec<f32>,
}

impl GpuLoad {
    /// 画面に出す名前(なければ "GPU0" のような番号)。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.label
        } else {
            &self.name
        }
    }

    /// メモリの使用率 [%]。GPU ごとに全体の大きさが違うので、比べるときは % で見る。
    pub fn mem_pct(&self) -> Option<f32> {
        match (self.mem_used_mib, self.mem_total_mib) {
            (Some(u), Some(t)) if t > 0 => Some(u as f32 / t as f32 * 100.0),
            _ => None,
        }
    }
}

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
    /// GPU ごとの負荷。2 台以上なら GPU タイルと USAGE % のグラフを GPU ごとに分ける。
    /// 空か 1 台なら gpu / gpu_temp / gpu_history を表示する。
    pub gpus: Vec<GpuLoad>,

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
