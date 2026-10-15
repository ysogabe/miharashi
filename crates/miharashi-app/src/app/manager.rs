//! 画面の登録と切り替え、メニュー、アラート層の重ね描き、共有データ(`World`)の更新。runtime からは `App` として見える。
//! キーは docs/DESIGN-screens.md §4.1: 4/6 = 画面の巡回、0 = メニュー、5・8/2 = 表示中の画面の中の操作。
use super::DataSource;
use crate::screen::{
    App, Composite, KeyPress, PanelView, Provider, RenderContext, Screen, ScreenAction, ScreenInfo,
    World,
};
use miharashi_render::alert::{self, AlertView};
use miharashi_render::menu;
use miharashi_render::{Canvas, TextEngine};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

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
        self.screens[self.current].on_leave(now);
        self.current = i;
        self.screens[i].on_enter(now);
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
        // アラート表示中のキーは閉じるだけにする
        if self.alert.take().is_some() {
            return true;
        }
        let now = Instant::now();
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
        self.switch_to(i, Instant::now());
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
        redraw
    }

    pub fn render(&mut self, canvas: &mut Canvas) {
        let titles = self.titles();
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
        let force_overlay = std::mem::take(&mut self.force_overlay);
        let titles = self.titles();
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
        let comp = screens[*current].render_composite(&mut ctx);
        let draw_alert = match &comp {
            Composite::Canvas => true,
            Composite::Photo {
                overlay_changed, ..
            } => *overlay_changed,
            // 航空機の画面は、重ねるものがあるときは Canvas で返る
            Composite::Radar { .. } => false,
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

    fn animating(&self) -> bool {
        self.screens[self.current].animating()
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
        })
    }
}
