//! `vrm-view --agents` の背景になる、T3 Agent 画面のカードの動き(合成のダミー)。
//! 実際の Agent 画面(`miharashi-app` の `AgentScreen`)と同じ道で、CPU で描いた背景(見出し・集計・枠)と
//! カードの面(`miharashi_render::agents::render_card`)を、`miharashi-gpu` の `Frame::Agents` に渡して
//! GPU で合成する。カードの位置・大きさは pose だけで動かし、中身が変わったときだけ面を描き直して転送する。
//! `miharashi-app` には依存しない(配置は `agent_layout` に写し、動きは `AgentScreen` から必要な分を写した)。
//!
//! 動き(実際の画面と同じ規則):
//! - 状態が数秒ごとに変わる → そのカードの面を描き直す(テクスチャの再転送)。要確認 > 実行中 > 他の順に
//!   枠の中で並び替わり、動くカードは 500ms で滑る(pose だけのフレーム)。
//! - 状態の変化ごとに、そのカードが画面の中央へ拡大して移り(700ms)、3〜5 秒止まり、元の位置へ戻る(800ms)。
//!   そのあいだ scrim(0.46)でほかを暗くする。中央では別の大きさの面を描くので、行きと帰りに描き直しが起きる。
//! - 背景は 1 秒ごとに描き直して転送する(見出しの時計)。状態の変化・フォーカスの終わり・配置の変化でも描き直す。
//!
//! 含めないもの: 新しいスレッドが右から入る動き、画面が消えている間の一時停止、T3 との通信(中身は全て架空)。
use crate::agent_layout::{self as layout, CardMode};
use miharashi_gpu::{AgentGpuCard, Frame};
use miharashi_render::agents::{self as draw, AgentCardContent, AgentCardSurface, AgentPose};
use miharashi_render::{Canvas, Pixmap, TextEngine};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NEW_INTRO: Duration = Duration::from_millis(700);
const RETURN: Duration = Duration::from_millis(800);
const MOVE_TWEEN: Duration = Duration::from_millis(500);
/// ヘッダーと集計行の下端、フッターの区切り線より上が、枠を並べる領域
const BOARD_TOP: f32 = 212.0;
const BOARD_BOTTOM_MARGIN: f32 = 80.0;
const MAX_QUEUE: usize = 32;
const SCRIM: f32 = 0.46;

/// 合成のプロジェクト名(架空)
const PROJECTS: [&str; 5] = [
    "星見台ダッシュボード",
    "ねこ当番アプリ",
    "公園マップ API",
    "レシピ帳",
    "天気カレンダー",
];
/// 合成のタスク名(架空)
const TITLES: [&str; 12] = [
    "一覧画面の並び替えを直す",
    "通知の文言を見直す",
    "検索の速度を測る",
    "設定画面を作る",
    "古いデータを移行する",
    "テストの失敗を調べる",
    "ログの出力を整理する",
    "画像の読み込みを速くする",
    "アイコンを差し替える",
    "入力の検証を足す",
    "翻訳の抜けを確認する",
    "起動時のちらつきを止める",
];
const MODELS: [(&str, &str); 3] = [
    ("Codex", "サンプルモデルA"),
    ("Claude", "サンプルモデルB"),
    ("Codex", "サンプルモデルC"),
];
/// 状態の巡り方(スレッドごとに順に進む)
const STATES: [draw::AgentState; 7] = [
    draw::AgentState::Running,
    draw::AgentState::Approval,
    draw::AgentState::Running,
    draw::AgentState::Completed,
    draw::AgentState::Input,
    draw::AgentState::Running,
    draw::AgentState::Error,
];

fn attention(s: draw::AgentState) -> bool {
    matches!(
        s,
        draw::AgentState::Approval | draw::AgentState::Input | draw::AgentState::Error
    )
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smooth(v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

struct Thread {
    key: String,
    /// GPU 側のカードの id(安定)
    stable: u64,
    project: usize,
    title: &'static str,
    state: draw::AgentState,
    state_idx: usize,
    model: (&'static str, &'static str),
    last_activity: Instant,
    seen: u64,
}

#[derive(Clone)]
struct Event {
    key: String,
    before: String,
    after: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusStage {
    In,
    Hold,
    Out,
}

struct Focus {
    event: Event,
    started: Instant,
    hold: Duration,
    stage: FocusStage,
}

#[derive(Clone, Copy)]
struct PoseTween {
    from: AgentPose,
    to: AgentPose,
    started: Instant,
}

impl PoseTween {
    fn at(&self, now: Instant) -> AgentPose {
        let p = smooth(
            now.saturating_duration_since(self.started).as_secs_f32() / MOVE_TWEEN.as_secs_f32(),
        );
        AgentPose {
            x: lerp(self.from.x, self.to.x, p),
            y: lerp(self.from.y, self.to.y, p),
            width: lerp(self.from.width, self.to.width, p),
            height: lerp(self.from.height, self.to.height, p),
            alpha: lerp(self.from.alpha, self.to.alpha, p),
        }
    }

    fn moving(&self) -> bool {
        self.from != self.to
    }
}

struct Group {
    label: String,
    /// `threads` の添字。重要な順
    threads: Vec<usize>,
}

/// 区間ごとの計測(`take_stats` で読んで空にする)。
#[derive(Default, Clone, Copy, Debug)]
pub struct StageStats {
    /// 画面を組み立てた回数(pose だけのフレームや、何も変わらないフレームは含まない)
    pub scenes: u32,
    /// カードの面を描き直した回数
    pub cards_redrawn: u32,
    /// 背景を描き直した回数(そのたびに画面 1 枚分を転送する)
    pub bg_redrawn: u32,
    /// カードの面の描き直しにかかった時間 [ms]
    pub cards_ms: f64,
    /// 背景の描き直しにかかった時間 [ms]
    pub bg_ms: f64,
    /// 画面の組み立て全体(配置の計算、pose、上の 2 つを含む)[ms]
    pub scene_ms: f64,
}

impl StageStats {
    pub fn add(&mut self, o: &StageStats) {
        self.scenes += o.scenes;
        self.cards_redrawn += o.cards_redrawn;
        self.bg_redrawn += o.bg_redrawn;
        self.cards_ms += o.cards_ms;
        self.bg_ms += o.bg_ms;
        self.scene_ms += o.scene_ms;
    }
}

pub struct AgentStage {
    size: (u32, u32),
    threads: Vec<Thread>,
    change_every: Duration,
    next_change: Instant,
    changes: u64,
    queue: VecDeque<Event>,
    focus: Option<Focus>,
    poses: HashMap<String, PoseTween>,
    board: layout::Layout,
    surfaces: HashMap<String, Arc<Pixmap>>,
    revisions: HashMap<String, u64>,
    content_keys: HashMap<String, String>,
    background: Pixmap,
    background_dirty: bool,
    /// 背景を GPU へまだ渡していない(`Frame::Agents` の `background` に載せる)
    bg_pending: bool,
    header_due: Instant,
    scene_cards: Vec<AgentCardSurface>,
    scrim_alpha: f32,
    focus_card_id: Option<u64>,
    /// フォーカス中のカードの、いまの中心(画面の px、左上が原点)と段階
    focus_target: Option<((f32, f32), FocusStage)>,
    started: Instant,
    first: bool,
    stats: StageStats,
}

impl AgentStage {
    /// `cards` 枚のダミーのスレッドを、`cards / 3` 個ほどのプロジェクト(最大 5)に分けて置く。
    /// 状態は `change_every` ごとに 1 スレッドずつ(順番に)変わる。
    pub fn new(size: (u32, u32), cards: usize, change_every: Duration, now: Instant) -> Self {
        let cards = cards.clamp(1, 24);
        let projects = cards.div_ceil(3).clamp(1, PROJECTS.len());
        let threads = (0..cards)
            .map(|i| Thread {
                key: format!("t{i}"),
                stable: 1000 + i as u64,
                project: i % projects,
                title: TITLES[i % TITLES.len()],
                state: STATES[i % STATES.len()],
                state_idx: i % STATES.len(),
                model: MODELS[i % MODELS.len()],
                // 並びが決まるよう、少しずつずらしておく
                last_activity: now
                    .checked_sub(Duration::from_secs(10 * i as u64))
                    .unwrap_or(now),
                seen: i as u64,
            })
            .collect();
        Self {
            size,
            threads,
            change_every,
            next_change: now + change_every,
            changes: 0,
            queue: VecDeque::new(),
            focus: None,
            poses: HashMap::new(),
            board: layout::Layout::default(),
            surfaces: HashMap::new(),
            revisions: HashMap::new(),
            content_keys: HashMap::new(),
            background: Pixmap::new(size.0, size.1).expect("画面の大きさが不正"),
            background_dirty: true,
            bg_pending: false,
            header_due: now + Duration::from_secs(1),
            scene_cards: Vec::new(),
            scrim_alpha: 0.0,
            focus_card_id: None,
            focus_target: None,
            started: now,
            first: true,
            stats: StageStats::default(),
        }
    }

    fn scale(&self) -> f32 {
        (self.size.0 as f32 / 1920.0)
            .min(self.size.1 as f32 / 1080.0)
            .max(0.35)
    }

    pub fn card_count(&self) -> usize {
        self.threads.len()
    }

    pub fn project_count(&self) -> usize {
        self.threads
            .iter()
            .map(|t| t.project)
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn take_stats(&mut self) -> StageStats {
        std::mem::take(&mut self.stats)
    }

    fn rank(&self, a: &Thread, b: &Thread) -> std::cmp::Ordering {
        let tier = |t: &Thread| {
            if attention(t.state) {
                0
            } else if t.state == draw::AgentState::Running {
                1
            } else {
                2
            }
        };
        tier(a)
            .cmp(&tier(b))
            .then_with(|| b.last_activity.cmp(&a.last_activity))
            .then_with(|| a.seen.cmp(&b.seen))
    }

    /// プロジェクトの順に並べ、各枠の中を重要な順に並べる。
    fn groups(&self) -> Vec<Group> {
        let mut groups: Vec<(usize, Group)> = Vec::new();
        for (i, t) in self.threads.iter().enumerate() {
            match groups.iter_mut().find(|(p, _)| *p == t.project) {
                Some((_, g)) => g.threads.push(i),
                None => groups.push((
                    t.project,
                    Group {
                        label: PROJECTS[t.project].to_string(),
                        threads: vec![i],
                    },
                )),
            }
        }
        groups.sort_by_key(|(p, _)| *p);
        groups
            .into_iter()
            .map(|(_, mut g)| {
                g.threads
                    .sort_by(|&a, &b| self.rank(&self.threads[a], &self.threads[b]));
                g
            })
            .collect()
    }

    fn layout(&self, groups: &[Group]) -> layout::Layout {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let area = layout::Rect {
            x: 32.0,
            y: BOARD_TOP,
            w: w - 64.0,
            h: h - BOARD_TOP - BOARD_BOTTOM_MARGIN,
        };
        let ids = groups
            .iter()
            .map(|g| {
                g.threads
                    .iter()
                    .map(|&i| self.threads[i].key.clone())
                    .collect()
            })
            .collect::<Vec<Vec<String>>>();
        layout::layout(area, self.scale(), &ids)
    }

    /// 次のスレッドの状態を 1 つ進めて、フォーカスの待ち行列に積む。
    fn apply_change(&mut self, now: Instant) {
        let i = (self.changes % self.threads.len() as u64) as usize;
        self.changes += 1;
        let t = &mut self.threads[i];
        let before = t.state;
        t.state_idx = (t.state_idx + 1) % STATES.len();
        t.state = STATES[t.state_idx];
        t.last_activity = now;
        if self.queue.len() < MAX_QUEUE {
            self.queue.push_back(Event {
                key: t.key.clone(),
                before: before.label().into(),
                after: t.state.label().into(),
            });
        }
        self.background_dirty = true;
    }

    fn activate_event(&mut self, now: Instant) -> bool {
        if self.focus.is_some() {
            return false;
        }
        let board = self.layout(&self.groups());
        let Some(pos) = self
            .queue
            .iter()
            .position(|e| board.cards.contains_key(&e.key))
        else {
            return false;
        };
        let event = self.queue.remove(pos).expect("位置は確認済み");
        let urgent = self
            .threads
            .iter()
            .find(|t| t.key == event.key)
            .is_some_and(|t| attention(t.state));
        self.focus = Some(Focus {
            event,
            started: now,
            hold: Duration::from_secs(if urgent { 5 } else { 3 }),
            stage: FocusStage::In,
        });
        true
    }

    fn advance(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        if self.focus.is_none() {
            redraw |= self.activate_event(now);
        }
        if let Some(f) = self.focus.as_mut() {
            let elapsed = now.saturating_duration_since(f.started);
            let stage = if elapsed < NEW_INTRO {
                FocusStage::In
            } else if elapsed < NEW_INTRO + f.hold {
                FocusStage::Hold
            } else {
                FocusStage::Out
            };
            if stage != f.stage {
                f.stage = stage;
                redraw = true;
            }
            if elapsed >= NEW_INTRO + f.hold + RETURN {
                self.focus = None;
                self.scrim_alpha = 0.0;
                self.focus_card_id = None;
                self.focus_target = None;
                self.background_dirty = true;
                redraw = true;
            } else if matches!(stage, FocusStage::In | FocusStage::Out) {
                redraw = true;
            }
        }
        redraw
    }

    /// 時刻を `now` まで進め、変化があれば画面(背景とカードの面と pose)を組み立てる。
    /// 組み立てたら true。何も変わらないフレームでは何もせず、前回の pose と面をそのまま使う。
    pub fn update(&mut self, now: Instant, text: &mut TextEngine) -> bool {
        let mut redraw = std::mem::take(&mut self.first);
        if now >= self.next_change {
            self.apply_change(now);
            self.next_change = now + self.change_every;
            redraw = true;
        }
        redraw |= self.advance(now);
        // 動いているカードがある間は描き続け、終わった動きは正確な位置で止める
        for tween in self.poses.values_mut() {
            if !tween.moving() {
                continue;
            }
            if now.saturating_duration_since(tween.started) >= MOVE_TWEEN {
                tween.from = tween.to;
            }
            redraw = true;
        }
        if now >= self.header_due {
            self.header_due = now + Duration::from_secs(1);
            self.background_dirty = true;
            redraw = true;
        }
        if redraw {
            let t0 = Instant::now();
            self.render_scene(now, text);
            self.stats.scenes += 1;
            self.stats.scene_ms += t0.elapsed().as_secs_f64() * 1e3;
        }
        redraw
    }

    fn tween_pose(&mut self, key: &str, target: AgentPose, now: Instant) -> AgentPose {
        let tween = self.poses.entry(key.to_string()).or_insert(PoseTween {
            from: target,
            to: target,
            started: now,
        });
        if tween.to != target {
            *tween = PoseTween {
                from: tween.at(now),
                to: target,
                started: now,
            };
        }
        tween.at(now)
    }

    fn content(
        &self,
        t: &Thread,
        now: Instant,
        badge: Option<String>,
        compact: bool,
        focused: bool,
    ) -> AgentCardContent {
        let detail = match t.state {
            draw::AgentState::Approval => "手元の T3 で承認".to_string(),
            draw::AgentState::Input => "手元の T3 で入力".to_string(),
            draw::AgentState::Error => "ビルドに失敗しました".to_string(),
            draw::AgentState::Running => "目標 · 変更を仕上げて確認する".to_string(),
            _ => t.state.label().to_string(),
        };
        let secs = now.saturating_duration_since(t.last_activity).as_secs();
        let age = Some(if secs < 60 {
            "1分未満".to_string()
        } else {
            format!("{}分", secs / 60)
        });
        AgentCardContent {
            project: if focused {
                PROJECTS[t.project].to_string()
            } else {
                String::new()
            },
            title: t.title.to_string(),
            state: t.state,
            detail: badge.unwrap_or(detail),
            provider_model: Some(format!("{} · {}", t.model.0, t.model.1)),
            age,
            progress: None,
            badge: None,
            compact,
            one_line: false,
        }
    }

    fn ensure_surface(
        &mut self,
        key: &str,
        content: &AgentCardContent,
        size: (f32, f32),
        text: &mut TextEngine,
    ) {
        let size = (
            size.0.round().max(1.0) as u32,
            size.1.round().max(1.0) as u32,
        );
        // 面の中身が同じかを、描画に効く項目と大きさで見る
        let id = format!(
            "{}|{}|{:?}|{}|{}|{}|{}|{}x{}",
            content.project,
            content.title,
            content.state,
            content.detail,
            content.provider_model.as_deref().unwrap_or(""),
            content.age.as_deref().unwrap_or(""),
            content.compact,
            size.0,
            size.1
        );
        if self.content_keys.get(key) != Some(&id) || !self.surfaces.contains_key(key) {
            let t0 = Instant::now();
            let rev = self.revisions.entry(key.to_string()).or_insert(0);
            *rev = rev.wrapping_add(1);
            self.content_keys.insert(key.to_string(), id);
            self.surfaces.insert(
                key.to_string(),
                Arc::new(draw::render_card(content, size, text)),
            );
            self.stats.cards_redrawn += 1;
            self.stats.cards_ms += t0.elapsed().as_secs_f64() * 1e3;
        }
    }

    fn render_scene(&mut self, now: Instant, text: &mut TextEngine) {
        let groups = self.groups();
        let board = self.layout(&groups);
        if board != self.board {
            self.board = board;
            self.background_dirty = true;
        }
        if self.background_dirty {
            let t0 = Instant::now();
            let mut bg = Canvas::new(self.size.0, self.size.1);
            self.draw_background(&mut bg, text, &groups, now);
            self.background = bg.pixmap;
            self.background_dirty = false;
            self.bg_pending = true;
            self.stats.bg_redrawn += 1;
            self.stats.bg_ms += t0.elapsed().as_secs_f64() * 1e3;
        }
        let scale = self.scale();
        let focus_key = self.focus.as_ref().map(|f| f.event.key.clone());
        let mut shown = HashSet::new();
        let mut cards = Vec::new();
        self.focus_target = None;
        for &ti in groups.iter().flat_map(|g| &g.threads) {
            let key = self.threads[ti].key.clone();
            let stable = self.threads[ti].stable;
            let Some(slot) = self.board.cards.get(&key).copied() else {
                continue;
            };
            let is_focus = focus_key.as_deref() == Some(key.as_str());
            shown.insert(key.clone());
            let target = AgentPose {
                x: slot.rect.x,
                y: slot.rect.y,
                width: slot.rect.w,
                height: slot.rect.h,
                alpha: 1.0,
            };
            let home = self.tween_pose(&key, target, now);
            let mut pose = home;
            if is_focus {
                let (started, hold, event, stage) = {
                    let f = self.focus.as_ref().expect("フォーカス中");
                    (f.started, f.hold, f.event.clone(), f.stage)
                };
                let elapsed = now.saturating_duration_since(started).as_secs_f32();
                let intro = NEW_INTRO.as_secs_f32();
                let end_in = intro + hold.as_secs_f32();
                let end = end_in + RETURN.as_secs_f32();
                let center = (self.size.0 as f32 * 0.5, self.size.1 as f32 * 0.52);
                let focus_w = 880.0 * scale;
                let focus_h = 420.0 * scale;
                // 行き帰りの間も面の縦横比は変えず、文字が枠の形に引き伸ばされないようにする
                let w = home.width;
                let h = w * focus_h / focus_w;
                let home_c = (home.x + home.width / 2.0, home.y + home.height / 2.0);
                if elapsed < intro {
                    let p = smooth(elapsed / intro);
                    pose.x = lerp(home_c.0 - w / 2.0, center.0 - focus_w / 2.0, p);
                    pose.y = lerp(home_c.1 - h / 2.0, center.1 - focus_h / 2.0, p);
                    pose.width = lerp(w, focus_w, p);
                    pose.height = lerp(h, focus_h, p);
                    pose.alpha = 1.0;
                    self.scrim_alpha = SCRIM * p;
                } else if elapsed < end_in {
                    pose.x = center.0 - focus_w / 2.0;
                    pose.y = center.1 - focus_h / 2.0;
                    pose.width = focus_w;
                    pose.height = focus_h;
                    pose.alpha = 1.0;
                    self.scrim_alpha = SCRIM;
                } else {
                    let p = smooth((elapsed - end_in) / (end - end_in));
                    pose.x = lerp(center.0 - focus_w / 2.0, home_c.0 - w / 2.0, p);
                    pose.y = lerp(center.1 - focus_h / 2.0, home_c.1 - h / 2.0, p);
                    pose.width = lerp(focus_w, w, p);
                    pose.height = lerp(focus_h, h, p);
                    pose.alpha = 1.0;
                    self.scrim_alpha = SCRIM * (1.0 - p);
                }
                self.focus_card_id = Some(stable);
                self.focus_target = Some((
                    (pose.x + pose.width / 2.0, pose.y + pose.height / 2.0),
                    stage,
                ));
                let summary = format!("{} → {}", event.before, event.after);
                let content = self.content(&self.threads[ti], now, Some(summary), false, true);
                self.ensure_surface(&key, &content, (focus_w, focus_h), text);
            } else {
                let compact = slot.mode == CardMode::Strip;
                let content = self.content(&self.threads[ti], now, None, compact, false);
                self.ensure_surface(&key, &content, (slot.rect.w, slot.rect.h), text);
            }
            cards.push((key, stable, pose, (is_focus, slot.z)));
        }
        self.surfaces.retain(|k, _| shown.contains(k));
        self.content_keys.retain(|k, _| shown.contains(k));
        let placed = &self.board.cards;
        self.poses.retain(|k, _| placed.contains_key(k));
        if self.focus.is_none() {
            self.scrim_alpha = 0.0;
            self.focus_card_id = None;
        }
        // 後ろの細い面、手前の面、フォーカスの順
        cards.sort_by_key(|(_, _, _, order)| *order);
        self.scene_cards.clear();
        for (key, stable, pose, _) in cards {
            self.scene_cards.push(AgentCardSurface {
                id: stable,
                revision: self.revisions[&key],
                surface: self.surfaces[&key].clone(),
                pose,
            });
        }
    }

    /// 実際の画面の `draw_background` を、架空のスレッド向けに簡単にしたもの(見出し・時計・集計・枠・フッター)。
    fn draw_background(
        &self,
        canvas: &mut Canvas,
        text: &mut TextEngine,
        groups: &[Group],
        now: Instant,
    ) {
        use miharashi_render::canvas::rgba;
        let w = canvas.width() as f32;
        let h = canvas.height() as f32;
        let scale = self.scale();
        canvas.clear(rgba(8, 12, 20, 255));
        text.draw(canvas, "T3 AGENTS", 32.0, 24.0, 36.0, [223, 231, 240, 255]);
        text.draw(canvas, "OVERVIEW", 280.0, 28.0, 25.0, [64, 200, 255, 255]);
        text.draw(
            canvas,
            "ダミー環境",
            32.0,
            100.0,
            24.0,
            [223, 231, 240, 255],
        );
        // 時計(1 秒ごとに変わる)。起動からの経過で作る
        let secs = now.saturating_duration_since(self.started).as_secs();
        let clock = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
        text.draw_right(canvas, &clock, w - 32.0, 26.0, 22.0, [143, 163, 183, 255]);
        text.draw(canvas, "LIVE", 285.0, 100.0, 23.0, [103, 217, 173, 255]);
        text.draw(
            canvas,
            &format!("最終取得 {clock}"),
            520.0,
            100.0,
            20.0,
            [143, 163, 183, 255],
        );
        let count = |f: &dyn Fn(draw::AgentState) -> bool| {
            self.threads.iter().filter(|t| f(t.state)).count()
        };
        let counts = [
            (
                "実行中",
                count(&|s| s == draw::AgentState::Running),
                [64, 200, 255, 255],
            ),
            (
                "承認待ち",
                count(&|s| s == draw::AgentState::Approval),
                [255, 188, 102, 255],
            ),
            (
                "入力・計画",
                count(&|s| s == draw::AgentState::Input),
                [196, 160, 255, 255],
            ),
            (
                "異常",
                count(&|s| s == draw::AgentState::Error),
                [255, 127, 140, 255],
            ),
            (
                "応答完了",
                count(&|s| s == draw::AgentState::Completed),
                [103, 217, 173, 255],
            ),
            (
                "その他",
                count(&|s| s == draw::AgentState::Idle),
                [143, 163, 183, 255],
            ),
        ];
        let mut x = 32.0;
        for (label, n, color) in counts {
            text.draw(canvas, &format!("{label} {n}"), x, 174.0, 20.0, color);
            x += 175.0;
        }
        let mut summary = format!(
            "{} スレッド · {} プロジェクト",
            self.threads.len(),
            groups.len()
        );
        let urgent = count(&attention);
        if urgent > 0 {
            summary.push_str(&format!(" · 要確認 {urgent} 件"));
        }
        text.draw_right(
            canvas,
            &summary,
            w - 32.0,
            174.0,
            21.0,
            [143, 163, 183, 255],
        );
        for lane in &self.board.lanes {
            let Some(group) = groups.get(lane.group) else {
                continue;
            };
            let r = lane.rect;
            canvas.fill_rect(r.x, r.y, r.w, r.h, rgba(120, 150, 190, 12));
            canvas.stroke_rect(r.x, r.y, r.w, r.h, 1.0, rgba(120, 150, 190, 28));
            let head = lane.head;
            let name_size = 28.0 * scale;
            let count_size = 23.0 * scale;
            let n = group.threads.len().to_string();
            let count_w = text.measure(&n, count_size);
            text.draw(
                canvas,
                &group.label,
                head.x,
                head.y,
                name_size,
                [223, 231, 240, 255],
            );
            let mut x = head.x + text.measure(&group.label, name_size) + 10.0 * scale;
            text.draw(
                canvas,
                &n,
                x,
                head.y + 4.0 * scale,
                count_size,
                [143, 163, 183, 255],
            );
            x += count_w + 18.0 * scale;
            // スレッドごとに状態の点を 1 つ
            let r_dot = 6.5 * scale;
            let cy = head.y + name_size * 0.62;
            for &ti in &group.threads {
                if x + r_dot * 2.0 > head.x + head.w {
                    break;
                }
                let [cr, cg, cb] = self.threads[ti].state.color();
                canvas.fill_circle(x + r_dot, cy, r_dot, rgba(cr, cg, cb, 255));
                x += r_dot * 2.0 + 7.0 * scale;
            }
            for (mx, my, hidden) in &lane.more {
                text.draw(
                    canvas,
                    &format!("ほか {hidden} 件"),
                    mx + 12.0 * scale,
                    my + 2.0 * scale,
                    22.0 * scale,
                    [143, 163, 183, 255],
                );
            }
        }
        text.draw(
            canvas,
            "自動表示 · 対応は手元の T3 で",
            32.0,
            h - 28.0,
            20.0,
            [143, 163, 183, 255],
        );
        canvas.fill_rect(0.0, 80.0, w, 1.0, rgba(48, 67, 87, 255));
        canvas.fill_rect(0.0, h - 72.0, w, 1.0, rgba(48, 67, 87, 255));
    }

    /// GPU へ渡すカード(`runtime.rs` の `Composite::Agents` → `Frame::Agents` と同じ変換)。
    pub fn gpu_cards(&self) -> Vec<AgentGpuCard<'_>> {
        self.scene_cards
            .iter()
            .map(|c| AgentGpuCard {
                id: c.id,
                revision: c.revision,
                pixels: c.surface.data(),
                size: (c.surface.width(), c.surface.height()),
                rect: (c.pose.x, c.pose.y, c.pose.width, c.pose.height),
                alpha: c.pose.alpha,
            })
            .collect()
    }

    /// いま描く `Frame::Agents`。描いたあとに `mark_sent` を呼ぶこと。
    pub fn frame<'a>(&'a self, cards: &'a [AgentGpuCard<'a>]) -> Frame<'a> {
        Frame::Agents {
            background: self.bg_pending.then(|| self.background.data()),
            cards,
            scrim_alpha: self.scrim_alpha,
            focus_card_id: self.focus_card_id,
        }
    }

    /// 背景を GPU に渡したので、次からは `None` で渡す。
    pub fn mark_sent(&mut self) {
        self.bg_pending = false;
    }

    pub fn scrim_alpha(&self) -> f32 {
        self.scrim_alpha
    }

    pub fn focus_stage(&self) -> Option<FocusStage> {
        self.focus_target.map(|(_, s)| s)
    }

    /// フォーカス中のカードが、いま画面のどこにいるか(中心。段階が Hold なら指さしてよい)。
    pub fn aim_target(&self) -> Option<AimTarget> {
        self.focus_target.map(|(center, stage)| AimTarget {
            center,
            pointing: stage == FocusStage::Hold,
        })
    }

    /// 画面の矩形 `(x, y, w, h)`(左上が原点)と重なるカードの数。(重なる枚数, 半分以上隠れる枚数, 全部の枚数)
    pub fn overlap(&self, r: (f32, f32, f32, f32)) -> (usize, usize, usize) {
        let (mut any, mut half) = (0, 0);
        for slot in self.board.cards.values() {
            let a = slot.rect;
            let w = (r.0 + r.2).min(a.x + a.w) - r.0.max(a.x);
            let h = (r.1 + r.3).min(a.y + a.h) - r.1.max(a.y);
            if w > 0.0 && h > 0.0 {
                any += 1;
                if w * h >= 0.5 * a.w * a.h {
                    half += 1;
                }
            }
        }
        (any, half, self.board.cards.len())
    }

    /// 中央に拡大したカードの矩形 `(x, y, w, h)`(左上が原点)。
    pub fn focus_rect(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (w, h) = (880.0 * s, 420.0 * s);
        (
            (self.size.0 as f32 - w) * 0.5,
            self.size.1 as f32 * 0.52 - h * 0.5,
            w,
            h,
        )
    }
}

pub use crate::gesture::{AimTarget, AimTracker};

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> TextEngine {
        TextEngine::new("Noto Sans CJK JP")
    }

    /// 時刻を進めて、フォーカスの一巡を通す。
    #[test]
    fn runs_focus_cycle_and_redraws_only_what_changes() {
        let t0 = Instant::now();
        let mut text = text();
        let mut s = AgentStage::new((2560, 1080), 8, Duration::from_secs(2), t0);
        assert_eq!((s.card_count(), s.project_count()), (8, 3));
        assert!(s.update(t0, &mut text), "最初のフレームは組み立てる");
        {
            let cards = s.gpu_cards();
            assert_eq!(cards.len(), 8);
            assert!(matches!(
                s.frame(&cards),
                Frame::Agents {
                    background: Some(_),
                    ..
                }
            ));
        }
        s.mark_sent();
        let first = s.take_stats();
        assert_eq!(first.cards_redrawn, 8);
        assert_eq!(first.bg_redrawn, 1);
        // 何も変わらない 0.5 秒後は何もしない(背景を送り直さない)
        assert!(!s.update(t0 + Duration::from_millis(500), &mut text));
        {
            let cards = s.gpu_cards();
            assert!(matches!(
                s.frame(&cards),
                Frame::Agents {
                    background: None,
                    ..
                }
            ));
        }
        // 2 秒で状態が変わり、フォーカスが始まる
        let mut max_scrim = 0.0f32;
        let mut stages = HashSet::new();
        let mut total = StageStats::default();
        let mut t = t0 + Duration::from_millis(500);
        let end = t0 + Duration::from_secs(12);
        while t < end {
            s.update(t, &mut text);
            max_scrim = max_scrim.max(s.scrim_alpha());
            if let Some(st) = s.focus_stage() {
                stages.insert(st);
            }
            total.add(&s.take_stats());
            t += Duration::from_millis(33);
        }
        assert!(
            (max_scrim - SCRIM).abs() < 1e-4,
            "scrim は 0.46 まで: {max_scrim}"
        );
        assert_eq!(stages.len(), 3, "In / Hold / Out を通る: {stages:?}");
        assert!(
            total.cards_redrawn >= 6,
            "状態の変化と、行き帰りの描き直し: {total:?}"
        );
        assert!(total.bg_redrawn >= 10, "1 秒ごとの背景: {total:?}");
        // 全てのカードに面があり、フォーカスが終われば scrim は消える
        assert_eq!(s.gpu_cards().len(), 8);
    }

    #[test]
    fn dummy_cards_overlap_left_region_but_focus_card_does_not() {
        let t0 = Instant::now();
        let mut text = text();
        let mut s = AgentStage::new((2560, 1080), 8, Duration::from_secs(5), t0);
        s.update(t0, &mut text);
        let (any, half, all) = s.overlap((0.0, 0.0, 853.0, 1080.0));
        assert_eq!(all, 8);
        assert!(any >= 2 && half >= 1, "左の枠は隠れる: {any} {half}");
        let (any, _, _) = s.overlap((853.0, 0.0, 853.0, 1080.0));
        assert!(any >= 1);
        // 中央の拡大カードは左 1/3 の領域に入らない
        let f = s.focus_rect();
        assert!(f.0 >= 853.0 - 20.0, "{f:?}");
    }

    #[test]
    fn aim_tracker_ramps_and_releases() {
        let t0 = Instant::now();
        let mut a = AimTracker::default();
        let head = (426.0, 240.0);
        let target = AimTarget {
            center: (1280.0, 560.0),
            pointing: true,
        };
        let mut t = t0;
        assert!(a.step(t, None, head, 540.0).is_none());
        // 目標が現れた瞬間は重み 0(まだ向かない)
        assert!(a.step(t, Some(target), head, 540.0).is_none());
        t += Duration::from_millis(10);
        let first = a.step(t, Some(target), head, 540.0).unwrap();
        assert!(first.look < 0.01 && first.dir[0] > 0.5);
        t += Duration::from_millis(250);
        let mid = a.step(t, Some(target), head, 540.0).unwrap();
        assert!(mid.look > 0.3 && mid.look < 0.8, "{mid:?}");
        t += Duration::from_millis(600);
        let full = a.step(t, Some(target), head, 540.0).unwrap();
        assert!((full.look - 1.0).abs() < 1e-4 && (full.point - 1.0).abs() < 1e-4);
        // 目標が消えたら重みが 0.5 秒で戻り、戻りきったら None
        t += Duration::from_millis(250);
        let fade = a.step(t, None, head, 540.0).unwrap();
        assert!(fade.look < 1.0 && fade.look > 0.0);
        t += Duration::from_millis(600);
        assert!(a.step(t, None, head, 540.0).is_none());
    }

    /// Agent 画面(`Frame::Agents`)を GPU で描いた上に、VRM の層を重ねる(vrm-view の描き方を表示なしで確かめる)。
    /// `VRM_MODEL` にモデルのパス、`VRM_AGENTS_PNG_DIR` に出力先を渡すと、実寸(2560x1080)で時刻を変えた PNG を出す。
    /// 何も渡さなければ合成モデルを小さな画面で描いて、画素の性質だけを確かめる。
    fn render_at(
        off: &miharashi_gpu::Offscreen,
        size: (u32, u32),
        model_path: Option<&str>,
        region: crate::StageRegion,
        frame: crate::Framing,
        follow: bool,
        secs: f32,
    ) -> Vec<u8> {
        use crate::{Model, Renderer, VrmLayer, synth};
        use miharashi_gpu::compose::Compositor;
        let (rx, ry, rw, rh) = region.rect(size);
        let t0 = Instant::now();
        let mut text = text();
        let mut stage = AgentStage::new(size, 8, Duration::from_secs(3), t0);
        let model = match model_path {
            Some(p) => Model::from_path(std::path::Path::new(p)).unwrap(),
            None => Model::from_glb(&synth::build_vrm_glb(&synth::Opts::default())).unwrap(),
        };
        let (_, img) = off
            .with_fbo(size, |gl| {
                let target = unsafe {
                    glow::HasContext::get_parameter_framebuffer(gl, glow::FRAMEBUFFER_BINDING)
                };
                let mut comp = Compositor::new(gl, size).unwrap();
                let layer = VrmLayer::new(gl, (rw, rh)).unwrap();
                unsafe { glow::HasContext::bind_framebuffer(gl, glow::FRAMEBUFFER, target) };
                let mut r = Renderer::new(gl, model).unwrap();
                r.clear_color = [0.0; 4];
                r.set_gesture(true).unwrap();
                let head = (
                    rx as f32 + rw as f32 / 2.0,
                    ry as f32
                        + rh as f32
                            * match frame {
                                crate::Framing::Bust => 0.22,
                                crate::Framing::Waist => 0.17,
                                crate::Framing::Full => 0.10,
                            },
                );
                let mut tracker = AimTracker::default();
                // 30fps で最後の時刻まで進める(カードの状態・フォーカス・身振りの時間を実際と同じに)
                let step = 1.0 / 30.0;
                let n = (secs / step).round() as u32;
                for k in 0..=n {
                    let t = k as f32 * step;
                    let now = t0 + Duration::from_secs_f32(t);
                    stage.update(now, &mut text);
                    if follow {
                        r.set_aim(tracker.step(now, stage.aim_target(), head, size.1 as f32 * 0.5));
                    }
                    r.animate(t, step);
                }
                let cards = stage.gpu_cards();
                let f = stage.frame(&cards);
                miharashi_gpu::draw_frame_with(&mut comp, gl, size, target, &f, |gl, _| {
                    r.update(gl);
                    let cam = r.framing_camera(frame, (rw, rh), 0.0);
                    layer.bind(gl);
                    r.render_camera(gl, (rw, rh), &cam.0, cam.1);
                    layer.composite(gl, target, (rx as i32, ry as i32));
                })
                .unwrap();
                r.destroy(gl);
                layer.destroy(gl);
                comp.destroy(gl);
            })
            .unwrap();
        img
    }

    #[test]
    fn vrm_layer_overlays_agent_frame_offscreen() {
        use crate::{Framing, StageRegion};
        let Ok(off) = miharashi_gpu::Offscreen::open(std::path::Path::new("/dev/dri/renderD128"))
        else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        if let Ok(dir) = std::env::var("VRM_AGENTS_PNG_DIR") {
            let model = std::env::var("VRM_MODEL").ok();
            let size = (2560u32, 1080u32);
            let dir = std::path::Path::new(&dir);
            std::fs::create_dir_all(dir).unwrap();
            // (名前, 時刻, 領域, 構図, 連動)。カードの変化は 3 秒ごと、最初のフォーカスは 3.0〜9.5 秒
            let shots: &[(&str, f32, StageRegion, Framing, bool)] = &[
                (
                    "a-before-right",
                    2.0,
                    StageRegion::Right,
                    Framing::Bust,
                    true,
                ),
                ("b-in-right", 3.4, StageRegion::Right, Framing::Bust, true),
                ("c-hold-right", 6.0, StageRegion::Right, Framing::Bust, true),
                ("d-out-right", 9.1, StageRegion::Right, Framing::Bust, true),
                ("e-hold-left", 6.0, StageRegion::Left, Framing::Bust, true),
                (
                    "f-hold-right-nofollow",
                    6.0,
                    StageRegion::Right,
                    Framing::Bust,
                    false,
                ),
                (
                    "g-hold-right-waist",
                    6.0,
                    StageRegion::Right,
                    Framing::Waist,
                    true,
                ),
            ];
            for (name, secs, region, frame, follow) in shots {
                let img = render_at(
                    &off,
                    size,
                    model.as_deref(),
                    *region,
                    *frame,
                    *follow,
                    *secs,
                );
                image::RgbaImage::from_raw(size.0, size.1, img)
                    .unwrap()
                    .save(dir.join(format!("{name}.png")))
                    .unwrap();
            }
            return;
        }
        // 合成モデルで: 領域の中にモデルが描かれ、領域の外はカードのまま。フォーカス中は scrim で暗くなる
        let size = (1280u32, 540u32);
        let (rx, _, rw, _) = StageRegion::Right.rect(size);
        let before = render_at(
            &off,
            size,
            None,
            StageRegion::Right,
            Framing::Full,
            false,
            1.0,
        );
        let hold = render_at(
            &off,
            size,
            None,
            StageRegion::Right,
            Framing::Full,
            false,
            6.0,
        );
        let px = |img: &[u8], x: u32, y: u32| -> [u8; 4] {
            img[((y * size.0 + x) * 4) as usize..][..4]
                .try_into()
                .unwrap()
        };
        assert_eq!(px(&before, 3, 3)[3], 255, "不透明な背景");
        // 領域の外の隅(左上のヘッダー)は、フォーカス中に暗くならない背景のまま(背景は scrim の下で暗くなる)
        let lum = |p: [u8; 4]| p[0] as u32 + p[1] as u32 + p[2] as u32;
        let mut dark = 0;
        for x in (0..rx).step_by(7) {
            for y in (100..440).step_by(7) {
                if lum(px(&hold, x, y)) < lum(px(&before, x, y)) {
                    dark += 1;
                }
            }
        }
        assert!(dark > 200, "フォーカス中は領域の外が暗くなる: {dark}");
        let changed = (0..size.1)
            .flat_map(|y| (rx..rx + rw).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&before, x, y) != px(&hold, x, y))
            .count();
        assert!(changed > 500, "領域内にモデルがある: {changed}");
    }
}
