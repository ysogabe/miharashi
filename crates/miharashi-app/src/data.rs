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
}

#[cfg(target_os = "linux")]
impl LiveData {
    pub fn new(interval: std::time::Duration, gpu: miharashi_linux::metrics::GpuSel) -> Self {
        Self {
            sampler: miharashi_linux::metrics::Sampler::spawn(interval, gpu),
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
    }
}
