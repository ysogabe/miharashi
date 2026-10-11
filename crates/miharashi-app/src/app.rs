//! 画面ロジック(PC 状況 + フォトフレーム)。実行ループ(runtime)からは `App` として見える。
use crate::config::Config;
use crate::photos::{Feed, FeedEvent, FeedStatus, PhotoItem, Source};
use miharashi_render::{Canvas, Pixmap, TextEngine, screens};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Prev,
    Switch,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Status,
    Photo,
}

/// データ収集(実測値)の抽象。実行ループも画面も具体的な収集方法を知らない。
pub trait DataSource {
    /// 1 秒ごとなど、更新の時期に呼ぶ。
    fn update(&mut self, snap: &mut screens::Snapshot);
}

pub struct KioskApp {
    screen: Screen,
    snap: screens::Snapshot,
    data: Box<dyn DataSource>,
    feed: Feed,
    current: Option<PhotoItem>,
    fade: Option<(Pixmap, Instant)>,
    notice: Option<String>,
    last_photo_change: Instant,
    photo_interval: Duration,
    fade_len: Duration,
    next_tick: Instant,
    tick_len: Duration,
    text: TextEngine,
    pub ticks: u64,
    pub keys: u64,
}

pub fn text_engine(font: &str) -> TextEngine {
    let path = std::path::Path::new(font);
    if !font.is_empty() && path.exists() {
        match TextEngine::from_font_file("Noto Sans CJK JP", path) {
            Ok(t) => return t,
            Err(e) => eprintln!("font {font}: {e}。システムフォントを使う"),
        }
    }
    TextEngine::new("Noto Sans CJK JP")
}

pub fn set_clock(s: &mut screens::Snapshot) {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let wd =
        ["月", "火", "水", "木", "金", "土", "日"][now.weekday().num_days_from_monday() as usize];
    s.date = format!("{} ({wd})", now.format("%Y/%m/%d"));
    s.time = now.format("%H:%M:%S").to_string();
}

fn describe(status: &FeedStatus) -> Option<String> {
    match status {
        FeedStatus::Online => None,
        FeedStatus::Retrying { retry_in, .. } => Some(format!(
            "GEOFRAME に接続できません({}秒後に再試行)",
            retry_in.as_secs().max(1)
        )),
    }
}

impl KioskApp {
    pub fn new(
        cfg: &Config,
        size: (u32, u32),
        source: Source,
        data: Box<dyn DataSource>,
        snap: screens::Snapshot,
    ) -> Self {
        let g = &cfg.geoframe;
        let feed = Feed::spawn(
            source,
            size,
            Duration::from_secs(g.reconnect_min_secs),
            Duration::from_secs(g.reconnect_max_secs),
        );
        Self {
            screen: if cfg.ui.start_screen == "photo" {
                Screen::Photo
            } else {
                Screen::Status
            },
            snap,
            data,
            feed,
            current: None,
            fade: None,
            notice: None,
            last_photo_change: Instant::now(),
            photo_interval: Duration::from_secs(g.photo_interval_secs),
            fade_len: Duration::from_millis(800),
            next_tick: Instant::now(),
            tick_len: Duration::from_secs(1),
            text: text_engine(&cfg.ui.font),
            ticks: 0,
            keys: 0,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn photo_position(&self) -> usize {
        self.current.as_ref().map_or(0, |p| p.info.position.0)
    }

    pub fn feed_notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn snapshot(&self) -> &screens::Snapshot {
        &self.snap
    }

    /// 戻り値: 再描画が必要か。
    pub fn on_key(&mut self, key: Key) -> bool {
        self.keys += 1;
        match (key, self.screen) {
            (Key::Switch, Screen::Status) | (Key::Next, Screen::Status) => {
                self.screen = Screen::Photo
            }
            (Key::Switch, Screen::Photo) => self.screen = Screen::Status,
            (Key::Next, Screen::Photo) => {
                self.feed.next();
                self.last_photo_change = Instant::now();
            }
            (Key::Prev, Screen::Photo) => {
                self.feed.prev();
                self.last_photo_change = Instant::now();
            }
            (Key::Prev, Screen::Status) => {}
        }
        true
    }

    /// 実行ループから毎周期呼ぶ。戻り値: 再描画が必要か。
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        while let Some(ev) = self.feed.try_recv() {
            match ev {
                FeedEvent::Photo(item) => {
                    if let Some(old) = self.current.take() {
                        self.fade = Some((old.pix, now));
                    }
                    self.current = Some(item);
                    self.last_photo_change = now;
                    redraw = true;
                }
                FeedEvent::Status(s) => {
                    self.notice = describe(&s);
                    redraw = true;
                }
            }
        }
        if self.screen == Screen::Photo
            && now.duration_since(self.last_photo_change) >= self.photo_interval
        {
            self.feed.next();
            self.last_photo_change = now;
        }
        if self
            .fade
            .as_ref()
            .is_some_and(|(_, t0)| now.duration_since(*t0) >= self.fade_len)
        {
            self.fade = None;
        }
        if now >= self.next_tick {
            self.data.update(&mut self.snap);
            set_clock(&mut self.snap);
            self.ticks += 1;
            self.next_tick += self.tick_len;
            if self.next_tick < now {
                self.next_tick = now + self.tick_len;
            }
            redraw = true;
        }
        redraw || (self.screen == Screen::Photo && self.fade.is_some())
    }

    pub fn render(&mut self, canvas: &mut Canvas) {
        match self.screen {
            Screen::Status => screens::status(canvas, &mut self.text, &self.snap),
            Screen::Photo => {
                let now = Instant::now();
                let fade = self.fade.as_ref().map(|(old, t0)| {
                    (
                        old,
                        (now.duration_since(*t0).as_secs_f32() / self.fade_len.as_secs_f32())
                            .min(1.0),
                    )
                });
                let (pix, info) = (
                    self.current.as_ref().map(|p| &p.pix),
                    self.current.as_ref().map(|p| &p.info),
                );
                screens::photo_frame(
                    canvas,
                    &mut self.text,
                    &self.snap,
                    pix,
                    info,
                    fade,
                    self.notice.as_deref(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DummyData, dummy_snapshot};

    fn app_with_photo(size: (u32, u32)) -> (KioskApp, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "miharashi-app-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        image::RgbaImage::from_pixel(64, 48, image::Rgba([30, 120, 60, 255]))
            .save(&p)
            .unwrap();
        let cfg = Config::default();
        let app = KioskApp::new(
            &cfg,
            size,
            Source::File(p, Some("テスト".into())),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
        );
        (app, dir)
    }

    fn pump_until(app: &mut KioskApp, cond: impl Fn(&KioskApp) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            app.tick(Instant::now());
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn key_flow_between_status_and_photo() {
        let (mut app, dir) = app_with_photo((320, 180));
        assert_eq!(app.screen(), Screen::Status);
        app.on_key(Key::Prev);
        assert_eq!(app.screen(), Screen::Status, "状況画面の 4 は何もしない");
        app.on_key(Key::Next);
        assert_eq!(
            app.screen(),
            Screen::Photo,
            "状況画面の 6 でフォトフレームへ"
        );
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), Screen::Status, "5 で状況画面へ戻る");
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), Screen::Photo, "5 でフォトフレームへ");
        assert!(
            pump_until(&mut app, |a| a.photo_position() == 1),
            "写真が読み込まれる"
        );
        app.on_key(Key::Next);
        assert!(
            pump_until(&mut app, |a| a.photo_position() == 1),
            "写真切替後も位置が有効"
        );
        assert_eq!(app.keys, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn px(c: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * c.width() + x) * 4) as usize;
        c.pixmap.data()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn both_screens_render_expected_regions_at_1080p() {
        let (mut app, dir) = app_with_photo((1920, 1080));
        let mut c = Canvas::new(1920, 1080);
        app.render(&mut c);
        // 状況画面: CPU タイルの左端にアクセント色、背景は暗色。
        assert_eq!(
            px(&c, 34, 150),
            [64, 200, 255, 255],
            "CPU タイルのアクセント"
        );
        assert_eq!(px(&c, 5, 540), [8, 12, 20, 255], "背景");
        // グラフ領域にシリーズ色(CPU のシアン)が描かれている。
        let mut found = false;
        for y in 440..1000 {
            for x in 100..1880 {
                if px(&c, x, y) == [64, 200, 255, 255] {
                    found = true;
                }
            }
        }
        assert!(found, "CPU 使用率の折れ線");

        app.on_key(Key::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        // 写真が拡大されて全面に出る。フェードが終わるまで待って確認する。
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut c2 = Canvas::new(1920, 1080);
        app.render(&mut c2);
        let center = px(&c2, 960, 400);
        assert!(
            center[1] > center[0] && center[1] > center[2],
            "写真(緑)が表示される: {center:?}"
        );
        let band = px(&c2, 1000, 1070);
        assert!(
            band[1] < center[1] / 2 + 10,
            "下部の半透明の帯で暗くなる: {band:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
