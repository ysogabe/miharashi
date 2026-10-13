//! 写真の取得元(GEOFRAME か単一ファイル)、並び順、デコードを担当する。
//! `Feed` は別スレッドで取得・縮小するので、描画ループをブロックしない。
use chrono::{Datelike, NaiveDate};
use miharashi_render::{Pixmap, photo, photo_layout::PhotoLayout, screens::PhotoInfo};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

pub enum Source {
    Geoframe(String),
    /// (ファイル, 表示名)。表示名がなければファイル名を使う。
    File(PathBuf, Option<String>),
}

enum Origin {
    Url(String),
    File(PathBuf),
}

struct Entry {
    origin: Origin,
    place: String,
    taken: String,
}

pub struct PhotoItem {
    /// 背景と写真を合成した、画面いっぱいの完成フレーム
    pub frame: Pixmap,
    pub layout: PhotoLayout,
    pub info: PhotoInfo,
}

pub struct Catalog {
    entries: Vec<Entry>,
    order: Vec<usize>,
    pos: usize,
    size: (u32, u32),
    /// 写真の配置を巡回させる次の候補(左 → 右 → 中央)
    slot: usize,
}

/// 撮影日時の文字列("2026-03-08 13:22" など)を日付と時刻に分ける。
fn parse_taken(s: &str) -> Option<(NaiveDate, Option<(u32, u32)>)> {
    let date = NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()?;
    let time = s.get(11..16).and_then(|t| {
        let (h, m) = t.split_once(':')?;
        Some((
            h.parse().ok().filter(|h| *h < 24)?,
            m.parse().ok().filter(|m| *m < 60)?,
        ))
    });
    Some((date, time))
}

/// 撮影日からの経過を「今日」「3日前」「7か月前」「2年前」の形にする。
pub fn relative_age(taken: NaiveDate, today: NaiveDate) -> String {
    let days = (today - taken).num_days();
    if days <= 0 {
        return "今日".into();
    }
    let mut months =
        (today.year() - taken.year()) * 12 + today.month() as i32 - taken.month() as i32;
    if today.day() < taken.day() {
        months -= 1;
    }
    match days {
        1 => "昨日".into(),
        2..=6 => format!("{days}日前"),
        7..=29 => format!("{}週間前", days / 7),
        _ if months < 1 => format!("{days}日前"),
        _ if months < 12 => format!("{months}か月前"),
        _ => format!("{}年前", months / 12),
    }
}

/// 表示用の (撮影日時, 経過)。撮影日時が読めなければ空文字。
fn taken_labels(raw: &str, today: NaiveDate) -> (String, String) {
    let Some((date, time)) = parse_taken(raw) else {
        return (String::new(), String::new());
    };
    let mut taken = format!("撮影 {}", date.format("%Y/%m/%d"));
    if let Some((h, m)) = time {
        taken.push_str(&format!(" {h:02}:{m:02}"));
    }
    (taken, relative_age(date, today))
}

fn read_all(req: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
    let mut req = req;
    req.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

impl Catalog {
    pub fn open(source: &Source, size: (u32, u32)) -> Result<Self, String> {
        let (entries, preferred) = match source {
            Source::File(p, caption) => {
                let name = p
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let e = Entry {
                    origin: Origin::File(p.clone()),
                    place: caption.clone().unwrap_or(name),
                    taken: String::new(),
                };
                (vec![e], vec![0])
            }
            Source::Geoframe(base) => Self::list_geoframe(base)?,
        };
        if entries.is_empty() {
            return Err("写真が 1 枚もない".into());
        }
        // 推奨・お気に入りが 5 枚以上あればそれらだけ、なければ全件を対象にする。
        let mut order = if preferred.len() >= 5 {
            preferred
        } else {
            (0..entries.len()).collect()
        };
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e3779b97f4a7c15, |d| d.as_nanos() as u64)
            | 1;
        for i in (1..order.len()).rev() {
            order.swap(i, (xorshift(&mut seed) % (i as u64 + 1)) as usize);
        }
        Ok(Self {
            entries,
            order,
            pos: 0,
            size,
            slot: 0,
        })
    }

    fn list_geoframe(base: &str) -> Result<(Vec<Entry>, Vec<usize>), String> {
        let base = base.trim_end_matches('/');
        let body = read_all(
            ureq::get(&format!("{base}/api/photos"))
                .call()
                .map_err(|e| format!("GEOFRAME: {e}"))?,
        )?;
        let list: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let arr = list.as_array().ok_or("unexpected /api/photos")?;
        let (mut entries, mut preferred) = (Vec::new(), Vec::new());
        for p in arr {
            let Some(file) = p["file"].as_str() else {
                continue;
            };
            if p["quality_recommended"] == true || p["fav"] == true {
                preferred.push(entries.len());
            }
            let place = format!(
                "{} {}",
                p["pref"].as_str().unwrap_or(""),
                p["title"].as_str().unwrap_or("")
            );
            entries.push(Entry {
                origin: Origin::Url(format!("{base}/{file}")),
                place: place.trim().to_string(),
                taken: p["time"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok((entries, preferred))
    }

    pub fn advance(&mut self, delta: isize) {
        let n = self.order.len() as isize;
        self.pos = (self.pos as isize + delta).rem_euclid(n) as usize;
    }

    pub fn current(&mut self) -> Result<PhotoItem, String> {
        let e = &self.entries[self.order[self.pos]];
        let bytes = match &e.origin {
            Origin::Url(u) => read_all(ureq::get(u).call().map_err(|err| format!("{u}: {err}"))?)?,
            Origin::File(p) => std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))?,
        };
        let (taken, age) = taken_labels(&e.taken, chrono::Local::now().date_naive());
        let info = PhotoInfo {
            place: e.place.clone(),
            taken,
            age,
            position: (self.pos + 1, self.order.len()),
        };
        // 背景と写真の合成は、新しい写真ごとにここ(取得スレッド)で 1 回だけ行う。
        let (frame, layout) = photo::compose(&bytes, self.size.0, self.size.1, &mut self.slot)?;
        Ok(PhotoItem {
            frame,
            layout,
            info,
        })
    }
}

/// 指数バックオフ(min から倍々で max まで)。成功したら `reset`。
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    next: Duration,
    attempts: u32,
}

impl Backoff {
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            next: min,
            attempts: 0,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        self.attempts += 1;
        d
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn reset(&mut self) {
        self.next = self.min;
        self.attempts = 0;
    }
}

enum Cmd {
    Next,
    Prev,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedStatus {
    Online,
    /// 取得に失敗し、`retry_in` 後に再試行する。最後に取得した写真は表示し続ける。
    Retrying {
        attempt: u32,
        retry_in: Duration,
        error: String,
    },
}

pub enum FeedEvent {
    Photo(PhotoItem),
    Status(FeedStatus),
}

pub struct Feed {
    tx: Sender<Cmd>,
    rx: Receiver<FeedEvent>,
}

impl Feed {
    /// 一覧の取得と最初の 1 枚の読み込みも別スレッドで行う。GEOFRAME が止まっても、
    /// `min`..`max` の指数バックオフで再接続を続ける(描画ループには影響しない)。
    pub fn spawn(source: Source, size: (u32, u32), min: Duration, max: Duration) -> Self {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, rx) = channel();
        std::thread::spawn(move || {
            let mut backoff = Backoff::new(min, max);
            let mut catalog: Option<Catalog> = None;
            let mut delta = 1isize;
            let mut offline = false;
            let mut photo_failures = 0u32;
            loop {
                let loaded = (|| {
                    if catalog.is_none() {
                        catalog = Some(Catalog::open(&source, size)?);
                    }
                    catalog
                        .as_mut()
                        .map_or_else(|| Err("catalog".to_string()), Catalog::current)
                })();
                match loaded {
                    Ok(item) => {
                        if offline && ev_tx.send(FeedEvent::Status(FeedStatus::Online)).is_err() {
                            return;
                        }
                        offline = false;
                        backoff.reset();
                        photo_failures = 0;
                        if ev_tx.send(FeedEvent::Photo(item)).is_err() {
                            return;
                        }
                        match cmd_rx.recv() {
                            Ok(Cmd::Next) => delta = 1,
                            Ok(Cmd::Prev) => delta = -1,
                            Err(_) => return,
                        }
                        if let Some(c) = catalog.as_mut() {
                            c.advance(delta);
                        }
                    }
                    Err(error) => {
                        photo_failures += 1;
                        // 一覧は取れているのに写真だけ失敗したときは、2 回までは別の写真を試す。
                        // それでも続くなら、一覧ごと取り直す(サーバー再起動後に一覧が変わりうる)。
                        if let Some(c) = catalog.as_mut() {
                            if photo_failures < 3 {
                                c.advance(delta);
                                continue;
                            }
                            catalog = None;
                        }
                        let wait = backoff.next_delay();
                        offline = true;
                        let status = FeedStatus::Retrying {
                            attempt: backoff.attempts(),
                            retry_in: wait,
                            error,
                        };
                        if ev_tx.send(FeedEvent::Status(status)).is_err() {
                            return;
                        }
                        // 待機中でも、キー操作(次/前)が来たら即座に再試行する。
                        match cmd_rx.recv_timeout(wait) {
                            Ok(Cmd::Next) => delta = 1,
                            Ok(Cmd::Prev) => delta = -1,
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
            }
        });
        Self { tx, rx }
    }

    pub fn next(&self) {
        let _ = self.tx.send(Cmd::Next);
    }

    pub fn prev(&self) {
        let _ = self.tx.send(Cmd::Prev);
    }

    pub fn try_recv(&self) -> Option<FeedEvent> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(64, 48, image::Rgba([10, 120, 200, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// GEOFRAME の最小モック。/api/photos と /derivatives/a.png だけを返す。
    fn mock_server(port: u16) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let png = png_bytes();
        let h = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let Ok((mut s, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (ctype, body): (&str, Vec<u8>) = if req.starts_with("GET /api/photos") {
                    ("application/json", br#"[{"file":"derivatives/a.png","title":"T","pref":"P","time":"2026-01-01 00:00"}]"#.to_vec())
                } else {
                    ("image/png", png.clone())
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(&body);
            }
        });
        (stop, h)
    }

    fn free_port() -> u16 {
        TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn wait_event(feed: &Feed, secs: u64, want: impl Fn(&FeedEvent) -> bool) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < end {
            if let Some(ev) = feed.try_recv()
                && want(&ev)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn write_png(path: &std::path::Path, rgb: [u8; 3]) {
        let img = image::RgbaImage::from_pixel(64, 48, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
        img.save(path).unwrap();
    }

    #[test]
    fn catalog_next_prev_wrap_and_report_position() {
        let dir = std::env::temp_dir().join(format!("miharashi-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let colors = [[200, 0, 0], [0, 200, 0], [0, 0, 200]];
        let entries: Vec<Entry> = colors
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let p = dir.join(format!("{i}.png"));
                write_png(&p, *c);
                Entry {
                    origin: Origin::File(p),
                    place: format!("p{i}"),
                    taken: String::new(),
                }
            })
            .collect();
        let mut cat = Catalog {
            entries,
            order: vec![0, 1, 2],
            pos: 0,
            size: (32, 18),
            slot: 0,
        };
        let red = |c: &mut Catalog| c.current().unwrap().frame.data()[0];
        assert_eq!(
            (red(&mut cat), cat.current().unwrap().info.position),
            (200, (1, 3))
        );
        cat.advance(1);
        assert_eq!(cat.current().unwrap().info.place, "p1");
        cat.advance(1);
        cat.advance(1);
        assert_eq!(
            cat.current().unwrap().info.position,
            (1, 3),
            "末尾の次は先頭へ戻る"
        );
        cat.advance(-1);
        assert_eq!(cat.current().unwrap().info.place, "p2", "先頭の前は末尾へ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn relative_age_labels() {
        let today = d(2026, 10, 12);
        let age = |t: NaiveDate| relative_age(t, today);
        assert_eq!(age(d(2026, 10, 12)), "今日");
        assert_eq!(age(d(2026, 10, 11)), "昨日");
        assert_eq!(age(d(2026, 10, 9)), "3日前");
        assert_eq!(age(d(2026, 10, 1)), "1週間前");
        assert_eq!(age(d(2026, 3, 8)), "7か月前");
        assert_eq!(age(d(2026, 9, 12)), "1か月前");
        assert_eq!(age(d(2026, 9, 13)), "4週間前");
        assert_eq!(age(d(2024, 10, 12)), "2年前");
        assert_eq!(age(d(2025, 10, 13)), "11か月前");
        assert_eq!(age(d(2025, 10, 12)), "1年前");
        assert_eq!(age(d(2026, 12, 1)), "今日", "未来の日付でも落ちない");
    }

    #[test]
    fn taken_labels_parse_and_omit_when_missing() {
        let today = d(2026, 10, 12);
        assert_eq!(
            taken_labels("2026-03-08 13:22", today),
            ("撮影 2026/03/08 13:22".to_string(), "7か月前".to_string())
        );
        assert_eq!(
            taken_labels("2026-03-08", today).0,
            "撮影 2026/03/08",
            "時刻なし"
        );
        assert_eq!(taken_labels("", today), (String::new(), String::new()));
        assert_eq!(taken_labels("garbage-text", today).0, "");
    }

    #[test]
    fn backoff_doubles_up_to_max_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(8));
        let seq: Vec<u64> = (0..6).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(seq, [1, 2, 4, 8, 8, 8]);
        assert_eq!(b.attempts(), 6);
        b.reset();
        assert_eq!(b.next_delay().as_secs(), 1);
    }

    #[test]
    fn feed_recovers_after_geoframe_stops_and_restarts() {
        let port = free_port();
        let (stop, h) = mock_server(port);
        let url = format!("http://127.0.0.1:{port}");
        let feed = Feed::spawn(
            Source::Geoframe(url),
            (64, 36),
            Duration::from_millis(50),
            Duration::from_millis(200),
        );
        assert!(
            wait_event(&feed, 5, |e| matches!(e, FeedEvent::Photo(_))),
            "最初の写真"
        );

        // GEOFRAME 停止 → 取得に失敗して再試行状態になる。
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
        feed.next();
        assert!(
            wait_event(&feed, 5, |e| matches!(
                e,
                FeedEvent::Status(FeedStatus::Retrying { .. })
            )),
            "停止を検出して再試行"
        );

        // 再開 → 自動的に写真取得が回復する(キー操作なし)。
        let (stop2, h2) = mock_server(port);
        assert!(
            wait_event(&feed, 10, |e| matches!(e, FeedEvent::Photo(_))),
            "再開後に写真が回復"
        );
        stop2.store(true, Ordering::Relaxed);
        h2.join().unwrap();
    }

    #[test]
    fn feed_starts_offline_then_connects_when_server_appears() {
        let port = free_port();
        let url = format!("http://127.0.0.1:{port}");
        let feed = Feed::spawn(
            Source::Geoframe(url),
            (64, 36),
            Duration::from_millis(50),
            Duration::from_millis(200),
        );
        assert!(wait_event(&feed, 5, |e| matches!(
            e,
            FeedEvent::Status(FeedStatus::Retrying { .. })
        )));
        let (stop, h) = mock_server(port);
        assert!(wait_event(&feed, 10, |e| matches!(e, FeedEvent::Photo(_))));
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
    }
}
