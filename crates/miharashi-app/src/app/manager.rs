//! 画面の登録と切り替え、アラート層の重ね描き、共有データ(`World`)の更新。runtime からは `App` として見える。
use super::DataSource;
use crate::screen::{
    App, Composite, KeyPress, Outcome, PanelView, Provider, RenderContext, Screen, ScreenAction,
    ScreenInfo, World,
};
use miharashi_render::alert::{self, AlertView};
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

    /// 戻り値: 再描画が必要か。
    pub fn on_key(&mut self, key: KeyPress) -> bool {
        self.keys += 1;
        self.force_overlay = true;
        // アラート表示中のキーは閉じるだけにする
        if self.alert.take().is_some() {
            return true;
        }
        let now = Instant::now();
        let action = match key {
            KeyPress::Prev => ScreenAction::Up,
            KeyPress::Switch => ScreenAction::Select,
            KeyPress::Next => ScreenAction::Down,
        };
        if self.screens[self.current].handle_action(action, now) == Outcome::Ignored {
            // 画面で使わなかったキー。S2(docs/DESIGN-screens.md §4.1)で 4/6 = 画面の巡回、0 = メニューに変える。
            // それまでは今の操作のまま: 6 = 次の画面、4 = 前の画面(端で止まる)、5 = 先頭(状況画面)へ戻る
            let to = match action {
                ScreenAction::Down => (self.current + 1).min(self.screens.len() - 1),
                ScreenAction::Up => self.current.saturating_sub(1),
                ScreenAction::Select => 0,
            };
            self.switch_to(to, now);
        }
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
        let Self {
            screens,
            current,
            world,
            text,
            alert,
            ..
        } = self;
        let mut ctx = RenderContext {
            canvas,
            text,
            world,
            alert: alert.is_some(),
            force_overlay: true,
        };
        screens[*current].render(&mut ctx);
        if let Some((view, _)) = alert {
            alert::overlay(ctx.canvas, ctx.text, view);
        }
    }

    /// GPU 経路の描画。アラートは画面の完成画面か、写真の重ね絵の上に描く。
    pub fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        let force_overlay = std::mem::take(&mut self.force_overlay);
        let Self {
            screens,
            current,
            world,
            text,
            alert,
            ..
        } = self;
        let mut ctx = RenderContext {
            canvas,
            text,
            world,
            alert: alert.is_some(),
            force_overlay,
        };
        let comp = screens[*current].render_composite(&mut ctx);
        let draw_alert = match &comp {
            Composite::Canvas => true,
            Composite::Photo {
                overlay_changed, ..
            } => *overlay_changed,
            // 航空機の画面はアラート中は Canvas で返る
            Composite::Radar { .. } => false,
        };
        if draw_alert && let Some((view, _)) = alert {
            alert::overlay(ctx.canvas, ctx.text, view);
        }
        comp
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
