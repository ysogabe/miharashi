//! だいだい画面(`daidai`)。Muse からの通知カードを縦に並べ、ドット絵のアバターを小さく上下に揺らす。
//! 操作は無い(4/6 の巡回とメニューだけ)。カードが無いときはアバターと時計の控えめな表示になる。
//!
//! 再描画は「見た目が変わったとき」だけ要求する(カード・ポーズ・分・揺れの画素)。揺れは整数画素に丸めるので、
//! 揺れの周期のあいだに数回しか描き直さず、実行ループを動かし続ける(`animating`)必要もない。
//! カードと背景の層は取っておき、毎フレームはそれを戻してアバターだけを重ねる。
//!
//! ポーズ動画(`daidai_video`、Issue #99)があるポーズは、アバターを PNG の揺れでなく動画のループ再生にする。
//! 再生中は実行ループを動かし続け(`animating` = 描き直しの上限 `frame_interval` つき)、画面が見えていない・
//! モニターが消えているあいだは何も読まず(デコーダーも捨てる)、アラート・メニューが上に載っているあいだは
//! 読み進めずに最後のフレームを出したままにする(`WithScene` と同じ。閉じたら続きから)。
//! 動画が使えないポーズは、これまでどおり PNG の揺れ。
use crate::daidai::{AvatarSet, DaidaiSnapshot};
use crate::daidai_video::VideoPlayback;
use crate::screen::{RenderContext, Screen, ScreenId, World};
use chrono::Datelike;
use miharashi_render::daidai::{self, CardView, DaidaiView, Pose};
use miharashi_render::{Canvas, Pixmap};
use std::time::{Duration, Instant};

/// 描き終えた内容(これと違うときだけ再描画を要求する)。
#[derive(Clone, PartialEq, Eq)]
struct Drawn {
    view: DaidaiView,
    pose: Pose,
    bob: i32,
}

pub struct DaidaiScreen {
    avatars: AvatarSet,
    /// ポーズ動画(無い・使えないポーズは PNG の `avatars`)
    video: VideoPlayback,
    visible: bool,
    /// モニターが点いているか(消えているあいだは動画を読まない)
    display_on: bool,
    /// アラート・メニューが上に載っているか(載っているあいだは動画を進めない)
    covered: bool,
    /// 表示を始めた時刻(揺れの位相の起点)
    entered: Instant,
    /// 直近の tick の時刻。描画はこれで揺れを決める(実時計を直接読まない)
    last_now: Instant,
    /// 直近の tick で見た内容。render が使う
    current: Option<(DaidaiView, Pose)>,
    /// 最後に描いた内容
    drawn: Option<Drawn>,
    /// カード・背景の層と、その内容
    base: Option<(DaidaiView, Pixmap)>,
    /// 最後に描いた画面の高さ(揺れの画素数は高さで決まる。まだ描いていなければ 1080)
    height: u32,
    /// 期限内のカードが無いか(自動巡回の `skip_if_empty`)
    empty: bool,
}

impl DaidaiScreen {
    pub fn new(avatars: AvatarSet) -> Self {
        let now = Instant::now();
        Self {
            avatars,
            video: VideoPlayback::none(),
            visible: false,
            display_on: true,
            covered: false,
            entered: now,
            last_now: now,
            current: None,
            drawn: None,
            base: None,
            height: 1080,
            empty: true,
        }
    }

    /// ポーズ動画を使う(使わなければ PNG だけ)。
    pub fn with_video(mut self, video: VideoPlayback) -> Self {
        self.video = video;
        self
    }

    /// 動画を読み進めてよいか(見えていて、モニターが点いていて、上に何も載っていない)
    fn video_active(&self) -> bool {
        self.visible && self.display_on && !self.covered
    }

    fn bob(&self, canvas_h: u32) -> i32 {
        let secs = self
            .last_now
            .saturating_duration_since(self.entered)
            .as_secs_f32();
        daidai::bob_offset(secs, canvas_h)
    }
}

/// 壁時計とフィードから、層に出す内容を作る(期限切れのカードは入れない)。
pub fn view_of<Tz: chrono::TimeZone>(
    snap: &DaidaiSnapshot,
    wall: &chrono::DateTime<Tz>,
) -> DaidaiView
where
    Tz::Offset: std::fmt::Display,
{
    let wd =
        ["月", "火", "水", "木", "金", "土", "日"][wall.weekday().num_days_from_monday() as usize];
    DaidaiView {
        cards: snap
            .active(wall)
            .into_iter()
            .map(|c| CardView {
                kind: c.kind,
                title: c.title.clone(),
                body: c.body.clone(),
            })
            .collect(),
        time: wall.format("%H:%M").to_string(),
        date: format!("{} ({wd})", wall.format("%Y/%m/%d")),
    }
}

impl Screen for DaidaiScreen {
    fn id(&self) -> ScreenId {
        "daidai"
    }

    fn title(&self) -> &'static str {
        "DAIDAI"
    }

    fn on_enter(&mut self, now: Instant) {
        self.visible = true;
        self.entered = now;
        self.last_now = now;
        self.drawn = None;
    }

    fn on_leave(&mut self, _now: Instant) {
        self.visible = false;
        self.video.stop();
    }

    fn set_presentation(&mut self, visible: bool, covered: bool, display_on: bool, _now: Instant) {
        self.display_on = display_on;
        self.covered = covered;
        if !(visible && display_on) {
            // 見えない・消灯中は、デコーダーごと捨てて何も読まない(戻ったら先頭から)
            self.video.stop();
            self.drawn = None;
        }
    }

    fn tick(&mut self, world: &World, now: Instant) -> bool {
        self.last_now = now;
        let view = view_of(&world.daidai, &world.wall());
        self.empty = view.cards.is_empty();
        self.current = Some((view, world.daidai.pose));
        // 非表示のときも内容は追う(is_empty のため)が、描き直しは表示中だけ
        if !self.visible {
            return false;
        }
        if self.video_active() {
            let was_playing = self.video.playing();
            self.video.update(world.daidai.pose, now);
            if self.video.playing() {
                // 動画は毎周期描く(間隔の上限は `frame_interval`。描かずに `animating` だけ立てると実行ループが空回りする)
                return true;
            }
            if was_playing {
                // 再生の途中で読めなくなった。最後の動画のフレームが残らないよう、PNG で描き直す
                self.drawn = None;
            }
        } else if !(self.visible && self.display_on) {
            self.video.stop();
        }
        let (view, pose) = self.current.clone().expect("直前に入れた");
        // 動画のフレームを出している(載っているあいだ止めている)ときは揺らさない
        let bob = if self.video.frame(pose).is_some() {
            0
        } else {
            self.bob(self.height)
        };
        let want = Drawn { view, pose, bob };
        self.drawn.as_ref() != Some(&want)
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        let (view, pose) = self.current.clone().unwrap_or_else(|| {
            (
                view_of(&ctx.world.daidai, &ctx.world.wall()),
                ctx.world.daidai.pose,
            )
        });
        let (w, h) = (ctx.canvas.width(), ctx.canvas.height());
        let stale = self
            .base
            .as_ref()
            .is_none_or(|(v, px)| *v != view || (px.width(), px.height()) != (w, h));
        if stale {
            daidai::daidai_base(ctx.canvas, ctx.text, &view);
            self.base = Some((view.clone(), ctx.canvas.pixmap.clone()));
        } else if let Some((_, px)) = &self.base {
            ctx.canvas.pixmap.data_mut().copy_from_slice(px.data());
        }
        // 動画のフレームがあれば動画(揺らさない。動画が動く)、無ければ PNG を揺らす
        let bob = match self.video.frame(pose) {
            Some(frame) => {
                daidai::draw_avatar(ctx.canvas, frame, 0);
                0
            }
            None => {
                let bob = self.bob(h);
                daidai::draw_avatar(ctx.canvas, self.avatars.get(pose), bob);
                bob
            }
        };
        self.height = h;
        self.drawn = Some(Drawn { view, pose, bob });
    }

    fn invalidate(&mut self) {
        self.drawn = None;
    }

    fn is_empty(&self) -> bool {
        self.empty
    }

    fn animating(&self) -> bool {
        self.video_active() && self.video.playing()
    }

    fn frame_interval(&self) -> Option<Duration> {
        self.animating().then(|| self.video.interval())
    }
}

/// `png` の fixture 用: フィードの見本で 1 枚描く。
pub fn render_fixture(
    canvas: &mut Canvas,
    text: &mut miharashi_render::TextEngine,
    avatars: &AvatarSet,
    view: &DaidaiView,
    pose: Pose,
    secs: f32,
) {
    daidai::daidai_screen(
        canvas,
        text,
        view,
        avatars.get(pose),
        daidai::bob_offset(secs, canvas.height()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daidai::parse_feed;
    use crate::screen::assert_key_labels_match_actions;
    use chrono::TimeZone;
    use miharashi_render::TextEngine;
    use std::time::Duration;

    fn wall(h: u32, m: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 11, 8, h, m, 0)
            .single()
            .unwrap()
    }

    fn world_with(feed: &str, at: chrono::DateTime<chrono::Local>) -> World {
        World {
            daidai: parse_feed(feed),
            wall: Some(at),
            ..World::default()
        }
    }

    /// 12:00(テストの壁時計と同じタイムゾーン)で期限が切れるカードと、期限なしのカード
    fn feed() -> String {
        format!(
            r#"{{"version":1,"avatar_pose":"working","cards":[
            {{"id":"a","kind":"morning","title":"おはよう","body":"朝です","expires_at":"{}"}},
            {{"id":"b","kind":"request","title":"お願い","body":"確認","expires_at":null}}]}}"#,
            wall(12, 0).to_rfc3339()
        )
    }

    fn render_once(s: &mut DaidaiScreen, w: &World) -> Canvas {
        let mut canvas = Canvas::new(960, 540);
        let mut text = TextEngine::new("Noto Sans CJK JP");
        let mut ctx = RenderContext {
            canvas: &mut canvas,
            text: &mut text,
            world: w,
            covered: false,
            force_overlay: false,
        };
        s.render(&mut ctx);
        canvas
    }

    #[test]
    fn id_and_keys() {
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        assert_eq!(s.id(), "daidai");
        assert_key_labels_match_actions(&mut s, Instant::now());
    }

    #[test]
    fn expired_cards_are_not_drawn_and_the_screen_reports_empty() {
        // 12:00 を過ぎると期限付きのカードが消え、期限なしのカードだけが残る
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        let t0 = Instant::now();
        s.on_enter(t0);
        let before = world_with(&feed(), wall(11, 59));
        s.tick(&before, t0);
        assert!(!s.is_empty());
        assert_eq!(s.current.as_ref().unwrap().0.cards.len(), 2);
        let after = world_with(&feed(), wall(12, 0));
        s.tick(&after, t0);
        assert_eq!(s.current.as_ref().unwrap().0.cards.len(), 1);
        assert_eq!(s.current.as_ref().unwrap().0.cards[0].title, "お願い");
        // 全部切れれば空
        let only_expired =
            r#"{"version":1,"cards":[{"title":"x","expires_at":"2020-01-01T00:00:00Z"}]}"#;
        s.tick(&world_with(only_expired, wall(9, 0)), t0);
        assert!(s.is_empty());
        let wa = world_with(only_expired, wall(9, 0));
        let a = render_once(&mut s, &wa);
        let wb = world_with("", wall(9, 0));
        s.tick(&wb, t0);
        let b = render_once(&mut s, &wb);
        assert_eq!(
            a.pixmap.data(),
            b.pixmap.data(),
            "期限切れだけのフィードは、フィード無しと同じ絵"
        );
    }

    #[test]
    fn broken_feeds_render_the_empty_state_without_panicking() {
        for feed in ["", "{", "[]", "null", r#"{"version":9}"#, "\u{0}"] {
            let mut s = DaidaiScreen::new(AvatarSet::load(""));
            let t0 = Instant::now();
            s.on_enter(t0);
            let w = world_with(feed, wall(7, 12));
            s.tick(&w, t0);
            assert!(s.is_empty(), "{feed:?}");
            let c = render_once(&mut s, &w);
            assert!(c.pixmap.data().iter().any(|&b| b != 0));
        }
    }

    #[test]
    fn redraws_only_when_the_picture_changes_and_the_avatar_bobs() {
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        let t0 = Instant::now();
        s.on_enter(t0);
        let w = world_with(&feed(), wall(8, 0));
        assert!(s.tick(&w, t0), "最初は描く");
        render_once(&mut s, &w);
        // 揺れの画素が変わらない間は描き直さない
        assert!(!s.tick(&w, t0 + Duration::from_millis(10)));
        // 周期の 1/4 では揺れが最大 → 描き直す
        let quarter = Duration::from_secs_f32(daidai::BOB_PERIOD_SECS / 4.0);
        assert!(s.tick(&w, t0 + quarter));
        // 描いたあとはまた静か。同じ揺れなら変わらない
        let bobbed = render_once(&mut s, &w);
        assert!(!s.tick(&w, t0 + quarter));
        // 揺れが違えば絵も違う(アバターだけが動く)
        s.on_enter(t0);
        s.tick(&w, t0);
        let rest = render_once(&mut s, &w);
        assert_ne!(rest.pixmap.data(), bobbed.pixmap.data());
        // 分が変わると描き直す
        let w2 = world_with(&feed(), wall(8, 1));
        assert!(s.tick(&w2, t0));
        // ポーズが変わっても描き直す
        render_once(&mut s, &w2);
        let mut w3 = world_with(&feed(), wall(8, 1));
        w3.daidai.pose = Pose::Making;
        assert!(s.tick(&w3, t0));
    }

    #[test]
    fn hidden_screen_never_asks_for_a_redraw_but_tracks_emptiness() {
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        let w = world_with(&feed(), wall(8, 0));
        assert!(!s.tick(&w, Instant::now()));
        assert!(!s.is_empty());
    }

    #[test]
    fn pose_changes_the_avatar_drawn() {
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        let t0 = Instant::now();
        s.on_enter(t0);
        let mut w = world_with(&feed(), wall(8, 0));
        w.daidai.pose = Pose::Idle;
        s.tick(&w, t0);
        let idle = render_once(&mut s, &w);
        w.daidai.pose = Pose::Milestone;
        s.tick(&w, t0);
        let milestone = render_once(&mut s, &w);
        assert_ne!(idle.pixmap.data(), milestone.pixmap.data());
    }

    #[test]
    fn the_cached_layer_does_not_leak_a_previous_bob() {
        // 取っておいた層にアバターを重ねたものと、毎回全部描いたものが一致する
        let mut s = DaidaiScreen::new(AvatarSet::load(""));
        let t0 = Instant::now();
        s.on_enter(t0);
        let w = world_with(&feed(), wall(8, 0));
        s.tick(&w, t0);
        render_once(&mut s, &w);
        let quarter = Duration::from_secs_f32(daidai::BOB_PERIOD_SECS / 4.0);
        s.tick(&w, t0 + quarter);
        let mut canvas = Canvas::new(960, 540);
        let mut text = TextEngine::new("Noto Sans CJK JP");
        let mut ctx = RenderContext {
            canvas: &mut canvas,
            text: &mut text,
            world: &w,
            covered: false,
            force_overlay: false,
        };
        s.render(&mut ctx);
        let mut fresh = Canvas::new(960, 540);
        let avatars = AvatarSet::load("");
        render_fixture(
            &mut fresh,
            &mut text,
            &avatars,
            &view_of(&w.daidai, &w.wall()),
            Pose::Working,
            quarter.as_secs_f32(),
        );
        assert_eq!(canvas.pixmap.data(), fresh.pixmap.data());
    }

    // ---- ポーズ動画(Issue #99)----

    use crate::daidai_video::VideoPlayback;
    use crate::daidai_video::tests::{Stats, fake_opener};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Probe = (Rc<RefCell<Stats>>, Rc<RefCell<Vec<Pose>>>);

    /// 192 枚(24fps)の偽の動画つきの画面。`missing` のポーズは動画が無い(PNG)。
    fn with_video(missing: &'static [Pose]) -> (DaidaiScreen, Probe) {
        let stats = Rc::new(RefCell::new(Stats::default()));
        let opens = Rc::new(RefCell::new(Vec::new()));
        let opener = fake_opener(192, 24.0, missing, stats.clone(), opens.clone());
        let s =
            DaidaiScreen::new(AvatarSet::load("")).with_video(VideoPlayback::new(Some(opener), 12));
        (s, (stats, opens))
    }

    /// 24fps の動画の `n` 枚目の真ん中の時刻
    fn at(t0: Instant, n: f64) -> Instant {
        t0 + Duration::from_secs_f64(n / 24.0)
    }

    fn world_pose(pose: Pose) -> World {
        let mut w = world_with(&feed(), wall(8, 0));
        w.daidai.pose = pose;
        w
    }

    /// アバターの中心(動画の絵は 1 色なので、その色の赤)
    fn avatar_centre_red(c: &Canvas) -> u8 {
        let g = daidai::avatar_geometry(c.width(), c.height());
        let (x, y) = (
            g.x as u32 + g.size as u32 / 2,
            g.y as u32 + g.size as u32 / 2,
        );
        c.pixmap.data()[((y * c.width() + x) * 4) as usize]
    }

    #[test]
    fn a_playing_clip_animates_with_an_interval_cap_only_while_shown_and_on() {
        let (mut s, (stats, opens)) = with_video(&[]);
        let w = world_pose(Pose::Working);
        let t0 = Instant::now();
        // まだ見えていない: 何も読まず、動かさない
        assert!(!s.tick(&w, t0));
        assert!(!s.animating());
        assert_eq!(s.frame_interval(), None);
        assert!(opens.borrow().is_empty(), "非表示では動画を開かない");
        // 見えて、点いている: 開いて再生を始める。毎周期描き、間隔の上限がつく
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        assert!(s.tick(&w, t0));
        assert!(s.animating());
        let interval = s.frame_interval().expect("再生中は間隔の上限");
        assert!(
            (interval.as_secs_f64() - 1.0 / 12.0).abs() < 1e-9,
            "{interval:?}"
        );
        assert!(s.tick(&w, at(t0, 2.5)), "描画の周期ごとに描き直す");
        assert_eq!(stats.borrow().decoded, 3);
        // 消灯: デコーダーを捨てて、何も読まず、動かさない
        s.set_presentation(true, false, false, at(t0, 3.0));
        assert!(!s.animating());
        assert_eq!(s.frame_interval(), None);
        let frozen = stats.borrow().decoded;
        for k in 4..40 {
            s.tick(&w, at(t0, f64::from(k) + 0.5));
        }
        assert_eq!(stats.borrow().decoded, frozen, "消灯中は読まない");
        assert_eq!(opens.borrow().len(), 1);
        // 点灯したら、先頭から再開
        s.set_presentation(true, false, true, at(t0, 40.0));
        assert!(s.tick(&w, at(t0, 40.5)));
        assert!(s.animating());
        assert_eq!(opens.borrow().len(), 2);
        // アラート・メニューが載っているあいだは、読み進めず、最後のフレームを出したまま
        let held = render_once(&mut s, &w);
        let decoded = stats.borrow().decoded;
        s.set_presentation(true, true, true, at(t0, 41.0));
        assert!(!s.animating());
        assert_eq!(s.frame_interval(), None);
        assert!(!s.tick(&w, at(t0, 50.5)), "動かない絵は描き直さない");
        assert_eq!(
            stats.borrow().decoded,
            decoded,
            "載っているあいだは読まない"
        );
        assert_eq!(
            avatar_centre_red(&render_once(&mut s, &w)),
            avatar_centre_red(&held),
            "最後のフレームのまま"
        );
        // 閉じたら続きから(デコーダーは持ったまま。開き直さない)
        s.set_presentation(true, false, true, at(t0, 60.0));
        assert!(s.tick(&w, at(t0, 60.5)));
        assert!(s.animating());
        assert_eq!(opens.borrow().len(), 2);
        // 別の画面へ移ると止まる
        s.on_leave(at(t0, 61.0));
        s.set_presentation(false, false, true, at(t0, 61.0));
        assert!(!s.animating());
        let frozen = stats.borrow().decoded;
        assert!(!s.tick(&w, at(t0, 90.5)), "非表示は再描画を求めない");
        assert_eq!(stats.borrow().decoded, frozen);
    }

    #[test]
    fn the_video_frame_replaces_the_png_in_the_avatar_frame_without_bobbing() {
        let (mut s, _) = with_video(&[]);
        let w = world_pose(Pose::Working);
        let t0 = Instant::now();
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        s.tick(&w, t0);
        let first = render_once(&mut s, &w);
        assert_eq!(avatar_centre_red(&first), 0, "動画の 1 枚目");
        // 5 枚進む: 揺れの周期の 1/4 まで待っても、位置は動かず、絵だけが変わる
        s.tick(&w, at(t0, 5.5));
        let later = render_once(&mut s, &w);
        assert_eq!(avatar_centre_red(&later), 5);
        // 動画の絵は 1 色なので、アバターの枠(角丸の切り抜き)を除いて、背景・カード・時計は変わらない
        let g = daidai::avatar_geometry(960, 540);
        let corner = ((g.y as u32 * 960 + g.x as u32) * 4) as usize;
        // 角丸の切り抜き: 枠の角の画素は動画の色にならず、後ろの背景のまま(PNG のときと同じ)
        let (mut png, _) = with_video(&[Pose::Working]);
        png.on_enter(t0);
        png.set_presentation(true, false, true, t0);
        png.tick(&w, t0);
        let png_px = render_once(&mut png, &w);
        assert_eq!(
            &later.pixmap.data()[corner..corner + 4],
            &png_px.pixmap.data()[corner..corner + 4],
            "四隅は背景のまま"
        );
        // 動画の外(右側のカードの領域)は PNG のときと同じ
        let card = ((270 * 960 + 700) * 4) as usize;
        assert_eq!(
            &later.pixmap.data()[card..card + 4],
            &png_px.pixmap.data()[card..card + 4]
        );
    }

    #[test]
    fn a_pose_without_a_clip_keeps_the_png_and_its_bob() {
        // 動画が無いポーズ(Making)は、これまでどおり PNG で、動かし続けない
        let (mut s, (_, opens)) = with_video(&[Pose::Making]);
        let t0 = Instant::now();
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        let w = world_pose(Pose::Making);
        assert!(s.tick(&w, t0), "最初は描く");
        let a = render_once(&mut s, &w);
        assert!(!s.animating());
        assert_eq!(s.frame_interval(), None);
        assert!(
            !s.tick(&w, t0 + Duration::from_millis(10)),
            "PNG は変わるときだけ描く"
        );
        let quarter = Duration::from_secs_f32(daidai::BOB_PERIOD_SECS / 4.0);
        assert!(s.tick(&w, t0 + quarter), "PNG の揺れは続く");
        // 動画の機能を使わない画面と同じ絵
        let mut plain = DaidaiScreen::new(AvatarSet::load(""));
        plain.on_enter(t0);
        plain.tick(&w, t0);
        assert_eq!(
            render_once(&mut plain, &w).pixmap.data(),
            a.pixmap.data(),
            "動画が無ければ、動画なしの画面と同じ絵"
        );
        assert_eq!(
            *opens.borrow(),
            [Pose::Making],
            "開いて断られたら、開き直さない"
        );
        for k in 1..6 {
            s.tick(&w, t0 + Duration::from_secs(k));
        }
        assert_eq!(opens.borrow().len(), 1);
    }

    #[test]
    fn a_pose_change_switches_between_clip_and_png() {
        let (mut s, (_, opens)) = with_video(&[Pose::Idle]);
        let t0 = Instant::now();
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        // idle(動画なし)→ working(動画あり)
        s.tick(&world_pose(Pose::Idle), t0);
        assert!(!s.animating());
        let w = world_pose(Pose::Working);
        assert!(s.tick(&w, at(t0, 1.5)));
        assert!(s.animating(), "動画のあるポーズに変わったら再生");
        assert_eq!(avatar_centre_red(&render_once(&mut s, &w)), 0);
        // working → idle(動画なし)に戻ると PNG
        let w = world_pose(Pose::Idle);
        s.tick(&w, at(t0, 3.5));
        assert!(!s.animating());
        // 開けなかった idle は開き直さない
        assert_eq!(*opens.borrow(), [Pose::Idle, Pose::Working]);
    }

    #[test]
    fn a_clip_that_dies_midway_is_redrawn_as_the_png_not_left_on_screen() {
        struct Dies(u32);
        impl crate::daidai_video::FrameSource for Dies {
            fn decode_next(&mut self) -> Result<(), String> {
                if self.0 == 0 {
                    return Err("壊れた".into());
                }
                self.0 -= 1;
                Ok(())
            }
            fn frame(&mut self) -> Result<Pixmap, String> {
                let mut p = Pixmap::new(1, 1).unwrap();
                p.fill(miharashi_render::canvas::rgba(200, 0, 0, 255));
                Ok(p)
            }
            fn fps(&self) -> f64 {
                24.0
            }
        }
        let mut s = DaidaiScreen::new(AvatarSet::load("")).with_video(VideoPlayback::new(
            Some(Box::new(|_| Ok(Box::new(Dies(3))))),
            12,
        ));
        let t0 = Instant::now();
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        let w = world_pose(Pose::Working);
        assert!(s.tick(&w, t0));
        assert_eq!(avatar_centre_red(&render_once(&mut s, &w)), 200, "動画の絵");
        // 読めなくなる周期: その場で PNG に戻り、描き直しを求める(揺れが 0 でも動画の絵を残さない)
        let mut died_at = None;
        for k in 1..8 {
            let r = s.tick(&w, at(t0, f64::from(k) * 2.0 + 0.5));
            if !s.animating() {
                assert!(r, "PNG に戻った周期で描き直しを求める");
                died_at = Some(k);
                break;
            }
            render_once(&mut s, &w);
        }
        assert!(died_at.is_some(), "動画は途中で止まる");
        // 動画の絵(赤 200 の 1 色)は残らず、動画の無い画面と同じ PNG の絵になる(同じ時刻の揺れ)
        let later = t0 + Duration::from_millis(700);
        s.tick(&w, later);
        let mut plain = DaidaiScreen::new(AvatarSet::load(""));
        plain.on_enter(t0);
        plain.tick(&w, later);
        assert_eq!(
            render_once(&mut s, &w).pixmap.data(),
            render_once(&mut plain, &w).pixmap.data()
        );
    }

    /// ScreenManager を通しても、動画が再生中の画面だけが動き続け、間隔の上限がつく。
    #[test]
    fn manager_animates_only_while_the_clip_plays_on_the_shown_screen() {
        use crate::app::manager::ScreenManager;
        use crate::data::{DummyData, dummy_snapshot};
        use crate::screen::App;
        let (s, _) = with_video(&[]);
        let mut app = ScreenManager::new(
            vec![Box::new(s)],
            "daidai",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            crate::app::text_engine(""),
            Duration::from_secs(60),
        );
        let t0 = Instant::now();
        app.tick(t0);
        assert!(App::animating(&app));
        let interval = App::frame_interval(&app).expect("上限");
        assert!((interval.as_secs_f64() - 1.0 / 12.0).abs() < 1e-9);
        assert!(app.tick(at(t0, 2.5)), "再生中は毎周期描き直す");
        // 消灯・復帰
        app.set_display_on(false, at(t0, 3.0));
        assert!(!App::animating(&app), "消灯中は動かさない");
        assert_eq!(App::frame_interval(&app), None);
        app.set_display_on(true, at(t0, 4.0));
        app.tick(at(t0, 4.5));
        assert!(App::animating(&app), "点灯したら再開");
        // メニューが載っているあいだは動かさず、閉じたら戻る
        assert!(app.on_key(crate::screen::KeyPress::Menu));
        app.tick(at(t0, 5.5));
        assert!(app.menu_open());
        assert!(!App::animating(&app), "メニュー中は継続描画にしない");
        assert_eq!(App::frame_interval(&app), None);
        assert!(app.on_key(crate::screen::KeyPress::Menu));
        app.tick(at(t0, 6.5));
        assert!(!app.menu_open());
        assert!(App::animating(&app), "閉じると戻る");
    }
}
