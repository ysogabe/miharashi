//! CPU・GPU の電力量 [Wh] の積算。今日分はローカル 0 時でリセットし、累計は JSON に保存する。
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 保存する内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct State {
    /// "今日" のローカル日付 (YYYY-MM-DD)
    date: String,
    cpu_wh: f64,
    gpu_wh: f64,
    /// 累計 (CPU+GPU)
    total_wh: f64,
}

/// 1 回の積算で許す最大の経過秒。スリープ復帰などで巨大な dt を積まない。
const MAX_DT_SECS: f64 = 10.0;
const SAVE_INTERVAL: Duration = Duration::from_secs(60);

pub struct EnergyAccumulator {
    path: Option<PathBuf>,
    date: NaiveDate,
    cpu_wh: f64,
    gpu_wh: f64,
    total_wh: f64,
    cpu_seen: bool,
    gpu_seen: bool,
    last_save: Option<Instant>,
    warned: bool,
}

impl EnergyAccumulator {
    /// `path` から読み込む。無い・壊れている場合は 0 から始める。日付が違えば今日分だけ 0。
    pub fn load(path: Option<PathBuf>, today: NaiveDate) -> Self {
        let mut s = Self {
            path,
            date: today,
            cpu_wh: 0.0,
            gpu_wh: 0.0,
            total_wh: 0.0,
            cpu_seen: false,
            gpu_seen: false,
            last_save: None,
            warned: false,
        };
        let Some(p) = &s.path else { return s };
        match std::fs::read_to_string(p) {
            Ok(text) => match serde_json::from_str::<State>(&text) {
                Ok(st) => {
                    s.total_wh = st.total_wh.max(0.0);
                    if st.date == today.to_string() {
                        s.cpu_wh = st.cpu_wh.max(0.0);
                        s.gpu_wh = st.gpu_wh.max(0.0);
                    }
                }
                Err(e) => eprintln!(
                    "energy: {} を解釈できない({e})。0 から積算する",
                    p.display()
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("energy: {} を読めない({e})。0 から積算する", p.display()),
        }
        s
    }

    /// 経過 `dt_secs` 秒の W を積算する。日付が変わったら今日分を 0 に戻す。
    pub fn add(&mut self, today: NaiveDate, cpu_w: Option<f32>, gpu_w: Option<f32>, dt_secs: f64) {
        if today != self.date {
            self.date = today;
            self.cpu_wh = 0.0;
            self.gpu_wh = 0.0;
        }
        let dt = dt_secs.clamp(0.0, MAX_DT_SECS);
        if let Some(w) = cpu_w {
            let wh = w as f64 * dt / 3600.0;
            self.cpu_wh += wh;
            self.total_wh += wh;
            self.cpu_seen = true;
        }
        if let Some(w) = gpu_w {
            let wh = w as f64 * dt / 3600.0;
            self.gpu_wh += wh;
            self.total_wh += wh;
            self.gpu_seen = true;
        }
    }

    /// (CPU 今日, GPU 今日, 累計)。まだ一度も測れていない側は None。
    pub fn values(&self) -> (Option<f64>, Option<f64>, Option<f64>) {
        (
            (self.cpu_seen || self.cpu_wh > 0.0).then_some(self.cpu_wh),
            (self.gpu_seen || self.gpu_wh > 0.0).then_some(self.gpu_wh),
            (self.cpu_seen || self.gpu_seen || self.total_wh > 0.0).then_some(self.total_wh),
        )
    }

    /// 前回の保存から 60 秒以上経っていれば保存する。
    pub fn save_if_due(&mut self, now: Instant) {
        if self
            .last_save
            .is_none_or(|t| now.duration_since(t) >= SAVE_INTERVAL)
        {
            self.last_save = Some(now);
            self.save();
        }
    }

    /// 一時ファイルに書いてから rename。失敗は最初の 1 回だけ報告し、メモリ上の積算は続ける。
    pub fn save(&mut self) {
        let Some(p) = self.path.clone() else { return };
        let st = State {
            date: self.date.to_string(),
            cpu_wh: self.cpu_wh,
            gpu_wh: self.gpu_wh,
            total_wh: self.total_wh,
        };
        let res = (|| -> std::io::Result<()> {
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            let mut tmp = p.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            std::fs::write(
                &tmp,
                serde_json::to_vec(&st).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(&tmp, &p)
        })();
        if let Err(e) = res
            && !self.warned
        {
            self.warned = true;
            eprintln!(
                "energy: {} に保存できない({e})。メモリ上の積算だけ続ける",
                p.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("miharashi-energy-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn integrates_watts_and_resets_today_at_midnight() {
        let mut e = EnergyAccumulator::load(None, d(1));
        assert_eq!(e.values(), (None, None, None));
        e.add(d(1), Some(360.0), Some(180.0), 10.0); // 1 Wh, 0.5 Wh
        let (c, g, t) = e.values();
        assert!((c.unwrap() - 1.0).abs() < 1e-9 && (g.unwrap() - 0.5).abs() < 1e-9);
        assert!((t.unwrap() - 1.5).abs() < 1e-9);
        e.add(d(2), Some(360.0), None, 10.0); // 日付が変わった
        let (c, g, t) = e.values();
        assert!((c.unwrap() - 1.0).abs() < 1e-9);
        assert!((g.unwrap() - 0.0).abs() < 1e-9);
        assert!((t.unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn huge_dt_is_clamped() {
        let mut e = EnergyAccumulator::load(None, d(1));
        e.add(d(1), Some(360.0), None, 3600.0);
        assert!((e.values().0.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn persists_round_trip() {
        let dir = tmp("rt");
        let path = dir.join("sub/energy.json");
        let mut e = EnergyAccumulator::load(Some(path.clone()), d(1));
        e.add(d(1), Some(3600.0), Some(1800.0), 10.0);
        e.save_if_due(Instant::now());
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
        // 同じ日: 今日分も累計も続く
        let same = EnergyAccumulator::load(Some(path.clone()), d(1));
        assert_eq!(same.values(), e.values());
        // 別の日: 今日分は 0、累計は保持
        let next = EnergyAccumulator::load(Some(path), d(2));
        let (c, g, t) = next.values();
        assert_eq!((c, g), (None, None));
        assert_eq!(t, e.values().2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_is_throttled() {
        let dir = tmp("thr");
        let path = dir.join("e.json");
        let mut e = EnergyAccumulator::load(Some(path.clone()), d(1));
        let t0 = Instant::now();
        e.save_if_due(t0);
        std::fs::remove_file(&path).unwrap();
        e.save_if_due(t0 + Duration::from_secs(30));
        assert!(!path.exists());
        e.save_if_due(t0 + Duration::from_secs(61));
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unwritable_path_does_not_panic() {
        // 通常ファイルの下にはディレクトリを作れない
        let dir = tmp("nw");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("file");
        std::fs::write(&blocker, "x").unwrap();
        let mut e = EnergyAccumulator::load(Some(blocker.join("sub/energy.json")), d(1));
        e.add(d(1), Some(100.0), None, 1.0);
        e.save();
        e.save();
        assert!(e.warned);
        assert!(e.values().0.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
