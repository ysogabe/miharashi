//! 写真画面(フォトフレーム)。写真の取得(`Feed`)とフェード、旅・季節のカード、写真の合間の航空機の画面(#27)を持つ。
//! 8/2 = 前後の写真、5 = 写真の情報(撮影地・日付)の表示切替。
use crate::config::Config;
use crate::photos::{Feed, FeedEvent, FeedStatus, HomeLookup, PhotoItem, PhotoPixels, Source};
use crate::screen::{
    Composite, FeedState, Outcome, Provider, RenderContext, Screen, ScreenAction, ScreenId, World,
};
use miharashi_render::aircraft;
use miharashi_render::canvas::rgba;
use miharashi_render::screens;
use miharashi_render::trip_card::{self, TripCard};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct PhotoScreen {
    /// 表示中か。写真の自動切替と航空機の画面は表示中だけ動かす
    visible: bool,
    feed: Feed,
    current: Option<PhotoItem>,
    /// 表示中の写真の通し番号(GPU のテクスチャの識別に使う)
    current_id: u64,
    /// フェード中の (旧フレーム, その通し番号, 開始時刻)
    fade: Option<(PhotoItem, u64, Instant)>,
    /// GPU 経路で、重ね絵(時刻や写真の情報)を描き直す必要があるか。フェードの途中では描き直さない
    overlay_dirty: bool,
    notice: Option<String>,
    /// 写真の情報(撮影地・日付)を出すか。5 で切り替える。時刻と日付はいつも出す
    show_info: bool,
    /// 写真フィードの状態。`FeedStateProvider` が `World` に写す
    feed_state: Arc<Mutex<FeedState>>,
    /// 表示中の、旅・季節の切り替わりのカード
    card: Option<TripCard>,
    card_len: Duration,
    last_photo_change: Instant,
    photo_interval: Duration,
    fade_len: Duration,
    /// 航空機の最新の取得結果(無効なら None)
    aircraft: Option<crate::aircraft::Shared>,
    /// 前回の航空機の画面から出した写真の枚数と、航空機の画面を閉じる時刻
    photos_since_air: u32,
    air_until: Option<Instant>,
    air_every: u32,
    air_len: Duration,
    /// GPU 経路(GLES)か。航空機の画面の回る線を毎フレーム描く
    gpu: bool,
    /// 航空機の画面を CPU で最後に描いた内容(CPU 経路は変わったときだけ描き直す)
    air_drawn: Option<aircraft::AircraftView>,
    /// GPU 経路で、レーダーの背景を最後に描いた内容
    radar_bg: Option<aircraft::AircraftView>,
    /// レーダーの下地(地図と距離の輪)。受信機と半径が同じ間は描き直さない
    radar_base: Option<aircraft::RadarBase>,
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

impl PhotoScreen {
    /// `gpu_max` は GPU 経路のとき Some(GPU で扱える写真の最大の幅と高さ)。写真の合成を GPU に任せる。
    pub fn new(cfg: &Config, size: (u32, u32), gpu_max: Option<u32>, source: Source) -> Self {
        let g = &cfg.geoframe;
        let feed = Feed::spawn(
            source,
            size,
            gpu_max,
            Duration::from_secs(g.reconnect_min_secs),
            Duration::from_secs(g.reconnect_max_secs),
            Some(HomeLookup {
                ha: cfg.home_assistant.clone(),
                radius_km: g.home_radius_km,
            }),
        );
        Self {
            visible: false,
            feed,
            current: None,
            current_id: 0,
            fade: None,
            overlay_dirty: true,
            notice: None,
            show_info: true,
            feed_state: Arc::new(Mutex::new(FeedState {
                online: true,
                ..FeedState::default()
            })),
            card: None,
            card_len: Duration::from_secs(g.card_secs),
            last_photo_change: Instant::now(),
            photo_interval: Duration::from_secs(g.photo_interval_secs),
            fade_len: Duration::from_millis(800),
            aircraft: None,
            photos_since_air: 0,
            air_until: None,
            air_every: cfg.aircraft.every_photos,
            air_len: Duration::from_secs(cfg.aircraft.show_secs),
            gpu: gpu_max.is_some(),
            air_drawn: None,
            radar_bg: None,
            radar_base: None,
        }
    }

    /// 航空機の取得結果をつなぐ。
    pub fn with_aircraft(mut self, shared: Option<crate::aircraft::Shared>) -> Self {
        self.aircraft = shared;
        self
    }

    /// 写真フィードの状態を `World` に写す Provider。
    pub fn feed_provider(&self) -> FeedStateProvider {
        FeedStateProvider(self.feed_state.clone())
    }

    /// 航空機の画面を出しているか。
    pub fn showing_aircraft(&self) -> bool {
        self.air_until.is_some()
    }

    fn photo_position(&self) -> usize {
        self.current.as_ref().map_or(0, |p| p.info.position.0)
    }

    /// 新しい(30 秒以内の)取得結果で、半径内に機体がいるときの内容。
    fn aircraft_view(&self) -> Option<aircraft::AircraftView> {
        let g = self.aircraft.as_ref()?.lock().ok()?;
        let (at, view) = g.as_ref()?;
        (at.elapsed() < Duration::from_secs(30) && !view.planes.is_empty()).then(|| view.clone())
    }

    fn publish_feed_state(&self, status: Option<&'static str>) {
        if let Ok(mut s) = self.feed_state.lock() {
            if status.is_some() {
                s.status = status;
            }
            s.position = self.current.as_ref().map(|p| p.info.position);
            s.online = self.notice.is_none();
            s.notice.clone_from(&self.notice);
        }
    }

    /// フェードの進み具合(新フレームの不透明度 0〜1)。
    fn fade_alpha(&self, t0: Instant) -> f32 {
        (Instant::now().duration_since(t0).as_secs_f32() / self.fade_len.as_secs_f32()).min(1.0)
    }
}

impl Screen for PhotoScreen {
    fn id(&self) -> ScreenId {
        "photo"
    }

    fn title(&self) -> &'static str {
        "PHOTO FRAME"
    }

    fn on_enter(&mut self, _now: Instant) {
        self.visible = true;
        self.overlay_dirty = true;
    }

    fn on_leave(&mut self, _now: Instant) {
        self.visible = false;
    }

    fn tick(&mut self, _world: &World, now: Instant) -> bool {
        let mut redraw = false;
        while let Some(ev) = self.feed.try_recv() {
            match ev {
                FeedEvent::Photo(item) => {
                    if let Some(old) = self.current.take() {
                        self.fade = Some((old, self.current_id, now));
                    }
                    self.current = Some(item);
                    self.current_id += 1;
                    self.card = None;
                    self.photos_since_air += 1;
                    self.last_photo_change = now;
                    self.publish_feed_state(self.notice.is_none().then_some("online"));
                    redraw = true;
                }
                FeedEvent::Card(c) => {
                    self.card = Some(c);
                    self.last_photo_change = now;
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
                    self.publish_feed_state(Some(state));
                    redraw = true;
                }
            }
        }
        // 写真画面で写真を every_photos 枚出したら、半径内に機体がいれば航空機の画面を挟む
        if self.visible
            && self.air_until.is_none()
            && self.card.is_none()
            && self.photos_since_air >= self.air_every
            && now.duration_since(self.last_photo_change) >= self.photo_interval
            && self.aircraft_view().is_some()
        {
            self.air_until = Some(now + self.air_len);
            self.photos_since_air = 0;
            redraw = true;
        }
        if let Some(until) = self.air_until {
            if now >= until || !self.visible {
                self.air_until = None;
                self.air_drawn = None;
                self.radar_bg = None;
                // 閉じたらすぐ次の写真へ
                self.feed.next();
                self.last_photo_change = now;
                redraw = true;
            }
            // GPU 経路は回る線を毎フレーム描く。CPU 経路は内容が変わったときだけ描き直す
            if self.gpu || (self.air_until.is_some() && self.aircraft_view() != self.air_drawn) {
                redraw = true;
            }
        }
        let dwell = if self.card.is_some() {
            self.card_len
        } else {
            self.photo_interval
        };
        if self.visible
            && self.air_until.is_none()
            && now.duration_since(self.last_photo_change) >= dwell
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
        self.overlay_dirty |= redraw;
        redraw || (self.visible && self.fade.is_some())
    }

    fn handle_action(&mut self, action: ScreenAction, now: Instant) -> Outcome {
        self.overlay_dirty = true;
        // 航空機の画面のキーは写真へ戻るだけにする
        if self.air_until.take().is_some() {
            self.last_photo_change = now;
            return Outcome::Consumed;
        }
        match action {
            ScreenAction::Up => self.feed.prev(),
            ScreenAction::Down => self.feed.next(),
            ScreenAction::Select => self.show_info = !self.show_info,
        }
        self.last_photo_change = now;
        Outcome::Consumed
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        if self.air_until.is_some() {
            self.air_drawn = self.aircraft_view();
            if let Some(v) = &self.air_drawn {
                aircraft::aircraft_screen(ctx.canvas, ctx.text, v);
                return;
            }
        }
        // GPU 経路の写真(デコードしただけ)は、CPU で描くときにここで合成する
        let size = (ctx.canvas.width(), ctx.canvas.height());
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
            self.current
                .as_ref()
                .filter(|_| self.show_info)
                .map(|p| &p.info),
        );
        screens::photo_frame(
            ctx.canvas,
            ctx.text,
            &ctx.world.system,
            photo,
            info,
            fade,
            self.notice.as_deref(),
        );
        if let Some(card) = &self.card {
            trip_card::trip_card(ctx.canvas, ctx.text, card);
        }
    }

    /// GPU 経路の描画。写真は写真と重ね絵に分け、重ね絵は変わったときだけ描き直す。
    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        // 航空機の画面: 背景は内容が変わったときだけ描き、回る線と点は GPU に任せる。
        // アラートを重ねるときは CPU で完成画面を描く
        if let Some(until) = self.air_until
            && !ctx.covered
            && let Some(v) = self.aircraft_view()
        {
            let changed = self.radar_bg.as_ref() != Some(&v);
            if changed {
                aircraft::aircraft_background_cached(
                    ctx.canvas,
                    ctx.text,
                    &v,
                    &mut self.radar_base,
                );
            }
            let geom = aircraft::radar_geometry(&v, (ctx.canvas.width(), ctx.canvas.height()));
            self.radar_bg = Some(v);
            self.overlay_dirty = true;
            let start = until.checked_sub(self.air_len).unwrap_or(until);
            let turns = start.elapsed().as_secs_f32() / aircraft::SWEEP_PERIOD_SECS;
            return Composite::Radar {
                background_changed: changed,
                geom,
                sweep: turns.fract() * std::f32::consts::TAU,
            };
        }
        if self.current.is_none() || self.air_until.is_some() {
            // 写真がまだ 1 枚もないときと、アラートを重ねる航空機の画面は Canvas が完成画面
            self.render(ctx);
            // 次に写真を出すときは重ね絵を描き直す
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
        let overlay_changed = std::mem::take(&mut self.overlay_dirty) || ctx.force_overlay;
        let item = self.current.as_ref().expect("上で確かめた");
        if overlay_changed {
            ctx.canvas.clear(rgba(0, 0, 0, 0));
            screens::photo_overlay(
                ctx.canvas,
                ctx.text,
                &ctx.world.system,
                &item.layout,
                self.show_info.then_some(&item.info),
                self.notice.as_deref(),
            );
            if let Some(card) = &self.card {
                trip_card::trip_card(ctx.canvas, ctx.text, card);
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

    fn animating(&self) -> bool {
        self.gpu && self.showing_aircraft()
    }

    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        out.push(("photo", self.photo_position().to_string()));
        out.push(("aircraft", self.showing_aircraft().to_string()));
    }
}

/// 写真画面が書いた写真フィードの状態を `World` に写す。状況画面の HEALTH は `system.feed_status` を読む。
pub struct FeedStateProvider(Arc<Mutex<FeedState>>);

impl Provider for FeedStateProvider {
    fn update(&mut self, world: &mut World) {
        let Ok(s) = self.0.lock() else { return };
        if world.photo_feed != *s {
            world.photo_feed = s.clone();
        }
        // 実測値の更新(ダミーは Snapshot を丸ごと入れ替える)で消えても、毎周期書き直す
        if let Some(status) = s.status
            && world.system.feed_status.as_deref() != Some(status)
        {
            world.system.feed_status = Some(status.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_photo;
    use miharashi_render::Canvas;

    fn screen(size: (u32, u32)) -> (PhotoScreen, World, std::path::PathBuf) {
        let (dir, p) = test_photo();
        let mut s = PhotoScreen::new(
            &Config::default(),
            size,
            None,
            Source::File(p, Some("テスト".into())),
        );
        s.on_enter(Instant::now());
        (s, World::default(), dir)
    }

    fn pump_until(s: &mut PhotoScreen, w: &World, cond: impl Fn(&PhotoScreen) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            s.tick(w, Instant::now());
            if cond(s) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// 届いた写真を受け取り、フェードも終わって、再描画の要求が 0.2 秒続けて無くなるまで回す。
    fn settle(s: &mut PhotoScreen, w: &World) {
        let mut quiet = 0;
        for _ in 0..500 {
            if s.tick(w, Instant::now()) {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet >= 20 {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("落ち着かない");
    }

    /// GPU 経路で描き、航空機の画面なら (背景を描き直したか, 点の数, 回る線の角度)。
    fn radar(s: &mut PhotoScreen, w: &World, c: &mut Canvas) -> Option<(bool, usize, f32)> {
        let mut text = crate::app::text_engine("");
        let mut ctx = RenderContext {
            canvas: c,
            text: &mut text,
            world: w,
            covered: false,
            force_overlay: false,
        };
        match s.render_composite(&mut ctx) {
            Composite::Radar {
                background_changed,
                geom,
                sweep,
            } => Some((background_changed, geom.blips.len(), sweep)),
            _ => None,
        }
    }

    /// 写真を every_photos 枚出したら、半径内に機体がいるときだけ航空機の画面を挟み、時間で閉じる。
    #[test]
    fn aircraft_interlude_between_photos() {
        let (mut s, w, dir) = screen((640, 270));
        s.air_every = 1;
        s.air_len = Duration::from_secs(5);
        let shared: crate::aircraft::Shared = Arc::new(Mutex::new(None));
        s.aircraft = Some(shared.clone());
        s.handle_action(ScreenAction::Down, Instant::now());
        // 最初の写真と、8/2 で進めた写真の 2 枚が届くまで待つ
        assert!(pump_until(&mut s, &w, |s| s.current_id == 2));
        settle(&mut s, &w);
        let later = Instant::now() + Duration::from_secs(31);
        s.tick(&w, later);
        assert!(!s.showing_aircraft(), "機体がいなければ挟まない");
        // 自動で進めた 3 枚目が届き、フェードが終わるまで待つ(あとの「描き直さない」の確認がぶれないように)
        assert!(pump_until(&mut s, &w, |s| s.current_id == 3));
        settle(&mut s, &w);
        // 上の tick で次の写真へ進んだので、もう 1 枚分の時間をおく
        let later = later + Duration::from_secs(31);
        s.photos_since_air = 1;
        *shared.lock().unwrap() = Some((Instant::now(), crate::aircraft::demo_view()));
        s.tick(&w, later);
        assert!(s.showing_aircraft(), "機体がいれば挟む");
        // CPU 経路: 1 回描いたら、内容が変わるまで描き直さない
        let mut c = Canvas::new(640, 270);
        let mut text = crate::app::text_engine("");
        s.render(&mut RenderContext {
            canvas: &mut c,
            text: &mut text,
            world: &w,
            covered: false,
            force_overlay: true,
        });
        assert!(!s.tick(&w, later), "CPU 経路は止まった絵を描き直さない");
        s.gpu = true;
        assert!(s.tick(&w, later), "GPU 経路は回る線を毎フレーム描く");
        assert!(s.animating(), "回る線の間は vsync だけで間隔をとる");
        // GPU 経路: 背景は 1 回だけ描き、回る線と点は GPU に任せる
        let (changed, blips, sweep) = radar(&mut s, &w, &mut c).expect("航空機の画面は Radar");
        assert!(changed);
        assert_eq!(blips, 5);
        assert!((0.0..std::f32::consts::TAU).contains(&sweep));
        assert_eq!(radar(&mut s, &w, &mut c).map(|r| r.0), Some(false));
        s.tick(&w, later + Duration::from_secs(6));
        assert!(!s.showing_aircraft(), "時間で閉じる");
        // キーでも閉じる
        s.photos_since_air = 1;
        s.tick(&w, later + Duration::from_secs(40));
        assert!(s.showing_aircraft());
        assert_eq!(
            s.handle_action(ScreenAction::Select, Instant::now()),
            Outcome::Consumed,
            "どのキーでも閉じるだけ"
        );
        assert!(!s.showing_aircraft());
        // 非表示のあいだは挟まず、写真も自動で進めない
        s.on_leave(Instant::now());
        s.photos_since_air = 1;
        let before = s.last_photo_change;
        s.tick(&w, later + Duration::from_secs(80));
        assert!(!s.showing_aircraft(), "非表示では挟まない");
        assert_eq!(s.last_photo_change, before, "非表示では写真を進めない");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
