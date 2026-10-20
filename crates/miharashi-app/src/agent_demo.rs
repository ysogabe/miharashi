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
    project_entry("web", id, title, state)
}

fn project_entry(project: &str, id: &str, title: &str, state: AgentState) -> AgentEntry {
    AgentEntry {
        thread_id: id.into(),
        project_id: format!("demo-{project}"),
        project_title: format!("仮データ / {project}"),
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
        server_version: Some("0.1.0".into()),
        protocol_version: Some(2),
        entries: vec![
            entry("demo-running", "カードボードの確認", AgentState::Running),
            entry("demo-idle", "待機カード", AgentState::Idle),
            entry("demo-done", "一覧の並び順を整える", AgentState::Completed),
            project_entry("api", "demo-api", "応答の型を定義する", AgentState::Running),
            project_entry(
                "docs",
                "demo-docs",
                "使い方の説明を更新",
                AgentState::Completed,
            ),
            project_entry(
                "tools",
                "demo-tools",
                "設定ファイルの確認",
                AgentState::Idle,
            ),
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
        before_goal: None,
        after_goal: None,
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
        before_goal: None,
        after_goal: None,
        before_title: Some(title.clone()),
        after_title: Some(title),
        observed_at: wall,
    }]
    .into();
    snapshot
}

/// 新しく検出した作業が、実行中から応答完了になったところ。
fn completed_snapshot(base: &AgentSnapshot, wall: SystemTime, mono: Instant) -> AgentSnapshot {
    let mut snapshot = base.clone();
    let mut entries = snapshot.entries.to_vec();
    let Some(done) = entries.iter_mut().find(|e| e.thread_id == "demo-new") else {
        return snapshot;
    };
    done.state = AgentState::Completed;
    let title = done.title.clone();
    snapshot.entries = entries.into();
    snapshot.change_cursor = 2;
    snapshot.last_success = Some(wall);
    snapshot.last_success_mono = Some(mono);
    snapshot.changes = vec![AgentChange {
        id: 2,
        thread_id: "demo-new".into(),
        kind: AgentChangeKind::State,
        before_state: Some(AgentState::Running),
        after_state: Some(AgentState::Completed),
        before_goal: None,
        after_goal: None,
        before_title: Some(title.clone()),
        after_title: Some(title),
        observed_at: wall,
    }]
    .into();
    snapshot
}

/// 2 つ目の出来事。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Second {
    /// 承認待ちになる(`agents-demo`)
    Approval,
    /// 実行中から応答完了になる(紹介動画)
    Completed,
}

/// 時間が進むにつれて、一覧 → 新着 → 2 つ目の出来事、と変わる筋書き。仮想の時刻(`wall0`・`base` からの経過)だけで決まり、
/// 実時計を読まない。5 秒ごとの取得成功も真似る(取得の鮮度だけが進む)。
pub struct Scenario {
    wall0: SystemTime,
    base: Instant,
    initial: AgentSnapshot,
    fresh: AgentSnapshot,
    second: AgentSnapshot,
    new_at: Duration,
    second_at: Option<Duration>,
}

impl Scenario {
    pub fn new(
        wall0: SystemTime,
        base: Instant,
        new_at: Duration,
        second: Second,
        second_at: Option<Duration>,
    ) -> Self {
        let initial = initial_snapshot(wall0, base);
        let fresh = new_thread_snapshot(&initial, wall0 + new_at, base + new_at);
        let at = second_at.unwrap_or(new_at);
        let second = match second {
            Second::Approval => changed_snapshot(&fresh, wall0 + at, base + at),
            Second::Completed => completed_snapshot(&fresh, wall0 + at, base + at),
        };
        Self {
            wall0,
            base,
            initial,
            fresh,
            second,
            new_at,
            second_at,
        }
    }

    /// 開始から `elapsed` 後の一覧。
    pub fn at(&self, elapsed: Duration) -> AgentSnapshot {
        let mut snapshot = if self.second_at.is_some_and(|t| elapsed >= t) {
            self.second.clone()
        } else if elapsed >= self.new_at {
            self.fresh.clone()
        } else {
            self.initial.clone()
        };
        // 取得は 5 秒ごとに成功する。鮮度だけが進み、活動の経過は変わらない。
        let poll = Duration::from_secs((elapsed.as_secs() / 5) * 5);
        snapshot.last_success = Some(self.wall0 + poll);
        snapshot.last_success_mono = Some(self.base + poll);
        snapshot
    }
}

/// `agents-demo` と紹介動画の、仮想の壁時計の開始(いつ作っても同じ日時にする)。
pub fn demo_wall() -> chrono::DateTime<chrono::Local> {
    crate::video::wall_start()
}

/// Many threads in a few projects, to review stacking without a live server.
fn busy_snapshot(base: &AgentSnapshot) -> AgentSnapshot {
    let mut entries = Vec::new();
    let web = [
        ("承認を待つ作業", AgentState::Approval),
        ("画面の配置を計算", AgentState::Running),
        ("動画の再生を確認", AgentState::Running),
        ("一覧の並び順を整える", AgentState::Completed),
        ("天気の表示を直す", AgentState::Completed),
        ("待機中の調整", AgentState::Idle),
        ("説明画像の更新", AgentState::Idle),
        ("文言の見直し", AgentState::Idle),
        ("軌跡を短くする", AgentState::Idle),
    ];
    for (i, (title, state)) in web.into_iter().enumerate() {
        entries.push(project_entry("web", &format!("busy-web-{i}"), title, state));
    }
    let api = [
        ("計測の結果が取れない", AgentState::Error),
        ("応答の型を定義する", AgentState::Running),
        ("一覧の取得", AgentState::Completed),
        ("使用量の記録", AgentState::Idle),
    ];
    for (i, (title, state)) in api.into_iter().enumerate() {
        entries.push(project_entry("api", &format!("busy-api-{i}"), title, state));
    }
    for (project, items) in [
        (
            "panel",
            &[
                ("設定画面の構成", AgentState::Input),
                ("文字を大きく", AgentState::Completed),
                ("アイコンの差し替え", AgentState::Idle),
            ][..],
        ),
        (
            "device",
            &[
                ("値の平滑化", AgentState::Running),
                ("配線図の更新", AgentState::Idle),
            ][..],
        ),
        ("docs", &[("月次の集計", AgentState::Completed)][..]),
        ("tools", &[("補完設定", AgentState::Idle)][..]),
    ] {
        for (i, (title, state)) in items.iter().enumerate() {
            entries.push(project_entry(
                project,
                &format!("busy-{project}-{i}"),
                title,
                *state,
            ));
        }
    }
    for e in &mut entries {
        e.has_pending_approvals = e.state == AgentState::Approval;
        e.has_pending_user_input = e.state == AgentState::Input;
    }
    AgentSnapshot {
        entries: entries.into(),
        changes: Vec::new().into(),
        ..base.clone()
    }
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

    let mut busy_screen = AgentScreen::new(size, false, stale_secs);
    busy_screen.on_enter(changed_at);
    let busy_world = World {
        agents: busy_snapshot(&changed),
        ..World::default()
    };
    save(
        &mut busy_screen,
        &busy_world,
        changed_at,
        "agents-busy",
        out,
        text,
        size,
    )?;

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

    let wall = demo_wall();
    // 仮想の Instant の起点。画面の生成時に実時刻で取った時刻より後にそろえ、生成の速さで絵が変わらないようにする
    let base = Instant::now() + Duration::from_secs(3600);
    let scenario = Scenario::new(
        SystemTime::from(wall),
        base,
        Duration::from_secs(1),
        Second::Approval,
        Some(Duration::from_secs(7)),
    );

    let mut world = World::default();
    let mut screen = AgentScreen::new(size, false, cfg.agents.stale_secs);
    screen.on_enter(base);
    let mut text = crate::app::text_engine(&cfg.ui.font);
    let frames = usize::try_from(seconds)
        .ok()
        .and_then(|s| s.checked_mul(fps as usize))
        .ok_or("frame 数が大きすぎます")?;
    for index in 0..frames {
        let elapsed = Duration::from_secs_f64(index as f64 / f64::from(fps));
        let now = base + elapsed;
        world.wall = Some(wall + chrono::Duration::from_std(elapsed).unwrap_or_default());
        world.agents = scenario.at(elapsed);
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
