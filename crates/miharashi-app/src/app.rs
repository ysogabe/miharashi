//! 画面ロジック(PC 状況 + フォトフレーム)。実行ループ(runtime)からは `App` として見える。
use crate::config::Config;
use crate::photos::{Feed, FeedEvent, FeedStatus, PhotoItem, Source};
use miharashi_render::screens::{self, StatusPage};
use miharashi_render::{Canvas, Pixmap, TextEngine};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Prev,
    Switch,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// 状況画面(OVERVIEW / DETAILS)
    Status(StatusPage),
    Photo,
}

/// データ収集(実測値)の抽象。実行ループも画面も具体的な収集方法を知らない。
pub trait DataSource {
    /// 1 秒ごとなど、更新の時期に呼ぶ。
    fn update(&mut self, snap: &mut screens::Snapshot);
}

pub struct KioskApp {
    screen: Screen,
    /// フォトフレームから戻るときの、最後に見ていた状況ページ
    status_page: StatusPage,
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
                Screen::Status(StatusPage::Overview)
            },
            status_page: StatusPage::Overview,
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
            // 状況画面: 5 でページ切替、6 / → でフォトフレームへ。
            (Key::Switch, Screen::Status(p)) => {
                self.status_page = p.toggled();
                self.screen = Screen::Status(self.status_page);
            }
            (Key::Next, Screen::Status(p)) => {
                self.status_page = p;
                self.screen = Screen::Photo;
            }
            (Key::Switch, Screen::Photo) => self.screen = Screen::Status(self.status_page),
            (Key::Next, Screen::Photo) => {
                self.feed.next();
                self.last_photo_change = Instant::now();
            }
            (Key::Prev, Screen::Photo) => {
                self.feed.prev();
                self.last_photo_change = Instant::now();
            }
            (Key::Prev, Screen::Status(_)) => {}
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
                        self.fade = Some((old.frame, now));
                    }
                    self.current = Some(item);
                    self.last_photo_change = now;
                    if self.notice.is_none() {
                        self.snap.feed_status = Some("online".into());
                    }
                    redraw = true;
                }
                FeedEvent::Status(s) => {
                    self.notice = describe(&s);
                    // 状況画面の HEALTH にも同じ状態を出す
                    let state = if self.notice.is_some() {
                        "再接続中"
                    } else {
                        "online"
                    };
                    self.snap.feed_status = Some(state.into());
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
            Screen::Status(page) => screens::status(canvas, &mut self.text, &self.snap, page),
            Screen::Photo => {
                let now = Instant::now();
                let fade = self.fade.as_ref().map(|(old, t0)| {
                    (
                        old,
                        (now.duration_since(*t0).as_secs_f32() / self.fade_len.as_secs_f32())
                            .min(1.0),
                    )
                });
                let (photo, info) = (
                    self.current.as_ref().map(|p| (&p.frame, &p.layout)),
                    self.current.as_ref().map(|p| &p.info),
                );
                screens::photo_frame(
                    canvas,
                    &mut self.text,
                    &self.snap,
                    photo,
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

    const OVERVIEW: Screen = Screen::Status(StatusPage::Overview);
    const DETAILS: Screen = Screen::Status(StatusPage::Details);

    #[test]
    fn key_flow_between_status_and_photo() {
        let (mut app, dir) = app_with_photo((320, 180));
        assert_eq!(app.screen(), OVERVIEW);
        app.on_key(Key::Prev);
        assert_eq!(app.screen(), OVERVIEW, "状況画面の 4 は何もしない");
        app.on_key(Key::Next);
        assert_eq!(
            app.screen(),
            Screen::Photo,
            "状況画面の 6 でフォトフレームへ"
        );
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), OVERVIEW, "5 で状況画面へ戻る");
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), DETAILS, "状況画面の 5 でページ切替");
        assert!(
            pump_until(&mut app, |a| a.photo_position() == 1),
            "写真が読み込まれる"
        );
        app.on_key(Key::Next);
        assert_eq!(app.screen(), Screen::Photo);
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), DETAILS, "最後に見ていたページへ戻る");
        app.on_key(Key::Next);
        app.on_key(Key::Next);
        assert!(
            pump_until(&mut app, |a| a.photo_position() == 1),
            "写真切替後も位置が有効"
        );
        assert_eq!(app.keys, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_page_toggles_with_switch_key() {
        let (mut app, dir) = app_with_photo((320, 180));
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), DETAILS);
        app.on_key(Key::Prev);
        assert_eq!(app.screen(), DETAILS, "4 は何もしない");
        app.on_key(Key::Switch);
        assert_eq!(app.screen(), OVERVIEW);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_screen_status_starts_on_overview() {
        let (app, dir) = app_with_photo((320, 180));
        assert_eq!(app.screen(), OVERVIEW);
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
        for y in 400..900 {
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
        // テスト写真は 4:3 なので 1080p では左右どちらかに高さいっぱいで置かれる(中央 x=960 は必ず写真内)。
        let center = px(&c2, 960, 300);
        assert_eq!(center, [30, 120, 60, 255], "写真(緑)が表示される");
        let outside = px(&c2, 1900, 1070).min(px(&c2, 20, 1070));
        assert!(
            outside[1] < 70 && outside[1] > 20,
            "余白は暗いぼかし背景: {outside:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn has_color(
        c: &Canvas,
        color: [u8; 4],
        x: std::ops::Range<u32>,
        y: std::ops::Range<u32>,
    ) -> bool {
        y.into_iter()
            .any(|y| x.clone().any(|x| px(c, x, y) == color))
    }

    #[test]
    fn status_pages_render_at_1080p_and_21_9() {
        for size in [(1920u32, 1080u32), (2560, 1080)] {
            let (mut app, dir) = app_with_photo(size);
            for page in [OVERVIEW, DETAILS] {
                assert_eq!(app.screen(), page);
                let mut c = Canvas::new(size.0, size.1);
                app.render(&mut c);
                assert_eq!(px(&c, 5, 540), [8, 12, 20, 255], "背景 {size:?}");
                if page == OVERVIEW {
                    // CPU / GPU タイルの左端のアクセント色。
                    assert_eq!(px(&c, 3 + 32, 150), [64, 200, 255, 255], "CPU {size:?}");
                    let gx = 32 + ((size.0 - 64 - 80) / 5 + 20) + 3;
                    assert_eq!(px(&c, gx, 150), [255, 170, 60, 255], "GPU {size:?}");
                    assert!(has_color(
                        &c,
                        [64, 200, 255, 255],
                        100..size.0 - 100,
                        400..900
                    ));
                } else {
                    // DETAILS にはアクセント色の帯がない。タイル枠内は暗色のまま。
                    assert_ne!(px(&c, 35, 150), [64, 200, 255, 255], "{size:?}");
                    assert_eq!(px(&c, 40, 330), [16, 24, 36, 255], "タイル背景 {size:?}");
                    // ページ指示のシアン色の文字が見出しにある。
                    assert!(has_color(&c, [64, 200, 255, 255], 300..700, 20..60));
                }
                app.on_key(Key::Switch);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
