//! 写真画面(フォトフレーム)。写真の取得(`Feed`)とフェード、旅・季節のカードを持つ。
//! 8/2 = 前後の写真、5 = 写真の情報(撮影地・日付)の表示切替。
use crate::config::Config;
use crate::photos::{Feed, FeedEvent, FeedStatus, HomeLookup, PhotoItem, PhotoPixels, Source};
use crate::screen::{
    Composite, FeedState, Outcome, Provider, RenderContext, Screen, ScreenAction, ScreenId, World,
};
use miharashi_render::canvas::rgba;
use miharashi_render::screens;
use miharashi_render::trip_card::{self, TripCard};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct PhotoScreen {
    /// 表示中か。写真の自動切替は表示中だけ動かす
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
        }
    }

    /// 写真フィードの状態を `World` に写す Provider。
    pub fn feed_provider(&self) -> FeedStateProvider {
        FeedStateProvider(self.feed_state.clone())
    }

    fn photo_position(&self) -> usize {
        self.current.as_ref().map_or(0, |p| p.info.position.0)
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

    fn invalidate(&mut self) {
        self.overlay_dirty = true;
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
        let dwell = if self.card.is_some() {
            self.card_len
        } else {
            self.photo_interval
        };
        if self.visible && now.duration_since(self.last_photo_change) >= dwell {
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
        match action {
            ScreenAction::Up => self.feed.prev(),
            ScreenAction::Down => self.feed.next(),
            ScreenAction::Select => self.show_info = !self.show_info,
        }
        self.last_photo_change = now;
        Outcome::Consumed
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
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
        if self.current.is_none() {
            // 写真がまだ 1 枚もないときは Canvas が完成画面
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

    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        out.push(("photo", self.photo_position().to_string()));
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

    /// 非表示のあいだは写真を自動で進めず、表示に戻ると続きから進める。
    #[test]
    fn auto_advance_only_while_visible() {
        let (mut s, w, dir) = screen((640, 270));
        assert!(pump_until(&mut s, &w, |s| s.current_id == 1));
        settle(&mut s, &w);
        s.on_leave(Instant::now());
        let before = s.last_photo_change;
        s.tick(&w, Instant::now() + Duration::from_secs(80));
        assert_eq!(s.last_photo_change, before, "非表示では写真を進めない");
        s.on_enter(Instant::now());
        let later = Instant::now() + Duration::from_secs(80);
        s.tick(&w, later);
        assert_eq!(
            s.last_photo_change, later,
            "表示に戻ると時間の過ぎた写真を進める"
        );
        assert!(
            pump_until(&mut s, &w, |s| s.current_id == 2),
            "次の写真が届く"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
