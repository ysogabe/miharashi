//! だいだい画面(`daidai`)。Muse からの通知カードを縦に並べ、ドット絵のアバターを小さく上下に揺らす。
//! 操作は無い(4/6 の巡回とメニューだけ)。カードが無いときはアバターと時計の控えめな表示になる。
//!
//! 再描画は「見た目が変わったとき」だけ要求する(カード・ポーズ・分・揺れの画素)。揺れは整数画素に丸めるので、
//! 揺れの周期のあいだに数回しか描き直さず、実行ループを動かし続ける(`animating`)必要もない。
//! カードと背景の層は取っておき、毎フレームはそれを戻してアバターだけを重ねる。
use crate::daidai::{AvatarSet, DaidaiSnapshot};
use crate::screen::{RenderContext, Screen, ScreenId, World};
use chrono::Datelike;
use miharashi_render::daidai::{self, CardView, DaidaiView, Pose};
use miharashi_render::{Canvas, Pixmap};
use std::time::Instant;

/// 描き終えた内容(これと違うときだけ再描画を要求する)。
#[derive(Clone, PartialEq, Eq)]
struct Drawn {
    view: DaidaiView,
    pose: Pose,
    bob: i32,
}

pub struct DaidaiScreen {
    avatars: AvatarSet,
    visible: bool,
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
            visible: false,
            entered: now,
            last_now: now,
            current: None,
            drawn: None,
            base: None,
            height: 1080,
            empty: true,
        }
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
        let (view, pose) = self.current.clone().expect("直前に入れた");
        let want = Drawn {
            view,
            pose,
            bob: self.bob(self.height),
        };
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
        let bob = self.bob(h);
        daidai::draw_avatar(ctx.canvas, self.avatars.get(pose), bob);
        self.height = h;
        self.drawn = Some(Drawn { view, pose, bob });
    }

    fn invalidate(&mut self) {
        self.drawn = None;
    }

    fn is_empty(&self) -> bool {
        self.empty
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
}
