//! 画面の登録と切り替え、メニュー、アラート層の重ね描き、共有データ(`World`)の更新。runtime からは `App` として見える。
//! キーは docs/DESIGN-screens.md §4.1: 4/6 = 画面の巡回、0 = メニュー、5・8/2 = 表示中の画面の中の操作。
//! 動かない線が多い画面には、数分に一度、焼き付き対策の演出(グリッチ → 暗転 → 再表示)を入れ、
//! そのたびに画面全体の位置を数 px ずらす。
//! 自動巡回(§4.2)が有効なら、設定の順番と秒数で画面を回す。操作があればしばらく止め、時間がたったら次の手順から戻る。
use super::DataSource;
use crate::config::RotationStep;
use crate::screen::{
    App, Composite, KeyPress, PanelRotation, PanelView, Provider, RenderContext, Screen,
    ScreenAction, ScreenInfo, World,
};
use miharashi_render::alert::{self, AlertView};
use miharashi_render::glitch;
use miharashi_render::menu;
use miharashi_render::{Canvas, TextEngine};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// 自動巡回の手順(画面の添字に直したもの)。
struct Step {
    screen: usize,
    secs: Duration,
    skip_if_empty: bool,
}

/// 自動巡回の状態。
struct Rotation {
    steps: Vec<Step>,
    /// 操作のあと止める時間
    pause: Duration,
    /// 今の手順
    index: usize,
    /// 今の手順の残り時間。アラートとメニューの間は減らさない
    remaining: Duration,
    /// 操作で止めている間の終わり
    paused_until: Option<Instant>,
    /// 前回の tick の時刻(残り時間を減らす)
    last: Instant,
}

pub struct ScreenManager {
    /// 登録した画面。並び順が切り替わる順と、操作パネルの画面一覧の順になる
    screens: Vec<Box<dyn Screen>>,
    current: usize,
    world: World,
    /// 実測値(1 秒ごと)
    data: Box<dyn DataSource>,
    /// 毎周期 `World` に写すもの(写真フィードの状態)
    providers: Vec<Box<dyn Provider>>,
    text: TextEngine,
    next_tick: Instant,
    tick_len: Duration,
    /// GPU 経路の重ね絵を次の描画で描き直させる(時刻の更新、キー、アラートの変化)
    force_overlay: bool,
    /// メニューを開いているときの、選んでいる画面の添字
    menu: Option<usize>,
    /// アラート(地震・災害)の受け口(無効なら None)
    alerts: Option<Receiver<AlertView>>,
    /// 表示中のアラートと、閉じる時刻
    alert: Option<(AlertView, Instant)>,
    alert_len: Duration,
    /// runtime が知らせる出力の電源状態。Agent 演出は消灯中に止める。
    display_on: bool,
    /// 焼き付き対策の間隔と、入れる画面の id(無効なら None)
    refresh: Option<(Duration, Vec<String>)>,
    /// 次の演出の時刻(対象の画面を続けて出しているあいだだけ近づく)
    refresh_due: Instant,
    /// 演出中なら (開始時刻, フレームの番号)
    refreshing: Option<(Instant, u32)>,
    /// 演出の元にする画面(演出の最初に 1 回だけ描く)
    clean: Option<Canvas>,
    /// 演出をした回数。画面全体をずらす位置(`glitch::shift_offset`)もこれで決める
    refreshes: usize,
    /// 自動巡回(無効なら None)
    rotation: Option<Rotation>,
    pub ticks: u64,
    pub keys: u64,
}

impl ScreenManager {
    /// `start` は最初に出す画面の id。登録に無い id なら先頭の画面にする。
    pub fn new(
        screens: Vec<Box<dyn Screen>>,
        start: &str,
        data: Box<dyn DataSource>,
        system: miharashi_render::screens::Snapshot,
        text: TextEngine,
        alert_len: Duration,
    ) -> Self {
        assert!(!screens.is_empty(), "画面が 1 つもない");
        let current = screens.iter().position(|s| s.id() == start).unwrap_or(0);
        let now = Instant::now();
        let mut m = Self {
            screens,
            current,
            world: World {
                system,
                ..World::default()
            },
            data,
            providers: Vec::new(),
            text,
            next_tick: now,
            tick_len: Duration::from_secs(1),
            force_overlay: true,
            menu: None,
            alerts: None,
            alert: None,
            alert_len,
            display_on: true,
            refresh: None,
            refresh_due: now,
            refreshing: None,
            clean: None,
            refreshes: 0,
            rotation: None,
            ticks: 0,
            keys: 0,
        };
        m.screens[current].on_enter(now);
        m
    }

    /// 毎周期 `World` に写すものを足す。
    pub fn with_provider(mut self, mut p: Box<dyn Provider>) -> Self {
        // 最初の tick より前の操作パネルの状態にも、Provider の初期値を出す
        p.update(&mut self.world);
        self.providers.push(p);
        self
    }

    /// 焼き付き対策: `ids` の画面を `every` ごとに暗転して描き直す。
    pub fn with_refresh(mut self, every: Duration, ids: Vec<String>) -> Self {
        self.refresh_due = Instant::now() + every;
        self.refresh = Some((every, ids));
        self
    }

    /// 自動巡回を始める。登録に無い画面の手順は飛ばす(航空機や室内環境の画面が無効なとき)。
    /// 手順が 1 つも残らなければ巡回しない。最初の手順の画面から始める。
    pub fn with_rotation(mut self, steps: &[RotationStep], pause: Duration) -> Self {
        let steps: Vec<Step> = steps
            .iter()
            .filter_map(|st| {
                let i = self.screens.iter().position(|s| s.id() == st.screen);
                if i.is_none() {
                    eprintln!(
                        "rotation: 画面 {} は出していないので、この手順は飛ばす",
                        st.screen
                    );
                }
                Some(Step {
                    screen: i?,
                    secs: Duration::from_secs(st.secs),
                    skip_if_empty: st.skip_if_empty,
                })
            })
            .collect();
        let Some(first) = steps.first() else {
            eprintln!("rotation: 回せる画面が無いので巡回しない");
            return self;
        };
        let now = Instant::now();
        let (screen, remaining) = (first.screen, first.secs);
        self.switch_to(screen, now);
        self.rotation = Some(Rotation {
            steps,
            pause,
            index: 0,
            remaining,
            paused_until: None,
            last: now,
        });
        self
    }

    /// 自動巡回の状態("on" / "paused")。無効なら None。
    pub fn rotation_state(&self) -> Option<&'static str> {
        self.rotation.as_ref().map(|r| {
            if r.paused_until.is_some() {
                "paused"
            } else {
                "on"
            }
        })
    }

    /// 操作があったので巡回を止める。
    fn pause_rotation(&mut self, now: Instant) {
        if let Some(r) = self.rotation.as_mut() {
            r.paused_until = Some(now + r.pause);
        }
    }

    /// 自動巡回を進める。戻り値: 画面を切り替えたか。
    fn tick_rotation(&mut self, now: Instant) -> bool {
        let frozen = self.alert.is_some() || self.menu.is_some();
        let Self {
            rotation, screens, ..
        } = self;
        let Some(r) = rotation.as_mut() else {
            return false;
        };
        let dt = now.saturating_duration_since(r.last);
        r.last = now;
        if let Some(until) = r.paused_until {
            if now < until {
                return false;
            }
            // 止める前の手順の続き(次の手順)から
            r.paused_until = None;
            r.remaining = Duration::ZERO;
        }
        // アラートとメニューの間は時間を進めない
        if frozen {
            return false;
        }
        r.remaining = r.remaining.saturating_sub(dt);
        if !r.remaining.is_zero() {
            return false;
        }
        let n = r.steps.len();
        let next = (1..=n).map(|k| (r.index + k) % n).find(|&i| {
            let st = &r.steps[i];
            !(st.skip_if_empty && screens[st.screen].is_empty())
        });
        let Some(i) = next else {
            // どの手順も飛ばす状態なら、今の画面のまま次の手順の時間だけ待つ
            r.remaining = r.steps[r.index].secs;
            return false;
        };
        r.index = i;
        r.remaining = r.steps[i].secs;
        let screen = r.steps[i].screen;
        let changed = screen != self.current;
        self.switch_to(screen, now);
        self.force_overlay |= changed;
        changed
    }

    /// 焼き付き対策の演出中か(テスト用)。
    #[cfg(test)]
    pub fn refreshing(&self) -> bool {
        self.refreshing.is_some()
    }

    /// 演出をやめ(途中なら打ち切り)、次の演出までの時間を数え直す。
    fn reset_refresh(&mut self, now: Instant) {
        if self.refreshing.take().is_some() {
            self.clean = None;
            // GPU 経路の背景や重ね絵は、演出のあいだ使っていないので描き直させる
            self.screens[self.current].invalidate();
            self.force_overlay = true;
        }
        if let Some((every, _)) = &self.refresh {
            self.refresh_due = now + *every;
        }
    }

    /// 焼き付き対策を進める。戻り値: 再描画が必要か。
    fn tick_refresh(&mut self, now: Instant) -> bool {
        let Some((_, ids)) = &self.refresh else {
            return false;
        };
        if let Some((t0, _)) = self.refreshing {
            if now.duration_since(t0).as_secs_f32() >= glitch::REFRESH_SECS {
                self.reset_refresh(now);
            }
            return true;
        }
        let eligible = ids.iter().any(|id| id == self.screen_id())
            && self.alert.is_none()
            && self.menu.is_none();
        if !eligible {
            self.reset_refresh(now);
        } else if now >= self.refresh_due {
            self.refreshing = Some((now, 0));
            // 位置を次へ。乱れと暗転のあいだに動くので、ずれた瞬間は目立たない
            self.refreshes += 1;
            return true;
        }
        false
    }

    /// 表示中の画面をずらす量 [px]。焼き付き対策の対象の画面だけ。
    fn offset(&self, canvas_h: u32) -> (i32, i32) {
        match &self.refresh {
            Some((_, ids)) if ids.iter().any(|id| id == self.screen_id()) => {
                glitch::shift_offset(self.refreshes, canvas_h)
            }
            _ => (0, 0),
        }
    }

    /// 演出の 1 フレームを描く。元の画面は最初に 1 回だけ描いて取っておく。
    fn render_refresh(&mut self, canvas: &mut Canvas) {
        let Some((t0, frame)) = self.refreshing.as_mut() else {
            return;
        };
        *frame = frame.wrapping_add(1);
        let (t, seed) = (t0.elapsed().as_secs_f32(), *frame);
        if self
            .clean
            .as_ref()
            .is_none_or(|c| (c.width(), c.height()) != (canvas.width(), canvas.height()))
        {
            let (dx, dy) = self.offset(canvas.height());
            let mut c = Canvas::new(canvas.width(), canvas.height());
            let mut ctx = RenderContext {
                canvas: &mut c,
                text: &mut self.text,
                world: &self.world,
                covered: true,
                force_overlay: true,
            };
            self.screens[self.current].render(&mut ctx);
            glitch::shift(&mut c, dx, dy);
            self.clean = Some(c);
        }
        glitch::refresh_frame(canvas, self.clean.as_ref().expect("上で描いた"), t, seed);
    }

    /// アラートの受け口をつなぐ。
    pub fn with_alerts(mut self, rx: Option<Receiver<AlertView>>) -> Self {
        self.alerts = rx;
        self
    }

    /// 表示中の画面の id。
    pub fn screen_id(&self) -> &'static str {
        self.screens[self.current].id()
    }

    /// 画面が読んでいる実測値と時刻(テスト用)。
    #[cfg(test)]
    pub fn world(&self) -> &miharashi_render::screens::Snapshot {
        &self.world.system
    }

    /// アラートを表示中か(画面を消していても点ける)。
    pub fn alert_active(&self) -> bool {
        self.alert.is_some()
    }

    fn switch_to(&mut self, i: usize, now: Instant) {
        if i == self.current {
            return;
        }
        self.reset_refresh(now);
        self.screens[self.current].on_leave(now);
        self.current = i;
        self.screens[i].on_enter(now);
        self.update_presentation(now);
    }

    fn update_presentation(&mut self, now: Instant) {
        let covered = self.alert.is_some() || self.menu.is_some();
        for (i, screen) in self.screens.iter_mut().enumerate() {
            screen.set_presentation(i == self.current, covered, self.display_on, now);
        }
    }

    pub fn set_display_on(&mut self, on: bool, now: Instant) {
        if self.display_on != on {
            self.display_on = on;
            self.update_presentation(now);
        }
    }

    /// メニューを開いているか(テスト用)。
    #[cfg(test)]
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// 戻り値: 再描画が必要か。
    pub fn on_key(&mut self, key: KeyPress) -> bool {
        self.keys += 1;
        self.force_overlay = true;
        let now = Instant::now();
        self.pause_rotation(now);
        // 演出中のキーは演出を打ち切ってから、ふつうに扱う
        self.reset_refresh(now);
        // アラート表示中のキーは閉じるだけにする
        if self.alert.take().is_some() {
            return true;
        }
        let n = self.screens.len();
        // メニュー中は画面に操作を渡さない。8/2(と 4/6)で選び、5 で移り、0 で閉じる
        if let Some(sel) = self.menu {
            match key {
                KeyPress::Up | KeyPress::Prev => self.menu = Some((sel + n - 1) % n),
                KeyPress::Down | KeyPress::Next => self.menu = Some((sel + 1) % n),
                KeyPress::Select => {
                    self.menu = None;
                    self.switch_to(sel, now);
                }
                KeyPress::Menu => self.menu = None,
            }
            return true;
        }
        let action = match key {
            KeyPress::Prev => {
                self.switch_to((self.current + n - 1) % n, now);
                return true;
            }
            KeyPress::Next => {
                self.switch_to((self.current + 1) % n, now);
                return true;
            }
            KeyPress::Menu => {
                self.menu = Some(self.current);
                return true;
            }
            KeyPress::Select => ScreenAction::Select,
            KeyPress::Up => ScreenAction::Up,
            KeyPress::Down => ScreenAction::Down,
        };
        // 画面で使わない操作は何もしない
        let _ = self.screens[self.current].handle_action(action, now);
        true
    }

    /// 指定した id の画面にする(操作パネル)。登録に無い id なら何もしない。戻り値: 再描画が必要か。
    pub fn show_screen(&mut self, id: &str) -> bool {
        let Some(i) = self.screens.iter().position(|s| s.id() == id) else {
            return false;
        };
        self.keys += 1;
        self.force_overlay = true;
        self.alert = None;
        self.menu = None;
        let now = Instant::now();
        self.pause_rotation(now);
        self.switch_to(i, now);
        true
    }

    /// 実行ループから毎周期呼ぶ。戻り値: 再描画が必要か。
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut redraw = false;
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
        self.update_presentation(now);
        if now >= self.next_tick {
            self.data.update(&mut self.world.system);
            super::set_clock(&mut self.world.system);
            self.ticks += 1;
            self.next_tick += self.tick_len;
            if self.next_tick < now {
                self.next_tick = now + self.tick_len;
            }
            redraw = true;
        }
        redraw |= self.tick_rotation(now);
        self.force_overlay |= redraw;
        // 非表示の画面も回す(写真の受信を続ける)。再描画は表示中の画面の分だけ見る
        for (i, s) in self.screens.iter_mut().enumerate() {
            let r = s.tick(&self.world, now);
            if i == self.current {
                redraw |= r;
            }
        }
        // 画面が受け取ったばかりの状態(写真フィード)も、この周期のうちに写す
        for p in &mut self.providers {
            p.update(&mut self.world);
        }
        redraw | self.tick_refresh(now)
    }

    pub fn render(&mut self, canvas: &mut Canvas) {
        if self.refreshing.is_some() {
            return self.render_refresh(canvas);
        }
        let titles = self.titles();
        let (dx, dy) = self.offset(canvas.height());
        let Self {
            screens,
            current,
            world,
            text,
            alert,
            menu,
            ..
        } = self;
        let mut ctx = RenderContext {
            canvas,
            text,
            world,
            covered: alert.is_some() || menu.is_some(),
            force_overlay: true,
        };
        screens[*current].render(&mut ctx);
        glitch::shift(ctx.canvas, dx, dy);
        layers(
            ctx.canvas,
            ctx.text,
            &titles,
            *current,
            *menu,
            alert.as_ref(),
        );
    }

    fn titles(&self) -> Vec<&'static str> {
        self.screens.iter().map(|s| s.title()).collect()
    }

    /// GPU 経路の描画。メニューとアラートは画面の完成画面か、写真の重ね絵の上に描く。
    pub fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        if self.refreshing.is_some() {
            self.render_refresh(canvas);
            return Composite::Canvas;
        }
        let force_overlay = std::mem::take(&mut self.force_overlay);
        let titles = self.titles();
        let (dx, dy) = self.offset(canvas.height());
        let Self {
            screens,
            current,
            world,
            text,
            alert,
            menu,
            ..
        } = self;
        let mut ctx = RenderContext {
            canvas,
            text,
            world,
            covered: alert.is_some() || menu.is_some(),
            force_overlay,
        };
        let mut comp = screens[*current].render_composite(&mut ctx);
        // 焼き付き対策の位置のずらし。写真(重ね絵つき)は対象にしない
        match &mut comp {
            Composite::Canvas => glitch::shift(ctx.canvas, dx, dy),
            Composite::Radar {
                background_changed,
                geom,
                ..
            } => {
                if *background_changed {
                    glitch::shift(ctx.canvas, dx, dy);
                }
                let (fx, fy) = (dx as f32, dy as f32);
                geom.center = (geom.center.0 + fx, geom.center.1 + fy);
                for b in &mut geom.blips {
                    (b.x, b.y) = (b.x + fx, b.y + fy);
                }
            }
            Composite::Photo { .. } => {}
            Composite::Agents { .. } => {}
        }
        let draw_alert = match &comp {
            Composite::Canvas => true,
            Composite::Photo {
                overlay_changed, ..
            } => *overlay_changed,
            // 航空機の画面は、重ねるものがあるときは Canvas で返る
            Composite::Radar { .. } => false,
            Composite::Agents { .. } => false,
        };
        if draw_alert {
            layers(
                ctx.canvas,
                ctx.text,
                &titles,
                *current,
                *menu,
                alert.as_ref(),
            );
        }
        comp
    }
}

/// 画面の上に重ねるもの。メニューの上にアラート。
fn layers(
    canvas: &mut Canvas,
    text: &mut TextEngine,
    titles: &[&str],
    current: usize,
    menu: Option<usize>,
    alert: Option<&(AlertView, Instant)>,
) {
    if let Some(sel) = menu {
        menu::menu(canvas, text, titles, current, sel);
    }
    if let Some((view, _)) = alert {
        alert::overlay(canvas, text, view);
    }
}

impl App for ScreenManager {
    fn on_key(&mut self, key: KeyPress) -> bool {
        ScreenManager::on_key(self, key)
    }

    fn tick(&mut self, now: Instant) -> bool {
        ScreenManager::tick(self, now)
    }

    fn render(&mut self, canvas: &mut Canvas) {
        ScreenManager::render(self, canvas)
    }

    fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        ScreenManager::render_composite(self, canvas)
    }

    fn show_screen(&mut self, id: &str) -> bool {
        ScreenManager::show_screen(self, id)
    }

    fn wants_screen(&self) -> bool {
        self.alert_active()
    }

    fn set_display_on(&mut self, on: bool, now: Instant) {
        ScreenManager::set_display_on(self, on, now)
    }

    fn animating(&self) -> bool {
        self.refreshing.is_some() || self.screens[self.current].animating()
    }

    fn stats(&self) -> Vec<(&'static str, String)> {
        let s = &self.world.system;
        // 並びは tools/soak_summary.py が読む順(ticks keys screen photo aircraft feed cpu)
        let mut out = vec![
            ("ticks", self.ticks.to_string()),
            ("keys", self.keys.to_string()),
            ("screen", self.screen_id().to_string()),
        ];
        for sc in &self.screens {
            sc.stats(&mut out);
        }
        let feed = self.world.photo_feed.notice.as_deref().unwrap_or("online");
        out.extend([
            ("feed", feed.to_string()),
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
        ]);
        if let Some(p) = self.screens[self.current].page() {
            out.push(("page", p.to_lowercase()));
        }
        if self.refresh.is_some() {
            out.push(("refreshes", self.refreshes.to_string()));
        }
        if let Some(r) = self.rotation_state() {
            out.push(("rotation", r.to_string()));
        }
        out
    }

    fn panel_view(&self) -> Option<PanelView> {
        Some(PanelView {
            screens: self
                .screens
                .iter()
                .map(|s| ScreenInfo {
                    id: s.id(),
                    title: s.title(),
                })
                .collect(),
            current: self.current,
            page: self.screens[self.current].page(),
            photo: self.world.photo_feed.position,
            feed_online: self.world.photo_feed.online,
            rotation: self.rotation.as_ref().map(|r| match r.paused_until {
                Some(until) => PanelRotation::Paused { until },
                None => PanelRotation::On,
            }),
        })
    }
}
