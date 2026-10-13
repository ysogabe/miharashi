//! `DataSource` の実装: 実測(Linux)とダミー(試験・開発用)。
use crate::app::DataSource;
use miharashi_render::screens::Snapshot;

pub const HISTORY: usize = 120;

fn dummy_history(phase: f32, n: usize, base: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let p = phase + i as f32;
            base + 25.0 * (p * 0.21).sin() + 12.0 * (p * 0.67).sin()
        })
        .collect()
}

/// ダミーの初期スナップショット(実測前の表示、`--dummy`、PNG の既定)。
pub fn dummy_snapshot(phase: f32) -> Snapshot {
    let cpu_history = dummy_history(phase, HISTORY, 45.0);
    let gpu_history = dummy_history(phase + 40.0, HISTORY, 55.0);
    let (cpu, gpu) = (*cpu_history.last().unwrap(), *gpu_history.last().unwrap());
    let mut s = Snapshot {
        cpu: Some(cpu),
        cpu_temp: Some(50.0 + cpu * 0.15),
        gpu: Some(gpu),
        gpu_temp: Some(55.0 + gpu * 0.15),
        mem_used: Some(24.0),
        mem_total: Some(64.0),
        ssd_temps: vec![("SSD-A".into(), 41.0), ("SSD-B".into(), 39.0)],
        fans: vec![
            ("CPU".into(), "1200 rpm".into()),
            ("GPU".into(), "32%".into()),
        ],
        cpu_history,
        gpu_history,
        cpu_power_w: Some(95.0 + (cpu - 45.0) * 1.2),
        gpu_power_w: Some(142.0 + (gpu - 55.0) * 1.5),
        cpu_energy_today_wh: Some(812.0),
        gpu_energy_today_wh: Some(1304.0),
        total_energy_wh: Some(48_230.0),
        price_per_kwh: None,
        cpu_power_history: dummy_history(phase + 10.0, HISTORY, 95.0),
        gpu_power_history: dummy_history(phase + 50.0, HISTORY, 140.0),
        psi_cpu: Some(2.4),
        psi_mem: Some(8.1),
        psi_io: Some(0.6),
        swap_used_gb: Some(3.1),
        swap_total_gb: Some(8.0),
        load_avg: Some([1.52, 1.31, 1.18]),
        vram_used_mib: Some(6144),
        vram_total_mib: Some(16384),
        gpu_clock_mhz: Some(2100),
        gpu_clock_max_mhz: Some(2850),
        gpu_pstate: Some("P2".into()),
        gpu_processes: vec![
            ("blender".into(), 4242, 3072),
            ("Xorg".into(), 1180, 512),
            ("firefox".into(), 5310, 256),
        ],
        net_iface: Some("enp11s0".into()),
        net_rx_mbps: Some(86.0),
        net_tx_mbps: Some(12.0),
        net_rx_history: dummy_history(phase + 20.0, HISTORY, 80.0),
        net_tx_history: dummy_history(phase + 30.0, HISTORY, 20.0),
        psi_cpu_history: dummy_history(phase + 5.0, HISTORY, 30.0),
        psi_mem_history: dummy_history(phase + 15.0, HISTORY, 20.0),
        disk_root_used_pct: Some(46.0),
        disk_root_used_total_gb: Some((428.0, 932.0)),
        disk_io: vec![
            ("nvme0n1".into(), 120.5, 35.2),
            ("nvme1n1".into(), 0.0, 4.8),
        ],
        failed_units: Some(0),
        peaks_today: vec![
            ("CPU温度".into(), "78°C".into(), "14:02".into()),
            ("GPU温度".into(), "71°C".into(), "15:40".into()),
            ("電力".into(), "291 W".into(), "15:40".into()),
            ("受信".into(), "480 Mbps".into(), "09:12".into()),
            ("メモリ圧迫".into(), "36%".into(), "11:25".into()),
        ],
        ..Default::default()
    };
    crate::app::set_clock(&mut s);
    s
}

/// 実測値の履歴は空(0)から貯める。
pub fn live_snapshot() -> Snapshot {
    Snapshot {
        cpu_history: vec![0.0; HISTORY],
        gpu_history: vec![0.0; HISTORY],
        cpu_power_history: vec![0.0; HISTORY],
        gpu_power_history: vec![0.0; HISTORY],
        net_rx_history: vec![0.0; HISTORY],
        net_tx_history: vec![0.0; HISTORY],
        psi_cpu_history: vec![0.0; HISTORY],
        psi_mem_history: vec![0.0; HISTORY],
        ..Default::default()
    }
}

pub struct DummyData {
    phase: f32,
}

impl DummyData {
    pub fn new() -> Self {
        Self { phase: 0.0 }
    }
}

impl DataSource for DummyData {
    fn update(&mut self, snap: &mut Snapshot) {
        self.phase += 1.0;
        let mut next = dummy_snapshot(self.phase);
        std::mem::swap(snap, &mut next);
    }
}

#[cfg(target_os = "linux")]
pub struct LiveData {
    sampler: miharashi_linux::metrics::Sampler,
    /// 電気代の単価。0 以下は表示しない。
    price_per_kwh: Option<f32>,
}

#[cfg(target_os = "linux")]
impl LiveData {
    pub fn new(
        interval: std::time::Duration,
        gpu: miharashi_linux::metrics::GpuSel,
        power: &crate::config::Power,
    ) -> Self {
        let opts = miharashi_linux::metrics::SamplerOptions {
            state_file: (!power.state_file.is_empty()).then(|| power.state_file.clone().into()),
            rapl_helper_file: power.rapl_helper_file.clone().into(),
        };
        Self {
            sampler: miharashi_linux::metrics::Sampler::spawn(interval, gpu, opts),
            price_per_kwh: (power.price_per_kwh > 0.0).then_some(power.price_per_kwh),
        }
    }
}

#[cfg(target_os = "linux")]
impl DataSource for LiveData {
    fn update(&mut self, s: &mut Snapshot) {
        let m = self.sampler.latest();
        s.cpu = m.cpu_percent;
        s.cpu_temp = m.cpu_temp;
        s.gpu = m.gpu_percent;
        s.gpu_temp = m.gpu_temp;
        s.mem_used = m.mem_used_gb;
        s.mem_total = m.mem_total_gb;
        s.ssd_temps = m.ssd_temps;
        s.fans = m.fans;
        // マザーボードのファンが読めない場合は、その旨を表示する。
        if !s.fans.iter().any(|(n, _)| n != "GPU") {
            s.fans.insert(0, ("CPU/CASE".into(), "--".into()));
        }
        // 取得できなかった回は履歴を進めない(0 を混ぜない)。
        if let Some(v) = m.cpu_percent {
            push_history(&mut s.cpu_history, v);
        }
        if let Some(v) = m.gpu_percent {
            push_history(&mut s.gpu_history, v);
        }

        s.cpu_power_w = m.cpu_power_w;
        s.gpu_power_w = m.gpu_power_w;
        s.cpu_energy_today_wh = m.cpu_energy_today_wh;
        s.gpu_energy_today_wh = m.gpu_energy_today_wh;
        s.total_energy_wh = m.total_energy_wh;
        s.price_per_kwh = self.price_per_kwh;
        s.psi_cpu = m.psi_cpu;
        s.psi_mem = m.psi_mem;
        s.psi_io = m.psi_io;
        s.swap_used_gb = m.swap_used_gb;
        s.swap_total_gb = m.swap_total_gb;
        s.load_avg = m.load_avg;
        s.vram_used_mib = m.vram_used_mib;
        s.vram_total_mib = m.vram_total_mib;
        s.gpu_clock_mhz = m.gpu_clock_mhz;
        s.gpu_clock_max_mhz = m.gpu_clock_max_mhz;
        s.gpu_pstate = m.gpu_pstate;
        s.gpu_processes = m.gpu_processes;
        s.net_iface = m.net_iface;
        s.net_rx_mbps = m.net_rx_mbps;
        s.net_tx_mbps = m.net_tx_mbps;
        s.disk_root_used_pct = m.disk_root_used_pct;
        s.disk_root_used_total_gb = m.disk_root_used_total_gb;
        s.disk_io = m.disk_io;
        s.failed_units = m.failed_units;
        s.peaks_today = m.peaks_today;
        for (hist, v) in [
            (&mut s.cpu_power_history, m.cpu_power_w),
            (&mut s.gpu_power_history, m.gpu_power_w),
            (&mut s.net_rx_history, m.net_rx_mbps),
            (&mut s.net_tx_history, m.net_tx_mbps),
            (&mut s.psi_cpu_history, m.psi_cpu),
            (&mut s.psi_mem_history, m.psi_mem),
        ] {
            if let Some(v) = v {
                push_history(hist, v);
            }
        }
    }
}

fn push_history(h: &mut Vec<f32>, v: f32) {
    if h.len() >= HISTORY {
        h.remove(0);
    }
    h.push(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_capped() {
        let mut h = vec![0.0; HISTORY];
        push_history(&mut h, 5.0);
        assert_eq!(h.len(), HISTORY);
        assert_eq!(*h.last().unwrap(), 5.0);
    }

    #[test]
    fn dummy_snapshot_has_all_fields() {
        let s = dummy_snapshot(0.0);
        assert!(
            s.cpu.is_some()
                && s.gpu.is_some()
                && !s.ssd_temps.is_empty()
                && s.cpu_history.len() == HISTORY
        );
        for h in [
            &s.cpu_power_history,
            &s.gpu_power_history,
            &s.net_rx_history,
            &s.net_tx_history,
            &s.psi_cpu_history,
            &s.psi_mem_history,
        ] {
            assert_eq!(h.len(), HISTORY);
        }
        assert_eq!(s.disk_io.len(), 2);
        assert!((2..=3).contains(&s.gpu_processes.len()));
        assert!(s.price_per_kwh.is_none() && !s.peaks_today.is_empty());
    }
}
