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
    /// 動画専用。既存の親子の見本に子が増え、同じ子が応答完了になる。
    /// 静止画や agents-demo の筋書きは変えない。
    pub fn family_video(
        wall0: SystemTime,
        base: Instant,
        new_at: Duration,
        second_at: Option<Duration>,
    ) -> Self {
        let mut scenario = Self::new(wall0, base, new_at, Second::Completed, second_at);
        scenario.initial = family_snapshot(&scenario.initial);
        let mut fresh = new_thread_snapshot(&scenario.initial, wall0 + new_at, base + new_at);
        let mut entries = fresh.entries.to_vec();
        let child = entries
            .iter_mut()
            .find(|e| e.thread_id == "demo-new")
            .unwrap();
        child.project_id = "demo-agents".into();
        child.project_title = "仮データ / agents".into();
        child.parent_thread_id = Some("fam-parent".into());
        child.parent_title = Some("エージェント画面の改善".into());
        fresh.entries = entries.into();
        let at = second_at.unwrap_or(new_at);
        scenario.second = completed_snapshot(&fresh, wall0 + at, base + at);
        scenario.fresh = fresh;
        scenario
    }

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

/// 子のスレッド。`parent` は (親のスレッド ID, 親のタイトル)。
fn child_entry(
    project: &str,
    id: &str,
    title: &str,
    state: AgentState,
    parent: (&str, &str),
) -> AgentEntry {
    let mut e = project_entry(project, id, title, state);
    e.parent_thread_id = Some(parent.0.into());
    e.parent_title = Some(parent.1.into());
    e.provider = Some("Claude".into());
    e.model = Some("Sonnet".into());
    e.has_pending_approvals = state == AgentState::Approval;
    e.has_pending_user_input = state == AgentState::Input;
    e
}

/// 親 1・子 6(承認待ち 1、実行中 1、応答完了 4)・孫 1 の親子グループと、単独スレッド、「親情報なし」の子 1。
fn family_snapshot(base: &AgentSnapshot) -> AgentSnapshot {
    const PARENT: (&str, &str) = ("fam-parent", "エージェント画面の改善");
    let mut entries = vec![project_entry(
        "agents",
        PARENT.0,
        PARENT.1,
        AgentState::Running,
    )];
    for (id, title, state) in [
        ("fam-c-impl", "親子カードの配置", AgentState::Approval),
        ("fam-c-review", "描画と表示件数の確認", AgentState::Running),
        ("fam-c-r1", "親子関係の取得を調査", AgentState::Completed),
        ("fam-c-r2", "既存のレイアウトを調査", AgentState::Completed),
        ("fam-c-v1", "画像の比較", AgentState::Completed),
        ("fam-c-v2", "小さい画面での確認", AgentState::Completed),
    ] {
        entries.push(child_entry("agents", id, title, state, PARENT));
    }
    entries.push(child_entry(
        "agents",
        "fam-g-color",
        "色のコントラストを確認",
        AgentState::Running,
        ("fam-c-review", "描画と表示件数の確認"),
    ));
    entries.push(project_entry(
        "photos",
        "fam-solo-1",
        "写真の並べ替え",
        AgentState::Running,
    ));
    entries.push(project_entry(
        "photos",
        "fam-solo-2",
        "画像の取得処理を確認",
        AgentState::Completed,
    ));
    entries.push(project_entry(
        "photos",
        "fam-solo-3",
        "説明文の更新",
        AgentState::Idle,
    ));
    entries.push(child_entry(
        "docs",
        "fam-orphan",
        "月次の集計表を作る",
        AgentState::Running,
        ("fam-gone", "月次レポートの整理"),
    ));
    AgentSnapshot {
        entries: entries.into(),
        changes: Vec::new().into(),
        ..base.clone()
    }
}

/// 子が多すぎて「ほか N 件」と巡回が起きる場面。
fn family_many_snapshot(base: &AgentSnapshot) -> AgentSnapshot {
    const POOL: [&str; 12] = [
        "設定の確認",
        "ログの整理",
        "型の見直し",
        "テストの追加",
        "文言の修正",
        "画像の差し替え",
        "依存の更新",
        "計測の再実行",
        "配線図の更新",
        "色の調整",
        "一覧の並び順",
        "軌跡の長さ",
    ];
    let mut entries = Vec::new();
    // (プロジェクト, 親の題名, 承認待ちの子, 実行中の子, 静かな子)
    let parents = [
        ("agents", "エージェント画面の改善", 1, 2, 14),
        ("agents", "設定画面の作り直し", 0, 1, 12),
        ("photos", "写真の取り込みを速くする", 0, 2, 9),
        ("photos", "アルバムの並びを直す", 0, 0, 11),
        ("device", "センサーの値を滑らかにする", 0, 1, 7),
        ("device", "配線図と実機の突き合わせ", 1, 0, 8),
        ("docs", "使い方の文章を見直す", 0, 0, 6),
        ("docs", "紹介動画の差し替え", 0, 1, 5),
    ];
    for (n, (project, title, ask, run, quiet)) in parents.into_iter().enumerate() {
        let pid = format!("many-{n}");
        entries.push(project_entry(project, &pid, title, AgentState::Running));
        let parent = (pid.as_str(), title);
        for i in 0..ask {
            entries.push(child_entry(
                project,
                &format!("{pid}-a{i}"),
                POOL[i % POOL.len()],
                AgentState::Approval,
                parent,
            ));
        }
        for i in 0..run {
            entries.push(child_entry(
                project,
                &format!("{pid}-r{i}"),
                POOL[(i + 3) % POOL.len()],
                AgentState::Running,
                parent,
            ));
        }
        for i in 0..quiet {
            entries.push(child_entry(
                project,
                &format!("{pid}-q{i}"),
                POOL[(i + n) % POOL.len()],
                if i % 5 == 4 {
                    AgentState::Idle
                } else {
                    AgentState::Completed
                },
                parent,
            ));
        }
    }
    for i in 0..6 {
        entries.push(project_entry(
            "tools",
            &format!("many-solo-{i}"),
            POOL[(i * 2) % POOL.len()],
            if i == 0 {
                AgentState::Running
            } else {
                AgentState::Idle
            },
        ));
    }
    AgentSnapshot {
        entries: entries.into(),
        changes: Vec::new().into(),
        ..base.clone()
    }
}

/// 「ほか N 件」にまとめられた子が、応答完了から待機に変わったところ(静かなままなのでまとめられたまま)。
fn collapsed_child_snapshot(
    base: &AgentSnapshot,
    id: &str,
    wall: SystemTime,
    mono: Instant,
) -> AgentSnapshot {
    let at = Duration::from_secs(60);
    let mut entries = base.entries.to_vec();
    let mut title = String::new();
    if let Some(e) = entries.iter_mut().find(|e| e.thread_id == id) {
        e.state = AgentState::Idle;
        title = e.title.clone();
    }
    AgentSnapshot {
        entries: entries.into(),
        change_cursor: base.change_cursor + 1,
        last_success: Some(wall + at),
        last_success_mono: Some(mono + at),
        changes: vec![AgentChange {
            id: base.change_cursor + 1,
            thread_id: id.into(),
            kind: AgentChangeKind::State,
            before_state: Some(AgentState::Completed),
            after_state: Some(AgentState::Idle),
            before_goal: None,
            after_goal: None,
            before_title: Some(title.clone()),
            after_title: Some(title),
            observed_at: wall + at,
        }]
        .into(),
        ..base.clone()
    }
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

    let mut family_screen = AgentScreen::new(size, false, stale_secs);
    family_screen.on_enter(changed_at);
    let family_world = World {
        agents: family_snapshot(&changed),
        ..World::default()
    };
    save(
        &mut family_screen,
        &family_world,
        changed_at,
        "agents-family",
        out,
        text,
        size,
    )?;

    // 子が多すぎる場面: 「ほか N 件」と、15 秒ごとの自動巡回(要確認の組が最初のページ)。
    let mut many_screen = AgentScreen::new(size, false, stale_secs);
    many_screen.on_enter(changed_at);
    let many = family_many_snapshot(&changed);
    let mut many_world = World {
        agents: many.clone(),
        ..World::default()
    };
    save(
        &mut many_screen,
        &many_world,
        changed_at,
        "agents-family-many",
        out,
        text,
        size,
    )?;
    for page in 2..=3 {
        let at = changed_at + Duration::from_secs(16 * (page - 1));
        many_world.agents.last_success = Some(wall + Duration::from_secs(7 + 16 * (page - 1)));
        many_world.agents.last_success_mono = Some(at);
        save(
            &mut many_screen,
            &many_world,
            at,
            &format!("agents-family-many-p{page}"),
            out,
            text,
            size,
        )?;
    }
    // 「ほか N 件」にまとめた子の変更は、親のカードを元位置にしてポップアップする。
    let hidden_id = "many-0-q13";
    many_world.agents = collapsed_child_snapshot(&many, hidden_id, wall, changed_at);
    many_screen.on_enter(changed_at + Duration::from_secs(60));
    let pop_at = changed_at + Duration::from_secs(60);
    for (i, seconds) in [0.0f32, 0.35, 2.0].iter().enumerate() {
        save(
            &mut many_screen,
            &many_world,
            pop_at + Duration::from_secs_f32(*seconds),
            &format!("agents-family-hidden-{i:02}"),
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
