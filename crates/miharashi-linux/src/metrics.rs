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

/// NVML から読む GPU の追加指標。
#[derive(Default)]
struct GpuExtra {
    power_w: Option<f32>,
    vram: Option<(u32, u32)>,
    clock: Option<(u32, u32)>,
    pstate: Option<String>,
    processes: Vec<(String, u32, u32)>,
}

fn read_gpu_extra(d: &nvml_wrapper::Device) -> GpuExtra {
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
    GpuExtra {
        power_w: d.power_usage().ok().map(|mw| mw as f32 / 1000.0),
        vram: d
            .memory_info()
            .ok()
            .map(|m| ((m.used / MIB) as u32, (m.total / MIB) as u32)),
        clock: d
            .clock_info(Clock::Graphics)
            .ok()
            .zip(d.max_clock_info(Clock::Graphics).ok()),
        pstate: d.performance_state().ok().and_then(|p| {
            // c_variant は NVML_PSTATE_0..15 の値。Unknown は 32
            let n = p.as_c();
            (n <= 15).then(|| format!("P{n}"))
        }),
        processes: procs,
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
            loop {
                std::thread::sleep(interval);
                let tick = Instant::now();
                let dt = tick.duration_since(last_tick).as_secs_f64();
                last_tick = tick;
                let now = read_cpu_times();
                let cpu_percent = match (prev, now) {
                    (Some(p), Some(n)) if n.1 > p.1 => {
                        Some(100.0 * (n.0 - p.0) as f32 / (n.1 - p.1) as f32)
                    }
                    _ => None,
                };
                prev = now;
                let (mem_used_gb, mem_total_gb) = read_mem().unzip();
                let cpu_temp = temp_path.as_ref().and_then(|p| read_milli_c(p));
                let gpu_temp = gpu
                    .as_ref()
                    .and_then(|d| {
                        d.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
                            .ok()
                    })
                    .map(|t| t as f32);
                let gx = gpu.as_ref().map(read_gpu_extra).unwrap_or_default();
                let cpu_power_w = rapl.poll(tick);
                let netr = net.poll(tick);
                let psi_mem = read_psi("memory");
                let swap = std::fs::read_to_string("/proc/meminfo")
                    .ok()
                    .and_then(|t| sysinfo::parse_swap_gb(&t));
                let root = sysinfo::read_root_disk();
                let now_local = chrono::Local::now().naive_local();
                energy.add(now_local.date(), cpu_power_w, gx.power_w, dt);
                energy.save_if_due(tick);
                let (cpu_energy_today_wh, gpu_energy_today_wh, total_energy_wh) = energy.values();
                let net_rx_mbps = netr.as_ref().and_then(|n| n.1);
                let peaks_today = peaks.update(
                    now_local,
                    PeakInput {
                        cpu_temp,
                        gpu_temp,
                        cpu_w: cpu_power_w,
                        gpu_w: gx.power_w,
                        rx_mbps: net_rx_mbps,
                        psi_mem,
                    },
                );
                let s = Sample {
                    cpu_power_w,
                    gpu_power_w: gx.power_w,
                    cpu_energy_today_wh,
                    gpu_energy_today_wh,
                    total_energy_wh,
                    psi_cpu: read_psi("cpu"),
                    psi_mem,
                    psi_io: read_psi("io"),
                    swap_used_gb: swap.map(|s| s.0),
                    swap_total_gb: swap.map(|s| s.1),
                    load_avg: std::fs::read_to_string("/proc/loadavg")
                        .ok()
                        .and_then(|t| sysinfo::parse_loadavg(&t)),
                    vram_used_mib: gx.vram.map(|v| v.0),
                    vram_total_mib: gx.vram.map(|v| v.1),
                    gpu_clock_mhz: gx.clock.map(|c| c.0),
                    gpu_clock_max_mhz: gx.clock.map(|c| c.1),
                    gpu_pstate: gx.pstate,
                    gpu_processes: gx.processes,
                    net_iface: netr.as_ref().map(|n| n.0.clone()),
                    net_rx_mbps,
                    net_tx_mbps: netr.as_ref().and_then(|n| n.2),
                    disk_root_used_pct: root.map(|r| r.0),
                    disk_root_used_total_gb: root.map(|r| r.1),
                    disk_io: disk_io.poll(tick),
                    failed_units: failed.poll(tick),
                    peaks_today,
                    cpu_percent,
                    cpu_temp,
                    gpu_temp,
                    gpu_percent: gpu
                        .as_ref()
                        .and_then(|d| d.utilization_rates().ok())
                        .map(|u| u.gpu as f32),
                    mem_used_gb,
                    mem_total_gb,
                    ssd_temps: ssds
                        .iter()
                        .filter_map(|(n, p)| Some((n.clone(), read_milli_c(p)?)))
                        .collect(),
                    fans: {
                        let mut f: Vec<(String, String)> = fan_inputs
                            .iter()
                            .filter_map(|(n, p)| {
                                let rpm: u32 =
                                    std::fs::read_to_string(p).ok()?.trim().parse().ok()?;
                                Some((n.clone(), format!("{rpm} rpm")))
                            })
                            .collect();
                        if let Some(pct) = gpu.as_ref().and_then(|d| d.fan_speed(0).ok()) {
                            f.push(("GPU".to_string(), format!("{pct}%")));
                        }
                        f
                    },
                };
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn nvml_bus_id_pads_domain() {
        assert_eq!(GpuSel::nvml_bus_id("0000:01:00.0"), "00000000:01:00.0");
        assert_eq!(GpuSel::nvml_bus_id("00000000:01:00.0"), "00000000:01:00.0");
    }
}
