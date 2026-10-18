//! 画面の登録と切り替え、メニュー、アラート層の重ね描き、共有データ(`World`)の更新。runtime からは `App` として見える。
//! キーは docs/DESIGN-screens.md §4.1: 4/6 = 画面の巡回、0 = メニュー、5・8/2 = 表示中の画面の中の操作。
//! 動かない線が多い画面には、数分に一度、焼き付き対策の演出(グリッチ → 暗転 → 再表示)を入れ、
//! そのたびに画面全体の位置を数 px ずらす。
//! 自動巡回(§4.2)が有効なら、設定の順番と秒数で画面を回す。操作があればしばらく止め、時間がたったら次の手順から戻る。
use super::DataSource;
use crate::config::RotationStep;
use crate::screen::{
    App, Composite, GlScene, KeyPress, PanelKeys, PanelRotation, PanelView, Provider,
    RenderContext, Screen, ScreenAction, ScreenInfo, Top, World,
};
use miharashi_render::alert::{self, AlertView};
use miharashi_render::canvas::rgba;
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

/// 自動巡回が回っているか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RotationMode {
    /// 回っている
    On,
    /// 操作で止めている。`until` を過ぎたら次の手順から戻る
    Paused { until: Instant },
    /// 操作パネルから止めたまま。時間では戻らず、操作しても `Paused` にしない
    Off,
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
    /// 回っている・操作で止めている・止めたまま
    mode: RotationMode,
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
    /// モニターの手動オーバーライドが force_off か(消えているあいだのパネルのキーのラベルに使う)
    display_forced_off: bool,
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
    /// GL が使えるか(`Output.gpu_max_texture.is_some()`)。起動後は変わらない(変わるのは画面ごとの失敗だけ)
    gl_available: bool,
    /// 3D が失敗した画面(`screens` と同じ並び)。再起動まで 3D を出さない
    scene_failed: Vec<bool>,
    /// 3D の失敗の回数(状態ログの `scene_fail=`)
    scene_fails: u64,
    /// アラート・メニュー専用の最上段の層(`Composite::Gl` のときだけ使う)。出ていないあいだは持たない
    top: Option<Canvas>,
    /// 前回のフレームが最上段の重ね絵を出したか。`Gl` 以外のフレームは false(= `Off` とみなす)で、
    /// false の次は `Top::Upload` にする(`Keep` は前回が `Upload` か `Keep` のときだけ)
    top_valid: bool,
    /// 前回 `Composite::Gl` で返した画面(`report_scene_failure` の対象)
    scene_screen: Option<usize>,
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
        let n_screens = screens.len();
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
            display_forced_off: false,
            refresh: None,
            refresh_due: now,
            refreshing: None,
            clean: None,
            refreshes: 0,
            rotation: None,
            gl_available: false,
            scene_failed: vec![false; n_screens],
            scene_fails: 0,
            top: None,
            top_valid: false,
            scene_screen: None,
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

    /// GL が使えるか(`Output.gpu_max_texture.is_some()`)。既定は使えない。`with_rotation` より前に渡すこと
    /// (巡回の最初の手順から、GL が要る画面を外す)。実行中に切り替える口は作らない: 変わるのは画面ごとの失敗だけ。
    pub fn with_gl_available(mut self, available: bool) -> Self {
        if self.gl_available != available {
            self.gl_available = available;
            self.gl_state_changed();
        }
        self
    }

    /// 画面 `i` の 3D を出してよいか(GL が使えて、その画面の 3D が失敗していない)。
    fn gl_ok(&self, i: usize) -> bool {
        self.gl_available && !self.scene_failed[i]
    }

    /// 自動巡回に入れてよい画面か。GL が要る画面は、GL が使えない・失敗中なら飛ばす(`skip_if_empty` に関係なく)。
    fn rotatable(&self, i: usize) -> bool {
        !self.screens[i].needs_gl() || self.gl_ok(i)
    }

    /// GL の可否・失敗の状態が変わった。GL ありの `Composite::Gl`(下地は `base` の中)と、GL なしの
    /// `Photo` / `Agents` / `Canvas`(`base` を取り出したもの)では、画面が重ね絵・背景を描いたかどうかの前提が
    /// 変わるので、重ね絵を描き直させる(でないと写真画面では切り替えのフレームからアラート・メニューが消える)。
    fn gl_state_changed(&mut self) {
        self.force_overlay = true;
        self.top_valid = false;
        self.screens[self.current].invalidate();
    }

    /// 3D が動き続けているか。GL が使えて失敗していない、点灯中の画面だけ(消灯中は描かない。
    /// 内側の画面の `animating()` / `tick` には混ぜない)。
    fn scene_animating(&self) -> bool {
        self.display_on && self.gl_ok(self.current) && self.screens[self.current].scene_animating()
    }

    /// 持っているすべての `GlScene`(終了時の解放用)。3D が失敗した画面の場面は、失敗したその場で
    /// `release` 済み(`present_scene`)なので含めない(`release` を 1 つの場面に高々 1 回にする)。
    pub fn gl_scenes(&mut self) -> Vec<&mut dyn GlScene> {
        self.screens
            .iter_mut()
            .zip(&self.scene_failed)
            .filter(|(_, failed)| !**failed)
            .filter_map(|(s, _)| s.gl_scene())
            .collect()
    }

    /// 3D の描画が失敗した。直前に `Composite::Gl` で返した画面を、再起動まで GL なしの扱いにする。
    pub fn report_scene_failure(&mut self, msg: &str) {
        let i = self.scene_screen.take().unwrap_or(self.current);
        eprintln!(
            "3D: 画面 {} の描画に失敗(再起動まで 3D を出さない): {msg}",
            self.screens[i].id()
        );
        self.scene_failed[i] = true;
        self.scene_fails += 1;
        self.gl_state_changed();
        if i != self.current {
            self.screens[i].invalidate();
        }
    }

    /// 焼き付き対策: `ids` の画面を `every` ごとに暗転して描き直す。
    pub fn with_refresh(mut self, every: Duration, ids: Vec<String>) -> Self {
        self.refresh_due = Instant::now() + every;
        self.refresh = Some((every, ids));
        self
    }

    /// 自動巡回を始める。登録に無い画面の手順は飛ばす(航空機や室内環境の画面が無効なとき)。
    /// 手順が 1 つも残らない、または GL が使えず全部が飛ばす画面なら、巡回しない(今の画面のまま)。
    /// 最初の手順の画面から始める。
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
        if steps.is_empty() {
            eprintln!("rotation: 回せる画面が無いので巡回しない");
            return self;
        }
        // 最初の手順は、GL が使えず飛ばす画面(3D だけの画面)なら次から。
        // 全部が飛ばす画面なら、今の画面のまま巡回しない(先頭の案内の画面に切り替えない)
        let Some(start) = steps.iter().position(|st| self.rotatable(st.screen)) else {
            eprintln!("rotation: GL が要る画面しか無いので巡回しない");
            return self;
        };
        let now = Instant::now();
        let (screen, remaining) = (steps[start].screen, steps[start].secs);
        self.switch_to(screen, now);
        self.rotation = Some(Rotation {
            steps,
            pause,
            index: start,
            remaining,
            mode: RotationMode::On,
            last: now,
        });
        self
    }

    /// 自動巡回の状態("on" / "paused" / "off")。無効なら None。
    pub fn rotation_state(&self) -> Option<&'static str> {
        self.rotation.as_ref().map(|r| match r.mode {
            RotationMode::On => "on",
            RotationMode::Paused { .. } => "paused",
            RotationMode::Off => "off",
        })
    }

    /// 操作パネルから巡回を再開する(`on`)・止めたままにする(`!on`)。戻り値: 状態が変わったか(パネルへ送り直す)。
    /// 再開したら次の tick ですぐ次の手順へ移る(一時停止が明けたときと同じ)。巡回が無効・同じ状態なら何もしない。
    /// 画面は選んでいないので、キーの数に入れず、一時停止にもしない。
    pub fn set_rotation(&mut self, on: bool) -> bool {
        let Some(r) = self.rotation.as_mut() else {
            return false;
        };
        match (on, r.mode) {
            (true, RotationMode::On) | (false, RotationMode::Off) => return false,
            (true, _) => {
                r.mode = RotationMode::On;
                r.remaining = Duration::ZERO;
            }
            (false, _) => r.mode = RotationMode::Off,
        }
        true
    }

    /// 操作があったので巡回を止める。止めたまま(`Off`)なら、そのままにする。
    fn pause_rotation(&mut self, now: Instant) {
        if let Some(r) = self.rotation.as_mut()
            && r.mode != RotationMode::Off
        {
            r.mode = RotationMode::Paused {
                until: now + r.pause,
            };
        }
    }

    /// 自動巡回を進める。戻り値: 画面を切り替えたか。
    fn tick_rotation(&mut self, now: Instant) -> bool {
        let frozen = self.alert.is_some() || self.menu.is_some();
        let Self {
            rotation,
            screens,
            gl_available,
            scene_failed,
            ..
        } = self;
        let Some(r) = rotation.as_mut() else {
            return false;
        };
        let dt = now.saturating_duration_since(r.last);
        r.last = now;
        match r.mode {
            RotationMode::On => {}
            // 止めたままの間は時間を進めない
            RotationMode::Off => return false,
            RotationMode::Paused { until } if now < until => return false,
            RotationMode::Paused { .. } => {
                // 止める前の手順の続き(次の手順)から
                r.mode = RotationMode::On;
                r.remaining = Duration::ZERO;
            }
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
            let screen = &screens[st.screen];
            // GL が使えない・失敗中の 3D だけの画面は、CPU の案内を出すだけなので巡回では飛ばす
            let gl_ok = *gl_available && !scene_failed[st.screen];
            !(st.skip_if_empty && screen.is_empty()) && (!screen.needs_gl() || gl_ok)
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
            // 消灯・復帰をはさむと、GPU 側の重ね絵が最後に転送したものか分からない(描かずに戻ったフレームがある)
            self.top_valid = false;
            self.update_presentation(now);
        }
    }

    pub fn set_display_forced_off(&mut self, forced: bool) {
        self.display_forced_off = forced;
    }

    /// 今 6 つのキーを押すと何が起きるか(runtime と `on_key` と同じ順で決める: モニターの消灯 → アラート → メニュー → 画面)。
    fn panel_keys(&self) -> PanelKeys {
        // モニターが消えているあいだ、runtime はキーを `on_key` に渡さず、モニターを点けるだけにする。
        // force_off のときは点けられないので、どのキーも何もしない
        if !self.display_on {
            let wake = (!self.display_forced_off).then_some("画面を点ける");
            return PanelKeys {
                prev: wake,
                next: wake,
                select: wake,
                up: wake,
                down: wake,
                menu: wake,
            };
        }
        if self.alert.is_some() {
            let close = Some("閉じる");
            return PanelKeys {
                prev: close,
                next: close,
                select: close,
                up: close,
                down: close,
                menu: close,
            };
        }
        if self.menu.is_some() {
            return PanelKeys {
                prev: Some("上を選ぶ"),
                next: Some("下を選ぶ"),
                select: Some("移動"),
                up: Some("上を選ぶ"),
                down: Some("下を選ぶ"),
                menu: Some("閉じる"),
            };
        }
        let screen = self.screens[self.current].key_labels();
        PanelKeys {
            prev: Some("前の画面"),
            next: Some("次の画面"),
            select: screen.select,
            up: screen.up,
            down: screen.down,
            menu: Some("メニュー"),
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
        // 動き続ける 3D は毎周期描き直す(`force_overlay` は立てない。重ね絵の転送は増やさない)
        redraw |= self.scene_animating();
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
    /// 3D の画面(`Composite::Gl`)では、専用の最上段の層(`top`)に描いて 3D の上に重ねる。
    pub fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        if self.refreshing.is_some() {
            self.render_refresh(canvas);
            self.top_valid = false;
            self.scene_screen = None;
            return Composite::Canvas;
        }
        let force_overlay = std::mem::take(&mut self.force_overlay);
        let titles = self.titles();
        let (dx, dy) = self.offset(canvas.height());
        let gl_ok = self.gl_ok(self.current);
        let (w, h) = (canvas.width(), canvas.height());
        let Self {
            screens,
            current,
            world,
            text,
            alert,
            menu,
            top,
            top_valid,
            scene_screen,
            ..
        } = self;
        let covered = alert.is_some() || menu.is_some();
        if !covered {
            *top = None;
        }
        let mut ctx = RenderContext {
            canvas,
            text,
            world,
            covered,
            force_overlay,
        };
        let screen = &mut screens[*current];
        let mut comp = if screen.needs_gl() && !gl_ok {
            // 3D だけの画面は、GL が使えない・失敗中なら CPU の案内(`render`)が完成画面になる
            screen.render(&mut ctx);
            Composite::Canvas
        } else {
            screen.render_composite(&mut ctx)
        };
        if !gl_ok {
            comp = without_scene(comp, ctx.canvas);
        }
        // 焼き付き対策の位置のずらし。写真(重ね絵つき)と 3D は対象にしない
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
            Composite::Gl(_) => {}
        }
        let draw_alert = match &comp {
            Composite::Canvas => true,
            Composite::Photo {
                overlay_changed, ..
            } => *overlay_changed,
            // 航空機の画面は、重ねるものがあるときは Canvas で返る
            Composite::Radar { .. } => false,
            Composite::Agents { .. } => false,
            // 3D の上に出すので、画面の Canvas(下地)でなく最上段の層に描く
            Composite::Gl(_) => false,
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
        match &mut comp {
            Composite::Gl(g) => {
                g.top = if !covered {
                    Top::Off
                } else if force_overlay || !*top_valid {
                    if top
                        .as_ref()
                        .is_none_or(|c| (c.width(), c.height()) != (w, h))
                    {
                        *top = Some(Canvas::new(w, h));
                    }
                    let c = top.as_mut().expect("上で作った");
                    c.clear(rgba(0, 0, 0, 0));
                    layers(c, ctx.text, &titles, *current, *menu, alert.as_ref());
                    Top::Upload(&c.pixmap)
                } else {
                    Top::Keep
                };
                *top_valid = !matches!(g.top, Top::Off);
                *scene_screen = Some(*current);
            }
            // `Gl` 以外のフレームは最上段の層を出さない(`Off` とみなす)
            _ => {
                *top_valid = false;
                *scene_screen = None;
            }
        }
        comp
    }
}

/// `Composite::Gl` から 3D を外し、下地だけにする(GL が使えない・3D が失敗した画面)。下地が無ければ黒の Canvas。
/// 入れ子の `Gl` は、中まで外す。
fn without_scene<'a>(mut comp: Composite<'a>, canvas: &mut Canvas) -> Composite<'a> {
    while let Composite::Gl(g) = comp {
        comp = match g.base {
            Some(base) => *base,
            None => {
                canvas.clear(rgba(0, 0, 0, 255));
                Composite::Canvas
            }
        };
    }
    comp
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

    fn set_rotation(&mut self, on: bool) -> bool {
        ScreenManager::set_rotation(self, on)
    }

    fn wants_screen(&self) -> bool {
        self.alert_active()
    }

    fn set_display_on(&mut self, on: bool, now: Instant) {
        ScreenManager::set_display_on(self, on, now)
    }

    fn set_display_forced_off(&mut self, forced: bool) {
        ScreenManager::set_display_forced_off(self, forced)
    }

    fn animating(&self) -> bool {
        self.refreshing.is_some()
            || self.screens[self.current].animating()
            || self.scene_animating()
    }

    fn frame_interval(&self) -> Option<Duration> {
        // 演出(焼き付き対策)は自分の間隔で動く。動いていない画面に間隔は要らない
        if self.refreshing.is_some() || !self.animating() {
            return None;
        }
        self.screens[self.current].frame_interval()
    }

    fn gl_scenes(&mut self) -> Vec<&mut dyn GlScene> {
        ScreenManager::gl_scenes(self)
    }

    fn report_scene_failure(&mut self, msg: &str) {
        ScreenManager::report_scene_failure(self, msg)
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
        if self.scene_fails > 0 {
            out.push(("scene_fail", self.scene_fails.to_string()));
        }
        // GL の場面が作ったオブジェクトの「作成 − 削除」の合計(§3.3)。場面を持つ画面があるときだけ出す
        let live: Vec<i64> = self
            .screens
            .iter()
            .filter_map(|s| s.gl_live_objects())
            .collect();
        if !live.is_empty() {
            out.push(("gl_objs", live.iter().sum::<i64>().to_string()));
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
            menu: self.menu,
            keys: self.panel_keys(),
            rotation: self.rotation.as_ref().map(|r| match r.mode {
                RotationMode::On => PanelRotation::On,
                RotationMode::Paused { until } => PanelRotation::Paused { until },
                RotationMode::Off => PanelRotation::Off,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DummyData, dummy_snapshot};
    use crate::scene_check::{Base, CheckScreen};
    use crate::screen::ScreenId;

    struct Plain;

    impl Screen for Plain {
        fn id(&self) -> ScreenId {
            "plain"
        }
        fn title(&self) -> &'static str {
            "PLAIN"
        }
        fn render(&mut self, ctx: &mut RenderContext<'_>) {
            ctx.canvas.clear(rgba(0, 0, 0, 255));
        }
    }

    fn top_of(app: &mut ScreenManager) -> &'static str {
        let mut c = Canvas::new(320, 180);
        match app.render_composite(&mut c) {
            Composite::Gl(g) => match g.top {
                Top::Off => "off",
                Top::Keep => "keep",
                Top::Upload(_) => "upload",
            },
            _ => "not-gl",
        }
    }

    /// `Gl` 以外のフレームは最上段の層を出さない(`Off` とみなす)ので、そのあとの `Gl` は、
    /// `force_overlay` が立っていなくても `Keep` でなく `Upload` から始める。
    #[test]
    fn non_gl_frame_counts_as_off_for_the_top_layer() {
        let screens: Vec<Box<dyn Screen>> = vec![
            Box::new(CheckScreen::new(
                "a",
                false,
                Base::Canvas,
                Default::default(),
            )),
            Box::new(Plain),
        ];
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = ScreenManager::new(
            screens,
            "a",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            crate::app::text_engine(""),
            Duration::from_secs(60),
        )
        .with_gl_available(true)
        .with_alerts(Some(rx));
        tx.send(AlertView::Quake(crate::quake::demo_view()))
            .unwrap();
        app.tick(Instant::now());
        assert_eq!(top_of(&mut app), "upload");
        assert_eq!(top_of(&mut app), "keep");
        // 画面の切り替えは force_overlay も立てるので、キーを通さずに現在の画面だけを替える
        app.current = 1;
        assert_eq!(top_of(&mut app), "not-gl");
        app.current = 0;
        assert!(!app.force_overlay);
        assert_eq!(
            top_of(&mut app),
            "upload",
            "非 Gl のフレームのあとは Keep に頼らない"
        );
        assert_eq!(top_of(&mut app), "keep");
    }
}
