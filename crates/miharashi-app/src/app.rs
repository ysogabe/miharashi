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
