//! AgentScreen を仮時計で動かす、外部接続なしの連番 PNG デモ。
use crate::agents::{
    AgentChange, AgentChangeKind, AgentConnection, AgentEntry, AgentSnapshot, AgentState,
};
use crate::app::screens::agents::AgentScreen;
use crate::config::Config;
use crate::screen::{RenderContext, Screen, World};
use miharashi_render::Canvas;
use miharashi_render::TextEngine;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

fn entry(id: &str, title: &str, state: AgentState) -> AgentEntry {
    AgentEntry {
        thread_id: id.into(),
        project_id: "demo-project".into(),
        project_title: "仮データ / demo".into(),
        title: title.into(),
        provider: Some("Codex".into()),
        model: Some("選択モデル".into()),
        state,
        ..AgentEntry::default()
    }
}

fn initial_snapshot(wall: SystemTime, mono: Instant) -> AgentSnapshot {
    AgentSnapshot {
        environment_id: Some("demo".into()),
        environment_label: Some("仮データ · network disabled".into()),
        server_version: Some("0.0.45".into()),
        protocol_version: Some(1),
        entries: vec![
            entry("demo-running", "カードボードの確認", AgentState::Running),
            entry("demo-idle", "待機カード", AgentState::Idle),
        ]
        .into(),
        connection: AgentConnection::Live,
        last_success: Some(wall),
        last_success_mono: Some(mono),
        synchronized: true,
        ..AgentSnapshot::default()
    }
}

fn new_thread_snapshot(base: &AgentSnapshot, wall: SystemTime, mono: Instant) -> AgentSnapshot {
    let new = entry("demo-new", "新しく検出された作業", AgentState::Running);
    let mut snapshot = base.clone();
    snapshot.entries = snapshot
        .entries
        .iter()
        .cloned()
        .chain(std::iter::once(new.clone()))
        .collect::<Vec<_>>()
        .into();
    snapshot.change_cursor = 1;
    snapshot.last_success = Some(wall);
    snapshot.last_success_mono = Some(mono);
    snapshot.changes = vec![AgentChange {
        id: 1,
        thread_id: new.thread_id.clone(),
        kind: AgentChangeKind::New,
        before_state: None,
        after_state: Some(new.state),
        before_progress: None,
        after_progress: None,
        before_title: None,
        after_title: Some(new.title),
        observed_at: wall,
    }]
    .into();
    snapshot
}

fn changed_snapshot(base: &AgentSnapshot, wall: SystemTime, mono: Instant) -> AgentSnapshot {
    let mut snapshot = base.clone();
    let mut entries = snapshot.entries.to_vec();
    let Some(changed) = entries.iter_mut().find(|e| e.thread_id == "demo-new") else {
        return snapshot;
    };
    changed.state = AgentState::Approval;
    changed.has_pending_approvals = true;
    let title = changed.title.clone();
    snapshot.entries = entries.into();
    snapshot.change_cursor = 2;
    snapshot.last_success = Some(wall);
    snapshot.last_success_mono = Some(mono);
    snapshot.changes = vec![AgentChange {
        id: 2,
        thread_id: "demo-new".into(),
        kind: AgentChangeKind::State,
        before_state: Some(AgentState::Running),
        after_state: Some(AgentState::Approval),
        before_progress: None,
        after_progress: None,
        before_title: Some(title.clone()),
        after_title: Some(title),
        observed_at: wall,
    }]
    .into();
    snapshot
}

/// Shared synthetic states used by both still PNG fixtures and the timed demo.
pub fn fixture_snapshots(
    wall: SystemTime,
    base: Instant,
) -> (AgentSnapshot, AgentSnapshot, AgentSnapshot) {
    let initial = initial_snapshot(wall, base);
    let new_at = Duration::from_secs(1);
    let changed_at = Duration::from_secs(7);
    let fresh = new_thread_snapshot(&initial, wall + new_at, base + new_at);
    let changed = changed_snapshot(&fresh, wall + changed_at, base + changed_at);
    (initial, fresh, changed)
}

/// Individual review images using the same snapshots and AgentScreen state machine as `agents-demo`.
pub fn write_fixtures(
    out: &Path,
    size: (u32, u32),
    text: &mut TextEngine,
    stale_secs: u64,
) -> Result<(), String> {
    fn save(
        screen: &mut AgentScreen,
        world: &World,
        now: Instant,
        name: &str,
        out: &Path,
        text: &mut TextEngine,
        size: (u32, u32),
    ) -> Result<(), String> {
        screen.tick(world, now);
        let mut canvas = Canvas::new(size.0, size.1);
        let mut context = RenderContext {
            canvas: &mut canvas,
            text,
            world,
            covered: false,
            force_overlay: true,
        };
        screen.render(&mut context);
        canvas.save_png(&out.join(format!("{name}.png")))
    }

    let wall = SystemTime::now();
    let base = Instant::now();
    let (initial, fresh, changed) = fixture_snapshots(wall, base);
    let mut world = World {
        agents: initial,
        ..World::default()
    };
    let mut screen = AgentScreen::new(size, false, stale_secs);
    screen.on_enter(base);
    save(
        &mut screen,
        &world,
        base,
        "agents-overview",
        out,
        text,
        size,
    )?;

    world.agents = fresh.clone();
    let new_at = base + Duration::from_secs(1);
    for (i, seconds) in [0.0f32, 0.35, 0.75, 2.2, 3.8, 4.35].iter().enumerate() {
        save(
            &mut screen,
            &world,
            new_at + Duration::from_secs_f32(*seconds),
            &format!("agents-new-{i:02}"),
            out,
            text,
            size,
        )?;
    }

    world.agents = changed.clone();
    let changed_at = base + Duration::from_secs(7);
    for (i, seconds) in [0.0f32, 0.35, 0.8, 3.0, 5.8].iter().enumerate() {
        save(
            &mut screen,
            &world,
            changed_at + Duration::from_secs_f32(*seconds),
            &format!("agents-changed-{i:02}"),
            out,
            text,
            size,
        )?;
    }

    let mut stale = changed.clone();
    stale.connection = AgentConnection::Retrying;
    world.agents = stale;
    save(
        &mut screen,
        &world,
        changed_at + Duration::from_secs(40),
        "agents-stale",
        out,
        text,
        size,
    )?;

    let mut empty = changed;
    empty.entries = Vec::new().into();
    empty.changes = Vec::new().into();
    empty.connection = AgentConnection::Live;
    empty.last_success = Some(wall + Duration::from_secs(50));
    empty.last_success_mono = Some(changed_at + Duration::from_secs(50));
    world.agents = empty;
    save(
        &mut screen,
        &world,
        changed_at + Duration::from_secs(50),
        "agents-empty",
        out,
        text,
        size,
    )
}

/// `--out` に frame-000000.png 形式で連番を出す。fps は仮時計の刻みにだけ使い、通信しない。
pub fn render_sequence(
    cfg: &Config,
    size: (u32, u32),
    out: &Path,
    seconds: u64,
    fps: u32,
) -> Result<usize, String> {
    if size.0 == 0 || size.1 == 0 {
        return Err("--size は 1x1 以上を指定してください".into());
    }
    if !(1..=60).contains(&fps) {
        return Err("agents-demo の --fps は 1〜60".into());
    }
    if !(1..=120).contains(&seconds) {
        return Err("agents-demo の --seconds は 1〜120".into());
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;

    let wall = SystemTime::now();
    let base = Instant::now();
    let (initial, mut new_snapshot, mut changed_snapshot) = fixture_snapshots(wall, base);
    let new_at = Duration::from_secs(1);
    let changed_at = Duration::from_secs(7);

    let mut world = World::default();
    let mut screen = AgentScreen::new(size, false, cfg.agents.stale_secs);
    screen.on_enter(base);
    let mut text = crate::app::text_engine(&cfg.ui.font);
    let mut new_observed_at_injected = false;
    let mut changed_observed_at_injected = false;
    let frames = usize::try_from(seconds)
        .ok()
        .and_then(|s| s.checked_mul(fps as usize))
        .ok_or("frame 数が大きすぎます")?;
    for index in 0..frames {
        let elapsed = Duration::from_secs_f64(index as f64 / f64::from(fps));
        let now = base + elapsed;
        // AgentScreen ages notices against SystemTime::now(), while this demo advances only a
        // virtual Instant timeline. Stamp each change when it first enters the simulation so
        // slow 1080p rendering does not make a synthetic event appear already stale.
        if elapsed >= new_at && !new_observed_at_injected {
            if !new_snapshot.changes.is_empty() {
                let mut changes = new_snapshot.changes.to_vec();
                changes[0].observed_at = SystemTime::now();
                new_snapshot.changes = changes.into();
            }
            new_observed_at_injected = true;
        }
        if elapsed >= changed_at && !changed_observed_at_injected {
            if !changed_snapshot.changes.is_empty() {
                let mut changes = changed_snapshot.changes.to_vec();
                changes[0].observed_at = SystemTime::now();
                changed_snapshot.changes = changes.into();
            }
            changed_observed_at_injected = true;
        }
        let mut snapshot = if elapsed >= changed_at {
            changed_snapshot.clone()
        } else if elapsed >= new_at {
            new_snapshot.clone()
        } else {
            initial.clone()
        };
        // Model successful 5-second polls. This advances freshness without resetting activity ages.
        let last_poll_secs = (elapsed.as_secs() / 5) * 5;
        snapshot.last_success = Some(wall + Duration::from_secs(last_poll_secs));
        snapshot.last_success_mono = Some(base + Duration::from_secs(last_poll_secs));
        world.agents = snapshot;
        screen.tick(&world, now);
        let mut canvas = Canvas::new(size.0, size.1);
        let mut context = RenderContext {
            canvas: &mut canvas,
            text: &mut text,
            world: &world,
            covered: false,
            force_overlay: true,
        };
        screen.render(&mut context);
        let path = out.join(format!("frame-{index:06}.png"));
        canvas.save_png(&path)?;
    }
    Ok(frames)
}
