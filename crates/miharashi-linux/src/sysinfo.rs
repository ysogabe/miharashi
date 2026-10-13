//! /proc・/sys の追加指標(PSI・スワップ・ネットワーク・ディスク・RAPL・ピーク)。
//! 解釈は文字列を受け取る純粋関数に分け、状態を持つ集計は `*Tracker` にまとめる。
use std::time::{Duration, Instant};

/// PSI ファイルの `some avg10=` の値 [%]。
pub fn parse_psi_some_avg10(text: &str) -> Option<f32> {
    let line = text.lines().find(|l| l.starts_with("some "))?;
    line.split_whitespace()
        .find_map(|t| t.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// /proc/meminfo のスワップ (使用, 全体) [GiB]。
pub fn parse_swap_gb(meminfo: &str) -> Option<(f32, f32)> {
    let get = |key: &str| -> Option<f32> {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let (total, free) = (get("SwapTotal:")?, get("SwapFree:")?);
    Some((
        (total - free).max(0.0) / 1024.0 / 1024.0,
        total / 1024.0 / 1024.0,
    ))
}

/// /proc/loadavg の 1/5/15 分。
pub fn parse_loadavg(text: &str) -> Option<[f32; 3]> {
    let mut it = text.split_whitespace().map(|t| t.parse::<f32>());
    Some([it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?])
}

/// /proc/net/route から既定経路(Destination が 00000000 で最小 Metric)のインターフェース名。
pub fn parse_default_iface(route: &str) -> Option<String> {
    route
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() > 6 && f[1] == "00000000" {
                Some((f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
            } else {
                None
            }
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, name)| name)
}

/// /proc/net/dev から指定インターフェースの (rx bytes, tx bytes)。
pub fn parse_net_dev(text: &str, iface: &str) -> Option<(u64, u64)> {
    for l in text.lines().skip(2) {
        let (name, rest) = l.split_once(':')?;
        if name.trim() == iface {
            let f: Vec<&str> = rest.split_whitespace().collect();
            return Some((f.first()?.parse().ok()?, f.get(8)?.parse().ok()?));
        }
    }
    None
}

/// /sys/block/<dev>/stat の (読み込みセクタ, 書き込みセクタ)。
pub fn parse_block_stat(text: &str) -> Option<(u64, u64)> {
    let f: Vec<&str> = text.split_whitespace().collect();
    Some((f.get(2)?.parse().ok()?, f.get(6)?.parse().ok()?))
}

/// パーティションや loop/ram/zram を除いた物理ディスク名か(nvmeXnY, sdX, vdX)。
pub fn is_whole_disk(name: &str) -> bool {
    let all = |s: &str, f: fn(char) -> bool| !s.is_empty() && s.chars().all(f);
    if let Some(rest) = name.strip_prefix("nvme") {
        return match rest.split_once('n') {
            Some((c, n)) => all(c, |c| c.is_ascii_digit()) && all(n, |c| c.is_ascii_digit()),
            None => false,
        };
    }
    ["sd", "vd"].iter().any(|p| {
        name.strip_prefix(p)
            .is_some_and(|r| all(r, |c| c.is_ascii_lowercase()))
    })
}

/// RAPL カウンターの増分 [uJ]。ラップは `max` で補正。値が変わっていなければ None。
pub fn rapl_delta_uj(prev: u64, cur: u64, max: u64) -> Option<u64> {
    if cur == prev {
        None
    } else if cur > prev {
        Some(cur - prev)
    } else if max > prev {
        Some(max - prev + cur)
    } else {
        None
    }
}

/// ヘルパーファイルの "<energy_uj> <max_energy_range_uj>"。
pub fn parse_rapl_helper(text: &str) -> Option<(u64, u64)> {
    let mut it = text.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// CPU 電力 [W] の追跡。ヘルパーファイル優先、無ければ sysfs を直接読む。
pub struct RaplTracker {
    helper: std::path::PathBuf,
    /// 前回の変化時点 (energy, 時刻)
    base: Option<(u64, Instant)>,
    watts: Option<f32>,
    warned: bool,
}

/// ヘルパーが止まったとみなす、値が変わらない時間。
const RAPL_STALE: Duration = Duration::from_secs(5);

impl RaplTracker {
    pub fn new(helper: impl Into<std::path::PathBuf>) -> Self {
        Self {
            helper: helper.into(),
            base: None,
            watts: None,
            warned: false,
        }
    }

    fn read_counter(&mut self) -> Option<(u64, u64)> {
        if let Ok(t) = std::fs::read_to_string(&self.helper) {
            return parse_rapl_helper(&t);
        }
        let dir = std::path::Path::new("/sys/class/powercap/intel-rapl:0");
        let rd = |n: &str| -> std::io::Result<u64> {
            std::fs::read_to_string(dir.join(n))?
                .trim()
                .parse()
                .map_err(|_| std::io::ErrorKind::InvalidData.into())
        };
        match (rd("energy_uj"), rd("max_energy_range_uj")) {
            (Ok(e), Ok(m)) => Some((e, m)),
            (Err(e), _) | (_, Err(e)) => {
                if !self.warned {
                    self.warned = true;
                    eprintln!(
                        "metrics: CPU 電力を読めない({e})。ヘルパー {} も無い。CPU の W は -- と表示する",
                        self.helper.display()
                    );
                }
                None
            }
        }
    }

    /// 現在の CPU 電力 [W]。最初の 1 回、読めないとき、ヘルパーが止まったときは None。
    pub fn poll(&mut self, now: Instant) -> Option<f32> {
        let Some((cur, max)) = self.read_counter() else {
            self.base = None;
            self.watts = None;
            return None;
        };
        let Some((prev, t0)) = self.base else {
            self.base = Some((cur, now));
            return None;
        };
        match rapl_delta_uj(prev, cur, max) {
            Some(d) => {
                let dt = now.duration_since(t0).as_secs_f32();
                if dt > 0.0 {
                    self.watts = Some(d as f32 / 1e6 / dt);
                }
                self.base = Some((cur, now));
            }
            // 未更新の間は前回の W を保つ(0 にしない)。長く続けば停止とみなす。
            None if now.duration_since(t0) > RAPL_STALE => self.watts = None,
            None => {}
        }
        self.watts
    }
}

/// 累積カウンターの差分から毎秒の値を出す。`scale` で単位を変換する。
fn rate(prev: u64, cur: u64, dt: f32, scale: f32) -> Option<f32> {
    (cur >= prev && dt > 0.0).then(|| (cur - prev) as f32 * scale / dt)
}

/// 既定経路インターフェースの rx/tx [Mbps]。
#[derive(Default)]
pub struct NetTracker {
    prev: Option<(String, u64, u64, Instant)>,
}

/// (インターフェース名, rx Mbps, tx Mbps)
pub type NetRate = (String, Option<f32>, Option<f32>);

impl NetTracker {
    /// 名前以外は、初回や切替直後は None。
    pub fn poll(&mut self, now: Instant) -> Option<NetRate> {
        let route = std::fs::read_to_string("/proc/net/route").ok()?;
        let iface = parse_default_iface(&route)?;
        let dev = std::fs::read_to_string("/proc/net/dev").ok()?;
        let (rx, tx) = parse_net_dev(&dev, &iface)?;
        let out = match &self.prev {
            Some((n, prx, ptx, t)) if *n == iface => {
                let dt = now.duration_since(*t).as_secs_f32();
                (rate(*prx, rx, dt, 8.0 / 1e6), rate(*ptx, tx, dt, 8.0 / 1e6))
            }
            _ => (None, None),
        };
        self.prev = Some((iface.clone(), rx, tx, now));
        Some((iface, out.0, out.1))
    }
}

/// 物理ディスクごとの読み書き [MB/s]。
pub struct DiskIoTracker {
    disks: Vec<String>,
    prev: Vec<Option<(u64, u64)>>,
    prev_t: Option<Instant>,
}

impl Default for DiskIoTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskIoTracker {
    pub fn new() -> Self {
        let mut disks: Vec<String> = std::fs::read_dir("/sys/block")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_whole_disk(n))
            .collect();
        disks.sort();
        let prev = vec![None; disks.len()];
        Self {
            disks,
            prev,
            prev_t: None,
        }
    }

    /// (名前, 読み MB/s, 書き MB/s)。初回は空。
    pub fn poll(&mut self, now: Instant) -> Vec<(String, f32, f32)> {
        let dt = self
            .prev_t
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        let mut out = Vec::new();
        for (i, name) in self.disks.iter().enumerate() {
            let cur = std::fs::read_to_string(format!("/sys/block/{name}/stat"))
                .ok()
                .and_then(|t| parse_block_stat(&t));
            if let (Some(p), Some(c)) = (self.prev[i], cur) {
                // セクタは 512 バイト固定
                let k = 512.0 / 1e6;
                if let (Some(r), Some(w)) = (rate(p.0, c.0, dt, k), rate(p.1, c.1, dt, k)) {
                    out.push((name.clone(), r, w));
                }
            }
            self.prev[i] = cur;
        }
        self.prev_t = Some(now);
        out
    }
}

/// ルートファイルシステムの (使用率 %, (使用 GiB, 全体 GiB))。
pub fn read_root_disk() -> Option<(f32, (f32, f32))> {
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: 有効な NUL 終端パスと、初期化済みの出力構造体を渡している。
    let rc = unsafe { libc::statvfs(c"/".as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let fr = st.f_frsize as f64;
    let total = st.f_blocks as f64 * fr;
    let used = (st.f_blocks - st.f_bfree) as f64 * fr;
    let avail = st.f_bavail as f64 * fr;
    if total <= 0.0 {
        return None;
    }
    // df と同じく、非特権ユーザーが使える分を基準にした使用率
    let pct = (used / (used + avail) * 100.0) as f32;
    let gib = 1024.0 * 1024.0 * 1024.0;
    Some((pct, ((used / gib) as f32, (total / gib) as f32)))
}

/// `systemctl --state=failed` の行数を一定間隔でだけ取得する。
#[derive(Default)]
pub struct FailedUnits {
    last: Option<Instant>,
    value: Option<u32>,
}

impl FailedUnits {
    pub fn poll(&mut self, now: Instant) -> Option<u32> {
        if self
            .last
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(30))
        {
            self.last = Some(now);
            self.value = std::process::Command::new("systemctl")
                .args(["list-units", "--state=failed", "--no-legend", "--plain"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| count_lines(&String::from_utf8_lossy(&o.stdout)));
        }
        self.value
    }
}

/// 空行を除いた行数。
pub fn count_lines(text: &str) -> u32 {
    text.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

type Peak = Option<(f32, String)>;

/// 今日のピーク(ローカル 0 時でリセット)。
#[derive(Default)]
pub struct Peaks {
    date: Option<chrono::NaiveDate>,
    cpu_temp: Peak,
    gpu_temp: Peak,
    power: Peak,
    rx: Peak,
    psi_mem: Peak,
}

/// ピーク計算に渡す、その時点の値。
#[derive(Default, Clone, Copy)]
pub struct PeakInput {
    pub cpu_temp: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub cpu_w: Option<f32>,
    pub gpu_w: Option<f32>,
    pub rx_mbps: Option<f32>,
    pub psi_mem: Option<f32>,
}

fn bump(slot: &mut Peak, v: Option<f32>, hhmm: &str) {
    if let Some(v) = v
        && slot.as_ref().is_none_or(|(m, _)| v > *m)
    {
        *slot = Some((v, hhmm.to_string()));
    }
}

impl Peaks {
    /// 値を取り込み、(項目名, 値, "HH:MM") の一覧を返す。データのある項目だけ。
    pub fn update(
        &mut self,
        now: chrono::NaiveDateTime,
        i: PeakInput,
    ) -> Vec<(String, String, String)> {
        if self.date != Some(now.date()) {
            *self = Self {
                date: Some(now.date()),
                ..Default::default()
            };
        }
        let hhmm = now.format("%H:%M").to_string();
        bump(&mut self.cpu_temp, i.cpu_temp, &hhmm);
        bump(&mut self.gpu_temp, i.gpu_temp, &hhmm);
        let power = match (i.cpu_w, i.gpu_w) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        bump(&mut self.power, power, &hhmm);
        bump(&mut self.rx, i.rx_mbps, &hhmm);
        bump(&mut self.psi_mem, i.psi_mem, &hhmm);
        let mut out = Vec::new();
        let mut add = |label: &str, slot: &Peak, f: &dyn Fn(f32) -> String| {
            if let Some((v, t)) = slot {
                out.push((label.to_string(), f(*v), t.clone()));
            }
        };
        add("CPU温度", &self.cpu_temp, &|v| format!("{v:.0}°C"));
        add("GPU温度", &self.gpu_temp, &|v| format!("{v:.0}°C"));
        add("電力", &self.power, &|v| format!("{v:.0} W"));
        add("受信", &self.rx, &|v| format!("{v:.0} Mbps"));
        add("メモリ圧迫", &self.psi_mem, &|v| format!("{v:.0}%"));
        out
    }
}

/// `/proc/<pid>/stat` の utime + stime(クロックティック)。comm に空白や ")" が入っても
/// 最後の ")" の後ろから数える。
pub fn parse_proc_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // ")" の後ろは state(3 番目のフィールド)から始まる。utime = 14、stime = 15
    let mut f = rest.split_whitespace().skip(11);
    let utime: u64 = f.next()?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some(utime + stime)
}

/// 自プロセスの CPU 使用率 [%](1 コア = 100%、全スレッドの合計)。`poll` の間隔で平均する。
pub struct SelfCpu {
    prev: Option<(u64, Instant)>,
    hz: f64,
}

impl Default for SelfCpu {
    fn default() -> Self {
        // SAFETY: sysconf は引数を読むだけ
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            prev: None,
            hz: if hz > 0 { hz as f64 } else { 100.0 },
        }
    }
}

impl SelfCpu {
    /// 前回の `poll` からの CPU%。初回と読めなかったときは None。
    pub fn poll(&mut self, now: Instant) -> Option<f32> {
        let ticks = parse_proc_cpu_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)?;
        let prev = self.prev.replace((ticks, now))?;
        let dt = now.duration_since(prev.1).as_secs_f64();
        (dt > 0.0).then(|| (ticks.saturating_sub(prev.0) as f64 / self.hz / dt * 100.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_cpu_ticks() {
        let stat = "46025 (mi ha)ra) S 1 46025 46025 0 -1 4194560 3216 0 0 0 1043 372 0 0 20 0 4 0 100 0 0";
        assert_eq!(parse_proc_cpu_ticks(stat), Some(1043 + 372));
        assert_eq!(parse_proc_cpu_ticks("1 (x) S 1"), None);
        let mut c = SelfCpu::default();
        let t0 = Instant::now();
        assert_eq!(c.poll(t0), None);
        let v = c
            .poll(t0 + Duration::from_secs(1))
            .expect("2 回目は値が出る");
        assert!((0.0..=100.0 * 64.0).contains(&v), "{v}");
    }

    #[test]
    fn psi_some_avg10() {
        let t = "some avg10=1.25 avg60=0.80 avg300=0.40 total=123456\nfull avg10=0.50 avg60=0.10 avg300=0.00 total=789\n";
        assert_eq!(parse_psi_some_avg10(t), Some(1.25));
        assert_eq!(parse_psi_some_avg10(""), None);
    }

    #[test]
    fn swap_and_loadavg() {
        let m = "MemTotal: 65536000 kB\nSwapTotal:       8388608 kB\nSwapFree:        5242880 kB\n";
        let (u, t) = parse_swap_gb(m).unwrap();
        assert!((u - 3.0).abs() < 1e-4 && (t - 8.0).abs() < 1e-4);
        assert_eq!(
            parse_loadavg("0.52 0.41 0.30 1/800 12345\n"),
            Some([0.52, 0.41, 0.30])
        );
        assert_eq!(parse_loadavg("x"), None);
    }

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
enp11s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
enp11s0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn default_route_picks_lowest_metric() {
        assert_eq!(parse_default_iface(ROUTE).as_deref(), Some("enp11s0"));
        assert_eq!(parse_default_iface("Iface\tDestination\n"), None);
    }

    #[test]
    fn net_dev_bytes() {
        let t = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0\nenp11s0: 123456789 100 0 0 0 0 0 0 987654 50 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(t, "enp11s0"), Some((123456789, 987654)));
        assert_eq!(parse_net_dev(t, "eth9"), None);
    }

    #[test]
    fn block_stat_sectors() {
        let t = "  1000   20  8000   300   500   10  4000   200   0   400   500   0 0 0 0 0 0\n";
        assert_eq!(parse_block_stat(t), Some((8000, 4000)));
    }

    #[test]
    fn whole_disk_filter() {
        for ok in ["nvme0n1", "nvme12n3", "sda", "vdb"] {
            assert!(is_whole_disk(ok), "{ok}");
        }
        for ng in [
            "nvme0n1p1",
            "nvme0c0n1",
            "sda1",
            "loop0",
            "ram0",
            "zram0",
            "dm-0",
            "sd",
            "nvme0",
        ] {
            assert!(!is_whole_disk(ng), "{ng}");
        }
    }

    #[test]
    fn rapl_delta_and_wrap() {
        assert_eq!(rapl_delta_uj(100, 350, 1000), Some(250));
        assert_eq!(rapl_delta_uj(900, 100, 1000), Some(200)); // ラップ
        assert_eq!(rapl_delta_uj(500, 500, 1000), None); // 未更新
        assert_eq!(rapl_delta_uj(5000, 100, 1000), None); // 範囲外
        assert_eq!(
            parse_rapl_helper("123456 262143328850\n"),
            Some((123456, 262143328850))
        );
        assert_eq!(parse_rapl_helper("abc"), None);
    }

    #[test]
    fn rapl_tracker_keeps_watts_when_unchanged() {
        let dir = std::env::temp_dir().join(format!("miharashi-rapl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("energy");
        let mut tr = RaplTracker::new(&f);
        let t0 = Instant::now();
        std::fs::write(&f, "1000000 100000000\n").unwrap();
        assert_eq!(tr.poll(t0), None); // 初回は捨てる
        std::fs::write(&f, "51000000 100000000\n").unwrap(); // 50 J / 1 s
        let w = tr.poll(t0 + Duration::from_secs(1)).unwrap();
        assert!((w - 50.0).abs() < 0.01, "{w}");
        // ヘルパー未更新: 0 W にせず前回値を保つ
        assert_eq!(tr.poll(t0 + Duration::from_secs(2)), Some(w));
        // 長く止まれば None
        assert_eq!(tr.poll(t0 + Duration::from_secs(10)), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn root_disk_is_sane() {
        let (pct, (used, total)) = read_root_disk().unwrap();
        assert!((0.0..=100.0).contains(&pct) && used <= total && total > 0.0);
    }

    #[test]
    fn failed_units_line_count() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(
            count_lines("a.service loaded failed failed A\n\nb.service x\n"),
            2
        );
    }

    #[test]
    fn peaks_reset_at_midnight() {
        let d = |h, m, day| {
            chrono::NaiveDate::from_ymd_opt(2026, 10, day)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap()
        };
        let mut p = Peaks::default();
        p.update(
            d(9, 12, 1),
            PeakInput {
                cpu_temp: Some(60.0),
                cpu_w: Some(100.0),
                gpu_w: Some(50.0),
                rx_mbps: Some(480.0),
                ..Default::default()
            },
        );
        let v = p.update(
            d(14, 2, 1),
            PeakInput {
                cpu_temp: Some(78.0),
                cpu_w: Some(90.0),
                gpu_w: Some(40.0),
                rx_mbps: Some(10.0),
                psi_mem: Some(36.0),
                ..Default::default()
            },
        );
        let get = |k: &str| v.iter().find(|x| x.0 == k).cloned();
        assert_eq!(
            get("CPU温度"),
            Some(("CPU温度".into(), "78°C".into(), "14:02".into()))
        );
        assert_eq!(get("受信").unwrap().2, "09:12");
        assert_eq!(get("電力").unwrap().1, "150 W");
        assert_eq!(get("メモリ圧迫").unwrap().1, "36%");
        assert!(get("GPU温度").is_none());
        let v = p.update(
            d(0, 1, 2),
            PeakInput {
                cpu_temp: Some(40.0),
                ..Default::default()
            },
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, "40°C");
    }
}
