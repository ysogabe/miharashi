//! 複数の T3 Server を 1 つの画面に出すときの検証(接続別の状態・複合キー・通知の独立・容量・見出し)。
use super::*;
use crate::agents::{AgentChange, AgentState};

fn entry(project: &str, id: &str, state: AgentState, parent: Option<&str>) -> AgentEntry {
    AgentEntry {
        thread_id: id.into(),
        project_id: project.into(),
        project_title: "Project".into(),
        title: id.into(),
        state,
        has_pending_approvals: state == AgentState::Approval,
        has_pending_user_input: state == AgentState::Input,
        parent_thread_id: parent.map(str::to_string),
        parent_title: parent.map(|p| format!("{p} のタイトル")),
        ..Default::default()
    }
}

fn change(server: &str, id: u64, thread: &str, after: AgentState) -> AgentChange {
    AgentChange {
        server_id: server.into(),
        id,
        thread_id: thread.into(),
        kind: AgentChangeKind::State,
        before_state: Some(AgentState::Running),
        after_state: Some(after),
        before_goal: None,
        after_goal: None,
        before_title: None,
        after_title: Some(thread.into()),
        observed_at: SystemTime::now(),
    }
}

struct Poll {
    entries: Vec<AgentEntry>,
    changes: Vec<AgentChange>,
    cursor: u64,
    mono: Instant,
    generation: u64,
    environment: Option<&'static str>,
}

impl Poll {
    fn new(entries: Vec<AgentEntry>, mono: Instant) -> Self {
        Self {
            entries,
            changes: Vec::new(),
            cursor: 0,
            mono,
            generation: 0,
            environment: None,
        }
    }
    fn snapshot(self) -> AgentSnapshot {
        AgentSnapshot {
            environment_id: self.environment.map(str::to_string),
            entries: self.entries.into(),
            changes: self.changes.into(),
            connection: AgentConnection::Live,
            last_success: Some(SystemTime::now()),
            last_success_mono: Some(self.mono),
            change_cursor: self.cursor,
            resync_generation: self.generation,
            synchronized: true,
            ..Default::default()
        }
    }
}

fn server(id: &str, label: &str, poll: Poll) -> ServerSnapshot {
    ServerSnapshot::new(id, label, poll.snapshot())
}

fn screen() -> AgentScreen {
    AgentScreen::new((1920, 1080), false, 30)
}

fn k(server: &str, id: &str) -> ThreadKey {
    ThreadKey {
        server: server.into(),
        thread: id.into(),
    }
}

fn one(world: &[ServerSnapshot], id: &str) -> usize {
    world.iter().position(|s| &*s.server_id == id).unwrap()
}

fn render(
    screen: &mut AgentScreen,
    world: &[ServerSnapshot],
    now: Instant,
) -> Vec<draw::AgentCardSurface> {
    screen.last_now = now;
    let world = crate::screen::World {
        agents: world.to_vec(),
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

/// 同じ project / thread / parent ID を持つ 2 台は、別のプロジェクト・別のカードで、
/// 親子も他方の接続とは結ばれない。画面は 1 つ(`agents`)のまま。
#[test]
fn the_same_ids_on_two_servers_stay_two_projects_cards_and_families() {
    let now = Instant::now();
    let mut s = screen();
    assert_eq!(s.id(), "agents", "画面の登録は 1 つだけ");
    let world = vec![
        server(
            "primary",
            "primary",
            Poll::new(
                vec![
                    entry("p", "boss", AgentState::Running, None),
                    entry("p", "kid", AgentState::Running, Some("boss")),
                ],
                now,
            ),
        ),
        server(
            "container",
            "コンテナ",
            Poll::new(
                vec![
                    entry("p", "boss", AgentState::Completed, None),
                    entry("p", "kid", AgentState::Approval, None),
                ],
                now,
            ),
        ),
    ];
    s.sync_all(&world, now);
    s.ensure_board();
    let groups = s.groups();
    let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["primary / Project", "コンテナ / Project"]);
    // primary の kid は primary の boss の子。container の kid は親なしの別スレッド。
    assert_eq!(s.arrangement.families[0].len(), 1);
    assert_eq!(s.arrangement.families[0][0].members.len(), 2);
    assert_eq!(s.arrangement.families[1].len(), 2);
    assert!(s.arrangement.families[1].iter().all(|f| !f.framed()));
    // 4 枚が別々のカード・別々の描画 ID。
    assert_eq!(s.board.cards.len(), 4);
    let ids: HashSet<u64> = s.id_map.values().copied().collect();
    assert_eq!(ids.len(), 4);
    for (server, thread) in [
        ("primary", "boss"),
        ("primary", "kid"),
        ("container", "boss"),
        ("container", "kid"),
    ] {
        assert!(
            s.board.cards.contains_key(&k(server, thread)),
            "{server}/{thread}"
        );
    }
    // 接続名が盤面のカードと通知のカードに出る。
    let boss_b = s
        .entries
        .iter()
        .find(|e| e.key() == k("container", "boss"))
        .unwrap()
        .clone();
    assert_eq!(
        s.content(&boss_b, now, None, false, false).project,
        "コンテナ"
    );
    assert_eq!(
        s.content(&boss_b, now, None, false, true).project,
        "コンテナ / Project"
    );
}

/// 親が別の接続にしか無い子は「親情報なし」になり、他方の接続の同じ ID の親には結ばれない。
#[test]
fn a_parent_id_is_resolved_inside_its_own_server_only() {
    let now = Instant::now();
    let mut s = screen();
    let world = vec![
        server(
            "primary",
            "primary",
            Poll::new(
                vec![entry("p", "kid", AgentState::Running, Some("boss"))],
                now,
            ),
        ),
        server(
            "container",
            "コンテナ",
            Poll::new(vec![entry("p", "boss", AgentState::Running, None)], now),
        ),
    ];
    s.sync_all(&world, now);
    s.ensure_board();
    let orphan = &s.arrangement.families[0][0];
    assert_eq!(orphan.orphan_of, Some(k("primary", "boss")));
    assert_eq!(orphan.members.len(), 1);
    assert!(
        !s.arrangement.families[1][0].framed(),
        "container の boss は子を持たない"
    );
}

/// 同じ change id (どちらも 1) の通知は、接続ごとに別の通知として並び、取りこぼしも混同もない。
#[test]
fn notifications_with_the_same_change_id_stay_separate_per_server() {
    let now = Instant::now();
    let mut s = screen();
    let base = |ids: &[(&str, AgentState)]| {
        ids.iter()
            .map(|(id, st)| entry("p", id, *st, None))
            .collect::<Vec<_>>()
    };
    let first = vec![
        server(
            "primary",
            "primary",
            Poll::new(base(&[("t1", AgentState::Running)]), now),
        ),
        server(
            "container",
            "コンテナ",
            Poll::new(base(&[("t1", AgentState::Running)]), now),
        ),
    ];
    s.sync_all(&first, now);
    assert!(s.queue.is_empty());
    let t = now + Duration::from_secs(1);
    let mut a = Poll::new(base(&[("t1", AgentState::Approval)]), t);
    a.changes = vec![change("primary", 1, "t1", AgentState::Approval)];
    a.cursor = 1;
    let mut b = Poll::new(base(&[("t1", AgentState::Input)]), t);
    b.changes = vec![change("container", 1, "t1", AgentState::Input)];
    b.cursor = 1;
    s.sync_all(
        &[
            server("primary", "primary", a),
            server("container", "コンテナ", b),
        ],
        t,
    );
    assert_eq!(
        s.queue.len(),
        2,
        "同じ thread id でも、別の接続の通知は合流しない"
    );
    let servers: Vec<&str> = s.queue.iter().map(|e| &*e.key.server).collect();
    assert_eq!(servers, ["primary", "container"]);
    assert_eq!(s.queue[0].change.id, s.queue[1].change.id);
    assert_ne!(s.queue[0].change, s.queue[1].change);
    // 1 つずつ順に出る。先に出たのは先に届いた接続の通知。
    assert!(s.activate_event(t));
    assert_eq!(s.focus.as_ref().unwrap().event.key, k("primary", "t1"));
    assert_eq!(s.queue.len(), 1);
    assert_eq!(s.queue[0].key, k("container", "t1"));
}

/// 片方の接続が stale になっても、その接続の通知だけが止まり、カードは「前回取得時」になる。
/// もう片方の通知は出続け、stale からの復帰は静か。
#[test]
fn a_stale_server_stops_only_its_own_notifications() {
    let now = Instant::now();
    let mut s = screen();
    let poll = |state, mono: Instant| Poll::new(vec![entry("p", "t1", state, None)], mono);
    s.sync_all(
        &[
            server("primary", "primary", poll(AgentState::Running, now)),
            server("container", "コンテナ", poll(AgentState::Running, now)),
        ],
        now,
    );
    // 両方に変更が届き、2 件並ぶ。
    let t = now + Duration::from_secs(5);
    let mut a = poll(AgentState::Approval, t);
    a.changes = vec![change("primary", 1, "t1", AgentState::Approval)];
    a.cursor = 1;
    let mut b = poll(AgentState::Approval, t);
    b.changes = vec![change("container", 1, "t1", AgentState::Approval)];
    b.cursor = 1;
    s.sync_all(
        &[
            server("primary", "primary", a),
            server("container", "コンテナ", b),
        ],
        t,
    );
    assert_eq!(s.queue.len(), 2);
    // primary の最終取得が止まったまま stale_secs(30)を超える。container は更新され続ける。
    let late = now + Duration::from_secs(40);
    let stale_a = poll(AgentState::Approval, t);
    let mut b2 = poll(AgentState::Approval, late);
    b2.changes = vec![change("container", 2, "t1", AgentState::Input)];
    b2.cursor = 2;
    let mut a2 = stale_a.snapshot();
    a2.connection = AgentConnection::Retrying;
    let world = vec![
        ServerSnapshot::new("primary", "primary", a2),
        server("container", "コンテナ", b2),
    ];
    s.sync_all(&world, late);
    assert!(s.servers[0].stale && !s.servers[1].stale);
    assert!(
        s.queue.iter().all(|e| &*e.key.server == "container"),
        "stale の接続の通知は捨てられる"
    );
    assert!(!s.queue.is_empty(), "健全な接続の通知は残る");
    assert!(s.advance(late), "健全な接続の通知は進む");
    assert_eq!(
        s.focus.as_ref().unwrap().event.key.server.as_ref(),
        "container"
    );
    // 前回取得時の表示は stale の接続のカードだけ。
    let a_entry = s
        .entries
        .iter()
        .find(|e| &*e.server_id == "primary")
        .unwrap()
        .clone();
    let b_entry = s
        .entries
        .iter()
        .find(|e| &*e.server_id == "container")
        .unwrap()
        .clone();
    assert!(
        s.content(&a_entry, late, None, false, false)
            .detail
            .starts_with("前回取得時")
    );
    assert!(
        !s.content(&b_entry, late, None, false, false)
            .detail
            .starts_with("前回取得時")
    );
    // stale の接続に新しい変更が混ざっても通知にしない。復帰も静か。
    let back = late + Duration::from_secs(3);
    let mut a3 = poll(AgentState::Error, back);
    a3.changes = vec![change("primary", 2, "t1", AgentState::Error)];
    a3.cursor = 2;
    s.sync_all(
        &[
            server("primary", "primary", a3),
            server("container", "コンテナ", poll(AgentState::Input, back)),
        ],
        back,
    );
    assert!(!s.servers[0].stale);
    assert!(
        s.queue.iter().all(|e| &*e.key.server != "primary"),
        "復帰の瞬間の変更は通知にしない"
    );
}

/// 片方の再同期(世代の更新)は、その接続の通知だけを捨てる。
#[test]
fn a_resync_on_one_server_keeps_the_other_servers_notifications() {
    let now = Instant::now();
    let mut s = screen();
    let poll = |state, mono: Instant| Poll::new(vec![entry("p", "t1", state, None)], mono);
    s.sync_all(
        &[
            server("primary", "primary", poll(AgentState::Running, now)),
            server("container", "コンテナ", poll(AgentState::Running, now)),
        ],
        now,
    );
    let t = now + Duration::from_secs(1);
    let mut a = poll(AgentState::Approval, t);
    a.changes = vec![change("primary", 1, "t1", AgentState::Approval)];
    a.cursor = 1;
    let mut b = poll(AgentState::Approval, t);
    b.changes = vec![change("container", 1, "t1", AgentState::Approval)];
    b.cursor = 1;
    s.sync_all(
        &[
            server("primary", "primary", a),
            server("container", "コンテナ", b),
        ],
        t,
    );
    assert_eq!(s.queue.len(), 2);
    let t2 = t + Duration::from_secs(1);
    let mut a2 = poll(AgentState::Approval, t2);
    a2.generation = 1;
    a2.cursor = 1;
    let mut b2 = poll(AgentState::Approval, t2);
    b2.changes = vec![change("container", 1, "t1", AgentState::Approval)];
    b2.cursor = 1;
    s.sync_all(
        &[
            server("primary", "primary", a2),
            server("container", "コンテナ", b2),
        ],
        t2,
    );
    assert_eq!(s.queue.len(), 1);
    assert_eq!(s.queue[0].key, k("container", "t1"));
    assert_eq!(s.servers[0].generation, 1);
    assert_eq!(s.servers[1].generation, 0);
}

/// 再接続で environment_id が変わった接続は、その接続のカード・履歴・キャッシュだけを捨てる。
#[test]
fn a_replaced_environment_drops_only_that_servers_cards() {
    let now = Instant::now();
    let mut s = screen();
    let with_env = |threads: &[&str], env| {
        let mut p = Poll::new(
            threads
                .iter()
                .map(|t| entry("p", t, AgentState::Running, None))
                .collect(),
            now,
        );
        p.environment = Some(env);
        p
    };
    s.sync_all(
        &[
            server("primary", "primary", with_env(&["old"], "env-1")),
            server("container", "コンテナ", with_env(&["keep"], "env-2")),
        ],
        now,
    );
    assert!(s.seen.contains_key(&k("primary", "old")));
    let t = now + Duration::from_secs(1);
    s.sync_all(
        &[
            server("primary", "primary", with_env(&["new"], "env-9")),
            server("container", "コンテナ", with_env(&["keep"], "env-2")),
        ],
        t,
    );
    assert!(
        !s.seen.contains_key(&k("primary", "old")),
        "古い環境のカードは残らない"
    );
    assert!(!s.id_map.contains_key(&k("primary", "old")));
    assert!(s.seen.contains_key(&k("primary", "new")));
    assert!(s.seen.contains_key(&k("container", "keep")));
    assert!(s.queue.is_empty(), "入れ替わりを新着の洪水にしない");
}

/// 旧設定(1 台)の見た目は変えない: 接続名をカードにも見出しにも付けない。
#[test]
fn a_single_server_shows_no_server_names() {
    let now = Instant::now();
    let mut s = screen();
    s.sync_all(
        &[server(
            "primary",
            "primary",
            Poll::new(vec![entry("p", "a", AgentState::Running, None)], now),
        )],
        now,
    );
    s.ensure_board();
    assert_eq!(s.groups()[0].label, "Project");
    let e = s.entries[0].clone();
    assert_eq!(s.content(&e, now, None, false, false).project, "");
    assert_eq!(s.content(&e, now, None, false, true).project, "Project");
}

/// 接続が World から消えたら、その接続のカードとキャッシュを捨てる。
#[test]
fn a_server_leaving_the_world_is_forgotten() {
    let now = Instant::now();
    let mut s = screen();
    let a = server(
        "primary",
        "primary",
        Poll::new(vec![entry("p", "a", AgentState::Running, None)], now),
    );
    let b = server(
        "container",
        "コンテナ",
        Poll::new(vec![entry("p", "a", AgentState::Running, None)], now),
    );
    s.sync_all(&[a.clone(), b], now);
    assert_eq!(s.seen.len(), 2);
    s.sync_all(&[a], now + Duration::from_secs(1));
    assert_eq!(s.seen.len(), 1);
    assert!(s.seen.contains_key(&k("primary", "a")));
    assert_eq!(s.servers.len(), 1);
    assert_eq!(s.entries.len(), 1);
}

fn approvals_and_running() -> Vec<ServerSnapshot> {
    let now = Instant::now();
    let a: Vec<AgentEntry> = (0..3)
        .map(|i| entry("p", &format!("ask{i}"), AgentState::Approval, None))
        .chain((0..10).map(|i| entry("p", &format!("run{i}"), AgentState::Running, None)))
        .collect();
    let b = vec![entry("p", "input", AgentState::Input, None)];
    vec![
        server("primary", "primary", Poll::new(a, now)),
        server("container", "コンテナ", Poll::new(b, now)),
    ]
}

/// 重要な状態が先に残り、未表示は重要でない側から出る。表示数 + 未表示数 = 接続ごとの候補数。
/// 面積を絞って容量を小さくしても、承認待ち 3 件(A)と入力待ち 1 件(B)の 4 件は必ず表示される。
#[test]
fn when_the_board_is_full_important_cards_win_and_hidden_counts_add_up() {
    let now = Instant::now();
    let world = approvals_and_running();
    let mut found = None;
    // 高さを変えて、盤面に入るカードが少ない大きさを探す。
    for h in (300..=1080).step_by(10) {
        let mut s = AgentScreen::new((640, h), false, 30);
        s.sync_all(&world, now);
        s.ensure_board();
        let hidden = s.hidden_by_server();
        let shown_a = s.servers[0]
            .entries
            .iter()
            .filter(|e| s.board.cards.contains_key(&e.key()))
            .count();
        let shown_b = s.servers[1]
            .entries
            .iter()
            .filter(|e| s.board.cards.contains_key(&e.key()))
            .count();
        // 表示 + 未表示 = 候補(どの大きさでも)。
        assert_eq!(shown_a + hidden[&s.servers[0].id], 13, "h={h}");
        assert_eq!(shown_b + hidden[&s.servers[1].id], 1, "h={h}");
        // 重要な状態は、容量が小さくても必ず表示される。
        for e in s.entries.iter().filter(|e| attention(e)) {
            assert!(
                s.board.cards.contains_key(&e.key()),
                "{} h={h}",
                e.thread_id
            );
        }
        // 未表示になるのは、実行中の側(重要でない側)だけ。
        for e in s
            .entries
            .iter()
            .filter(|e| !s.board.cards.contains_key(&e.key()))
        {
            assert_eq!(e.state, AgentState::Running, "h={h}");
        }
        if hidden[&s.servers[0].id] > 0 && found.is_none() {
            found = Some((h, hidden[&s.servers[0].id], hidden[&s.servers[1].id]));
        }
    }
    let (h, hidden_a, hidden_b) = found.expect("小さい盤面では未表示が出る");
    assert!(
        hidden_a > 0 && hidden_b == 0,
        "h={h}: 未表示は A だけ ({hidden_a}/{hidden_b})"
    );
}

/// 容量 4(承認待ち 3 + 入力待ち 1)にぴったり収まる盤面では、実行中の 10 件がすべて未表示になる。
#[test]
fn a_board_with_room_for_four_hides_all_ten_running_threads_of_server_a() {
    let now = Instant::now();
    let world = approvals_and_running();
    let mut exact = None;
    for h in (300..=1080).step_by(1) {
        let mut s = AgentScreen::new((640, h), false, 30);
        s.sync_all(&world, now);
        s.ensure_board();
        if s.board.cards.len() == 4 {
            exact = Some(h);
            let hidden = s.hidden_by_server();
            assert_eq!(hidden[&s.servers[0].id], 10, "h={h}");
            assert_eq!(hidden[&s.servers[1].id], 0, "h={h}");
            // 表示されている 4 件は、承認待ち 3 件(A)と入力待ち 1 件(B)。
            let mut shown: Vec<String> = s
                .board
                .cards
                .keys()
                .map(|k| format!("{}/{}", k.server, k.thread))
                .collect();
            shown.sort();
            assert_eq!(
                shown,
                [
                    "container/input",
                    "primary/ask0",
                    "primary/ask1",
                    "primary/ask2"
                ],
                "h={h}"
            );
            break;
        }
    }
    assert!(exact.is_some(), "容量 4 の盤面がある");
}

fn fixture(n: usize, label: &str, per_server: usize, now: Instant) -> Vec<ServerSnapshot> {
    (0..n)
        .map(|i| {
            let id = if i == 0 {
                "primary".to_string()
            } else {
                format!("s{i}")
            };
            let entries = (0..per_server)
                .map(|j| {
                    let state = match j % 4 {
                        0 => AgentState::Approval,
                        1 => AgentState::Running,
                        2 => AgentState::Completed,
                        _ => AgentState::Input,
                    };
                    entry("p", &format!("t{j}"), state, None)
                })
                .collect();
            let label = if i == 0 {
                "primary".to_string()
            } else {
                format!("{label}{i}")
            };
            server(&id, &label, Poll::new(entries, now))
        })
        .collect()
}

/// 接続数 1〜4、空、長い接続名、小解像度・VRM 併用相当の幅でも、接続別の欄は互いに重ならず、
/// 状態・件数・未表示は欄に収まる。
#[test]
fn server_chips_never_overlap_and_keep_status_and_counts() {
    let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
    let long = "とても長い接続名の接続とても長い接続名の接続とても";
    assert!(long.chars().count() <= 32);
    for width in [1280.0f32, 1707.0, 1920.0, 2560.0] {
        let fs = (width / 1920.0).clamp(0.6, 1.0);
        for n in 1..=4usize {
            let chip_w = (width - 64.0) / n as f32;
            for label in ["コンテナ", long, "a"] {
                let l = chip_layout(
                    &mut text,
                    "②",
                    label,
                    "再接続",
                    "123件",
                    Some("未表示 1234"),
                    chip_w,
                    fs,
                );
                assert!(
                    l.end_x <= chip_w,
                    "w={width} n={n} {label}: {} > {chip_w}",
                    l.end_x
                );
                assert!(l.status_x < l.count_x && l.count_x < l.hidden_x.unwrap());
                assert!(l.name.starts_with('②'), "the tag always stays: {}", l.name);
                let none = chip_layout(&mut text, "②", label, "LIVE", "0件", None, chip_w, fs);
                assert!(none.end_x <= chip_w && none.hidden_x.is_none());
            }
        }
    }
    // 長い名前は欄が狭いほど省略され、広ければそのまま出る。
    let wide = chip_layout(&mut text, "②", "コンテナ", "LIVE", "3件", None, 900.0, 1.0);
    assert_eq!(wide.name, "② コンテナ");
    let narrow = chip_layout(&mut text, "②", long, "LIVE", "3件", None, 300.0, 1.0);
    assert!(narrow.name.ends_with('…'));
}

#[test]
fn four_servers_render_on_one_screen_at_several_sizes_without_panicking() {
    let now = Instant::now();
    for size in [(1280, 720), (1707, 1080), (1920, 1080), (2560, 1080)] {
        for (n, per) in [(1, 3), (2, 0), (3, 40), (4, 120), (4, 0)] {
            let mut s = AgentScreen::new(size, false, 30);
            let world = fixture(n, "とても長い接続名のコンテナ", per, now);
            s.sync_all(&world, now);
            let cards = render(&mut s, &world, now);
            // 接続ごとに「表示 + 未表示 = 候補」。
            s.ensure_board();
            let hidden = s.hidden_by_server();
            for st in &s.servers {
                let shown = st
                    .entries
                    .iter()
                    .filter(|e| s.board.cards.contains_key(&e.key()))
                    .count();
                assert_eq!(shown + hidden[&st.id], st.entries.len(), "{size:?} n={n}");
            }
            assert!(cards.len() <= 2048);
            let _ = one(&world, "primary");
        }
    }
}

/// 1 台だけの構成でも、盤面に入りきらないスレッドは件数を隠さず「未表示 N」として見出しに出す。
/// (接続別の欄は 2 台以上のときだけなので、1 台用の見出しにも出す。)
#[test]
fn a_single_server_still_reports_its_hidden_threads() {
    let now = Instant::now();
    let entries: Vec<AgentEntry> = (0..60)
        .map(|i| entry("p", &format!("t{i}"), AgentState::Running, None))
        .collect();
    let world = vec![server("primary", "primary", Poll::new(entries, now))];
    let mut s = AgentScreen::new((1280, 720), false, 30);
    s.sync_all(&world, now);
    s.ensure_board();
    let hidden = s.hidden_by_server();
    let hidden_primary = hidden[&s.servers[0].id];
    let shown = s.board.cards.len();
    assert!(hidden_primary > 0, "小さい盤面では入りきらない");
    assert_eq!(shown + hidden_primary, 60, "表示 + 未表示 = 候補");
    // 見出しに「未表示」が描かれる(件数が 0 の同じ入力とは、その欄の画素が違う)。
    let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
    let draw = |hidden: &HashMap<ServerId, usize>, text: &mut miharashi_render::TextEngine| {
        let mut canvas = Canvas::new(1280, 720);
        draw_background(
            &mut canvas,
            text,
            &HeaderInput {
                servers: &s.servers,
                hidden,
                groups: &s.arrangement.groups,
                board: &s.board,
                scale: s.scale(),
                overflow: 0,
                now,
                stale_secs: 30,
                wall: SystemTime::UNIX_EPOCH,
            },
        );
        canvas.pixmap.data().to_vec()
    };
    let with = draw(&hidden, &mut text);
    let none: HashMap<ServerId, usize> = [(s.servers[0].id.clone(), 0)].into_iter().collect();
    let without = draw(&none, &mut text);
    assert_ne!(with, without, "未表示の件数が見出しに出る");
}

/// 集計の行(状態別の件数と合計)は、1280〜2560 幅・3 桁の件数・多いプロジェクト・更新の件数があっても
/// 左の項目と右の合計が重ならず、画面の外へ出ない。広い画面では従来どおりの 175px 間隔・20px。
#[test]
fn summary_row_never_collides_with_the_right_hand_totals() {
    let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
    let mk = |n: [usize; 6]| -> Vec<(&'static str, &'static str, usize, [u8; 4])> {
        [
            ("実行中", "実行"),
            ("承認待ち", "承認"),
            ("入力・計画", "入力"),
            ("異常", "異常"),
            ("応答完了", "完了"),
            ("その他", "他"),
        ]
        .iter()
        .zip(n)
        .map(|(&(l, a), c)| (l, a, c, [200, 200, 200, 255]))
        .collect()
    };
    let worst = mk([1234, 456, 789, 123, 4567, 890]);
    let totals = SummaryTotals {
        synchronized: true,
        threads: 480,
        projects: 24,
        urgent: 120,
        overflow: 35,
    };
    for width in [1024.0f32, 1280.0, 1707.0, 1920.0, 2560.0] {
        let row = summary_row(&mut text, width, &worst, &totals);
        assert_eq!(row.items.len(), 6, "w={width}");
        let mut end = 0.0f32;
        for (i, item) in row.items.iter().enumerate() {
            let w = text.measure(&item.text, row.size);
            assert!(
                item.x >= end - 0.01,
                "w={width}: item {i} overlaps the previous"
            );
            end = item.x + w;
        }
        assert!(end <= width - 32.0, "w={width}: items run off the screen");
        if !row.right.is_empty() {
            let rw = text.measure(&row.right, row.right_size);
            assert!(
                row.right_x >= end + 24.0 - 0.01,
                "w={width}: totals start {} but items end {end}",
                row.right_x
            );
            assert!(
                (row.right_x + rw - (width - 32.0)).abs() < 0.5,
                "right aligned"
            );
        }
        // 実際の画面で、どの文字が残るか: 要確認の件数は(幅が極端に狭くない限り)必ず残る。
        if width >= 1280.0 {
            assert!(row.right.contains("要確認 120"), "w={width}: {}", row.right);
        }
    }
    // 1280 では 175px 間隔のままでは収まらないので、左の文字が縮むか詰まる。
    let narrow = summary_row(&mut text, 1280.0, &worst, &totals);
    assert!(
        narrow.size < 20.0 || narrow.items[1].x - narrow.items[0].x != 175.0,
        "1280 is packed or shrunk"
    );
    // 少ない件数の広い画面では、従来の見た目(20px・175px 間隔・右は 21px)。
    let few = mk([3, 1, 0, 0, 2, 4]);
    let totals = SummaryTotals {
        synchronized: true,
        threads: 10,
        projects: 2,
        urgent: 1,
        overflow: 0,
    };
    let wide = summary_row(&mut text, 1920.0, &few, &totals);
    assert_eq!(wide.size, 20.0);
    assert_eq!(wide.right_size, 21.0);
    for (i, item) in wide.items.iter().enumerate() {
        assert_eq!(item.x, 32.0 + 175.0 * i as f32);
    }
    assert_eq!(wide.right, "10 スレッド · 2 プロジェクト · 要確認 1 件");
}

/// 枠の見出しは、似た長い接続名でも、目印(①〜④)が先頭に残り、幅に収まる。
#[test]
fn group_headings_keep_the_server_tag_and_fit() {
    let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
    let heading = |i: usize, server: &str, project: &str| Group {
        label: format!("{server} / {project}"),
        heading: Some(GroupHeading {
            tag: server_tag(i),
            server: server.into(),
            project: project.into(),
        }),
        entries: Vec::new(),
    };
    let server = "とても長い共通の接続名ですが末尾だけ違う接続-";
    let mut seen = HashSet::new();
    for i in 0..4 {
        for width in [60.0f32, 120.0, 200.0, 320.0, 500.0, 900.0] {
            let g = heading(
                i,
                &format!("{server}{i}"),
                "日本語のとても長いプロジェクト名",
            );
            let size = 28.0;
            let name = group_head_text(&mut text, &g, width, size);
            assert!(name.starts_with(&server_tag(i)), "w={width}: {name}");
            if width >= 120.0 {
                assert!(
                    text.measure(&name, size) <= width + 0.5,
                    "w={width}: {name}"
                );
            }
            if width >= 320.0 {
                assert!(name.contains(" / "), "w={width}: project kept: {name}");
            }
            if width == 500.0 {
                seen.insert(name);
            }
        }
    }
    assert_eq!(seen.len(), 4, "似た接続名でも見出しは 4 つとも違う");
    // 1 台だけの見出しは従来のまま。
    let one = Group {
        label: "Project".into(),
        heading: None,
        entries: Vec::new(),
    };
    assert_eq!(group_head_text(&mut text, &one, 500.0, 28.0), "Project");
}

fn vrm_scene() -> miharashi_vrm::VrmScene {
    let model = miharashi_vrm::Model::from_glb(&miharashi_vrm::synth::build_vrm_glb(
        &miharashi_vrm::synth::Opts {
            segments: 12,
            texture_size: 64,
        },
    ))
    .unwrap();
    miharashi_vrm::VrmScene::from_model(
        model,
        std::rc::Rc::new(miharashi_glscene::ObjectCount::default()),
    )
}

/// 4 台・似た長い接続名・各 120 件を、本番と同じ `WithScene`(VRM 併用)→ `Composite::Gl` の経路で描く。
/// 重要な状態は全部カードになり、表示 + 未表示 = 候補、未表示は重要でない側から、カードは重ならず、
/// 全カードに接続の目印があり、上部の欄・見出し・集計の行が互いに重ならない。
#[test]
fn four_long_named_servers_through_the_vrm_path_keep_priority_counts_and_layout() {
    use crate::app::screens::with_scene::WithScene;
    use crate::screen::{Composite, GlComposite, World};
    let t0 = Instant::now();
    let long = "とても長い共通の接続名ですが末尾だけ違う接続-";
    assert!(format!("{long}9").chars().count() <= 32);
    let world: Vec<ServerSnapshot> = (0..4usize)
        .map(|i| {
            let entries = (0..120usize)
                .map(|j| {
                    let state = match (i, j) {
                        (0, 0..=2) => AgentState::Approval,
                        (1, 0) => AgentState::Input,
                        (2, 0..=1) => AgentState::Error,
                        (_, j) if j % 3 == 0 => AgentState::Completed,
                        _ => AgentState::Running,
                    };
                    entry(&format!("p{}", j % 6), &format!("t{j}"), state, None)
                })
                .collect();
            let id = if i == 0 {
                "primary".to_string()
            } else {
                format!("s{i}")
            };
            server(&id, &format!("{long}{i}"), Poll::new(entries, t0))
        })
        .collect();
    let png_dir = std::env::var_os("MIHARASHI_TEST_PNG_DIR");
    for size in [(1280u32, 720u32), (1920, 1080), (2560, 1080)] {
        let mut s = WithScene::new(
            AgentScreen::new(size, true, 30),
            vrm_scene(),
            miharashi_vrm::StageRegion::Left,
            30,
        );
        s.on_enter(t0);
        s.set_presentation(true, false, true, t0);
        let w = World {
            agents: world.clone(),
            ..Default::default()
        };
        for k in 0..6u64 {
            s.tick(&w, t0 + Duration::from_millis(100 * k));
        }
        let mut canvas = Canvas::new(size.0, size.1);
        let mut text = miharashi_render::TextEngine::new("Noto Sans CJK JP");
        let (cards, region) = {
            let mut ctx = RenderContext {
                canvas: &mut canvas,
                text: &mut text,
                world: &w,
                covered: false,
                force_overlay: false,
            };
            let Composite::Gl(GlComposite {
                base: Some(base),
                region,
                ..
            }) = s.render_composite(&mut ctx)
            else {
                panic!("{size:?}: Composite::Gl with a base");
            };
            let Composite::Agents { cards, .. } = *base else {
                panic!("{size:?}: the base is the agent card board");
            };
            (cards.to_vec(), (region.x, region.y, region.w, region.h))
        };
        let inner = s.inner();
        let board = &inner.board;
        let hidden = inner.hidden_by_server();
        let tier = |e: &AgentEntry| match tier_of(e) {
            Tier::Attention => 0,
            Tier::Running => 1,
            Tier::Quiet => 2,
        };
        let all: Vec<&AgentEntry> = inner.entries.iter().collect();
        assert_eq!(all.len(), 480);
        // 重要な状態(承認待ち 3・入力待ち 1・異常 2)は全部カードになり、描かれる姿勢が画面の中にある。
        let mut urgent = 0;
        for e in all.iter().filter(|e| attention(e)) {
            urgent += 1;
            assert!(
                board.cards.contains_key(&e.key()),
                "{size:?}: {} {}",
                e.server_id,
                e.thread_id
            );
            let id = inner.id_map[&e.key()];
            let card = cards
                .iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("{size:?}: composite has {}", e.thread_id));
            let p = card.pose;
            assert!(p.x >= 0.0 && p.y >= BOARD_TOP - 0.5, "{size:?}: {p:?}");
            assert!(p.x + p.width <= size.0 as f32 + 0.5, "{size:?}: {p:?}");
            assert!(
                p.y + p.height <= (size.1 as f32 - BOARD_BOTTOM_MARGIN) + 0.5,
                "{size:?}: {p:?}"
            );
        }
        assert_eq!(urgent, 6, "A:承認待ち3 + B:入力待ち1 + C:異常2");
        // 接続ごとに 表示 + 未表示 = 120。未表示は、表示されているどれより重要ではない側だけ。
        let mut shown_total = 0;
        for st in &inner.servers {
            let shown = st
                .entries
                .iter()
                .filter(|e| board.cards.contains_key(&e.key()))
                .count();
            shown_total += shown;
            assert_eq!(shown + hidden[&st.id], 120, "{size:?} {}", st.id);
        }
        assert_eq!(shown_total, board.cards.len());
        // 「ほか N 件」にまとめた子は置き場があるので、未表示には数えない(ここは単独スレッドだけ)。
        let worst_shown = all
            .iter()
            .filter(|e| board.cards.contains_key(&e.key()))
            .map(|e| tier(e))
            .max()
            .unwrap();
        for e in all.iter().filter(|e| !board.cards.contains_key(&e.key())) {
            assert!(
                tier(e) >= worst_shown,
                "{size:?}: hidden {} {} is more important than a shown card",
                e.server_id,
                e.thread_id
            );
        }
        assert!(
            hidden.values().sum::<usize>() > 0,
            "{size:?}: 120 件×4 は入りきらない"
        );
        // 全カードに接続の目印があり、設定順の番号と合う。
        for key in board.cards.keys() {
            let Some(content) = inner.content_keys.get(key) else {
                continue;
            };
            let tag = inner.server_tag(&key.server);
            assert!(
                content.contains(&format!("Some({tag:?})")),
                "{size:?}: card {} lacks its tag {tag}: {content}",
                key.thread
            );
        }
        let tags: HashSet<String> = inner
            .servers
            .iter()
            .map(|st| inner.server_tag(&st.id))
            .collect();
        assert_eq!(tags.len(), 4, "似た接続名でも目印は 4 つとも違う");
        // 置かれたカードどうしは重ならない(重なった帯 Strip は手前のカードの後ろに置く設計なので除く)。
        let placed: Vec<(&ThreadKey, &layout::CardSlot)> = board
            .cards
            .iter()
            .filter(|(_, c)| c.mode != layout::CardMode::Strip)
            .collect();
        for (i, (ka, a)) in placed.iter().enumerate() {
            for (kb, b) in &placed[i + 1..] {
                let (ra, rb) = (a.rect, b.rect);
                let overlap = ra.x < rb.x + rb.w - 0.5
                    && rb.x < ra.x + ra.w - 0.5
                    && ra.y < rb.y + rb.h - 0.5
                    && rb.y < ra.y + ra.h - 0.5;
                assert!(!overlap, "{size:?}: {ka:?} {ra:?} overlaps {kb:?} {rb:?}");
            }
        }
        // 上部の接続別の欄: 互いに重ならず、未表示の件数は接続ごとの数のまま、目印が先頭。
        let chip_w = (size.0 as f32 - 64.0) / 4.0;
        let fs = (size.0 as f32 / 1920.0).clamp(0.6, 1.0);
        for (i, st) in inner.servers.iter().enumerate() {
            let n = hidden[&st.id];
            let hidden_text = (n > 0).then(|| format!("未表示 {n}"));
            let l = chip_layout(
                &mut text,
                &server_tag(i),
                &st.label,
                "LIVE",
                "120件",
                hidden_text.as_deref(),
                chip_w,
                fs,
            );
            assert!(l.name.starts_with(&server_tag(i)), "{size:?}: {}", l.name);
            assert!(
                l.end_x <= chip_w,
                "{size:?} chip {i}: {} > {chip_w}",
                l.end_x
            );
            // 実際に描いた背景の、この欄の「未表示」の位置に琥珀色の字がある(0 件の接続には無い)。
            let bg = inner.background.as_ref().expect("background drawn");
            let x0 = (32.0 + i as f32 * chip_w) as u32;
            let amber = (98u32..(98.0 + l.size * 1.3) as u32)
                .flat_map(|y| (x0..x0 + chip_w as u32).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let d = &bg.data()[((y * size.0 + x) * 4) as usize..][..3];
                    d[0] > 200 && (150..200).contains(&d[1]) && (80..130).contains(&d[2])
                })
                .count();
            assert_eq!(
                amber > 20,
                n > 0,
                "{size:?} chip {i}: amber ink {amber} hidden {n}"
            );
        }
        // 枠の見出し: 全部、目印で始まり、幅に収まる。接続が違えば見出しの文字も違う。
        let scale = board.scale_or(inner.scale());
        let mut by_server: HashMap<String, HashSet<String>> = HashMap::new();
        for lane in &board.lanes {
            let g = &inner.arrangement.groups[lane.group];
            let h = g.heading.as_ref().expect("multi headings");
            let head = lane.head;
            let name_size = 28.0 * scale;
            let room = if lane.cont {
                (head.w - 90.0 * scale).max(40.0)
            } else {
                (head.w - text.measure(&g.entries.len().to_string(), 23.0 * scale) - 24.0 * scale)
                    .max(40.0)
            };
            let name = group_head_text(&mut text, g, room, name_size);
            assert!(name.starts_with(&h.tag), "{size:?}: {name}");
            if room > 40.0 + 1.0 {
                assert!(
                    text.measure(&name, name_size) <= room + 0.5,
                    "{size:?}: {name}"
                );
            }
            by_server.entry(h.tag.clone()).or_default().insert(name);
        }
        assert!(by_server.len() >= 2, "{size:?}: 複数の接続の枠が盤面にある");
        // 集計の行: 左の項目と右の合計の間があく。
        let sum_total = 480;
        let row = summary_row(
            &mut text,
            size.0 as f32,
            &[
                ("実行中", "実行", 300, [0; 4]),
                ("承認待ち", "承認", 3, [0; 4]),
                ("入力・計画", "入力", 1, [0; 4]),
                ("異常", "異常", 2, [0; 4]),
                ("応答完了", "完了", 100, [0; 4]),
                ("その他", "他", 0, [0; 4]),
            ],
            &SummaryTotals {
                synchronized: true,
                threads: sum_total,
                projects: inner.arrangement.groups.len(),
                urgent: 6,
                overflow: 0,
            },
        );
        let last = row.items.last().unwrap();
        assert!(
            row.right.is_empty()
                || row.right_x >= last.x + text.measure(&last.text, row.size) + 24.0 - 0.01
        );
        // VRM の領域は画面の左 1/3(高さいっぱい)。盤面はその上にも広がる設計なので、重なる件数は記録だけする。
        let under: usize = cards
            .iter()
            .filter(|c| {
                let p = c.pose;
                p.x < (region.0 + region.2) as f32
                    && p.y < (region.1 + region.3) as f32
                    && p.x + p.width > region.0 as f32
            })
            .count();
        eprintln!(
            "{size:?}: cards={} board_cards={} hidden={hidden:?} vrm_region={region:?} cards_under_vrm={under}",
            cards.len(),
            board.cards.len()
        );
        if let Some(dir) = &png_dir {
            let dir = std::path::Path::new(dir);
            let frame = draw::render_scene_cpu(&draw::AgentScene {
                size,
                background: Some(inner.background.as_ref().unwrap().data()),
                cards: &cards,
                scrim_alpha: 0.0,
                focus_card_id: None,
            })
            .unwrap();
            frame
                .save_png(dir.join(format!("multi-vrm-{}x{}.png", size.0, size.1)))
                .unwrap();
        }
    }
}
