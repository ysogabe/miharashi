//! 写真の取得元(GEOFRAME か単一ファイル)、並び順、デコードを担当する。
//! `Feed` は別スレッドで取得・縮小するので、描画ループをブロックしない。
use miharashi_render::{photo, screens::PhotoInfo, Pixmap};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

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
    pub pix: Pixmap,
    pub info: PhotoInfo,
}

pub struct Catalog {
    entries: Vec<Entry>,
    order: Vec<usize>,
    pos: usize,
    size: (u32, u32),
}

fn read_all(req: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
    let mut req = req;
    req.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
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
                let name = p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let e = Entry { origin: Origin::File(p.clone()), place: caption.clone().unwrap_or(name), taken: String::new() };
                (vec![e], vec![0])
            }
            Source::Geoframe(base) => Self::list_geoframe(base)?,
        };
        if entries.is_empty() {
            return Err("写真が 1 枚もない".into());
        }
        // 推奨・お気に入りが 5 枚以上あればそれらだけ、なければ全件を対象にする。
        let mut order = if preferred.len() >= 5 { preferred } else { (0..entries.len()).collect() };
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e3779b97f4a7c15, |d| d.as_nanos() as u64)
            | 1;
        for i in (1..order.len()).rev() {
            order.swap(i, (xorshift(&mut seed) % (i as u64 + 1)) as usize);
        }
        Ok(Self { entries, order, pos: 0, size })
    }

    fn list_geoframe(base: &str) -> Result<(Vec<Entry>, Vec<usize>), String> {
        let base = base.trim_end_matches('/');
        let body = read_all(ureq::get(&format!("{base}/api/photos")).call().map_err(|e| format!("GEOFRAME: {e}"))?)?;
        let list: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let arr = list.as_array().ok_or("unexpected /api/photos")?;
        let (mut entries, mut preferred) = (Vec::new(), Vec::new());
        for p in arr {
            let Some(file) = p["file"].as_str() else { continue };
            if p["quality_recommended"] == true || p["fav"] == true {
                preferred.push(entries.len());
            }
            let place = format!("{} {}", p["pref"].as_str().unwrap_or(""), p["title"].as_str().unwrap_or(""));
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

    pub fn current(&self) -> Result<PhotoItem, String> {
        let e = &self.entries[self.order[self.pos]];
        let bytes = match &e.origin {
            Origin::Url(u) => read_all(ureq::get(u).call().map_err(|err| format!("{u}: {err}"))?)?,
            Origin::File(p) => std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))?,
        };
        let pix = photo::load_cover(&bytes, self.size.0, self.size.1)?;
        Ok(PhotoItem {
            pix,
            info: PhotoInfo { place: e.place.clone(), taken: e.taken.clone(), position: (self.pos + 1, self.order.len()) },
        })
    }
}

enum Cmd {
    Next,
    Prev,
}

pub struct Feed {
    tx: Sender<Cmd>,
    rx: Receiver<Result<PhotoItem, String>>,
}

impl Feed {
    /// 一覧の取得と最初の 1 枚の読み込みも別スレッドで行う。
    pub fn spawn(source: Source, size: (u32, u32)) -> Self {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (res_tx, rx) = channel();
        std::thread::spawn(move || {
            let mut catalog = match Catalog::open(&source, size) {
                Ok(c) => c,
                Err(e) => {
                    let _ = res_tx.send(Err(e));
                    return;
                }
            };
            let mut delta = 0isize;
            loop {
                // 取得に失敗した写真は飛ばす(最大 5 枚)。
                let mut result = catalog.current();
                for _ in 0..5 {
                    if result.is_ok() {
                        break;
                    }
                    catalog.advance(if delta < 0 { -1 } else { 1 });
                    result = catalog.current();
                }
                if res_tx.send(result).is_err() {
                    return;
                }
                match cmd_rx.recv() {
                    Ok(Cmd::Next) => delta = 1,
                    Ok(Cmd::Prev) => delta = -1,
                    Err(_) => return,
                }
                catalog.advance(delta);
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

    pub fn try_recv(&self) -> Option<Result<PhotoItem, String>> {
        self.rx.try_recv().ok()
    }
}
