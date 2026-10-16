//! 操作不要の T3 Agent カードボード。ボードは 1 枚で、スレッドはプロジェクトごとの枠にまとめる。
mod layout;

use crate::agents::{AgentChangeKind, AgentConnection, AgentEntry, AgentSnapshot};
use crate::screen::{Composite, Outcome, RenderContext, Screen, ScreenAction, ScreenId};
use miharashi_render::agents as draw;
use miharashi_render::{Canvas, Pixmap};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const MAX_QUEUE: usize = 32;
const NEW_INTRO: Duration = Duration::from_millis(700);
const RETURN: Duration = Duration::from_millis(800);
const MAX_AGE_SECS: Duration = Duration::from_secs(30);
const MOVE_TWEEN: Duration = Duration::from_millis(500);
/// ヘッダーと集計行の下端、フッターの区切り線より上が、枠を並べる領域。
const BOARD_TOP: f32 = 212.0;
const BOARD_BOTTOM_MARGIN: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    New,
    Changed,
}

#[derive(Clone, Debug)]
struct QueuedEvent {
    thread_id: String,
    kind: EventKind,
    before: String,
    after: String,
    observed_at: SystemTime,
    priority: u8,
}

#[derive(Clone, Debug)]
struct Focus {
    event: QueuedEvent,
    started: Instant,
    hold: Duration,
    stage: FocusStage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusStage {
    In,
    Hold,
    Out,
}

/// 並び替えや件数の変化で配置が変わったとき、カードを新しい位置へ滑らせる。
#[derive(Clone, Copy, Debug)]
struct PoseTween {
    from: draw::AgentPose,
    to: draw::AgentPose,
    started: Instant,
}

impl PoseTween {
    fn at(&self, now: Instant) -> draw::AgentPose {
        let x = (now.saturating_duration_since(self.started).as_secs_f32()
            / MOVE_TWEEN.as_secs_f32())
        .clamp(0.0, 1.0);
        let p = x * x * (3.0 - 2.0 * x);
        draw::AgentPose {
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

/// 1 つのプロジェクトのスレッド。重要な順に並ぶ。
struct Group {
    label: String,
    entries: Vec<AgentEntry>,
}

pub struct AgentScreen {
    size: (u32, u32),
    stale_secs: u64,
    gpu: bool,
    surfaces: HashMap<String, Arc<Pixmap>>,
    revisions: HashMap<String, u64>,
    content_keys: HashMap<String, String>,
    entry_keys: HashMap<String, String>,
    /// 初めて見た順。並びの最後の決め手と、表示中スレッドの集合を兼ねる。
    seen: HashMap<String, u64>,
    project_seen: HashMap<String, u64>,
    next_seen: u64,
    last_activity: HashMap<String, Instant>,
    poses: HashMap<String, PoseTween>,
    board: layout::Layout,
    goal_notified: HashMap<String, Instant>,
    stale: bool,
    overflow_updates: usize,
    queue: VecDeque<QueuedEvent>,
    pending_new: HashSet<String>,
    focus: Option<Focus>,
    change_cursor: u64,
    resync_generation: u64,
    observed_sync: bool,
    presentable: bool,
    paused_at: Option<Instant>,
    background: Option<Pixmap>,
    background_dirty: bool,
    scene_cards: Vec<draw::AgentCardSurface>,
    scrim_alpha: f32,
    focus_card_id: Option<u64>,
    id_map: HashMap<String, u64>,
    text_dirty: bool,
    entries: Vec<AgentEntry>,
    last_snapshot: AgentSnapshot,
    last_now: Instant,
    header_due: Instant,
}

impl AgentScreen {
    pub fn new(size: (u32, u32), gpu: bool, stale_secs: u64) -> Self {
        Self {
            size,
            stale_secs,
            gpu,
            surfaces: HashMap::new(),
            revisions: HashMap::new(),
            content_keys: HashMap::new(),
            entry_keys: HashMap::new(),
            seen: HashMap::new(),
            project_seen: HashMap::new(),
            next_seen: 0,
            last_activity: HashMap::new(),
            poses: HashMap::new(),
            board: layout::Layout::default(),
            goal_notified: HashMap::new(),
            stale: false,
            overflow_updates: 0,
            queue: VecDeque::new(),
            focus: None,
            change_cursor: 0,
            resync_generation: 0,
            observed_sync: false,
            presentable: true,
            paused_at: None,
            background: None,
            background_dirty: true,
            scene_cards: Vec::new(),
            scrim_alpha: 0.0,
            focus_card_id: None,
            id_map: HashMap::new(),
            text_dirty: true,
            entries: Vec::new(),
            pending_new: HashSet::new(),
            last_snapshot: AgentSnapshot::default(),
            last_now: Instant::now(),
            header_due: Instant::now() + Duration::from_secs(1),
        }
    }

    fn sync(&mut self, snap: &AgentSnapshot, now: Instant) -> bool {
        // A large time step can skip over the frame that normally ends a focus.
        // Settle it before processing new changes so a same-thread update is queued
        // as its own event instead of being merged into an already-expired focus.
        let mut redraw = self.finish_expired_focus(now);
        let was_stale = self.stale;
        let entries_same = Arc::ptr_eq(&snap.entries, &self.last_snapshot.entries);
        let changes_same = Arc::ptr_eq(&snap.changes, &self.last_snapshot.changes);
        let meta_changed = snap.connection != self.last_snapshot.connection
            || snap.last_success != self.last_snapshot.last_success
            || snap.environment_id != self.last_snapshot.environment_id
            || snap.environment_label != self.last_snapshot.environment_label
            || snap.error_summary != self.last_snapshot.error_summary;
        if meta_changed {
            self.background_dirty = true;
            redraw = true;
        }
        let generation_changed = self.resync_generation != snap.resync_generation;
        if generation_changed {
            self.resync_generation = snap.resync_generation;
            self.queue.clear();
            self.pending_new.clear();
            self.focus = None;
            self.change_cursor = snap.change_cursor;
            self.background_dirty = true;
            redraw = true;
        }
        let now_stale = snap.last_success_mono.is_some_and(|t| {
            now.saturating_duration_since(t) > Duration::from_secs(self.stale_secs)
        });
        if now_stale != self.stale {
            if now_stale {
                self.queue.clear();
                self.pending_new.clear();
                self.focus = None;
                self.scrim_alpha = 0.0;
            } else {
                self.queue.clear();
                self.pending_new.clear();
                self.focus = None;
                self.change_cursor = snap.change_cursor;
            }
            self.stale = now_stale;
            self.background_dirty = true;
            redraw = true;
        }
        let sync_marker_changed = snap.synchronized != self.last_snapshot.synchronized
            || snap.change_cursor != self.last_snapshot.change_cursor;
        if entries_same
            && changes_same
            && !generation_changed
            && !sync_marker_changed
            && was_stale == self.stale
        {
            self.last_snapshot = snap.clone();
            return redraw;
        }
        let current_ids: std::collections::HashSet<&str> =
            snap.entries.iter().map(|e| e.thread_id.as_str()).collect();
        if let Some(f) = &self.focus
            && !current_ids.contains(f.event.thread_id.as_str())
        {
            self.focus = None;
            self.scrim_alpha = 0.0;
            redraw = true;
        }
        self.queue
            .retain(|e| current_ids.contains(e.thread_id.as_str()));
        self.pending_new
            .retain(|id| current_ids.contains(id.as_str()));
        let entries = snap.entries.to_vec();
        for entry in &entries {
            if !self.project_seen.contains_key(&entry.project_id) {
                self.project_seen
                    .insert(entry.project_id.clone(), self.next_seen);
                self.next_seen += 1;
            }
            if !self.seen.contains_key(&entry.thread_id) {
                self.seen.insert(entry.thread_id.clone(), self.next_seen);
                self.next_seen += 1;
                self.last_activity.insert(entry.thread_id.clone(), now);
                self.id_map.insert(
                    entry.thread_id.clone(),
                    stable_id(&format!(
                        "{}:{}",
                        snap.environment_id.as_deref().unwrap_or("t3"),
                        entry.thread_id
                    )),
                );
                self.background_dirty = true;
                redraw = true;
            }
        }
        for entry in &entries {
            let key = content_key(entry);
            if self.entry_keys.get(&entry.thread_id) != Some(&key) {
                self.entry_keys.insert(entry.thread_id.clone(), key);
                redraw = true;
            }
        }
        let removed: Vec<String> = self
            .seen
            .keys()
            .filter(|id| !current_ids.contains(id.as_str()))
            .cloned()
            .collect();
        let live_projects = entries
            .iter()
            .map(|e| e.project_id.as_str())
            .collect::<HashSet<_>>();
        self.project_seen
            .retain(|id, _| live_projects.contains(id.as_str()));
        for id in removed {
            self.seen.remove(&id);
            self.surfaces.remove(&id);
            self.content_keys.remove(&id);
            self.entry_keys.remove(&id);
            self.last_activity.remove(&id);
            self.poses.remove(&id);
            self.goal_notified.remove(&id);
            self.id_map.remove(&id);
            self.pending_new.remove(&id);
            self.background_dirty = true;
            redraw = true;
        }
        let fresh_sync = snap.synchronized && !self.observed_sync;
        if fresh_sync {
            self.observed_sync = true;
            self.change_cursor = snap.change_cursor;
        }
        if !snap.synchronized {
            self.observed_sync = false;
        }
        if !generation_changed && !fresh_sync && !(was_stale && !self.stale) && !changes_same {
            let previous_cursor = self.change_cursor;
            for change in snap.changes.iter().filter(|c| c.id > previous_cursor) {
                self.change_cursor = self.change_cursor.max(change.id);
                self.last_activity.insert(change.thread_id.clone(), now);
                if self.presentable
                    && !self.stale
                    && let Some(entry) = entries.iter().find(|e| e.thread_id == change.thread_id)
                {
                    let kind = if change.kind == AgentChangeKind::New {
                        EventKind::New
                    } else {
                        EventKind::Changed
                    };
                    if change.kind == AgentChangeKind::Goal {
                        if self.goal_notified.get(&change.thread_id).is_some_and(|t| {
                            now.saturating_duration_since(*t) < Duration::from_secs(15)
                        }) {
                            continue;
                        }
                        self.goal_notified.insert(change.thread_id.clone(), now);
                    }
                    let before = describe_change(
                        change.before_state,
                        change.before_goal.as_deref(),
                        change.before_title.as_deref(),
                        change.kind,
                    );
                    let after = describe_change(
                        change.after_state.or(Some(entry.state)),
                        change.after_goal.as_deref().or(entry.goal.as_deref()),
                        change.after_title.as_deref().or(Some(entry.title.as_str())),
                        change.kind,
                    );
                    let priority = if attention(entry)
                        || matches!(
                            change.kind,
                            AgentChangeKind::New
                                | AgentChangeKind::Error
                                | AgentChangeKind::TurnCompleted
                                | AgentChangeKind::Resumed
                        ) {
                        2
                    } else if change.kind == AgentChangeKind::Goal {
                        0
                    } else {
                        1
                    };
                    self.enqueue(QueuedEvent {
                        thread_id: change.thread_id.clone(),
                        kind,
                        before,
                        after,
                        observed_at: change.observed_at,
                        priority,
                    });
                }
                self.background_dirty = true;
                redraw = true;
            }
        } else {
            self.change_cursor = self.change_cursor.max(snap.change_cursor);
        }
        self.entries = entries;
        self.last_snapshot = snap.clone();
        if redraw {
            self.background_dirty = true;
            self.text_dirty = true;
        }
        redraw
    }

    fn enqueue(&mut self, event: QueuedEvent) {
        if event.kind == EventKind::New {
            self.pending_new.insert(event.thread_id.clone());
        }
        if self.queue.iter().any(|q| q.thread_id == event.thread_id) {
            if let Some(q) = self
                .queue
                .iter_mut()
                .find(|q| q.thread_id == event.thread_id)
            {
                q.after = event.after;
                q.observed_at = event.observed_at;
                if event.kind == EventKind::New {
                    q.kind = EventKind::New;
                }
            }
        } else if self
            .focus
            .as_ref()
            .is_some_and(|f| f.event.thread_id == event.thread_id)
        {
            if let Some(f) = self.focus.as_mut() {
                f.event.after = event.after;
                f.event.observed_at = event.observed_at;
            }
        } else if self.queue.len() < MAX_QUEUE {
            self.queue.push_back(event);
        } else {
            self.pending_new.remove(&event.thread_id);
            self.overflow_updates += 1;
            self.background_dirty = true;
        }
    }

    fn finish_expired_focus(&mut self, now: Instant) -> bool {
        let expired = self.focus.as_ref().is_some_and(|focus| {
            let total = NEW_INTRO + focus.hold + RETURN;
            now.saturating_duration_since(focus.started) >= total
        });
        if expired {
            self.focus = None;
            self.scrim_alpha = 0.0;
            self.focus_card_id = None;
            self.background_dirty = true;
        }
        expired
    }

    fn scale(&self) -> f32 {
        (self.size.0 as f32 / 1920.0)
            .min(self.size.1 as f32 / 1080.0)
            .max(0.35)
    }

    /// 要確認、実行中、最近動いたもの、T3 の更新時刻、初めて見た順。
    fn rank(&self, a: &AgentEntry, b: &AgentEntry) -> std::cmp::Ordering {
        let tier = |e: &AgentEntry| {
            if attention(e) {
                0
            } else if running(e) {
                1
            } else {
                2
            }
        };
        tier(a)
            .cmp(&tier(b))
            .then_with(|| {
                self.last_activity
                    .get(&b.thread_id)
                    .cmp(&self.last_activity.get(&a.thread_id))
            })
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| {
                self.seen
                    .get(&a.thread_id)
                    .cmp(&self.seen.get(&b.thread_id))
            })
    }

    /// プロジェクトを初めて見た順に並べ、各枠の中を重要な順に並べる。
    fn groups(&self) -> Vec<Group> {
        let mut groups: Vec<(u64, &str, Group)> = Vec::new();
        for e in &self.entries {
            if let Some((_, _, g)) = groups.iter_mut().find(|(_, id, _)| *id == e.project_id) {
                g.entries.push(e.clone());
            } else {
                let label = if e.project_title.is_empty() {
                    short(&e.project_id)
                } else {
                    e.project_title.clone()
                };
                groups.push((
                    self.project_seen
                        .get(&e.project_id)
                        .copied()
                        .unwrap_or(u64::MAX),
                    &e.project_id,
                    Group {
                        label,
                        entries: vec![e.clone()],
                    },
                ));
            }
        }
        groups.sort_by_key(|(seq, _, _)| *seq);
        groups
            .into_iter()
            .map(|(_, _, mut g)| {
                g.entries.sort_by(|a, b| self.rank(a, b));
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
            .map(|g| g.entries.iter().map(|e| e.thread_id.clone()).collect())
            .collect::<Vec<Vec<String>>>();
        layout::layout(area, self.scale(), &ids)
    }

    fn activate_event(&mut self, now: Instant) -> bool {
        if self.focus.is_some() || !self.presentable {
            return false;
        }
        let mut stale = Vec::new();
        let wall = SystemTime::now();
        for (i, e) in self.queue.iter().enumerate() {
            if self.seen.contains_key(&e.thread_id)
                && wall.duration_since(e.observed_at).unwrap_or_default() > MAX_AGE_SECS
            {
                stale.push(i);
            }
        }
        if !stale.is_empty() {
            let count = stale.len();
            let last = *stale.last().unwrap();
            let mut aggregate = self.queue.remove(last).unwrap();
            for i in stale.into_iter().rev().skip(1) {
                self.queue.remove(i);
            }
            aggregate.before = "前回取得以降".into();
            aggregate.after = if count > 1 {
                format!("ほか {} 件更新", count)
            } else {
                "更新内容をまとめました".into()
            };
            aggregate.priority = 1;
            self.queue.push_front(aggregate);
            self.overflow_updates = self
                .overflow_updates
                .saturating_add(count.saturating_sub(1));
            self.background_dirty = true;
        }
        let board = self.layout(&self.groups());
        let pos = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, e)| board.cards.contains_key(&e.thread_id))
            .max_by_key(|(i, e)| (e.priority, std::cmp::Reverse(*i)))
            .map(|(i, _)| i);
        let Some(pos) = pos else { return false };
        let Some(event) = self.queue.remove(pos) else {
            return false;
        };
        self.pending_new.remove(&event.thread_id);
        let urgent = self
            .entries
            .iter()
            .find(|e| e.thread_id == event.thread_id)
            .is_some_and(attention);
        self.focus = Some(Focus {
            event,
            started: now,
            hold: Duration::from_secs(if urgent { 5 } else { 3 }),
            stage: FocusStage::In,
        });
        true
    }

    fn advance(&mut self, now: Instant) -> bool {
        if !self.presentable || self.stale {
            return false;
        }
        let mut redraw = false;
        if self.focus.is_none() {
            redraw |= self.activate_event(now);
        }
        if let Some(f) = self.focus.as_mut() {
            let elapsed = now.saturating_duration_since(f.started);
            let intro = NEW_INTRO;
            let stage = if elapsed < intro {
                FocusStage::In
            } else if elapsed < intro + f.hold {
                FocusStage::Hold
            } else {
                FocusStage::Out
            };
            if stage != f.stage {
                f.stage = stage;
                redraw = true;
            }
            if elapsed >= intro + f.hold + RETURN {
                self.focus = None;
                redraw = true;
            } else if matches!(stage, FocusStage::In | FocusStage::Out) {
                redraw = true;
            }
        }
        redraw
    }

    fn render_scene(&mut self, ctx: &mut RenderContext<'_>, composite_cpu: bool) {
        let snap = self.last_snapshot.clone();
        let now = self.last_now;
        let groups = self.groups();
        let board = self.layout(&groups);
        if board != self.board {
            self.board = board;
            self.background_dirty = true;
        }
        if self.background_dirty || self.background.is_none() {
            let mut bg = Canvas::new(self.size.0, self.size.1);
            draw_background(
                &mut bg,
                ctx.text,
                &snap,
                &groups,
                &self.board,
                self.scale(),
                self.overflow_updates,
                now,
                self.stale_secs,
            );
            self.background = Some(bg.pixmap);
            self.background_dirty = false;
        }
        let focus = self.focus.clone();
        let scale = self.scale();
        let mut shown = HashSet::new();
        let mut cards = Vec::new();
        for entry in groups.iter().flat_map(|g| &g.entries) {
            let Some(slot) = self.board.cards.get(&entry.thread_id).copied() else {
                continue;
            };
            let is_focus = focus
                .as_ref()
                .is_some_and(|f| f.event.thread_id == entry.thread_id);
            if self.pending_new.contains(&entry.thread_id) && !is_focus {
                continue;
            }
            shown.insert(entry.thread_id.clone());
            let target = draw::AgentPose {
                x: slot.rect.x,
                y: slot.rect.y,
                width: slot.rect.w,
                height: slot.rect.h,
                alpha: if self.stale { 0.65 } else { 1.0 },
            };
            let home = self.tween_pose(&entry.thread_id, target, now);
            let mut pose = home;
            if let Some(f) = focus.as_ref().filter(|_| is_focus) {
                let elapsed = now.saturating_duration_since(f.started).as_secs_f32();
                let intro = NEW_INTRO.as_secs_f32();
                let end_in = intro + f.hold.as_secs_f32();
                let end = end_in + RETURN.as_secs_f32();
                let center = (self.size.0 as f32 * 0.5, self.size.1 as f32 * 0.52);
                let focus_w = 880.0 * scale;
                let focus_h = 420.0 * scale;
                // The focus surface keeps its own aspect while it leaves and returns,
                // so its text is never stretched to the shape of the board slot.
                let w = home.width;
                let h = w * focus_h / focus_w;
                let home_c = (home.x + home.width / 2.0, home.y + home.height / 2.0);
                let t = |v: f32| {
                    let x = v.clamp(0.0, 1.0);
                    x * x * (3.0 - 2.0 * x)
                };
                if elapsed < intro {
                    let p = t(elapsed / intro);
                    let start = if f.event.kind == EventKind::New {
                        (self.size.0 as f32 + focus_w, center.1)
                    } else {
                        home_c
                    };
                    pose.x = lerp(start.0 - w / 2.0, center.0 - focus_w / 2.0, p);
                    pose.y = lerp(start.1 - h / 2.0, center.1 - focus_h / 2.0, p);
                    pose.width = lerp(w, focus_w, p);
                    pose.height = lerp(h, focus_h, p);
                    pose.alpha = if f.event.kind == EventKind::New {
                        p
                    } else {
                        1.0
                    };
                    self.scrim_alpha = 0.46 * p;
                } else if elapsed < end_in {
                    pose.x = center.0 - focus_w / 2.0;
                    pose.y = center.1 - focus_h / 2.0;
                    pose.width = focus_w;
                    pose.height = focus_h;
                    pose.alpha = 1.0;
                    self.scrim_alpha = 0.46;
                } else {
                    let p = t((elapsed - end_in) / (end - end_in));
                    pose.x = lerp(center.0 - focus_w / 2.0, home_c.0 - w / 2.0, p);
                    pose.y = lerp(center.1 - focus_h / 2.0, home_c.1 - h / 2.0, p);
                    pose.width = lerp(focus_w, w, p);
                    pose.height = lerp(focus_h, h, p);
                    pose.alpha = 1.0;
                    self.scrim_alpha = 0.46 * (1.0 - p);
                }
                let id = self.id_map[&entry.thread_id];
                self.focus_card_id = Some(id);
                let summary = if f.event.kind == EventKind::New {
                    format!("新しく検出 · {}", state_label(entry.state))
                } else {
                    format!("{} → {}", f.event.before, f.event.after)
                };
                let content = self.content(entry, now, Some(summary), false, true);
                self.ensure_surface(&entry.thread_id, &content, (focus_w, focus_h), ctx.text);
            } else {
                let compact = slot.mode == layout::CardMode::Strip;
                let content = self.content(entry, now, None, compact, false);
                self.ensure_surface(
                    &entry.thread_id,
                    &content,
                    (slot.rect.w, slot.rect.h),
                    ctx.text,
                );
            }
            cards.push((
                entry.thread_id.clone(),
                self.id_map[&entry.thread_id],
                pose,
                (is_focus, slot.z),
            ));
        }
        self.surfaces.retain(|id, _| shown.contains(id));
        self.content_keys.retain(|id, _| shown.contains(id));
        let placed = &self.board.cards;
        self.poses.retain(|id, _| placed.contains_key(id));
        if focus.is_none() {
            self.scrim_alpha = 0.0;
            self.focus_card_id = None;
        }
        // Strips first, then the full cards in front of them, then the focus.
        cards.sort_by_key(|(_, _, _, order)| *order);
        self.scene_cards.clear();
        for (id, stable, pose, _) in cards {
            let surface = self.surfaces.get(&id).expect("surface prepared").clone();
            self.scene_cards.push(draw::AgentCardSurface {
                id: stable,
                revision: self.revisions[&id],
                surface,
                pose,
            });
        }
        self.id_map.retain(|id, _| self.seen.contains_key(id));
        let scene = draw::AgentScene {
            size: self.size,
            background: Some(self.background.as_ref().unwrap().data()),
            cards: &self.scene_cards,
            scrim_alpha: self.scrim_alpha,
            focus_card_id: self.focus_card_id,
        };
        if composite_cpu && let Ok(frame) = draw::render_scene_cpu(&scene) {
            ctx.canvas.pixmap.data_mut().copy_from_slice(frame.data());
        }
        self.text_dirty = false;
    }

    fn tween_pose(&mut self, id: &str, target: draw::AgentPose, now: Instant) -> draw::AgentPose {
        let tween = self.poses.entry(id.to_string()).or_insert(PoseTween {
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

    fn ensure_surface(
        &mut self,
        id: &str,
        content: &draw::AgentCardContent,
        size: (f32, f32),
        text: &mut miharashi_render::TextEngine,
    ) {
        let size = (
            size.0.round().max(1.0) as u32,
            size.1.round().max(1.0) as u32,
        );
        let key = format!(
            "{}|{}x{}",
            content_key_from_display(content),
            size.0,
            size.1
        );
        if self.content_keys.get(id) != Some(&key) || !self.surfaces.contains_key(id) {
            let rev = self.revisions.entry(id.to_string()).or_insert(0);
            *rev = rev.wrapping_add(1);
            self.content_keys.insert(id.to_string(), key);
            self.surfaces.insert(
                id.to_string(),
                Arc::new(draw::render_card(content, size, text)),
            );
        }
    }

    fn content(
        &self,
        e: &AgentEntry,
        now: Instant,
        badge: Option<String>,
        compact: bool,
        focused: bool,
    ) -> draw::AgentCardContent {
        let state = draw_state(e.state);
        let mut detail = if e.has_pending_approvals {
            "手元の T3 で承認".into()
        } else if e.needs_reauth {
            "手元の T3 で認証".into()
        } else if e.has_pending_user_input {
            "手元の T3 で入力".into()
        } else if let Some(error) = e
            .error_summary
            .as_ref()
            .filter(|_| e.state == crate::agents::AgentState::Error)
        {
            // V2 の lastError は thread に残り続けるので、いまの run が失敗したときだけ出す。
            error.clone()
        } else if let Some(goal) = &e.goal {
            format!("目標 · {goal}")
        } else if e.background_tasks > 0 {
            format!("バックグラウンドの作業 {} 件を待機", e.background_tasks)
        } else {
            state.label().into()
        };
        if self.stale {
            detail = format!("前回取得時 · {detail}");
        }
        let active = self.last_activity.get(&e.thread_id).copied().unwrap_or(now);
        let secs = now.saturating_duration_since(active).as_secs();
        let age = Some(if secs < 60 {
            "1分未満".into()
        } else if secs < 3600 {
            format!("{}分", secs / 60)
        } else {
            format!("{}時間", secs / 3600)
        });
        draw::AgentCardContent {
            project: match (focused, e.project_title.is_empty()) {
                (false, _) => String::new(),
                (true, true) => short(&e.project_id),
                (true, false) => e.project_title.clone(),
            },
            title: if e.title.is_empty() {
                "無題".into()
            } else {
                e.title.clone()
            },
            state,
            detail: badge.clone().unwrap_or(detail),
            provider_model: match (&e.provider, &e.model) {
                (Some(p), Some(m)) => Some(format!("{p} · {m}")),
                (Some(p), None) => Some(p.clone()),
                (None, Some(m)) => Some(m.clone()),
                _ => None,
            },
            age,
            // T3 V2 の一覧には計画の進み具合(何手中の何手目)が無いので、進捗バーは出さない。
            progress: None,
            badge: None,
            compact,
        }
    }
}

impl Screen for AgentScreen {
    fn id(&self) -> ScreenId {
        "agents"
    }
    fn title(&self) -> &'static str {
        "T3 AGENTS"
    }
    fn on_enter(&mut self, _: Instant) {
        // Show the current layout at once on return instead of sliding to it.
        self.poses.clear();
        self.text_dirty = true;
        self.background_dirty = true;
    }
    fn on_leave(&mut self, now: Instant) {
        self.set_presentation(false, false, true, now);
    }
    fn set_presentation(&mut self, visible: bool, covered: bool, display_on: bool, now: Instant) {
        let can_present = visible && !covered && display_on;
        if self.presentable && !can_present {
            self.queue.clear();
            self.pending_new.clear();
            self.focus = None;
            self.scrim_alpha = 0.0;
            self.paused_at = Some(now);
            self.background_dirty = true;
        } else if !self.presentable && can_present {
            self.poses.clear();
            self.queue.clear();
            self.pending_new.clear();
            self.focus = None;
            self.paused_at = None;
            self.change_cursor = self.last_snapshot.change_cursor;
            self.background_dirty = true;
        }
        self.presentable = can_present;
    }
    fn tick(&mut self, world: &crate::screen::World, now: Instant) -> bool {
        self.last_now = now;
        let mut redraw = self.sync(&world.agents, now);
        redraw |= self.advance(now);
        // Land each finished move on an exact frame, and keep drawing while any card moves.
        let mut moving = false;
        for tween in self.poses.values_mut() {
            if !tween.moving() {
                continue;
            }
            if now.saturating_duration_since(tween.started) >= MOVE_TWEEN {
                tween.from = tween.to;
                redraw = true;
            } else {
                moving = true;
            }
        }
        if self.presentable && moving {
            redraw = true;
        }
        if self.presentable && now >= self.header_due {
            self.header_due = now + Duration::from_secs(1);
            self.background_dirty = true;
            redraw = true;
        }
        redraw
    }
    fn handle_action(&mut self, _: ScreenAction, _: Instant) -> Outcome {
        Outcome::Ignored
    }
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        self.render_scene(ctx, true);
    }
    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        let background_changed = self.background_dirty || self.background.is_none();
        if self.gpu && !ctx.covered {
            // GLES uploads from the shared Canvas only when this background changes. Copy the
            // cached background there so pixels left by another screen cannot become its base.
            self.render_scene(ctx, false);
            if background_changed {
                ctx.canvas
                    .pixmap
                    .data_mut()
                    .copy_from_slice(self.background.as_ref().unwrap().data());
            }
            Composite::Agents {
                background_changed,
                cards: &self.scene_cards,
                scrim_alpha: self.scrim_alpha,
                focus_card_id: self.focus_card_id,
            }
        } else {
            self.render_scene(ctx, true);
            Composite::Canvas
        }
    }
    fn invalidate(&mut self) {
        self.background_dirty = true;
    }
    fn animating(&self) -> bool {
        self.gpu
            && self.presentable
            && (self
                .focus
                .as_ref()
                .is_some_and(|f| matches!(f.stage, FocusStage::In | FocusStage::Out))
                || self.poses.values().any(PoseTween::moving))
    }
    fn page(&self) -> Option<&'static str> {
        Some("OVERVIEW")
    }
    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        out.push(("agents", self.entries.len().to_string()));
    }
}

// These values form one frame-local header/status rendering context; keeping
// them explicit here makes the renderer's inputs easy to audit at its call site.
#[allow(clippy::too_many_arguments)]
fn draw_background(
    canvas: &mut Canvas,
    text: &mut miharashi_render::TextEngine,
    snap: &AgentSnapshot,
    groups: &[Group],
    board: &layout::Layout,
    scale: f32,
    overflow: usize,
    now: Instant,
    stale_secs: u64,
) {
    let w = canvas.width() as f32;
    let h = canvas.height() as f32;
    canvas.clear(miharashi_render::canvas::rgba(8, 12, 20, 255));
    text.draw(canvas, "T3 AGENTS", 32.0, 24.0, 36.0, [223, 231, 240, 255]);
    text.draw(canvas, "OVERVIEW", 280.0, 28.0, 25.0, [64, 200, 255, 255]);
    let env = snap
        .environment_label
        .as_deref()
        .or(snap.environment_id.as_deref())
        .unwrap_or("T3");
    let env = fit_header_label(env, 236.0, 24.0);
    text.draw(canvas, &env, 32.0, 100.0, 24.0, [223, 231, 240, 255]);
    text.draw_right(
        canvas,
        &chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string(),
        w - 32.0,
        26.0,
        22.0,
        [143, 163, 183, 255],
    );
    let stale = snap
        .last_success_mono
        .is_some_and(|t| now.saturating_duration_since(t) > Duration::from_secs(stale_secs));
    let (status, color) = match snap.connection {
        AgentConnection::Live if !stale => ("LIVE", [103, 217, 173, 255]),
        AgentConnection::Connecting => ("接続しています", [143, 163, 183, 255]),
        AgentConnection::AuthenticationRequired => ("認証が必要", [255, 188, 102, 255]),
        AgentConnection::Forbidden => ("閲覧権限がありません", [255, 127, 140, 255]),
        AgentConnection::ApiError => ("T3 API を確認してください", [255, 127, 140, 255]),
        _ if stale => ("STALE", [255, 188, 102, 255]),
        _ => ("再接続中", [255, 188, 102, 255]),
    };
    text.draw(canvas, status, 285.0, 100.0, 23.0, color);
    if let Some(at) = snap.last_success {
        let time = chrono::DateTime::<chrono::Local>::from(at).format("%H:%M:%S");
        text.draw(
            canvas,
            &format!("最終取得 {time}"),
            520.0,
            100.0,
            20.0,
            [143, 163, 183, 255],
        );
    }
    let counts = [
        (
            "実行中",
            snap.entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.state,
                        crate::agents::AgentState::Starting
                            | crate::agents::AgentState::Running
                            | crate::agents::AgentState::Monitoring
                    )
                })
                .count(),
            [64, 200, 255, 255],
        ),
        (
            "承認待ち",
            snap.entries
                .iter()
                .filter(|e| {
                    e.has_pending_approvals || e.state == crate::agents::AgentState::Approval
                })
                .count(),
            [255, 188, 102, 255],
        ),
        (
            "入力・計画",
            snap.entries
                .iter()
                .filter(|e| {
                    e.has_pending_user_input
                        || e.has_actionable_proposed_plan
                        || matches!(
                            e.state,
                            crate::agents::AgentState::Input | crate::agents::AgentState::Plan
                        )
                })
                .count(),
            [196, 160, 255, 255],
        ),
        (
            "異常",
            snap.entries
                .iter()
                .filter(|e| e.state == crate::agents::AgentState::Error)
                .count(),
            [255, 127, 140, 255],
        ),
        (
            "応答完了",
            snap.entries
                .iter()
                .filter(|e| e.state == crate::agents::AgentState::Completed)
                .count(),
            [103, 217, 173, 255],
        ),
        (
            "その他",
            snap.entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.state,
                        crate::agents::AgentState::Idle
                            | crate::agents::AgentState::Interrupted
                            | crate::agents::AgentState::Stopped
                            | crate::agents::AgentState::Unknown
                    )
                })
                .count(),
            [143, 163, 183, 255],
        ),
    ];
    let mut x = 32.0;
    for (label, count, color) in counts {
        text.draw(canvas, &format!("{label} {count}"), x, 174.0, 20.0, color);
        x += 175.0;
    }
    let total = snap.entries.len();
    let mut count = if snap.synchronized {
        format!("{total} スレッド · {} プロジェクト", groups.len())
    } else {
        "-- スレッド".into()
    };
    let urgent = snap.entries.iter().filter(|e| attention(e)).count();
    if urgent > 0 {
        count.push_str(&format!(" · 要確認 {urgent} 件"));
    }
    if overflow > 0 {
        count.push_str(&format!(" · ほか {overflow} 件更新"));
    }
    text.draw_right(canvas, &count, w - 32.0, 174.0, 21.0, [143, 163, 183, 255]);
    if snap.synchronized && total == 0 {
        text.draw(
            canvas,
            "対象のスレッドはありません",
            w * 0.5 - 190.0,
            h * 0.53,
            28.0,
            [143, 163, 183, 255],
        );
    }
    for lane in &board.lanes {
        let Some(group) = groups.get(lane.group) else {
            continue;
        };
        let r = lane.rect;
        canvas.fill_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            miharashi_render::canvas::rgba(120, 150, 190, 12),
        );
        canvas.stroke_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            1.0,
            miharashi_render::canvas::rgba(120, 150, 190, 28),
        );
        let head = lane.head;
        let name_size = 28.0 * scale;
        let count_size = 23.0 * scale;
        let count = group.entries.len().to_string();
        let count_w = text.measure(&count, count_size);
        let name = fit_header_label(
            &group.label,
            (head.w - count_w - 24.0 * scale).max(40.0),
            name_size,
        );
        text.draw(
            canvas,
            &name,
            head.x,
            head.y,
            name_size,
            [223, 231, 240, 255],
        );
        let mut x = head.x + text.measure(&name, name_size) + 10.0 * scale;
        text.draw(
            canvas,
            &count,
            x,
            head.y + 4.0 * scale,
            count_size,
            [143, 163, 183, 255],
        );
        x += count_w + 18.0 * scale;
        // One dot per thread, most important first, as many as the header has room for.
        let r_dot = 6.5 * scale;
        let cy = head.y + name_size * 0.62;
        for e in &group.entries {
            if x + r_dot * 2.0 > head.x + head.w {
                break;
            }
            let [cr, cg, cb] = draw_state(e.state).color();
            canvas.fill_circle(
                x + r_dot,
                cy,
                r_dot,
                miharashi_render::canvas::rgba(cr, cg, cb, 255),
            );
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
    canvas.fill_rect(
        0.0,
        80.0,
        w,
        1.0,
        miharashi_render::canvas::rgba(48, 67, 87, 255),
    );
    canvas.fill_rect(
        0.0,
        h - 72.0,
        w,
        1.0,
        miharashi_render::canvas::rgba(48, 67, 87, 255),
    );
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn stable_id(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h.max(1)
}
fn short(s: &str) -> String {
    s.chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}
fn fit_header_label(value: &str, max_width: f32, font_size: f32) -> String {
    let width = |c: char| {
        if c.is_ascii() {
            font_size * 0.55
        } else {
            font_size
        }
    };
    let full_width = value.chars().map(width).sum::<f32>();
    if full_width <= max_width {
        return value.to_string();
    }
    let ellipsis_width = font_size;
    let mut result = String::new();
    let mut used = 0.0;
    for c in value.chars() {
        let next = width(c);
        if used + next + ellipsis_width > max_width {
            break;
        }
        result.push(c);
        used += next;
    }
    result.push('…');
    result
}
fn attention(e: &AgentEntry) -> bool {
    e.has_pending_approvals
        || e.has_pending_user_input
        || e.has_actionable_proposed_plan
        || matches!(
            e.state,
            crate::agents::AgentState::Error
                | crate::agents::AgentState::Approval
                | crate::agents::AgentState::Input
                | crate::agents::AgentState::Plan
        )
}
fn running(e: &AgentEntry) -> bool {
    matches!(
        e.state,
        crate::agents::AgentState::Starting
            | crate::agents::AgentState::Running
            | crate::agents::AgentState::Monitoring
    )
}
fn draw_state(s: crate::agents::AgentState) -> draw::AgentState {
    match s {
        crate::agents::AgentState::Starting
        | crate::agents::AgentState::Running
        | crate::agents::AgentState::Monitoring => draw::AgentState::Running,
        crate::agents::AgentState::Approval => draw::AgentState::Approval,
        crate::agents::AgentState::Input | crate::agents::AgentState::Plan => {
            draw::AgentState::Input
        }
        crate::agents::AgentState::Error => draw::AgentState::Error,
        crate::agents::AgentState::Completed => draw::AgentState::Completed,
        crate::agents::AgentState::Idle
        | crate::agents::AgentState::Interrupted
        | crate::agents::AgentState::Stopped => draw::AgentState::Idle,
        crate::agents::AgentState::Unknown => draw::AgentState::Unknown,
    }
}
fn state_label(s: crate::agents::AgentState) -> String {
    draw_state(s).label().into()
}
fn describe_change(
    state: Option<crate::agents::AgentState>,
    goal: Option<&str>,
    title: Option<&str>,
    kind: AgentChangeKind,
) -> String {
    match kind {
        AgentChangeKind::Goal => goal
            .filter(|s| !s.is_empty())
            .map_or_else(|| "目標なし".into(), |g| format!("目標 · {g}")),
        AgentChangeKind::Title => title
            .filter(|s| !s.is_empty())
            .unwrap_or("無題")
            .to_string(),
        AgentChangeKind::Flags => "待ち状態の更新".into(),
        AgentChangeKind::Error => "異常".into(),
        AgentChangeKind::TurnCompleted => "応答完了".into(),
        AgentChangeKind::Resumed => "実行再開".into(),
        AgentChangeKind::New => "新しく検出".into(),
        AgentChangeKind::State => state.map(state_label).unwrap_or_else(|| "不明".into()),
    }
}
fn content_key(e: &AgentEntry) -> String {
    format!(
        "{:?}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{}{}{}",
        e.state,
        e.project_title,
        e.title,
        e.goal,
        e.background_tasks,
        e.provider,
        e.model,
        e.error_summary,
        e.raw_status,
        e.has_pending_approvals as u8,
        e.has_pending_user_input as u8,
        e.has_actionable_proposed_plan as u8
    )
}
fn content_key_from_display(c: &draw::AgentCardContent) -> String {
    format!(
        "{:?}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
        c.state,
        c.project,
        c.title,
        c.detail,
        c.provider_model,
        c.age,
        c.progress,
        c.badge,
        c.compact
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentChange, AgentChangeKind, AgentState};
    use std::time::UNIX_EPOCH;
    fn entry(id: &str, state: AgentState) -> AgentEntry {
        AgentEntry {
            thread_id: id.into(),
            project_id: "p".into(),
            project_title: "project".into(),
            title: id.into(),
            state,
            ..Default::default()
        }
    }
    #[test]
    fn homes_stay_stable_when_entries_arrive_and_drop() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let mut a = AgentSnapshot {
            entries: vec![
                entry("a", AgentState::Running),
                entry("b", AgentState::Idle),
            ]
            .into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&a, now);
        let slot = s.seen["b"];
        a.entries = vec![
            entry("a", AgentState::Running),
            entry("c", AgentState::Idle),
            entry("b", AgentState::Idle),
        ]
        .into();
        s.sync(&a, now);
        assert_eq!(s.seen["b"], slot);
    }
    #[test]
    fn same_thread_changes_coalesce_and_focus_is_single() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let mut snap = AgentSnapshot {
            entries: vec![entry("a", AgentState::Running)].into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        s.enqueue(QueuedEvent {
            thread_id: "a".into(),
            kind: EventKind::Changed,
            before: "待機".into(),
            after: "実行中".into(),
            observed_at: SystemTime::now(),
            priority: 1,
        });
        s.enqueue(QueuedEvent {
            thread_id: "a".into(),
            kind: EventKind::Changed,
            before: "待機".into(),
            after: "承認待ち".into(),
            observed_at: SystemTime::now(),
            priority: 2,
        });
        assert_eq!(s.queue.len(), 1);
        s.activate_event(now);
        assert!(s.focus.is_some());
        snap.entries = Vec::new().into();
        s.sync(&snap, now);
        assert!(s.focus.is_none());
    }
    #[test]
    fn same_thread_change_after_skipped_focus_deadline_gets_its_own_notice() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let initial = AgentSnapshot {
            entries: vec![entry("a", AgentState::Running)].into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&initial, now);
        s.enqueue(QueuedEvent {
            thread_id: "a".into(),
            kind: EventKind::New,
            before: "新しく検出".into(),
            after: "実行中".into(),
            observed_at: SystemTime::now(),
            priority: 2,
        });
        assert!(s.activate_event(now));

        let change_time = now + Duration::from_millis(5_350);
        let mut updated = entry("a", AgentState::Approval);
        updated.has_pending_approvals = true;
        let changed = AgentSnapshot {
            entries: vec![updated].into(),
            changes: vec![AgentChange {
                id: 1,
                thread_id: "a".into(),
                kind: AgentChangeKind::State,
                before_state: Some(AgentState::Running),
                after_state: Some(AgentState::Approval),
                before_goal: None,
                after_goal: None,
                before_title: None,
                after_title: None,
                observed_at: SystemTime::now(),
            }]
            .into(),
            change_cursor: 1,
            synchronized: true,
            ..initial
        };
        s.sync(&changed, change_time);

        assert!(s.focus.is_none());
        assert_eq!(s.queue.len(), 1);
        assert_eq!(s.queue[0].kind, EventKind::Changed);
        assert!(s.advance(change_time + Duration::from_millis(1)));
        assert_eq!(
            s.focus.as_ref().map(|f| f.event.thread_id.as_str()),
            Some("a")
        );
        assert_eq!(s.focus.as_ref().unwrap().event.kind, EventKind::Changed);
        assert!(s.focus.as_ref().unwrap().event.after.contains("承認待ち"));
    }

    #[test]
    fn header_label_is_fitted_to_its_reserved_width() {
        let source = "仮データ · network disabled";
        let fitted = fit_header_label(source, 236.0, 24.0);
        assert!(fitted.ends_with('…'));
        let estimated_width = fitted
            .chars()
            .map(|c| if c.is_ascii() { 24.0 * 0.55 } else { 24.0 })
            .sum::<f32>();
        assert!(estimated_width <= 236.0);
    }
    #[test]
    fn sync_suppresses_initial_change_flood_and_hidden_resume_is_quiet() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let change = AgentChange {
            id: 1,
            thread_id: "a".into(),
            kind: AgentChangeKind::New,
            before_state: None,
            after_state: Some(AgentState::Running),
            before_goal: None,
            after_goal: Some("目標".into()),
            before_title: None,
            after_title: Some("a".into()),
            observed_at: UNIX_EPOCH,
        };
        let snap = AgentSnapshot {
            entries: vec![entry("a", AgentState::Running)].into(),
            changes: vec![change].into(),
            change_cursor: 1,
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        assert!(s.queue.is_empty());
        s.set_presentation(false, false, true, now);
        s.set_presentation(true, false, true, now);
        assert!(s.queue.is_empty());
    }
    #[test]
    fn every_thread_is_on_the_single_board_grouped_by_project() {
        let now = Instant::now();
        let mut s = AgentScreen::new((2560, 1080), false, 30);
        let mut entries = (0..13)
            .map(|i| entry(&format!("a{i}"), AgentState::Idle))
            .collect::<Vec<_>>();
        for (i, e) in entries.iter_mut().enumerate() {
            e.project_id = format!("p{}", i % 3);
        }
        let mut snap = AgentSnapshot {
            entries: entries.clone().into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        let groups = s.groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups.iter().map(|g| g.entries.len()).sum::<usize>(), 13);
        let board = s.layout(&groups);
        assert_eq!(board.cards.len(), 13);
        let lane_of = |id: &str| {
            let x = board.cards[id].rect.x;
            board
                .lanes
                .iter()
                .position(|l| x >= l.rect.x && x < l.rect.x + l.rect.w)
                .unwrap()
        };
        assert_eq!(lane_of("a0"), lane_of("a3"));
        assert_ne!(lane_of("a0"), lane_of("a1"));

        // Removing and adding threads keeps one entry per thread and forgets the gone ones.
        snap.entries = entries
            .iter()
            .skip(6)
            .cloned()
            .chain((0..6).map(|i| entry(&format!("b{i}"), AgentState::Idle)))
            .collect::<Vec<_>>()
            .into();
        s.sync(&snap, now);
        assert_eq!(s.seen.len(), 13);
        assert!((0..6).all(|i| !s.seen.contains_key(&format!("a{i}"))));
        assert_eq!(s.layout(&s.groups()).cards.len(), 13);
    }
    #[test]
    fn attention_comes_first_in_its_project_then_running_then_recent() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let snap = AgentSnapshot {
            entries: vec![
                entry("idle", AgentState::Idle),
                entry("run", AgentState::Running),
                entry("ask", AgentState::Approval),
                entry("done", AgentState::Completed),
            ]
            .into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        s.last_activity
            .insert("done".into(), now + Duration::from_secs(5));
        let order = s.groups()[0]
            .entries
            .iter()
            .map(|e| e.thread_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(order, ["ask", "run", "done", "idle"]);
    }
    #[test]
    fn stale_recovery_is_quiet() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let snap = AgentSnapshot {
            entries: vec![entry("a", AgentState::Idle)].into(),
            last_success_mono: Some(now),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        let mut stale = snap.clone();
        stale.connection = crate::agents::AgentConnection::Retrying;
        stale.last_success_mono = Some(now - Duration::from_secs(31));
        s.sync(&stale, now);
        assert!(s.stale);
        let mut live = stale;
        live.connection = crate::agents::AgentConnection::Live;
        live.last_success_mono = Some(now + Duration::from_secs(3));
        s.sync(&live, now + Duration::from_secs(3));
        assert!(!s.stale);
        assert!(s.queue.is_empty());
        assert!(s.focus.is_none());
    }
    #[test]
    fn animation_runs_only_during_motion_phases() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), true, 30);
        let snap = AgentSnapshot {
            entries: vec![entry("a", AgentState::Running)].into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        s.enqueue(QueuedEvent {
            thread_id: "a".into(),
            kind: EventKind::New,
            before: "新しく検出".into(),
            after: "実行中".into(),
            observed_at: SystemTime::now(),
            priority: 2,
        });
        assert!(s.advance(now));
        assert!(Screen::animating(&s));
        assert!(s.advance(now + Duration::from_secs(1)));
        assert!(!Screen::animating(&s));
        assert!(!s.advance(now + Duration::from_secs(2)));
        assert!(s.advance(now + Duration::from_secs(4)));
        assert!(Screen::animating(&s));
        assert!(s.advance(now + Duration::from_secs(5)));
        assert!(!Screen::animating(&s));
    }
    #[test]
    fn cached_card_surface_is_reused_and_gone_threads_are_released() {
        let now = Instant::now();
        let mut s = AgentScreen::new((1920, 1080), false, 30);
        let entries = (0..13)
            .map(|i| entry(&format!("t{i}"), AgentState::Idle))
            .collect::<Vec<_>>();
        let mut snap = AgentSnapshot {
            entries: entries.clone().into(),
            synchronized: true,
            ..Default::default()
        };
        s.sync(&snap, now);
        let rev = {
            render_for_test(&mut s, &snap, now);
            s.revisions["t0"]
        };
        let surface = s.surfaces["t0"].clone();
        render_for_test(&mut s, &snap, now);
        assert_eq!(s.revisions["t0"], rev);
        assert!(Arc::ptr_eq(&s.surfaces["t0"], &surface));
        assert_eq!(s.surfaces.len(), s.board.cards.len());

        snap.entries = entries[..3].to_vec().into();
        s.sync(&snap, now + Duration::from_secs(1));
        render_for_test(&mut s, &snap, now + Duration::from_secs(1));
        assert_eq!(s.surfaces.len(), 3);
        assert!(!s.surfaces.contains_key("t5"));
    }
    #[test]
    fn change_descriptions_keep_goal_and_title_differences() {
        assert_eq!(
            describe_change(None, Some("実装"), None, AgentChangeKind::Goal),
            "目標 · 実装"
        );
        assert_eq!(
            describe_change(None, None, None, AgentChangeKind::Goal),
            "目標なし"
        );
        assert_eq!(
            describe_change(None, None, Some("変更後"), AgentChangeKind::Title),
            "変更後"
        );
    }

    fn render_for_test(
        screen: &mut AgentScreen,
        snap: &AgentSnapshot,
        now: Instant,
    ) -> Vec<draw::AgentCardSurface> {
        screen.last_now = now;
        let world = crate::screen::World {
            agents: snap.clone(),
            ..Default::default()
        };
        let mut canvas = Canvas::new(screen.size.0, screen.size.1);
        let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
        let mut ctx = RenderContext {
            canvas: &mut canvas,
            text: &mut text,
            world: &world,
            covered: false,
            force_overlay: true,
        };
        screen.render(&mut ctx);
        screen.scene_cards.clone()
    }

    #[test]
    fn gpu_composite_replaces_previous_screen_pixels_when_agent_background_changes() {
        fn render_agents(
            screen: &mut AgentScreen,
            canvas: &mut Canvas,
            text: &mut miharashi_render::TextEngine,
            world: &crate::screen::World,
            covered: bool,
        ) -> Option<bool> {
            let mut ctx = RenderContext {
                canvas,
                text,
                world,
                covered,
                force_overlay: true,
            };
            match screen.render_composite(&mut ctx) {
                Composite::Agents {
                    background_changed, ..
                } => Some(background_changed),
                Composite::Canvas => None,
                _ => panic!("Agent screen returned an unexpected composite"),
            }
        }

        let mut screen = AgentScreen::new((64, 48), true, 30);
        let world = crate::screen::World::default();
        let mut canvas = Canvas::new(64, 48);
        let now = Instant::now();
        let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
        assert!(
            render_agents(&mut screen, &mut canvas, &mut text, &world, false)
                .expect("GPU composite"),
            "first display must upload its background"
        );
        assert!(
            canvas.pixmap.data() == screen.background.as_ref().unwrap().data(),
            "the initial GPU upload source must contain the Agent background"
        );

        // A cached screen must still replace the shared Canvas when another screen
        // has drawn there before Agent is entered again.
        screen.on_leave(now);
        screen.on_enter(now + Duration::from_secs(1));
        screen.set_presentation(true, false, true, now + Duration::from_secs(1));
        assert!(
            screen.background.is_some(),
            "the Agent background is cached"
        );
        canvas.pixmap.data_mut().fill(0x7b);
        assert!(
            render_agents(&mut screen, &mut canvas, &mut text, &world, false)
                .expect("GPU composite"),
            "re-entering Agent must upload its changed background"
        );
        assert!(
            canvas.pixmap.data() == screen.background.as_ref().unwrap().data(),
            "re-entry must upload Agent pixels, not the previous screen's marker"
        );

        // A covered draw takes the complete Canvas path. Closing the overlay must
        // refresh the GPU background again even though its CPU cache still exists.
        screen.set_presentation(false, true, true, now + Duration::from_secs(2));
        canvas.pixmap.data_mut().fill(0x66);
        assert_eq!(
            render_agents(&mut screen, &mut canvas, &mut text, &world, true),
            None,
            "covered Agent screen must render a complete Canvas frame"
        );
        assert!(
            canvas.pixmap.data() == screen.background.as_ref().unwrap().data(),
            "covered rendering must replace the previous frame with the Agent scene"
        );
        screen.set_presentation(true, false, true, now + Duration::from_secs(3));
        canvas.pixmap.data_mut().fill(0x55);
        assert!(
            render_agents(&mut screen, &mut canvas, &mut text, &world, false)
                .expect("GPU composite"),
            "closing the overlay must upload the Agent background again"
        );
        assert!(
            canvas.pixmap.data() == screen.background.as_ref().unwrap().data(),
            "overlay recovery must not upload the covered Canvas as the Agent background"
        );

        // With no further background change, GPU reuses its background texture and
        // does not need any pixels from the shared Canvas.
        canvas.pixmap.data_mut().fill(0x33);
        assert_eq!(
            render_agents(&mut screen, &mut canvas, &mut text, &world, false),
            Some(false),
            "unchanged background must not be uploaded again"
        );
        assert!(canvas.pixmap.data().iter().all(|pixel| *pixel == 0x33));
    }

    #[test]
    fn two_queued_new_threads_stay_reserved_while_existing_card_is_focused() {
        let now = Instant::now();
        let mut screen = AgentScreen::new((1920, 1080), false, 30);
        let snap = AgentSnapshot {
            entries: vec![
                entry("existing", AgentState::Running),
                entry("new-a", AgentState::Running),
                entry("new-b", AgentState::Idle),
            ]
            .into(),
            synchronized: true,
            ..Default::default()
        };
        screen.sync(&snap, now);
        screen.enqueue(QueuedEvent {
            thread_id: "existing".into(),
            kind: EventKind::Changed,
            before: "待機".into(),
            after: "実行中".into(),
            observed_at: SystemTime::now(),
            priority: 1,
        });
        assert!(screen.activate_event(now));
        screen.enqueue(QueuedEvent {
            thread_id: "new-a".into(),
            kind: EventKind::New,
            before: "新しく検出".into(),
            after: "実行中".into(),
            observed_at: SystemTime::now(),
            priority: 2,
        });
        screen.enqueue(QueuedEvent {
            thread_id: "new-b".into(),
            kind: EventKind::New,
            before: "新しく検出".into(),
            after: "待機".into(),
            observed_at: SystemTime::now(),
            priority: 2,
        });

        let first = render_for_test(&mut screen, &snap, now);
        let visible_ids = first.iter().map(|card| card.id).collect::<HashSet<_>>();
        assert!(visible_ids.contains(&screen.id_map["existing"]));
        assert!(!visible_ids.contains(&screen.id_map["new-a"]));
        assert!(!visible_ids.contains(&screen.id_map["new-b"]));
        assert!(screen.pending_new.contains("new-a") && screen.pending_new.contains("new-b"));

        let done = now + Duration::from_secs(6);
        assert!(screen.advance(done));
        assert!(screen.advance(done + Duration::from_millis(1)));
        assert_eq!(
            screen.focus.as_ref().map(|f| f.event.thread_id.as_str()),
            Some("new-a")
        );
        let during_intro = render_for_test(&mut screen, &snap, done + Duration::from_millis(1));
        let ids = during_intro
            .iter()
            .map(|card| card.id)
            .collect::<HashSet<_>>();
        assert!(ids.contains(&screen.id_map["new-a"]));
        assert!(!ids.contains(&screen.id_map["new-b"]));
        assert!(!screen.pending_new.contains("new-a"));
        assert!(screen.pending_new.contains("new-b"));
    }

    #[test]
    fn reordered_cards_slide_to_their_new_place_and_cpu_does_not_claim_gpu_animation() {
        let now = Instant::now();
        let mut screen = AgentScreen::new((1920, 1080), false, 30);
        let mut entries = vec![
            entry("a", AgentState::Running),
            entry("b", AgentState::Idle),
        ];
        let mut snap = AgentSnapshot {
            entries: entries.clone().into(),
            synchronized: true,
            ..Default::default()
        };
        screen.sync(&snap, now);
        let before = render_for_test(&mut screen, &snap, now);
        let b_id = screen.id_map["b"];
        let pose = |cards: &[draw::AgentCardSurface]| {
            cards.iter().find(|card| card.id == b_id).unwrap().pose
        };
        let start = pose(&before);

        entries[1].state = AgentState::Approval;
        snap.entries = entries.into();
        let world = crate::screen::World {
            agents: snap.clone(),
            ..Default::default()
        };
        screen.tick(&world, now + Duration::from_secs(1));
        let first = render_for_test(&mut screen, &snap, now + Duration::from_secs(1));
        assert_eq!(
            pose(&first).y,
            start.y,
            "the move starts where the card was"
        );
        assert!(!Screen::animating(&screen));
        assert!(screen.tick(&world, now + Duration::from_millis(1_250)));
        let mid = render_for_test(&mut screen, &snap, now + Duration::from_millis(1_250));
        assert!(pose(&mid).y < start.y);
        assert!(screen.tick(&world, now + Duration::from_millis(1_600)));
        assert!(screen.poses.values().all(|t| !t.moving()));
        let end = render_for_test(&mut screen, &snap, now + Duration::from_millis(1_600));
        assert!(pose(&end).y < pose(&mid).y);
    }

    #[test]
    fn a_card_moving_behind_others_is_rasterized_again_as_a_strip() {
        let now = Instant::now();
        let mut screen = AgentScreen::new((1920, 1080), false, 30);
        let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
        let e = entry("a", AgentState::Idle);
        let full = screen.content(&e, now, None, false, false);
        screen.ensure_surface("a", &full, (400.0, 210.0), &mut text);
        let (rev, surface) = (screen.revisions["a"], screen.surfaces["a"].clone());
        let strip = screen.content(&e, now, None, true, false);
        screen.ensure_surface("a", &strip, (400.0, 210.0), &mut text);
        assert_ne!(screen.revisions["a"], rev);
        assert!(!Arc::ptr_eq(&screen.surfaces["a"], &surface));
    }

    #[test]
    fn agent_keys_do_nothing() {
        let mut screen = AgentScreen::new((1920, 1080), false, 30);
        assert_eq!(screen.key_labels(), crate::screen::KeyLabels::default());
        crate::screen::assert_key_labels_match_actions(&mut screen, Instant::now());
    }

    #[test]
    fn focused_card_names_its_project_and_board_cards_do_not() {
        let now = Instant::now();
        let screen = AgentScreen::new((1920, 1080), false, 30);
        let e = entry("a", AgentState::Running);
        assert_eq!(screen.content(&e, now, None, false, false).project, "");
        assert_eq!(
            screen
                .content(&e, now, Some("x".into()), false, true)
                .project,
            "project"
        );
    }

    #[test]
    fn queue_overflow_shows_new_card_normally_and_deletion_clears_reservation() {
        let now = Instant::now();
        let mut screen = AgentScreen::new((1920, 1080), false, 30);
        let entries = (0..MAX_QUEUE + 1)
            .map(|i| entry(&format!("n{i}"), AgentState::Idle))
            .collect::<Vec<_>>();
        let snap = AgentSnapshot {
            entries: entries.clone().into(),
            synchronized: true,
            ..Default::default()
        };
        screen.sync(&snap, now);
        for e in entries.iter().take(MAX_QUEUE + 1) {
            screen.enqueue(QueuedEvent {
                thread_id: e.thread_id.clone(),
                kind: EventKind::New,
                before: "新しく検出".into(),
                after: "待機".into(),
                observed_at: SystemTime::now(),
                priority: 1,
            });
        }
        assert_eq!(screen.queue.len(), MAX_QUEUE);
        let overflow_id = format!("n{}", MAX_QUEUE);
        assert!(!screen.pending_new.contains(&overflow_id));
        let cards = render_for_test(&mut screen, &snap, now);
        assert!(
            cards
                .iter()
                .any(|card| card.id == screen.id_map[&overflow_id])
        );

        let removed = entries[0].thread_id.clone();
        screen.pending_new.insert(removed.clone());
        let remaining = entries
            .iter()
            .filter(|e| e.thread_id != removed)
            .cloned()
            .collect::<Vec<_>>();
        let updated = AgentSnapshot {
            entries: remaining.into(),
            ..snap
        };
        screen.sync(&updated, now + Duration::from_secs(1));
        assert!(!screen.pending_new.contains(&removed));
        assert!(screen.queue.iter().all(|event| event.thread_id != removed));
    }
}
