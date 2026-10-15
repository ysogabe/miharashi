//! 航空機の画面(docs/DESIGN-screens.md §2.2)。左に受信機中心のレーダー、右に注目する機体の諸元と一覧。
//! 8/2 = 注目する機体を近い順に切り替え、5 = いちばん近い機体に戻す。
use crate::aircraft::{Shared, advanced};
use crate::screen::{Composite, Outcome, RenderContext, Screen, ScreenAction, ScreenId, World};
use miharashi_render::aircraft::{self, AircraftView};
use std::time::{Duration, Instant};

/// 取得結果がこれより古ければ、受信機から応答がないものとして機体を出さない。
const STALE: Duration = Duration::from_secs(30);
/// 取得してから位置を進める上限(取得が止まったときに、どこまでも進めない)。
const MAX_ADVANCE_SECS: u64 = 10;

pub struct AircraftScreen {
    shared: Shared,
    visible: bool,
    /// 表示を始めた時刻(回る線の角度の起点)
    entered: Instant,
    /// 注目する機体の hex。None ならいちばん近い機体
    focus: Option<String>,
    /// GPU 経路(GLES)か。回る線を毎フレーム描く
    gpu: bool,
    /// CPU で最後に描いた内容(変わったときだけ描き直す)
    drawn: Option<AircraftView>,
    /// GPU 経路で、レーダーの背景を最後に描いた内容
    radar_bg: Option<AircraftView>,
    /// レーダーの下地(地図と距離の輪)。受信機と半径が同じ間は描き直さない
    radar_base: Option<aircraft::RadarBase>,
}

impl AircraftScreen {
    pub fn new(shared: Shared, gpu: bool) -> Self {
        Self {
            shared,
            visible: false,
            entered: Instant::now(),
            focus: None,
            gpu,
            drawn: None,
            radar_bg: None,
            radar_base: None,
        }
    }

    /// 今出す内容。位置は取得からの秒数(1 秒きざみ)だけ進め、注目する機体を決める。
    fn view(&self, now: Instant) -> AircraftView {
        let latest = self.shared.lock().ok().and_then(|g| g.clone());
        let Some((at, mut v)) = latest else {
            return AircraftView {
                notice: Some("受信機に接続しています…".into()),
                ..Default::default()
            };
        };
        let age = now.saturating_duration_since(at);
        if age >= STALE {
            v.planes.clear();
            v.notice = Some("受信機から応答がありません".into());
            return v;
        }
        let mut v = advanced(&v, age.as_secs().min(MAX_ADVANCE_SECS) as f64);
        v.focus = self
            .focus
            .as_ref()
            .and_then(|hex| v.planes.iter().position(|p| &p.hex == hex))
            .unwrap_or(0);
        v
    }

    fn planes(&self) -> usize {
        self.shared
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|(at, v)| (at.elapsed() < STALE) as usize * v.planes.len())
            })
            .unwrap_or(0)
    }
}

impl Screen for AircraftScreen {
    fn id(&self) -> ScreenId {
        "aircraft"
    }

    fn title(&self) -> &'static str {
        "AIRCRAFT"
    }

    fn on_enter(&mut self, now: Instant) {
        self.visible = true;
        self.entered = now;
        self.drawn = None;
        self.radar_bg = None;
    }

    fn on_leave(&mut self, _now: Instant) {
        self.visible = false;
    }

    fn tick(&mut self, _world: &World, now: Instant) -> bool {
        // GPU 経路は回る線を毎フレーム描く。CPU 経路は内容(1 秒ごとに進める位置)が変わったときだけ
        self.visible && (self.gpu || self.drawn.as_ref() != Some(&self.view(now)))
    }

    fn handle_action(&mut self, action: ScreenAction, now: Instant) -> Outcome {
        let v = self.view(now);
        let n = v.planes.len();
        let pick = match action {
            ScreenAction::Select => None,
            _ if n == 0 => None,
            ScreenAction::Down => Some((v.focus + 1) % n),
            ScreenAction::Up => Some((v.focus + n - 1) % n),
        };
        self.focus = pick.filter(|&i| i != 0).map(|i| v.planes[i].hex.clone());
        Outcome::Consumed
    }

    /// CPU で完成画面を描く。回る線は止めて描く。下地(地図)は取っておいて、毎回は作り直さない。
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        let v = self.view(Instant::now());
        aircraft::aircraft_background_cached(ctx.canvas, ctx.text, &v, &mut self.radar_base);
        let g = aircraft::radar_geometry(&v, (ctx.canvas.width(), ctx.canvas.height()));
        aircraft::sweep_cpu(ctx.canvas, &g, aircraft::STILL_SWEEP, aircraft::TRAIL);
        self.drawn = Some(v);
    }

    /// GPU 経路: 背景は内容が変わったときだけ描き、回る線と点は GPU に任せる。
    /// メニューやアラートを重ねるときは CPU で完成画面を描く。
    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        if ctx.covered {
            self.radar_bg = None;
            self.render(ctx);
            return Composite::Canvas;
        }
        let v = self.view(Instant::now());
        let changed = self.radar_bg.as_ref() != Some(&v);
        if changed {
            aircraft::aircraft_background_cached(ctx.canvas, ctx.text, &v, &mut self.radar_base);
        }
        let geom = aircraft::radar_geometry(&v, (ctx.canvas.width(), ctx.canvas.height()));
        self.radar_bg = Some(v);
        let turns = self.entered.elapsed().as_secs_f32() / aircraft::SWEEP_PERIOD_SECS;
        Composite::Radar {
            background_changed: changed,
            geom,
            sweep: turns.fract() * std::f32::consts::TAU,
        }
    }

    fn animating(&self) -> bool {
        self.gpu && self.visible
    }

    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        out.push(("aircraft", self.planes().to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miharashi_render::Canvas;
    use std::sync::{Arc, Mutex};

    fn screen(gpu: bool) -> (AircraftScreen, Shared) {
        let shared: Shared = Arc::new(Mutex::new(None));
        let mut s = AircraftScreen::new(shared.clone(), gpu);
        s.on_enter(Instant::now());
        (s, shared)
    }

    fn ctx<'a>(
        c: &'a mut Canvas,
        t: &'a mut miharashi_render::TextEngine,
        w: &'a World,
        covered: bool,
    ) -> RenderContext<'a> {
        RenderContext {
            canvas: c,
            text: t,
            world: w,
            covered,
            force_overlay: false,
        }
    }

    #[test]
    fn notice_before_the_first_fetch_and_when_stale() {
        let (s, shared) = screen(false);
        let now = Instant::now();
        assert!(s.view(now).notice.unwrap().contains("接続"));
        *shared.lock().unwrap() = Some((now, crate::aircraft::demo_view()));
        let v = s.view(now);
        assert_eq!((v.notice, v.planes.len()), (None, 5));
        let v = s.view(now + Duration::from_secs(31));
        assert!(v.planes.is_empty(), "古い結果の機体は出さない");
        assert!(v.notice.unwrap().contains("応答"));
        assert_eq!(v.time, "22:41:05", "最終受信の時刻は出す");
    }

    /// 8/2 で注目する機体を近い順に切り替え(端で折り返す)、5 でいちばん近い機体に戻す。
    /// 注目は hex で覚えるので、取得し直して順番が変わっても同じ機体を追う。
    #[test]
    fn up_down_moves_the_focus_and_select_resets() {
        let (mut s, shared) = screen(false);
        let now = Instant::now();
        let mut demo = crate::aircraft::demo_view();
        for (i, p) in demo.planes.iter_mut().enumerate() {
            p.hex = format!("X{i}");
        }
        *shared.lock().unwrap() = Some((now, demo.clone()));
        let focus = |s: &AircraftScreen| s.view(now).focus;
        s.handle_action(ScreenAction::Down, now);
        s.handle_action(ScreenAction::Down, now);
        assert_eq!(focus(&s), 2);
        s.handle_action(ScreenAction::Up, now);
        s.handle_action(ScreenAction::Up, now);
        s.handle_action(ScreenAction::Up, now);
        assert_eq!(focus(&s), 4, "先頭の前は最後へ");
        assert_eq!(s.focus.as_deref(), Some("X4"));
        let mut v = demo.clone();
        v.planes.swap(1, 4);
        *shared.lock().unwrap() = Some((now, v));
        assert_eq!(focus(&s), 1, "並びが変わっても同じ機体");
        assert_eq!(
            s.handle_action(ScreenAction::Select, now),
            Outcome::Consumed
        );
        assert_eq!(focus(&s), 0, "5 でいちばん近い機体");
        assert_eq!(s.focus, None);
    }

    /// CPU 経路は 1 秒ごとに進める位置が変わったときだけ描き直す。GPU 経路は背景を変わったときだけ描き、
    /// 回る線は毎フレーム。重ねるものがあるときは CPU の完成画面にする。
    #[test]
    fn redraws_only_when_the_view_changes() {
        let (mut s, shared) = screen(false);
        let w = World::default();
        let mut text = crate::app::text_engine("");
        let mut c = Canvas::new(640, 270);
        let t0 = Instant::now();
        *shared.lock().unwrap() = Some((t0, crate::aircraft::demo_view()));
        assert!(s.tick(&w, t0), "まだ描いていない");
        s.render(&mut ctx(&mut c, &mut text, &w, false));
        assert!(!s.tick(&w, Instant::now()), "同じ秒のうちは描き直さない");
        assert!(
            s.tick(&w, t0 + Duration::from_secs(2)),
            "位置が進んだら描き直す"
        );
        s.on_leave(t0);
        assert!(
            !s.tick(&w, t0 + Duration::from_secs(3)),
            "非表示では描かない"
        );

        let (mut s, shared) = screen(true);
        *shared.lock().unwrap() = Some((Instant::now(), crate::aircraft::demo_view()));
        assert!(s.tick(&w, Instant::now()) && s.animating());
        let radar = |s: &mut AircraftScreen, c: &mut Canvas, text: &mut _, covered| match s
            .render_composite(&mut ctx(c, text, &w, covered))
        {
            Composite::Radar {
                background_changed,
                geom,
                ..
            } => Some((background_changed, geom.blips.len())),
            _ => None,
        };
        assert_eq!(radar(&mut s, &mut c, &mut text, false), Some((true, 5)));
        assert_eq!(radar(&mut s, &mut c, &mut text, false), Some((false, 5)));
        assert_eq!(
            radar(&mut s, &mut c, &mut text, true),
            None,
            "重ねるときは Canvas"
        );
        assert_eq!(
            radar(&mut s, &mut c, &mut text, false),
            Some((true, 5)),
            "閉じたら背景を描き直す"
        );
    }
}
