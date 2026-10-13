//! 画面ロジック(PC 状況 + フォトフレーム)。実行ループ(runtime)からは `App` として見える。
use crate::config::Config;
use crate::photos::{Feed, FeedEvent, FeedStatus, PhotoItem, PhotoPixels, Source};
#[cfg(test)]
use crate::screen::PhotoData;
use crate::screen::{App, Composite, KeyPress, PanelView, ScreenInfo};
use miharashi_render::alert::{self, AlertView};
use miharashi_render::canvas::rgba;
use miharashi_render::screens::{self, StatusPage};
use miharashi_render::{Canvas, TextEngine};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

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
    /// 表示中の写真の通し番号(GPU のテクスチャの識別に使う)
    current_id: u64,
    /// フェード中の (旧フレーム, その通し番号, 開始時刻)
    fade: Option<(PhotoItem, u64, Instant)>,
    /// GPU 経路で、重ね絵(時刻や写真の情報)を描き直す必要があるか。フェードの途中では描き直さない
    overlay_dirty: bool,
    notice: Option<String>,
    last_photo_change: Instant,
    photo_interval: Duration,
    fade_len: Duration,
    next_tick: Instant,
    tick_len: Duration,
    text: TextEngine,
    /// アラート(地震・災害)の受け口(無効なら None)
    alerts: Option<Receiver<AlertView>>,
    /// 表示中のアラートと、閉じる時刻
    alert: Option<(AlertView, Instant)>,
    alert_len: Duration,
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
    /// `gpu_max` は GPU 経路のとき Some(GPU で扱える写真の最大の幅と高さ)。写真の合成を GPU に任せる。
    pub fn new(
        cfg: &Config,
        size: (u32, u32),
        gpu_max: Option<u32>,
        source: Source,
        data: Box<dyn DataSource>,
        snap: screens::Snapshot,
    ) -> Self {
        let g = &cfg.geoframe;
        let feed = Feed::spawn(
            source,
            size,
            gpu_max,
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
            current_id: 0,
            fade: None,
            overlay_dirty: true,
            notice: None,
            last_photo_change: Instant::now(),
            photo_interval: Duration::from_secs(g.photo_interval_secs),
            fade_len: Duration::from_millis(800),
            next_tick: Instant::now(),
            tick_len: Duration::from_secs(1),
            text: text_engine(&cfg.ui.font),
            alerts: None,
            alert: None,
            alert_len: Duration::from_secs(cfg.alerts.show_minutes * 60),
            ticks: 0,
            keys: 0,
        }
    }

    /// アラートの受け口をつなぐ。
    pub fn with_alerts(mut self, rx: Option<Receiver<AlertView>>) -> Self {
        self.alerts = rx;
        self
    }

    /// アラートを表示中か(画面を消していても点ける)。
    pub fn alert_active(&self) -> bool {
        self.alert.is_some()
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
    pub fn on_key(&mut self, key: KeyPress) -> bool {
        self.keys += 1;
        self.overlay_dirty = true;
        // アラート表示中のキーは閉じるだけにする
        if self.alert.take().is_some() {
            return true;
        }
        match (key, self.screen) {
            // 状況画面: 5 でページ切替、6 / → でフォトフレームへ。
            (KeyPress::Switch, Screen::Status(p)) => {
                self.status_page = p.toggled();
                self.screen = Screen::Status(self.status_page);
            }
            (KeyPress::Next, Screen::Status(p)) => {
                self.status_page = p;
                self.screen = Screen::Photo;
            }
            (KeyPress::Switch, Screen::Photo) => self.screen = Screen::Status(self.status_page),
            (KeyPress::Next, Screen::Photo) => {
                self.feed.next();
                self.last_photo_change = Instant::now();
            }
            (KeyPress::Prev, Screen::Photo) => {
                self.feed.prev();
                self.last_photo_change = Instant::now();
            }
            (KeyPress::Prev, Screen::Status(_)) => {}
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
                        self.fade = Some((old, self.current_id, now));
                    }
                    self.current = Some(item);
                    self.current_id += 1;
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
        // 新しいアラートは前のものを置き換え、表示時間も数え直す
        let mut latest = None;
        while let Some(v) = self.alerts.as_ref().and_then(|rx| rx.try_recv().ok()) {
            latest = Some(v);
        }
        if let Some(v) = latest {
            self.alert = Some((v, now + self.alert_len));
            redraw = true;
        }
        // 表示時間が過ぎたら、ほかの入力が無くても必ず閉じる
        if self.alert.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.alert = None;
            redraw = true;
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
            .is_some_and(|(_, _, t0)| now.duration_since(*t0) >= self.fade_len)
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
        self.overlay_dirty |= redraw;
        redraw || (self.screen == Screen::Photo && self.fade.is_some())
    }

    /// フェードの進み具合(新フレームの不透明度 0〜1)。
    fn fade_alpha(&self, t0: Instant) -> f32 {
        (Instant::now().duration_since(t0).as_secs_f32() / self.fade_len.as_secs_f32()).min(1.0)
    }

    /// GPU 経路の描画。写真画面は写真と重ね絵に分け、重ね絵は変わったときだけ描き直す。
    pub fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        if self.screen != Screen::Photo || self.current.is_none() {
            // 状況画面と、写真がまだ 1 枚もないときは Canvas が完成画面
            self.render(canvas);
            // 次に写真画面になったら重ね絵を描き直す
            self.overlay_dirty = true;
            return Composite::Canvas;
        }
        // デコードしただけの写真は 1 回だけ GPU に渡し、次からは GPU 側の完成フレームを使う
        // (CPU 側の画素は捨てる。12MP の写真で 1 枚約 36MB)
        let fade_item = self.fade.as_mut().map(|f| &mut f.0);
        for it in self.current.iter_mut().chain(fade_item) {
            if it.sent && matches!(it.pixels, PhotoPixels::Source(_)) {
                it.pixels = PhotoPixels::Resident;
            }
            it.sent = true;
        }
        let overlay_changed = std::mem::take(&mut self.overlay_dirty);
        let item = self.current.as_ref().expect("上で確かめた");
        if overlay_changed {
            canvas.clear(rgba(0, 0, 0, 0));
            screens::photo_overlay(
                canvas,
                &mut self.text,
                &self.snap,
                &item.layout,
                Some(&item.info),
                self.notice.as_deref(),
            );
            if let Some((view, _)) = &self.alert {
                alert::overlay(canvas, &mut self.text, view);
            }
        }
        let old = self
            .fade
            .as_ref()
            .map(|(it, id, t0)| (it.photo_ref(*id), self.fade_alpha(*t0)));
        Composite::Photo {
            photo: item.photo_ref(self.current_id),
            old,
            overlay_changed,
        }
    }

    pub fn render(&mut self, canvas: &mut Canvas) {
        self.render_screen(canvas);
        if let Some((view, _)) = &self.alert {
            alert::overlay(canvas, &mut self.text, view);
        }
    }

    fn render_screen(&mut self, canvas: &mut Canvas) {
        match self.screen {
            Screen::Status(page) => screens::status(canvas, &mut self.text, &self.snap, page),
            Screen::Photo => {
                // GPU 経路の写真(デコードしただけ)は、CPU で描くときにここで合成する
                let size = (canvas.width(), canvas.height());
                let fade_item = self.fade.as_mut().map(|f| &mut f.0);
                for it in self.current.iter_mut().chain(fade_item) {
                    it.ensure_frame(size);
                }
                let fade = self
                    .fade
                    .as_ref()
                    .and_then(|(old, _, t0)| Some((old.frame()?, self.fade_alpha(*t0))));
                let (photo, info) = (
                    self.current
                        .as_ref()
                        .and_then(|p| Some((p.frame()?, &p.layout))),
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

impl App for KioskApp {
    fn on_key(&mut self, key: KeyPress) -> bool {
        KioskApp::on_key(self, key)
    }

    fn tick(&mut self, now: Instant) -> bool {
        KioskApp::tick(self, now)
    }

    fn render(&mut self, canvas: &mut Canvas) {
        KioskApp::render(self, canvas)
    }

    fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        KioskApp::render_composite(self, canvas)
    }

    fn wants_screen(&self) -> bool {
        self.alert_active()
    }

    fn stats(&self) -> Vec<(&'static str, String)> {
        let s = self.snapshot();
        vec![
            ("ticks", self.ticks.to_string()),
            ("keys", self.keys.to_string()),
            ("screen", format!("{:?}", self.screen())),
            ("photo", self.photo_position().to_string()),
            ("feed", self.feed_notice().unwrap_or("online").to_string()),
            ("cpu", format!("{:?}", s.cpu.map(|v| v.round()))),
            ("gpu", format!("{:?}", s.gpu.map(|v| v.round()))),
            (
                "gpus",
                s.gpus
                    .iter()
                    .map(|g| {
                        let u = g.usage.map_or("--".to_string(), |v| format!("{v:.0}"));
                        format!("{}:{u}", g.label)
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]
    }

    fn panel_view(&self) -> Option<PanelView> {
        let (current, page) = match self.screen {
            Screen::Status(StatusPage::Overview) => (0, Some("OVERVIEW")),
            Screen::Status(StatusPage::Details) => (0, Some("DETAILS")),
            Screen::Photo => (1, None),
        };
        Some(PanelView {
            screens: vec![
                ScreenInfo {
                    id: "status",
                    title: "STATUS",
                },
                ScreenInfo {
                    id: "photo",
                    title: "PHOTO FRAME",
                },
            ],
            current,
            page,
            photo: self.current.as_ref().map(|p| p.info.position),
            feed_online: self.notice.is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DummyData, dummy_snapshot};

    fn app_with_photo(size: (u32, u32)) -> (KioskApp, std::path::PathBuf) {
        app_with_photo_on(size, None)
    }

    /// `gpu_max` が Some なら GPU 経路(写真はデコードしただけで届く)。
    fn app_with_photo_on(size: (u32, u32), gpu_max: Option<u32>) -> (KioskApp, std::path::PathBuf) {
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
            gpu_max,
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
        app.on_key(KeyPress::Prev);
        assert_eq!(app.screen(), OVERVIEW, "状況画面の 4 は何もしない");
        app.on_key(KeyPress::Next);
        assert_eq!(
            app.screen(),
            Screen::Photo,
            "状況画面の 6 でフォトフレームへ"
        );
        app.on_key(KeyPress::Switch);
        assert_eq!(app.screen(), OVERVIEW, "5 で状況画面へ戻る");
        app.on_key(KeyPress::Switch);
        assert_eq!(app.screen(), DETAILS, "状況画面の 5 でページ切替");
        assert!(
            pump_until(&mut app, |a| a.photo_position() == 1),
            "写真が読み込まれる"
        );
        app.on_key(KeyPress::Next);
        assert_eq!(app.screen(), Screen::Photo);
        app.on_key(KeyPress::Switch);
        assert_eq!(app.screen(), DETAILS, "最後に見ていたページへ戻る");
        app.on_key(KeyPress::Next);
        app.on_key(KeyPress::Next);
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
        app.on_key(KeyPress::Switch);
        assert_eq!(app.screen(), DETAILS);
        app.on_key(KeyPress::Prev);
        assert_eq!(app.screen(), DETAILS, "4 は何もしない");
        app.on_key(KeyPress::Switch);
        assert_eq!(app.screen(), OVERVIEW);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panel_view_follows_the_screen() {
        let (mut app, dir) = app_with_photo((320, 180));
        let v = App::panel_view(&app).unwrap();
        let ids: Vec<_> = v.screens.iter().map(|s| s.id).collect();
        assert_eq!(ids, ["status", "photo"]);
        assert_eq!((v.current, v.page), (0, Some("OVERVIEW")));
        app.on_key(KeyPress::Switch);
        assert_eq!(App::panel_view(&app).unwrap().page, Some("DETAILS"));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        let v = App::panel_view(&app).unwrap();
        assert_eq!((v.current, v.page, v.photo), (1, None, Some((1, 1))));
        assert!(v.feed_online);
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

        app.on_key(KeyPress::Next);
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

    /// GPU 経路の合成(写真 + 透明背景の重ね絵)を CPU で再現し、CPU 経路の `render` と比べる。
    /// 重ね絵は変わったときだけ描き直すことも確かめる。
    #[test]
    fn composite_photo_plus_overlay_matches_cpu_render() {
        let size = (640u32, 270u32);
        let (mut app, dir) = app_with_photo(size);
        let mut c = Canvas::new(size.0, size.1);
        assert!(
            matches!(app.render_composite(&mut c), Composite::Canvas),
            "状況画面は Canvas が完成画面"
        );
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());

        let mut cpu = Canvas::new(size.0, size.1);
        app.render(&mut cpu);
        let mut overlay = Canvas::new(size.0, size.1);
        let (frame, changed) = match app.render_composite(&mut overlay) {
            Composite::Photo {
                photo,
                old,
                overlay_changed,
            } => {
                assert!(old.is_none(), "フェードは終わっている");
                let PhotoData::Frame(f) = photo.data else {
                    panic!("CPU 経路の写真は完成フレーム");
                };
                (f.data().to_vec(), overlay_changed)
            }
            Composite::Canvas => panic!("写真画面は Photo になる"),
        };
        assert!(changed, "最初は重ね絵を描く");
        // premultiplied の source-over: out = src + dst × (1 - src_a)
        let mut max = 0u8;
        for ((o, f), want) in overlay
            .pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(frame.as_chunks::<4>().0)
            .zip(cpu.pixmap.data().as_chunks::<4>().0)
        {
            for i in 0..3 {
                let v = o[i] as f32 + f[i] as f32 * (1.0 - o[3] as f32 / 255.0);
                max = max.max((v.round() as i32 - want[i] as i32).unsigned_abs() as u8);
            }
        }
        assert!(max <= 2, "CPU 経路との差 {max}");

        // 時刻などが変わらなければ描き直さない。キーを押すと描き直す
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: false,
                ..
            }
        ));
        app.on_key(KeyPress::Prev);
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: true,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GPU 経路: 写真はデコードしただけで 1 回だけ渡し、次からは GPU 側のフレームを使う
    /// (CPU 側の画素は捨てる)。CPU で描くときは、その場で完成フレームを作る。
    #[test]
    fn gpu_path_sends_source_once_then_resident() {
        let size = (640u32, 270u32);
        let kind = |app: &mut KioskApp| {
            let mut c = Canvas::new(size.0, size.1);
            match app.render_composite(&mut c) {
                Composite::Photo { photo, .. } => match photo.data {
                    PhotoData::Frame(_) => "frame",
                    PhotoData::Source { .. } => "source",
                    PhotoData::Resident => "resident",
                },
                Composite::Canvas => "canvas",
            }
        };
        let (mut app, dir) = app_with_photo_on(size, Some(16384));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        assert_eq!(kind(&mut app), "source", "1 回目はデコードしただけの写真");
        assert_eq!(kind(&mut app), "resident", "2 回目からは GPU 側のもの");
        let _ = std::fs::remove_dir_all(&dir);

        // 上限より大きい写真は、取得スレッドで完成フレームまで作る
        let (mut app, dir) = app_with_photo_on(size, Some(32));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        assert_eq!(kind(&mut app), "frame");
        let _ = std::fs::remove_dir_all(&dir);

        // GPU 経路の写真でも CPU の render は描ける
        let (mut app, dir) = app_with_photo_on(size, Some(16384));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| a.photo_position() == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut c = Canvas::new(size.0, size.1);
        app.render(&mut c);
        assert_eq!(px(&c, 320, 100), [30, 120, 60, 255], "写真(緑)");
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
                app.on_key(KeyPress::Switch);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
