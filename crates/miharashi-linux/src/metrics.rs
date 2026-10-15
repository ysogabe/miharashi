//! 実機のCPU・メモリ・温度・GPU(NVML)を1秒ごとに読む。取得できない項目は None。
use crate::energy::EnergyAccumulator;
use crate::sysinfo::{self, DiskIoTracker, FailedUnits, NetTracker, PeakInput, Peaks, RaplTracker};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default, Debug)]
pub struct Sample {
    pub cpu_percent: Option<f32>,
    pub cpu_temp: Option<f32>,
    pub gpu_percent: Option<f32>,
    pub gpu_temp: Option<f32>,
    /// GPU ごとの使用率・温度。NVML の番号順(nvidia-smi と同じ)、その後ろに amdgpu(PCI アドレス順)。
    pub gpus: Vec<GpuReading>,
    pub mem_used_gb: Option<f32>,
    pub mem_total_gb: Option<f32>,
    /// (ドライブ名, 温度℃)。NVMe の hwmon Composite。
    pub ssd_temps: Vec<(String, f32)>,
    /// (ファン名, 表示文字列)。hwmon の回転数と、NVML の GPU ファン速度(%)。
    pub fans: Vec<(String, String)>,
    pub cpu_power_w: Option<f32>,
    pub gpu_power_w: Option<f32>,
    pub cpu_energy_today_wh: Option<f64>,
    pub gpu_energy_today_wh: Option<f64>,
    pub total_energy_wh: Option<f64>,
    pub psi_cpu: Option<f32>,
    pub psi_mem: Option<f32>,
    pub psi_io: Option<f32>,
    pub swap_used_gb: Option<f32>,
    pub swap_total_gb: Option<f32>,
    pub load_avg: Option<[f32; 3]>,
    pub vram_used_mib: Option<u32>,
    pub vram_total_mib: Option<u32>,
    pub gpu_clock_mhz: Option<u32>,
    pub gpu_clock_max_mhz: Option<u32>,
    pub gpu_pstate: Option<String>,
    /// (プロセス名, PID, 使用 VRAM MiB)。VRAM 降順で最大 5 件。
    pub gpu_processes: Vec<(String, u32, u32)>,
    pub net_iface: Option<String>,
    pub net_rx_mbps: Option<f32>,
    pub net_tx_mbps: Option<f32>,
    pub disk_root_used_pct: Option<f32>,
    pub disk_root_used_total_gb: Option<(f32, f32)>,
    /// (デバイス名, 読み MB/s, 書き MB/s)
    pub disk_io: Vec<(String, f32, f32)>,
    pub failed_units: Option<u32>,
    /// (項目名, 値の表示文字列, "HH:MM")
    pub peaks_today: Vec<(String, String, String)>,
}

/// GPU 1 台分の使用率と温度。
#[derive(Clone, Default, Debug, PartialEq)]
pub struct GpuReading {
    /// "GPU0", "GPU1", ...(状態ログのキー。番号は変えない)
    pub label: String,
    /// 画面に出す名前("RTX 5060 Ti"、"Radeon(内蔵)" など)
    pub name: String,
    pub usage: Option<f32>,
    pub temp: Option<f32>,
}

/// 画面用に GPU の名前を短くする("NVIDIA GeForce RTX 5060 Ti" → "RTX 5060 Ti")。
pub fn short_gpu_name(name: &str) -> String {
    let n = name.trim();
    let n = n.strip_prefix("NVIDIA ").unwrap_or(n);
    let n = n.strip_prefix("GeForce ").unwrap_or(n);
    n.to_string()
}

/// amdgpu の GPU(内蔵 Radeon など)。sysfs から使用率と温度を読む。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SysfsGpu {
    pci: String,
    /// /sys/class/drm/cardN/device
    device: PathBuf,
}

impl SysfsGpu {
    fn usage(&self) -> Option<f32> {
        std::fs::read_to_string(self.device.join("gpu_busy_percent"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn temp(&self) -> Option<f32> {
        let rd = std::fs::read_dir(self.device.join("hwmon")).ok()?;
        rd.flatten()
            .find_map(|e| read_milli_c(&e.path().join("temp1_input")))
    }
}

/// `drm_dir`(通常 /sys/class/drm)の cardN のうち、`gpu_busy_percent` を持つもの(amdgpu)。
/// NVIDIA は NVML で読むので除く。cardN の番号は起動ごとに変わるので PCI アドレス順に並べる。
fn find_sysfs_gpus(drm_dir: &std::path::Path) -> Vec<SysfsGpu> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(drm_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_card = name
            .strip_prefix("card")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !is_card {
            continue;
        }
        let device = entry.path().join("device");
        if !device.join("gpu_busy_percent").exists() {
            continue;
        }
        let pci = std::fs::canonicalize(&device)
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or(name);
        out.push(SysfsGpu { pci, device });
    }
    out.sort_by(|a, b| a.pci.cmp(&b.pci));
    out.dedup_by(|a, b| a.pci == b.pci);
    out
}

/// サンプラーに渡す設定(電力まわりのパス)。
#[derive(Debug, Clone)]
pub struct SamplerOptions {
    /// 電力量の保存先 JSON。None なら保存しない。
    pub state_file: Option<PathBuf>,
    /// root ヘルパーが書き出す RAPL の値("<energy_uj> <max_energy_range_uj>")。
    pub rapl_helper_file: PathBuf,
}

impl Default for SamplerOptions {
    fn default() -> Self {
        Self {
            state_file: None,
            rapl_helper_file: "/run/miharashi-rapl/energy".into(),
        }
    }
}

/// NVML から間引いて読む GPU の指標(NVML の中では重いもの)。
#[derive(Default)]
struct GpuSlow {
    vram: Option<(u32, u32)>,
    /// (現在, 最大) [MHz]
    clock: Option<(u32, u32)>,
    pstate: Option<String>,
    processes: Vec<(String, u32, u32)>,
    /// ファンの回転 [%]
    fan: Option<u32>,
}

/// メモリ、クロック、P ステート、プロセス、ファンを読む。最大クロックは変わらないので呼び出し側で 1 回だけ読む。
fn read_gpu_slow(d: &nvml_wrapper::Device, max_clock: Option<u32>) -> GpuSlow {
    use nvml_wrapper::enum_wrappers::device::Clock;
    const MIB: u64 = 1024 * 1024;
    let mut procs: Vec<(String, u32, u32)> = Vec::new();
    let all = d
        .running_compute_processes()
        .unwrap_or_default()
        .into_iter()
        .chain(d.running_graphics_processes().unwrap_or_default());
    for p in all {
        let mib = match p.used_gpu_memory {
            nvml_wrapper::enums::device::UsedGpuMemory::Used(b) => (b / MIB) as u32,
            _ => 0,
        };
        // 同じ PID が compute と graphics の両方に出る場合は大きい方
        match procs.iter_mut().find(|x| x.1 == p.pid) {
            Some(x) => x.2 = x.2.max(mib),
            None => procs.push((proc_name(p.pid), p.pid, mib)),
        }
    }
    procs.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
    procs.truncate(5);
    GpuSlow {
        vram: d
            .memory_info()
            .ok()
            .map(|m| ((m.used / MIB) as u32, (m.total / MIB) as u32)),
        clock: d.clock_info(Clock::Graphics).ok().zip(max_clock),
        pstate: d.performance_state().ok().and_then(|p| {
            // c_variant は NVML_PSTATE_0..15 の値。Unknown は 32
            let n = p.as_c();
            (n <= 15).then(|| format!("P{n}"))
        }),
        processes: procs,
        fan: d.fan_speed(0).ok(),
    }
}

/// NVML のメモリ・クロック・P ステート・プロセス・ファン、amdgpu、ファンの回転を読む間隔
const SLOW_EVERY: Duration = Duration::from_secs(5);
/// SSD の温度を読む間隔
const SSD_EVERY: Duration = Duration::from_secs(30);
/// 監視自身の CPU 時間をログに出す間隔
const REPORT_EVERY: Duration = Duration::from_secs(600);

/// `round` 回目(0 から)のサンプリングで、`every` ごとの読み取りをするか。最初の回は必ず読む。
fn due(round: u64, interval: Duration, every: Duration) -> bool {
    let n = (every.as_millis() / interval.as_millis().max(1)).max(1) as u64;
    round.is_multiple_of(n)
}

/// 監視スレッドが読み取りの種類ごとに使った CPU 時間。REPORT_EVERY ごとにログへ出す。
/// 監視するものを増やしたときに、どれだけ CPU が増えたかをログで確かめられるようにする。
struct MonCost {
    parts: Vec<(&'static str, u64)>,
    total_ns: u64,
    rounds: u32,
    since: Instant,
}

impl MonCost {
    fn new(now: Instant) -> Self {
        Self {
            parts: Vec::new(),
            total_ns: 0,
            rounds: 0,
            since: now,
        }
    }

    fn add(&mut self, name: &'static str, ns: u64) {
        match self.parts.iter_mut().find(|p| p.0 == name) {
            Some(p) => p.1 += ns,
            None => self.parts.push((name, ns)),
        }
    }

    /// 1 回分のサンプリング全体(読み取り以外の集計も含む)の CPU 時間を足す。
    fn finish_round(&mut self, ns: u64) {
        self.total_ns += ns;
        self.rounds += 1;
    }

    /// REPORT_EVERY たったら 1 行にまとめて返し、数え直す。
    fn report_if_due(&mut self, now: Instant) -> Option<String> {
        let secs = now.duration_since(self.since).as_secs_f64();
        if secs < REPORT_EVERY.as_secs_f64() || self.rounds == 0 {
            return None;
        }
        let line = self.format(secs);
        *self = Self::new(now);
        Some(line)
    }

    fn format(&self, secs: f64) -> String {
        let ms = |ns: u64| ns as f64 / 1e6;
        let mut parts = self.parts.clone();
        parts.sort_by_key(|p| std::cmp::Reverse(p.1));
        let named: u64 = parts.iter().map(|p| p.1).sum();
        let mut detail: Vec<String> = parts
            .iter()
            .map(|(n, ns)| format!("{n} {:.0}", ms(*ns)))
            .collect();
        detail.push(format!(
            "その他 {:.0}",
            ms(self.total_ns.saturating_sub(named))
        ));
        format!(
            "metrics: 監視スレッドの CPU {:.0}s で {:.0}ms({:.2}%、1 回 {:.2}ms): {} [ms]",
            secs,
            ms(self.total_ns),
            ms(self.total_ns) / secs / 10.0,
            ms(self.total_ns) / self.rounds as f64,
            detail.join(" / ")
        )
    }
}

/// /proc/<pid>/comm のプロセス名。読めなければ "?"。
fn proc_name(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".into())
}

fn read_psi(name: &str) -> Option<f32> {
    sysinfo::parse_psi_some_avg10(&std::fs::read_to_string(format!("/proc/pressure/{name}")).ok()?)
}

/// 監視する GPU(表示先 GPU とは独立)。NVML のインデックスか PCI アドレス。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuSel {
    /// インデックス 0
    Auto,
    Index(u32),
    /// "0000:01:00.0"
    Pci(String),
}

impl GpuSel {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() || s == "auto" {
            return Ok(Self::Auto);
        }
        if let Some(p) = s.strip_prefix("pci:") {
            return Ok(Self::Pci(p.to_string()));
        }
        s.parse::<u32>().map(Self::Index).map_err(|_| {
            format!("gpu: auto / 数字(NVML インデックス)/ pci:0000:01:00.0 のいずれか(指定値: {s})")
        })
    }

    /// NVML の PCI バス ID は 8 桁ドメイン("00000000:01:00.0")。
    pub fn nvml_bus_id(addr: &str) -> String {
        match addr.split_once(':') {
            Some((dom, rest)) if dom.len() < 8 && addr.matches(':').count() == 2 => {
                format!("{dom:0>8}:{rest}")
            }
            _ => addr.to_string(),
        }
    }
}

pub struct Sampler {
    latest: Arc<Mutex<Sample>>,
}

impl Sampler {
    /// バックグラウンドスレッドで `interval` ごとにサンプリングする。
    pub fn spawn(interval: Duration, gpu_sel: GpuSel, opts: SamplerOptions) -> Self {
        let latest = Arc::new(Mutex::new(Sample::default()));
        let shared = latest.clone();
        std::thread::spawn(move || {
            let nvml = nvml_wrapper::Nvml::init().ok();
            let gpu = nvml.as_ref().and_then(|n| match &gpu_sel {
                GpuSel::Auto => n.device_by_index(0).ok(),
                GpuSel::Index(i) => n.device_by_index(*i).ok(),
                GpuSel::Pci(a) => n.device_by_pci_bus_id(GpuSel::nvml_bus_id(a)).ok(),
            });
            if gpu.is_none() {
                eprintln!(
                    "metrics: 監視 GPU({gpu_sel:?})を NVML で開けない。GPU の値は -- と表示する"
                );
            }
            // GPU ごとの表示用: NVML の全 GPU(番号順)+ amdgpu
            let nvml_all: Vec<nvml_wrapper::Device> = nvml
                .as_ref()
                .map(|n| {
                    (0..n.device_count().unwrap_or(0))
                        .filter_map(|i| n.device_by_index(i).ok())
                        .collect()
                })
                .unwrap_or_default();
            let sysfs_gpus = find_sysfs_gpus(std::path::Path::new("/sys/class/drm"));
            let display_names: Vec<String> = nvml_all
                .iter()
                .map(|d| short_gpu_name(&d.name().unwrap_or_else(|_| "NVIDIA".into())))
                .chain(sysfs_gpus.iter().map(|_| "Radeon(内蔵)".to_string()))
                .collect();
            let mut names: Vec<String> = nvml_all
                .iter()
                .map(|d| d.name().unwrap_or_else(|_| "NVIDIA".into()))
                .collect();
            names.extend(sysfs_gpus.iter().map(|g| format!("amdgpu {}", g.pci)));
            eprintln!(
                "metrics: GPU 一覧: {}",
                names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("GPU{i}={n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let temp_path = find_cpu_temp_input();
            let ssds = find_ssd_inputs();
            let fan_inputs = find_fan_inputs();
            let mut prev = read_cpu_times();
            let mut rapl = RaplTracker::new(&opts.rapl_helper_file);
            let mut net = NetTracker::default();
            let mut disk_io = DiskIoTracker::new();
            let mut failed = FailedUnits::default();
            let mut peaks = Peaks::default();
            let mut energy =
                EnergyAccumulator::load(opts.state_file.clone(), chrono::Local::now().date_naive());
            let mut last_tick = Instant::now();
            // 変わらない値は最初に 1 回だけ読む
            let gpu_index = gpu.as_ref().and_then(|d| d.index().ok());
            let max_clock = gpu.as_ref().and_then(|d| {
                d.max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics)
                    .ok()
            });
            // 間引いて読む値は、読まない回は前回の値をそのまま使う
            let mut slow = GpuSlow::default();
            let mut amd: Vec<(Option<f32>, Option<f32>)> = vec![(None, None); sysfs_gpus.len()];
            let mut ssd_temps: Vec<(String, f32)> = Vec::new();
            let mut hw_fans: Vec<(String, String)> = Vec::new();
            let mut cost = MonCost::new(Instant::now());
            let mut round: u64 = 0;
            loop {
                std::thread::sleep(interval);
                let c_start = thread_cpu_ns();
                let tick = Instant::now();
                let dt = tick.duration_since(last_tick).as_secs_f64();
                last_tick = tick;
                let slow_due = due(round, interval, SLOW_EVERY);
                let ssd_due = due(round, interval, SSD_EVERY);
                round += 1;
                macro_rules! part {
                    ($name:expr, $e:expr) => {{
                        let c0 = thread_cpu_ns();
                        let r = $e;
                        cost.add($name, thread_cpu_ns().saturating_sub(c0));
                        r
                    }};
                }

                let (cpu_percent, mem, psi, swap, load_avg, netr, disk, root, cpu_temp) =
                    part!("proc", {
                        let now = read_cpu_times();
                        let cpu_percent = match (prev, now) {
                            (Some(p), Some(n)) if n.1 > p.1 => {
                                Some(100.0 * (n.0 - p.0) as f32 / (n.1 - p.1) as f32)
                            }
                            _ => None,
                        };
                        prev = now;
                        (
                            cpu_percent,
                            read_mem(),
                            [read_psi("cpu"), read_psi("memory"), read_psi("io")],
                            std::fs::read_to_string("/proc/meminfo")
                                .ok()
                                .and_then(|t| sysinfo::parse_swap_gb(&t)),
                            std::fs::read_to_string("/proc/loadavg")
                                .ok()
                                .and_then(|t| sysinfo::parse_loadavg(&t)),
                            net.poll(tick),
                            disk_io.poll(tick),
                            sysinfo::read_root_disk(),
                            temp_path.as_ref().and_then(|p| read_milli_c(p)),
                        )
                    });
                let (mem_used_gb, mem_total_gb) = mem.unzip();
                let psi_mem = psi[1];

                // 監視 GPU の温度・使用率・電力は毎回。同じ GPU を GPU 一覧のために読み直さない
                let (gpu_temp, gpu_percent, gpu_power_w, nv) = part!("nvml", {
                    use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
                    let temp = gpu
                        .as_ref()
                        .and_then(|d| d.temperature(TemperatureSensor::Gpu).ok())
                        .map(|t| t as f32);
                    let util = gpu
                        .as_ref()
                        .and_then(|d| d.utilization_rates().ok())
                        .map(|u| u.gpu as f32);
                    let power = gpu
                        .as_ref()
                        .and_then(|d| d.power_usage().ok())
                        .map(|mw| mw as f32 / 1000.0);
                    let nv: Vec<(Option<f32>, Option<f32>)> = nvml_all
                        .iter()
                        .map(|d| {
                            if gpu_index.is_some() && d.index().ok() == gpu_index {
                                (util, temp)
                            } else {
                                (
                                    d.utilization_rates().ok().map(|u| u.gpu as f32),
                                    d.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32),
                                )
                            }
                        })
                        .collect();
                    (temp, util, power, nv)
                });
                // メモリ・クロック・P ステート・プロセス・ファンは SLOW_EVERY ごと(NVML の中で重い)
                if slow_due {
                    part!("nvml(間引き)", {
                        slow = gpu
                            .as_ref()
                            .map(|d| read_gpu_slow(d, max_clock))
                            .unwrap_or_default();
                    });
                    part!("amdgpu(間引き)", {
                        amd = sysfs_gpus.iter().map(|g| (g.usage(), g.temp())).collect();
                    });
                    part!("ファン(間引き)", {
                        hw_fans = fan_inputs
                            .iter()
                            .filter_map(|(n, p)| {
                                let rpm: u32 =
                                    std::fs::read_to_string(p).ok()?.trim().parse().ok()?;
                                Some((n.clone(), format!("{rpm} rpm")))
                            })
                            .collect();
                    });
                }
                // SSD の温度は NVMe に問い合わせるので遅く(1 回 約 15ms 待つ)、省電力状態も起こす
                if ssd_due {
                    part!("SSD 温度(間引き)", {
                        ssd_temps = ssds
                            .iter()
                            .filter_map(|(n, p)| Some((n.clone(), read_milli_c(p)?)))
                            .collect();
                    });
                }
                let cpu_power_w = part!("RAPL", rapl.poll(tick));
                let failed_units = part!("systemctl", failed.poll(tick));

                let now_local = chrono::Local::now().naive_local();
                energy.add(now_local.date(), cpu_power_w, gpu_power_w, dt);
                energy.save_if_due(tick);
                let (cpu_energy_today_wh, gpu_energy_today_wh, total_energy_wh) = energy.values();
                let net_rx_mbps = netr.as_ref().and_then(|n| n.1);
                let peaks_today = peaks.update(
                    now_local,
                    PeakInput {
                        cpu_temp,
                        gpu_temp,
                        cpu_w: cpu_power_w,
                        gpu_w: gpu_power_w,
                        rx_mbps: net_rx_mbps,
                        psi_mem,
                    },
                );
                let s = Sample {
                    cpu_power_w,
                    gpu_power_w,
                    cpu_energy_today_wh,
                    gpu_energy_today_wh,
                    total_energy_wh,
                    psi_cpu: psi[0],
                    psi_mem,
                    psi_io: psi[2],
                    swap_used_gb: swap.map(|s| s.0),
                    swap_total_gb: swap.map(|s| s.1),
                    load_avg,
                    vram_used_mib: slow.vram.map(|v| v.0),
                    vram_total_mib: slow.vram.map(|v| v.1),
                    gpu_clock_mhz: slow.clock.map(|c| c.0),
                    gpu_clock_max_mhz: slow.clock.map(|c| c.1),
                    gpu_pstate: slow.pstate.clone(),
                    gpu_processes: slow.processes.clone(),
                    net_iface: netr.as_ref().map(|n| n.0.clone()),
                    net_rx_mbps,
                    net_tx_mbps: netr.as_ref().and_then(|n| n.2),
                    disk_root_used_pct: root.map(|r| r.0),
                    disk_root_used_total_gb: root.map(|r| r.1),
                    disk_io: disk,
                    failed_units,
                    peaks_today,
                    cpu_percent,
                    cpu_temp,
                    gpu_temp,
                    gpu_percent,
                    gpus: nv
                        .into_iter()
                        .chain(amd.iter().copied())
                        .enumerate()
                        .map(|(i, (usage, temp))| GpuReading {
                            label: format!("GPU{i}"),
                            name: display_names.get(i).cloned().unwrap_or_default(),
                            usage,
                            temp,
                        })
                        .collect(),
                    mem_used_gb,
                    mem_total_gb,
                    ssd_temps: ssd_temps.clone(),
                    fans: {
                        let mut f = hw_fans.clone();
                        if let Some(pct) = slow.fan {
                            f.push(("GPU".to_string(), format!("{pct}%")));
                        }
                        f
                    },
                };
                cost.finish_round(thread_cpu_ns().saturating_sub(c_start));
                if let Some(line) = cost.report_if_due(Instant::now()) {
                    eprintln!("{line}");
                }
                if let Ok(mut g) = shared.lock() {
                    *g = s;
                }
            }
        });
        Self { latest }
    }

    pub fn latest(&self) -> Sample {
        self.latest.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// (busy, total) のジフィー数。
fn read_cpu_times() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = s
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v[4]; // idle + iowait
    Some((total - idle, total))
}

fn read_mem() -> Option<(f32, f32)> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let get = |key: &str| -> Option<f32> {
        s.lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse::<f32>()
            .ok()
    };
    let (total, avail) = (get("MemTotal:")?, get("MemAvailable:")?);
    Some(((total - avail) / 1024.0 / 1024.0, total / 1024.0 / 1024.0))
}

fn read_milli_c(path: &std::path::Path) -> Option<f32> {
    Some(
        std::fs::read_to_string(path)
            .ok()?
            .trim()
            .parse::<f32>()
            .ok()?
            / 1000.0,
    )
}

/// k10temp(AMD)または coretemp(Intel)の CPU 温度入力ファイルを探す。
fn find_cpu_temp_input() -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        if !matches!(name.trim(), "k10temp" | "coretemp" | "zenpower") {
            continue;
        }
        let mut fallback = None;
        for n in 1..=16 {
            let input = dir.join(format!("temp{n}_input"));
            if !input.exists() {
                continue;
            }
            let label =
                std::fs::read_to_string(dir.join(format!("temp{n}_label"))).unwrap_or_default();
            if matches!(label.trim(), "Tctl" | "Tdie") || label.starts_with("Package") {
                return Some(input);
            }
            fallback.get_or_insert(input);
        }
        return fallback;
    }
    None
}

/// NVMe の Composite 温度入力。(短いドライブ名, パス)。
fn find_ssd_inputs() -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") else {
        return out;
    };
    for entry in rd.flatten() {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            != "nvme"
        {
            continue;
        }
        let model = std::fs::read_to_string(dir.join("device/model")).unwrap_or_default();
        let name = model
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("NVMe")
            .to_string();
        for n in 1..=4 {
            let label =
                std::fs::read_to_string(dir.join(format!("temp{n}_label"))).unwrap_or_default();
            if label.trim() == "Composite" {
                out.push((name.clone(), dir.join(format!("temp{n}_input"))));
                break;
            }
        }
    }
    out.sort();
    out
}

/// hwmon の fanN_input(マザーボードのセンサードライバーが読み込まれていれば見つかる)。
fn find_fan_inputs() -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") else {
        return out;
    };
    for entry in rd.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        for n in 1..=8 {
            let input = dir.join(format!("fan{n}_input"));
            if input.exists() {
                let label =
                    std::fs::read_to_string(dir.join(format!("fan{n}_label"))).unwrap_or_default();
                let label = if label.trim().is_empty() {
                    format!("{name} {n}")
                } else {
                    label.trim().to_string()
                };
                out.push((label, input));
            }
        }
    }
    out.sort();
    out
}

/// 監視の読み取り 1 種類のコスト(1 回あたりの平均)。
#[derive(Clone, Debug)]
pub struct ProbeCost {
    pub name: &'static str,
    pub wall_us: f64,
    /// このスレッドが使った CPU 時間(ユーザー + カーネル)
    pub cpu_us: f64,
}

/// このスレッドの CPU 時間 [ns](CLOCK_THREAD_CPUTIME_ID)。
fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts を書くだけ
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// `Sampler` が 1 回のサンプリングで行う読み取りを 1 種類ずつ測る(`miharashi bench-metrics`)。
/// `rounds` 回、`interval` ごとに測り、1 回あたりの平均を返す。最初の 1 回(初期化)は "init:" で返す。
pub fn profile(
    rounds: u32,
    interval: Duration,
    gpu_sel: &GpuSel,
    opts: &SamplerOptions,
) -> Vec<ProbeCost> {
    use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
    let mut costs: Vec<ProbeCost> = Vec::new();
    let mut add = |name: &'static str, wall: Duration, cpu_ns: u64, n: u32| match costs
        .iter_mut()
        .find(|c| c.name == name)
    {
        Some(c) => {
            c.wall_us += wall.as_secs_f64() * 1e6 / n as f64;
            c.cpu_us += cpu_ns as f64 / 1e3 / n as f64;
        }
        None => costs.push(ProbeCost {
            name,
            wall_us: wall.as_secs_f64() * 1e6 / n as f64,
            cpu_us: cpu_ns as f64 / 1e3 / n as f64,
        }),
    };
    macro_rules! time {
        ($name:expr, $n:expr, $e:expr) => {{
            let (w0, c0) = (Instant::now(), thread_cpu_ns());
            let r = $e;
            add($name, w0.elapsed(), thread_cpu_ns() - c0, $n);
            r
        }};
    }
    let nvml = time!("init: NVML", 1, nvml_wrapper::Nvml::init().ok());
    let gpu = nvml.as_ref().and_then(|n| match gpu_sel {
        GpuSel::Auto => n.device_by_index(0).ok(),
        GpuSel::Index(i) => n.device_by_index(*i).ok(),
        GpuSel::Pci(a) => n.device_by_pci_bus_id(GpuSel::nvml_bus_id(a)).ok(),
    });
    let sysfs_gpus = find_sysfs_gpus(std::path::Path::new("/sys/class/drm"));
    let temp_path = find_cpu_temp_input();
    let ssds = find_ssd_inputs();
    let fan_inputs = find_fan_inputs();
    let mut rapl = RaplTracker::new(&opts.rapl_helper_file);
    let mut net = NetTracker::default();
    let mut disk_io = DiskIoTracker::new();
    let mut failed = FailedUnits::default();
    let n = rounds;
    for _ in 0..rounds {
        std::thread::sleep(interval);
        let tick = Instant::now();
        time!("proc: /proc/stat", n, read_cpu_times());
        time!("proc: /proc/meminfo(メモリ)", n, read_mem());
        time!("proc: /proc/meminfo(スワップ)", n, {
            std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|t| sysinfo::parse_swap_gb(&t))
        });
        time!("proc: /proc/loadavg", n, {
            std::fs::read_to_string("/proc/loadavg")
                .ok()
                .and_then(|t| sysinfo::parse_loadavg(&t))
        });
        time!("proc: PSI ×3", n, {
            (read_psi("cpu"), read_psi("memory"), read_psi("io"))
        });
        time!("proc: ネットワーク", n, net.poll(tick));
        time!("proc: ディスク I/O", n, disk_io.poll(tick));
        time!("fs: statvfs(/)", n, sysinfo::read_root_disk());
        time!(
            "sysfs: CPU 温度",
            n,
            temp_path.as_ref().and_then(|p| read_milli_c(p))
        );
        time!("sysfs: SSD 温度", n, {
            ssds.iter().filter_map(|(_, p)| read_milli_c(p)).count()
        });
        time!("sysfs: ファン", n, {
            fan_inputs
                .iter()
                .filter_map(|(_, p)| std::fs::read_to_string(p).ok())
                .count()
        });
        time!("sysfs: amdgpu 使用率と温度", n, {
            sysfs_gpus
                .iter()
                .map(|g| (g.usage(), g.temp()))
                .collect::<Vec<_>>()
        });
        time!("rapl: ヘルパーのファイル", n, rapl.poll(tick));
        time!("exec: systemctl(30 秒に 1 回)", n, failed.poll(tick));
        if let Some(d) = gpu.as_ref() {
            time!("nvml: 温度", n, d.temperature(TemperatureSensor::Gpu).ok());
            time!("nvml: 使用率", n, d.utilization_rates().ok());
            time!("nvml: 電力", n, d.power_usage().ok());
            time!("nvml: メモリ", n, d.memory_info().ok());
            time!("nvml: クロック(現在と最大)", n, {
                (
                    d.clock_info(Clock::Graphics).ok(),
                    d.max_clock_info(Clock::Graphics).ok(),
                )
            });
            time!("nvml: P ステート", n, d.performance_state().ok());
            time!("nvml: プロセス(compute + graphics)", n, {
                (
                    d.running_compute_processes().ok(),
                    d.running_graphics_processes().ok(),
                )
            });
            time!("nvml: ファン", n, d.fan_speed(0).ok());
        }
    }
    costs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_reads_are_spaced_and_first_round_reads_everything() {
        let sec = Duration::from_secs(1);
        let due5: Vec<u64> = (0..12).filter(|&r| due(r, sec, SLOW_EVERY)).collect();
        assert_eq!(due5, vec![0, 5, 10]);
        assert!(due(0, sec, SSD_EVERY) && !due(1, sec, SSD_EVERY) && due(30, sec, SSD_EVERY));
        // 間隔が長い設定(10 秒ごと)なら毎回読む
        assert!((0..3).all(|r| due(r, Duration::from_secs(10), SLOW_EVERY)));
    }

    #[test]
    fn monitor_cost_reports_every_ten_minutes() {
        let t0 = Instant::now();
        let mut c = MonCost::new(t0);
        for _ in 0..600 {
            c.add("nvml", 400_000);
            c.add("proc", 100_000);
            c.finish_round(600_000);
        }
        assert_eq!(c.report_if_due(t0 + Duration::from_secs(599)), None);
        let line = c.report_if_due(t0 + Duration::from_secs(600)).unwrap();
        assert_eq!(
            line,
            "metrics: 監視スレッドの CPU 600s で 360ms(0.06%、1 回 0.60ms): nvml 240 / proc 60 / その他 60 [ms]"
        );
        assert_eq!(c.rounds, 0, "出したら数え直す");
    }

    #[test]
    fn gpu_names_are_shortened_for_screen() {
        assert_eq!(short_gpu_name("NVIDIA GeForce RTX 5060 Ti"), "RTX 5060 Ti");
        assert_eq!(short_gpu_name("NVIDIA RTX A2000"), "RTX A2000");
        assert_eq!(short_gpu_name("Tesla T4"), "Tesla T4");
    }

    #[test]
    fn live_sampler_reports_cpu_and_memory() {
        let sp = Sampler::spawn(
            Duration::from_millis(100),
            GpuSel::Auto,
            SamplerOptions::default(),
        );
        std::thread::sleep(Duration::from_millis(450));
        let s = sp.latest();
        let cpu = s.cpu_percent.expect("CPU 使用率が取れる");
        assert!((0.0..=100.0).contains(&cpu), "{cpu}");
        let (used, total) = (s.mem_used_gb.unwrap(), s.mem_total_gb.unwrap());
        assert!(total > 0.5 && used > 0.0 && used <= total, "{used}/{total}");
    }

    #[test]
    fn gpu_selector_parse() {
        assert_eq!(GpuSel::parse("auto").unwrap(), GpuSel::Auto);
        assert_eq!(GpuSel::parse("1").unwrap(), GpuSel::Index(1));
        assert_eq!(
            GpuSel::parse("pci:0000:01:00.0").unwrap(),
            GpuSel::Pci("0000:01:00.0".into())
        );
        assert!(GpuSel::parse("rtx").is_err());
    }

    #[test]
    fn sysfs_gpus_are_amdgpu_cards_sorted_by_pci() {
        let root = std::env::temp_dir().join(format!("miharashi-drm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk_dev = |pci: &str, busy: Option<&str>| {
            let d = root.join("devices").join(pci);
            std::fs::create_dir_all(d.join("hwmon/hwmon3")).unwrap();
            if let Some(b) = busy {
                std::fs::write(d.join("gpu_busy_percent"), b).unwrap();
                std::fs::write(d.join("hwmon/hwmon3/temp1_input"), "38000\n").unwrap();
            }
            d
        };
        let drm = root.join("drm");
        let link = |card: &str, dev: &std::path::Path| {
            std::fs::create_dir_all(drm.join(card)).unwrap();
            std::os::unix::fs::symlink(dev, drm.join(card).join("device")).unwrap();
        };
        // card0 = 内蔵(10:00.0)、card1 = NVIDIA(gpu_busy_percent なし)、card2 = 別の amdgpu(03:00.0)
        link("card0", &mk_dev("0000:10:00.0", Some("7\n")));
        link("card1", &mk_dev("0000:01:00.0", None));
        link("card2", &mk_dev("0000:03:00.0", Some("55\n")));
        // コネクター(card0-HDMI-A-1)は対象外
        std::fs::create_dir_all(drm.join("card0-HDMI-A-1")).unwrap();

        let g = find_sysfs_gpus(&drm);
        let pcis: Vec<&str> = g.iter().map(|x| x.pci.as_str()).collect();
        assert_eq!(pcis, ["0000:03:00.0", "0000:10:00.0"]);
        assert_eq!(g[1].usage(), Some(7.0));
        assert_eq!(g[1].temp(), Some(38.0));
        assert!(find_sysfs_gpus(&root.join("none")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nvml_bus_id_pads_domain() {
        assert_eq!(GpuSel::nvml_bus_id("0000:01:00.0"), "00000000:01:00.0");
        assert_eq!(GpuSel::nvml_bus_id("00000000:01:00.0"), "00000000:01:00.0");
    }
}
