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
                assert!(!l.name.is_empty());
                let none = chip_layout(&mut text, label, "LIVE", "0件", None, chip_w, fs);
                assert!(none.end_x <= chip_w && none.hidden_x.is_none());
            }
        }
    }
    // 長い名前は欄が狭いほど省略され、広ければそのまま出る。
    let wide = chip_layout(&mut text, "コンテナ", "LIVE", "3件", None, 900.0, 1.0);
    assert_eq!(wide.name, "コンテナ");
    let narrow = chip_layout(&mut text, long, "LIVE", "3件", None, 300.0, 1.0);
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
