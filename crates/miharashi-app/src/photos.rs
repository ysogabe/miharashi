//! 写真の取得元(GEOFRAME か単一ファイル)、並び順、デコードを担当する。
//! `Feed` は別スレッドで取得・縮小するので、描画ループをブロックしない。
use crate::screen::{PhotoData, PhotoRef};
use chrono::{Datelike, NaiveDate};
use image::DynamicImage;
use miharashi_render::photo_layout::{self, PhotoLayout};
use miharashi_render::{Pixmap, photo, screens::PhotoInfo};
use crate::album::{self, Group, Meta, Scheduler, Slot};
use miharashi_render::trip_card::TripCard;
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

/// 写真の画素。CPU 経路は取得スレッドで完成フレームまで作り、GPU 経路(PoC-005 PR3b)は
/// デコードしただけの写真を渡して、縮小・ぼかし背景・配置を GPU で行う。
pub enum PhotoPixels {
    /// 背景と写真を合成した、画面いっぱいの完成フレーム
    Frame(Pixmap),
    /// デコードしただけの写真(RGB8 か RGBA8)
    Source(DynamicImage),
    /// GPU に渡し終えた(CPU 側の画素は捨てた)
    Resident,
}

pub struct PhotoItem {
    pub pixels: PhotoPixels,
    pub layout: PhotoLayout,
    pub info: PhotoInfo,
    /// GPU 経路で一度 GPU に渡したか(次の描画からは `Resident` にする)
    pub sent: bool,
}

impl PhotoItem {
    /// CPU 経路用に、デコードしただけの写真なら完成フレームを作って置き換える。
    pub fn ensure_frame(&mut self, (w, h): (u32, u32)) {
        if let PhotoPixels::Source(img) = &self.pixels {
            match photo::compose_image(img, w, h, &self.layout) {
                Ok(f) => self.pixels = PhotoPixels::Frame(f),
                Err(e) => eprintln!("photo: {e}"),
            }
        }
    }

    /// GPU の合成器に渡す形(`id` は写真の通し番号)。
    pub fn photo_ref(&self, id: u64) -> PhotoRef<'_> {
        PhotoRef {
            id,
            data: match &self.pixels {
                PhotoPixels::Frame(f) => PhotoData::Frame(f),
                PhotoPixels::Source(image) => PhotoData::Source {
                    image,
                    layout: &self.layout,
                },
                PhotoPixels::Resident => PhotoData::Resident,
            },
        }
    }

    /// 完成フレーム(CPU 経路)。まだ作っていないか、GPU に渡し終えていれば None。
    pub fn frame(&self) -> Option<&Pixmap> {
        match &self.pixels {
            PhotoPixels::Frame(f) => Some(f),
            _ => None,
        }
    }
}

pub struct Catalog {
    entries: Vec<Entry>,
    /// 写真ごとの、並べ方に使う情報(GEOFRAME のときだけ。単一ファイルでは空)
    metas: Vec<Meta>,
    groups: Vec<Group>,
    /// 旅・季節ごとに並べる(GEOFRAME のとき)。None なら `queue` を先頭から繰り返す
    sched: Option<Scheduler>,
    queue: Vec<Slot>,
    pos: usize,
    size: (u32, u32),
    /// 写真の配置を巡回させる次の候補(左 → 右 → 中央)
    slot: usize,
    /// GPU 経路なら、GPU で扱える写真の最大の幅と高さ。None なら CPU で完成フレームまで作る
    pub gpu_max: Option<u32>,
}

/// 次に見せるもの。
pub enum Shown {
    Photo(PhotoItem),
    /// 旅・季節の切り替わりのカード
    Card(TripCard),
}

/// 写真を旅と季節に分けるための設定。
#[derive(Debug, Clone)]
pub struct Grouping {
    /// 自宅の (緯度, 経度)。分からなければ None(GEOFRAME の旅番号を使う)
    pub home: Option<(f64, f64)>,
    pub radius_km: f64,
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

impl Catalog {
    pub fn open(source: &Source, size: (u32, u32), grouping: &Grouping) -> Result<Self, String> {
        let (entries, metas) = match source {
            Source::File(p, caption) => {
                let name = p
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let e = Entry {
                    origin: Origin::File(p.clone()),
                    place: caption.clone().unwrap_or(name),
                    taken: String::new(),
                };
                (vec![e], Vec::new())
            }
            Source::Geoframe(base) => Self::list_geoframe(base)?,
        };
        if entries.is_empty() {
            return Err("写真が 1 枚もない".into());
        }
        let mut cat = Self {
            entries,
            metas,
            groups: Vec::new(),
            sched: None,
            queue: Vec::new(),
            pos: 0,
            size,
            slot: 0,
            gpu_max: None,
        };
        if cat.metas.is_empty() {
            let n = cat.entries.len();
            cat.queue = (0..n)
                .map(|entry| Slot::Photo {
                    entry,
                    k: entry + 1,
                    n,
                })
                .collect();
            return Ok(cat);
        }
        // 海外の写真は地名が空なので、国と都市を引いて写真の説明にも使う
        let overseas = album::overseas_places(&cat.metas);
        for (i, (country, city)) in &overseas {
            if let Some(e) = cat.entries.get_mut(*i) {
                e.place = format!("{country} {city}").trim().to_string();
            }
        }
        cat.groups = album::build_groups(&cat.metas, &overseas, grouping.home, grouping.radius_km);
        let trips = cat.groups.iter().filter(|g| !g.everyday).count();
        eprintln!(
            "photos: 対象 {} 枚 / 旅 {} / 暮らし {} / 自宅={}",
            cat.entries.len(),
            trips,
            cat.groups.len() - trips,
            if grouping.home.is_some() {
                "HA"
            } else {
                "不明(GEOFRAME の旅番号を使う)"
            }
        );
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e3779b97f4a7c15, |d| d.as_nanos() as u64);
        let mut sched = Scheduler::new(seed);
        cat.queue = sched.next_round(&cat.groups, &cat.metas, chrono::Local::now().naive_local());
        cat.sched = Some(sched);
        Ok(cat)
    }

    /// GEOFRAME の一覧から、表示対象の写真と並べ方の情報を読む。
    fn list_geoframe(base: &str) -> Result<(Vec<Entry>, Vec<Meta>), String> {
        let base = base.trim_end_matches('/');
        let body = read_all(
            ureq::get(&format!("{base}/api/v1/photos"))
                .call()
                .map_err(|e| format!("GEOFRAME: {e}"))?,
        )?;
        let list: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let arr = list.as_array().ok_or("unexpected /api/v1/photos")?;
        let (mut entries, mut metas) = (Vec::new(), Vec::new());
        let text = |p: &serde_json::Value, k: &str| p[k].as_str().unwrap_or("").to_string();
        for p in arr {
            let Some(file) = p["file"].as_str() else {
                continue;
            };
            let fav = p["fav"] == true;
            if !album::eligible(
                p["quality_kind"].as_str().unwrap_or(""),
                p["quality_recommended"] == true,
                fav,
            ) {
                continue;
            }
            let place = format!("{} {}", text(p, "pref"), text(p, "title"));
            entries.push(Entry {
                origin: Origin::Url(format!("{base}/{file}")),
                place: place.trim().to_string(),
                taken: text(p, "time"),
            });
            metas.push(Meta {
                lat: p["lat"].as_f64(),
                lon: p["lon"].as_f64(),
                time: text(p, "time"),
                pref: text(p, "pref"),
                city: text(p, "city"),
                fav,
                fetched_at: text(p, "fetched_at"),
                trip: p["trip"].as_i64(),
            });
        }
        Ok((entries, metas))
    }

    pub fn advance(&mut self, delta: isize) {
        let next = self.pos as isize + delta;
        let Some(sched) = self.sched.as_mut() else {
            self.pos = next.rem_euclid(self.queue.len() as isize) as usize;
            return;
        };
        if next < 0 {
            return; // 最初より前へは戻らない
        }
        self.pos = next as usize;
        if self.pos >= self.queue.len() {
            let round = sched.next_round(&self.groups, &self.metas, chrono::Local::now().naive_local());
            self.queue.extend(round);
        }
        // 戻れる範囲を残して古いコマを捨てる(増え続けないように)
        const KEEP_BACK: usize = 200;
        if self.pos > 4000 {
            let drop = self.pos - KEEP_BACK;
            self.queue.drain(..drop);
            self.pos -= drop;
        }
    }

    pub fn current(&mut self) -> Result<Shown, String> {
        let (index, k, n) = match self.queue.get(self.pos).copied() {
            Some(Slot::Card(g)) => {
                let card = self.groups.get(g).ok_or("まとまりがない")?.card.clone();
                return Ok(Shown::Card(card));
            }
            Some(Slot::Photo { entry, k, n }) => (entry, k, n),
            None => return Err("再生順が空".into()),
        };
        if let Some(s) = self.sched.as_mut() {
            s.mark(index);
        }
        let e = &self.entries[index];
        let bytes = match &e.origin {
            Origin::Url(u) => read_all(ureq::get(u).call().map_err(|err| format!("{u}: {err}"))?)?,
            Origin::File(p) => std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))?,
        };
        let (taken, age) = taken_labels(&e.taken, chrono::Local::now().date_naive());
        let info = PhotoInfo {
            place: e.place.clone(),
            taken,
            age,
            position: (k, n),
        };
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let (w, h) = self.size;
        let layout = photo_layout::plan(&mut self.slot, w, h, img.width(), img.height());
        let pixels = match self.gpu_max {
            // GPU 経路: デコードだけして、縮小と合成は GPU に任せる
            Some(max) if img.width().max(img.height()) <= max => {
                PhotoPixels::Source(match img {
                    DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => img,
                    other if other.color().has_alpha() => {
                        DynamicImage::ImageRgba8(other.to_rgba8())
                    }
                    other => DynamicImage::ImageRgb8(other.to_rgb8()),
                })
            }
            // 背景と写真の合成は、新しい写真ごとにここ(取得スレッド)で 1 回だけ行う。
            _ => PhotoPixels::Frame(photo::compose_image(&img, w, h, &layout)?),
        };
        Ok(Shown::Photo(PhotoItem {
            pixels,
            layout,
            info,
            sent: false,
        }))
    }

    /// 次の写真(カードは飛ばす)。`png` の出力用。
    pub fn current_photo(&mut self) -> Result<PhotoItem, String> {
        for _ in 0..64 {
            if let Shown::Photo(p) = self.current()? {
                return Ok(p);
            }
            self.advance(1);
        }
        Err("写真が見つからない".into())
    }

    /// まとまりの一覧(`png` の確認用)。
    pub fn group_summary(&self) -> String {
        self.groups
            .iter()
            .map(|g| {
                format!(
                    "{}\t{}\t{}\t{}枚\t{}\n",
                    if g.everyday { "暮らし" } else { "旅" },
                    g.card.title,
                    g.card.period,
                    g.members.len(),
                    g.card.places.join("・")
                )
            })
            .collect()
    }

    /// いちばん大きい旅(`everyday` なら季節)のカード。`png` の出力用。
    pub fn sample_card(&self, everyday: bool) -> Option<TripCard> {
        self.groups
            .iter()
            .filter(|g| g.everyday == everyday && g.members.len() >= album::CARD_MIN)
            .max_by_key(|g| g.members.len())
            .map(|g| g.card.clone())
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
    /// 旅・季節の切り替わりのカード
    Card(TripCard),
    Status(FeedStatus),
}

/// 自宅の位置の調べ方(Home Assistant)。写真を旅と季節に分けるのに使う。
#[derive(Debug, Clone)]
pub struct HomeLookup {
    pub ha: crate::config::HomeAssistant,
    pub radius_km: f64,
}

impl HomeLookup {
    /// 自宅の位置。HA が未設定か、取れなければ None。
    pub fn resolve(&self) -> Option<(f64, f64)> {
        if self.ha.url.is_empty() {
            return None;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        match crate::quake::home_location(&agent, &self.ha) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("photos: 自宅の位置を取れない(GEOFRAME の旅番号でまとめる): {e}");
                None
            }
        }
    }
}

pub struct Feed {
    tx: Sender<Cmd>,
    rx: Receiver<FeedEvent>,
}

impl Feed {
    /// 一覧の取得と最初の 1 枚の読み込みも別スレッドで行う。GEOFRAME が止まっても、
    /// `min`..`max` の指数バックオフで再接続を続ける(描画ループには影響しない)。
    /// `gpu_max` は `Catalog::gpu_max` と同じ(GPU 経路なら Some)。
    /// `home` があれば、一覧を取り直すたびに自宅の位置を調べ(取れたら覚えておく)、旅と季節に分ける。
    pub fn spawn(
        source: Source,
        size: (u32, u32),
        gpu_max: Option<u32>,
        min: Duration,
        max: Duration,
        home: Option<HomeLookup>,
    ) -> Self {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, rx) = channel();
        let spawned = std::thread::Builder::new().name("photos".into()).spawn(move || {
            let mut grouping = Grouping {
                home: None,
                radius_km: home.as_ref().map_or(50.0, |h| h.radius_km),
            };
            let mut backoff = Backoff::new(min, max);
            let mut catalog: Option<Catalog> = None;
            let mut delta = 1isize;
            let mut offline = false;
            let mut photo_failures = 0u32;
            loop {
                let loaded = (|| {
                    if catalog.is_none() {
                        if grouping.home.is_none() && matches!(source, Source::Geoframe(_)) {
                            grouping.home = home.as_ref().and_then(HomeLookup::resolve);
                        }
                        let mut c = Catalog::open(&source, size, &grouping)?;
                        c.gpu_max = gpu_max;
                        catalog = Some(c);
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
                        let ev = match item {
                            Shown::Photo(p) => FeedEvent::Photo(p),
                            Shown::Card(c) => FeedEvent::Card(c),
                        };
                        if ev_tx.send(ev).is_err() {
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
        if let Err(e) = spawned {
            eprintln!("photos: スレッドを起動できない: {e}");
        }
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

    /// GEOFRAME の最小モック。/api/v1/photos と /derivatives/a.png だけを返す。
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
                let (ctype, body): (&str, Vec<u8>) = if req.starts_with("GET /api/v1/photos") {
                    ("application/json", br#"[{"file":"derivatives/a.png","title":"T","pref":"P","time":"2026-01-01 00:00","lat":35.0,"lon":139.0,"trip":0}]"#.to_vec())
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
            metas: Vec::new(),
            groups: Vec::new(),
            sched: None,
            queue: (0..3)
                .map(|entry| Slot::Photo {
                    entry,
                    k: entry + 1,
                    n: 3,
                })
                .collect(),
            pos: 0,
            size: (32, 18),
            slot: 0,
            gpu_max: None,
        };
        let red = |c: &mut Catalog| c.current_photo().unwrap().frame().unwrap().data()[0];
        assert_eq!(
            (red(&mut cat), cat.current_photo().unwrap().info.position),
            (200, (1, 3))
        );
        cat.advance(1);
        assert_eq!(cat.current_photo().unwrap().info.place, "p1");
        cat.advance(1);
        cat.advance(1);
        assert_eq!(
            cat.current_photo().unwrap().info.position,
            (1, 3),
            "末尾の次は先頭へ戻る"
        );
        cat.advance(-1);
        assert_eq!(cat.current_photo().unwrap().info.place, "p2", "先頭の前は末尾へ");
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
            None,
            Duration::from_millis(50),
            Duration::from_millis(200),
            None,
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
            None,
            Duration::from_millis(50),
            Duration::from_millis(200),
            None,
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
