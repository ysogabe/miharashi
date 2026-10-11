//! 実機のCPU・メモリ・温度・GPU(NVML)を1秒ごとに読む。取得できない項目は None。
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
}

pub struct Sampler {
    latest: Arc<Mutex<Sample>>,
}

impl Sampler {
    /// バックグラウンドスレッドで `interval` ごとにサンプリングする。
    pub fn spawn(interval: Duration) -> Self {
        let latest = Arc::new(Mutex::new(Sample::default()));
        let shared = latest.clone();
        std::thread::spawn(move || {
            let nvml = nvml_wrapper::Nvml::init().ok();
            let gpu = nvml.as_ref().and_then(|n| n.device_by_index(0).ok());
            let temp_path = find_cpu_temp_input();
            let ssds = find_ssd_inputs();
            let fan_inputs = find_fan_inputs();
            let mut prev = read_cpu_times();
            loop {
                std::thread::sleep(interval);
                let now = read_cpu_times();
                let cpu_percent = match (prev, now) {
                    (Some(p), Some(n)) if n.1 > p.1 => Some(100.0 * (n.0 - p.0) as f32 / (n.1 - p.1) as f32),
                    _ => None,
                };
                prev = now;
                let (mem_used_gb, mem_total_gb) = read_mem().unzip();
                let s = Sample {
                    cpu_percent,
                    cpu_temp: temp_path.as_ref().and_then(|p| read_milli_c(p)),
                    gpu_percent: gpu.as_ref().and_then(|d| d.utilization_rates().ok()).map(|u| u.gpu as f32),
                    gpu_temp: gpu
                        .as_ref()
                        .and_then(|d| d.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu).ok())
                        .map(|t| t as f32),
                    mem_used_gb,
                    mem_total_gb,
                    ssd_temps: ssds.iter().filter_map(|(n, p)| Some((n.clone(), read_milli_c(p)?))).collect(),
                    fans: {
                        let mut f: Vec<(String, String)> = fan_inputs
                            .iter()
                            .filter_map(|(n, p)| {
                                let rpm: u32 = std::fs::read_to_string(p).ok()?.trim().parse().ok()?;
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
    let v: Vec<u64> = s.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
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
        s.lines().find(|l| l.starts_with(key))?.split_whitespace().nth(1)?.parse::<f32>().ok()
    };
    let (total, avail) = (get("MemTotal:")?, get("MemAvailable:")?);
    Some(((total - avail) / 1024.0 / 1024.0, total / 1024.0 / 1024.0))
}

fn read_milli_c(path: &std::path::Path) -> Option<f32> {
    Some(std::fs::read_to_string(path).ok()?.trim().parse::<f32>().ok()? / 1000.0)
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
            let label = std::fs::read_to_string(dir.join(format!("temp{n}_label"))).unwrap_or_default();
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
    let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") else { return out };
    for entry in rd.flatten() {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("name")).unwrap_or_default().trim() != "nvme" {
            continue;
        }
        let model = std::fs::read_to_string(dir.join("device/model")).unwrap_or_default();
        let name = model.split(|c: char| c.is_whitespace()).next().unwrap_or("NVMe").to_string();
        for n in 1..=4 {
            let label = std::fs::read_to_string(dir.join(format!("temp{n}_label"))).unwrap_or_default();
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
    let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") else { return out };
    for entry in rd.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default().trim().to_string();
        for n in 1..=8 {
            let input = dir.join(format!("fan{n}_input"));
            if input.exists() {
                let label = std::fs::read_to_string(dir.join(format!("fan{n}_label"))).unwrap_or_default();
                let label = if label.trim().is_empty() { format!("{name} {n}") } else { label.trim().to_string() };
                out.push((label, input));
            }
        }
    }
    out.sort();
    out
}
