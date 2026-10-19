//! VRM を載せる装飾 `WithScene` のテスト(Issue #71 B T3、docs/DESIGN-screen-plugin-v2.md §6.1・§10・§11.2)。
//! 委譲・時計・動き続ける条件・GL なしの扱い・`kiosk` の登録は GPU なしで動く。描画のテストは `gpu` モジュール(GPU がなければ skip)。
use super::manager::ScreenManager;
use super::screens::with_scene::WithScene;
use super::text_engine;
use crate::data::{DummyData, dummy_snapshot};
use crate::screen::{
    App, Composite, KeyLabels, KeyPress, Outcome, RenderContext, Screen, ScreenAction, ScreenId,
    World,
};
use miharashi_glscene::ObjectCount;
use miharashi_render::canvas::rgba;
use miharashi_vrm::{LoadState, Model, StageRegion, VrmScene, synth};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SIZE: (u32, u32) = (480, 270);

fn synth_model() -> Model {
    Model::from_glb(&synth::build_vrm_glb(&synth::Opts {
        segments: 12,
        texture_size: 64,
    }))
    .unwrap()
}

fn ready_scene() -> VrmScene {
    VrmScene::from_model(synth_model(), Rc::new(ObjectCount::default()))
}

/// 呼び出しを記録し、すべてのメソッドに既定値とは違う値を返す画面(委譲の漏れを見つける)。
struct Probe {
    log: Rc<RefCell<Vec<String>>>,
}

impl Screen for Probe {
    fn id(&self) -> ScreenId {
        "probe"
    }
    fn title(&self) -> &'static str {
        "PROBE"
    }
    fn on_enter(&mut self, _: Instant) {
        self.log.borrow_mut().push("on_enter".into());
    }
    fn on_leave(&mut self, _: Instant) {
        self.log.borrow_mut().push("on_leave".into());
    }
    fn set_presentation(&mut self, visible: bool, covered: bool, display_on: bool, _: Instant) {
        self.log
            .borrow_mut()
            .push(format!("presentation {visible} {covered} {display_on}"));
    }
    fn tick(&mut self, _: &World, _: Instant) -> bool {
        self.log.borrow_mut().push("tick".into());
        true
    }
    fn handle_action(&mut self, action: ScreenAction, _: Instant) -> Outcome {
        self.log.borrow_mut().push(format!("action {action:?}"));
        Outcome::Consumed
    }
    fn key_labels(&self) -> KeyLabels {
        KeyLabels {
            select: Some("決定"),
            up: Some("前"),
            down: None,
        }
    }
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        self.log.borrow_mut().push("render".into());
        ctx.canvas.clear(rgba(10, 20, 30, 255));
    }
    fn invalidate(&mut self) {
        self.log.borrow_mut().push("invalidate".into());
    }
    fn animating(&self) -> bool {
        true
    }
    fn needs_gl(&self) -> bool {
        true
    }
    fn scene_animating(&self) -> bool {
        true
    }
    fn frame_interval(&self) -> Option<Duration> {
        Some(Duration::from_millis(77))
    }
    fn gl_live_objects(&self) -> Option<i64> {
        Some(99)
    }
    fn is_empty(&self) -> bool {
        true
    }
    fn page(&self) -> Option<&'static str> {
        Some("PAGE")
    }
    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        out.push(("probe", "1".into()));
    }
}

fn probe() -> (Probe, Rc<RefCell<Vec<String>>>) {
    let log = Rc::new(RefCell::new(Vec::new()));
    (Probe { log: log.clone() }, log)
}

fn world() -> World {
    World::default()
}

/// `Screen` のメソッド(`needs_gl`・`gl_scene`・`scene_animating`・`frame_interval`・`gl_live_objects` と
/// `render_composite` 以外)は、内側の画面にそのまま渡り、結果も内側のもの。
#[test]
fn delegates_everything_to_the_inner_screen() {
    let (inner, log) = probe();
    let mut s = WithScene::new(inner, ready_scene(), StageRegion::Left, 30);
    let t = Instant::now();
    assert_eq!((s.id(), s.title()), ("probe", "PROBE"));
    assert_eq!(s.key_labels().select, Some("決定"));
    assert_eq!(s.key_labels().up, Some("前"));
    assert_eq!(s.key_labels().down, None);
    assert!(s.animating(), "animating は内側のまま");
    assert!(s.is_empty());
    assert_eq!(s.page(), Some("PAGE"));
    let mut stats = Vec::new();
    s.stats(&mut stats);
    assert_eq!(stats, [("probe", "1".to_string())]);
    assert_eq!(
        s.handle_action(ScreenAction::Down, t),
        Outcome::Consumed,
        "操作の結果も内側のもの"
    );
    s.on_enter(t);
    s.set_presentation(true, true, false, t);
    assert!(s.tick(&world(), t), "tick の戻り値は内側のもの");
    s.invalidate();
    s.on_leave(t);
    let mut c = miharashi_render::Canvas::new(SIZE.0, SIZE.1);
    let mut text = text_engine("");
    let w = world();
    s.render(&mut RenderContext {
        canvas: &mut c,
        text: &mut text,
        world: &w,
        covered: false,
        force_overlay: false,
    });
    assert_eq!(
        log.borrow().as_slice(),
        [
            "action Down",
            "on_enter",
            "presentation true true false",
            "tick",
            "invalidate",
            "on_leave",
            "render",
        ]
    );
    assert_eq!(
        c.pixmap.data()[..4],
        [10, 20, 30, 255],
        "render は内側の CPU 描画"
    );
}

/// 3D に関する 5 つだけは、内側の値(ここでは Probe の 77ms・99・needs_gl)でなく装飾自身のもの。
#[test]
fn scene_methods_are_its_own() {
    let (inner, _) = probe();
    let mut s = WithScene::new(inner, ready_scene(), StageRegion::Left, 30);
    assert!(!s.needs_gl(), "装飾は GL が要る画面ではない");
    assert!(s.gl_scene().is_some());
    assert_eq!(s.gl_live_objects(), Some(0), "まだ何も作っていない");
    // 見えていないあいだは動かない(Probe は scene_animating = true だが引き継がない)
    assert!(!s.scene_animating());
    // 見えていれば、内側の間隔でなく自分の fps
    s.set_presentation(true, false, true, Instant::now());
    assert!(s.scene_animating());
    let i = s.frame_interval().unwrap().as_secs_f64();
    assert!((i - 1.0 / 30.0).abs() < 1e-9, "{i}");
    // 動いていなければ内側の間隔
    s.set_presentation(false, false, true, Instant::now());
    assert_eq!(s.frame_interval(), Some(Duration::from_millis(77)));
}

/// 動き続けるのは、読み込みが終わっていて、表示中かつ点灯中だけ。
/// 読み込み中・失敗・非表示・消灯・on_leave のあとでは止まる。
#[test]
fn scene_animates_only_while_ready_visible_and_on() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mk = |scene| WithScene::new(probe().0, scene, StageRegion::Left, 30);

    // Ready: 表示 → 動く。アラート・メニューが上にあっても動く
    let mut s = mk(ready_scene());
    assert!(!s.scene_animating(), "出る前は動かさない");
    s.on_enter(at(0));
    assert!(s.scene_animating());
    s.set_presentation(true, true, true, at(1));
    assert!(s.scene_animating(), "覆われていても下の VRM は動く");
    s.set_presentation(true, false, false, at(2));
    assert!(!s.scene_animating(), "消灯中は止まる");
    s.set_presentation(true, false, true, at(3));
    assert!(s.scene_animating());
    s.set_presentation(false, false, true, at(4));
    assert!(!s.scene_animating(), "非表示で止まる");
    s.set_presentation(true, false, true, at(5));
    s.on_leave(at(6));
    assert!(!s.scene_animating(), "on_leave で止まる");

    // 読み込み中は動かさず、終わった周期の tick は true(描き直させる)
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let glb = synth::build_vrm_glb(&synth::Opts {
        segments: 12,
        texture_size: 64,
    });
    let mut s = mk(VrmScene::from_loader(
        Rc::new(ObjectCount::default()),
        move || {
            gate_rx.recv().map_err(|e| e.to_string())?;
            Model::from_glb(&glb)
        },
    ));
    s.on_enter(at(0));
    assert_eq!(s.scene().state(), LoadState::Loading);
    assert!(!s.scene_animating(), "読み込み中は動かさない");
    gate_tx.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut became_ready_tick = false;
    while Instant::now() < end && !became_ready_tick {
        std::thread::sleep(Duration::from_millis(5));
        // Probe の tick は常に true なので、`WithScene` 自身の判定は内側が false の画面で見る(下)
        s.tick(&world(), at(2));
        became_ready_tick = s.scene().state() == LoadState::Ready;
    }
    assert!(became_ready_tick, "読み込みが終わらない");
    assert!(s.scene_animating(), "読み込みが終わったら動く");

    // 失敗(読み込みのエラー)は動かさない
    let mut s = mk(VrmScene::from_loader(
        Rc::new(ObjectCount::default()),
        || Err("壊れたモデル".into()),
    ));
    s.on_enter(at(0));
    let end = Instant::now() + Duration::from_secs(20);
    while s.scene().state() == LoadState::Loading && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(&world(), at(1));
    }
    assert_eq!(s.scene().state(), LoadState::Failed);
    assert!(
        !s.scene_animating(),
        "失敗したら毎フレームの描き直しを止める"
    );
    assert_eq!(s.frame_interval(), Some(Duration::from_millis(77)));

    // 読み始めていない(lazy)のも動かさない
    let s = mk(VrmScene::lazy(
        "/nonexistent/vrm/never-read.glb".into(),
        Rc::new(ObjectCount::default()),
    ));
    assert!(!s.scene_animating());
}

/// 内側の画面が再描画を求めなくても、読み込みが終わった周期の tick は true(1 回だけ)。
#[test]
fn tick_requests_one_redraw_when_loading_finishes() {
    struct Quiet;
    impl Screen for Quiet {
        fn id(&self) -> ScreenId {
            "quiet"
        }
        fn title(&self) -> &'static str {
            "QUIET"
        }
        fn render(&mut self, _: &mut RenderContext<'_>) {}
    }
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let glb = synth::build_vrm_glb(&synth::Opts {
        segments: 12,
        texture_size: 64,
    });
    let scene = VrmScene::from_loader(Rc::new(ObjectCount::default()), move || {
        gate_rx.recv().map_err(|e| e.to_string())?;
        Model::from_glb(&glb)
    });
    let mut s = WithScene::new(Quiet, scene, StageRegion::Left, 30);
    let t = Instant::now();
    s.on_enter(t);
    assert!(!s.tick(&world(), t), "読み込み中は何も求めない");
    gate_tx.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut trues = 0;
    let mut after = 0;
    while Instant::now() < end && after < 5 {
        std::thread::sleep(Duration::from_millis(5));
        let r = s.tick(&world(), t);
        trues += usize::from(r);
        if s.scene().state() == LoadState::Ready {
            after += 1;
        }
    }
    assert_eq!(trues, 1, "終わった周期だけ true");
}

/// 時刻は、表示中かつ点灯中に経った分だけ進む。表情の入力はモデルが持つ名前だけで、時刻で変わる。
#[test]
fn input_follows_the_active_clock() {
    struct Quiet;
    impl Screen for Quiet {
        fn id(&self) -> ScreenId {
            "quiet"
        }
        fn title(&self) -> &'static str {
            "QUIET"
        }
        fn render(&mut self, _: &mut RenderContext<'_>) {}
    }
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let w = world();
    let mut s = WithScene::new(Quiet, ready_scene(), StageRegion::Left, 30);
    assert!(s.input().gesture, "身振りは入れる");
    s.set_presentation(false, false, true, t0);
    s.tick(&w, at(10_000));
    assert_eq!(s.active(), Duration::ZERO, "非表示のあいだは進まない");

    s.set_presentation(true, false, true, at(10_000));
    s.tick(&w, at(11_000));
    assert_eq!(s.active(), Duration::from_secs(1));
    assert_eq!(s.input().t, 1.0);
    // 合成モデルは母音の表情を持たない。持たない名前は渡さない(渡すと draw が警告を出す)
    let names: Vec<_> = s
        .input()
        .expressions
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["blink", "happy"]);

    // まばたきの最中(周期 4 秒のうち 1.0〜1.3 秒)は weight が立ち、そうでなければ 0
    s.tick(&w, at(11_150));
    assert!(
        s.input().expressions[0].1 > 0.5,
        "{:?}",
        s.input().expressions
    );
    s.tick(&w, at(13_000));
    assert_eq!(s.input().expressions[0].1, 0.0);

    // 消灯のあいだは止まり、復帰でまとめて進まない
    s.set_presentation(true, false, false, at(14_000));
    assert_eq!(s.active(), Duration::from_secs(4));
    s.tick(&w, at(100_000));
    assert_eq!(s.active(), Duration::from_secs(4));
    s.set_presentation(true, false, true, at(100_000));
    s.tick(&w, at(100_500));
    assert_eq!(s.active(), Duration::from_millis(4500));
}

/// Agent 画面では、拡大中のカードの方へ頭を向ける入力になる(目標がなければ向けない)。写真画面では向けない。
#[test]
fn agent_focus_aims_the_head_and_photo_does_not() {
    use super::screens::agents::AgentScreen;
    let base = Instant::now();
    let wall = std::time::SystemTime::now();
    let (initial, fresh, _) = crate::agent_demo::fixture_snapshots(wall, base);
    let mut world = World {
        agents: initial,
        ..World::default()
    };
    let mut s = WithScene::new(
        AgentScreen::new((1280, 540), true, 30),
        ready_scene(),
        StageRegion::Left,
        30,
    );
    s.on_enter(base);
    s.set_presentation(true, false, true, base);
    let mut text = text_engine("");
    let mut c = miharashi_render::Canvas::new(1280, 540);
    let mut step = |s: &mut WithScene<AgentScreen>, world: &World, ms: u64| {
        let now = base + Duration::from_millis(ms);
        s.tick(world, now);
        let mut ctx = RenderContext {
            canvas: &mut c,
            text: &mut text,
            world,
            covered: false,
            force_overlay: false,
        };
        let comp = s.render_composite(&mut ctx);
        matches!(comp, Composite::Gl(_))
    };
    assert!(step(&mut s, &world, 0));
    assert!(s.input().aim.is_none(), "フォーカスがなければ向けない");
    world.agents = fresh;
    let mut max_look = 0.0f32;
    for k in 10..=32 {
        assert!(step(&mut s, &world, 100 * k));
        if let Some(a) = s.input().aim {
            max_look = max_look.max(a.look);
            assert_eq!(a.point, 0.0, "指さしはしない");
        }
    }
    assert!(max_look > 0.9, "拡大したカードの方を向く: {max_look}");
    assert!(
        s.input().dt > 0.0 && s.input().dt <= 0.1,
        "描画の間隔で進む: {}",
        s.input().dt
    );
}

/// GL が使えない・3D が失敗した画面は、ScreenManager が `Composite::Gl` の下地だけにする。
/// 元の画面だけを出したのと 1 画素も違わず、3D の分の再描画や間隔の上限を足さない。
#[test]
fn without_gl_the_manager_shows_only_the_inner_screen() {
    /// 時刻や実測に依らず、同じ絵を描く画面(2 つの ScreenManager の絵を比べるため)
    struct Solid;
    impl Screen for Solid {
        fn id(&self) -> ScreenId {
            "solid"
        }
        fn title(&self) -> &'static str {
            "SOLID"
        }
        fn render(&mut self, ctx: &mut RenderContext<'_>) {
            ctx.canvas.clear(rgba(40, 80, 120, 255));
        }
    }
    let new_mgr = |wrapped: bool, gl: bool| {
        let screens: Vec<Box<dyn Screen>> = vec![if wrapped {
            Box::new(WithScene::new(Solid, ready_scene(), StageRegion::Left, 30))
        } else {
            Box::new(Solid)
        }];
        ScreenManager::new(
            screens,
            "solid",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            text_engine(""),
            Duration::from_secs(60),
        )
        .with_gl_available(gl)
    };
    let t = Instant::now();
    let mut plain = new_mgr(false, false);
    let mut wrapped = new_mgr(true, false);
    plain.tick(t);
    wrapped.tick(t);
    assert!(wrapped.tick(t + Duration::from_millis(5)) == plain.tick(t + Duration::from_millis(5)));
    assert!(!App::animating(&wrapped), "GL なしでは動かし続けない");
    assert_eq!(App::frame_interval(&wrapped), None);
    let (mut a, mut b) = (
        miharashi_render::Canvas::new(SIZE.0, SIZE.1),
        miharashi_render::Canvas::new(SIZE.0, SIZE.1),
    );
    assert!(matches!(plain.render_composite(&mut a), Composite::Canvas));
    assert!(
        matches!(wrapped.render_composite(&mut b), Composite::Canvas),
        "Gl は外される"
    );
    assert!(a.pixmap.data() == b.pixmap.data(), "元の画面と同じ絵");
    // CPU の render も同じ
    plain.render(&mut a);
    wrapped.render(&mut b);
    assert!(a.pixmap.data() == b.pixmap.data());

    // GL があれば、同じ画面が Gl で包まれ、動き続ける。3D が失敗したら元の画面だけに戻り、動かない
    let mut gl = new_mgr(true, true);
    gl.tick(t);
    assert!(App::animating(&gl));
    assert!(
        gl.tick(t + Duration::from_millis(5)),
        "動く間は毎周期描き直す"
    );
    assert!(App::frame_interval(&gl).is_some());
    let mut c = miharashi_render::Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(gl.render_composite(&mut c), Composite::Gl(_)));
    gl.report_scene_failure("試験");
    assert!(!App::animating(&gl), "失敗したら止める");
    assert_eq!(App::frame_interval(&gl), None);
    assert!(matches!(gl.render_composite(&mut c), Composite::Canvas));
    assert!(
        !gl.tick(t + Duration::from_millis(10)),
        "失敗中は 3D の分の再描画を求めない"
    );
}

/// 巡回・画面の並び・操作・パネルの見せ方は、包んでも内側の画面のまま。
#[test]
fn id_keys_and_rotation_are_the_inner_screens() {
    use super::screens::photo::PhotoScreen;
    use crate::config::Config;
    use crate::photos::Source;
    let (dir, p) = super::tests::test_photo();
    let photo = PhotoScreen::new(&Config::default(), SIZE, Some(4096), Source::File(p, None));
    let feed = photo.feed_provider();
    let mut mgr = ScreenManager::new(
        vec![
            Box::new(super::screens::status::StatusScreen::new()),
            Box::new(WithScene::new(photo, ready_scene(), StageRegion::Left, 30)),
        ],
        "status",
        Box::new(DummyData::new()),
        dummy_snapshot(0.0),
        text_engine(""),
        Duration::from_secs(60),
    )
    .with_gl_available(true)
    .with_provider(Box::new(feed));
    let v = mgr.panel_view().unwrap();
    let ids: Vec<_> = v.screens.iter().map(|s| (s.id, s.title)).collect();
    assert_eq!(ids, [("status", "STATUS"), ("photo", "PHOTO FRAME")]);
    mgr.on_key(KeyPress::Next);
    assert_eq!(mgr.screen_id(), "photo");
    let keys = mgr.panel_view().unwrap().keys;
    assert_eq!(
        (keys.select, keys.up, keys.down),
        (Some("情報を隠す"), Some("前の写真"), Some("次の写真")),
        "写真画面のキーのラベル"
    );
    // 巡回に出る(GL が要る画面ではないので、GL がなくても飛ばさない)
    let steps = [
        crate::config::RotationStep {
            screen: "photo".into(),
            secs: 10,
            skip_if_empty: false,
        },
        crate::config::RotationStep {
            screen: "status".into(),
            secs: 10,
            skip_if_empty: false,
        },
    ];
    for gl in [true, false] {
        let (_, p) = super::tests::test_photo();
        let photo = PhotoScreen::new(&Config::default(), SIZE, Some(4096), Source::File(p, None));
        let mut m = ScreenManager::new(
            vec![
                Box::new(super::screens::status::StatusScreen::new()),
                Box::new(WithScene::new(photo, ready_scene(), StageRegion::Left, 30)),
            ],
            "status",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            text_engine(""),
            Duration::from_secs(60),
        )
        .with_gl_available(gl)
        .with_rotation(&steps, Duration::from_secs(300));
        assert_eq!(m.screen_id(), "photo", "gl={gl}: 最初の手順は写真");
        let mut t = Instant::now();
        t += Duration::from_secs(10);
        m.tick(t);
        assert_eq!(m.screen_id(), "status", "gl={gl}");
        t += Duration::from_secs(10);
        m.tick(t);
        assert_eq!(m.screen_id(), "photo", "gl={gl}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `kiosk` が VRM を載せるのは、有効で、GL が使えて、その画面が `vrm.screens` にあるときだけ。
/// 無効(既定)・`renderer = cpu`(GL なし)では、画面の構成も状態ログも今までと同じ。
#[test]
fn kiosk_wraps_only_when_enabled_with_gl_and_listed() {
    use crate::config::Config;
    use crate::photos::Source;
    // (画面の id の並び, 状態ログに gl_objs があるか = 場面を持つ画面があるか)
    let build = |toml: &str, gl: Option<u32>| {
        let cfg = Config::parse(toml).unwrap();
        let (dir, p) = super::tests::test_photo();
        let app = super::kiosk(
            &cfg,
            SIZE,
            gl,
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            None,
        );
        let _ = std::fs::remove_dir_all(dir);
        let ids: Vec<_> = app
            .panel_view()
            .unwrap()
            .screens
            .iter()
            .map(|s| s.id)
            .collect();
        let has_scene = app.stats().iter().any(|(k, _)| *k == "gl_objs");
        (ids, has_scene)
    };
    let on = "[vrm]\nenabled = true\nmodel = \"/nonexistent/vrm/model.vrm\"\n";
    let plain = (vec!["status", "photo"], false);
    assert_eq!(build("", None), plain);
    assert_eq!(build("", Some(4096)), plain, "既定は無効");
    assert_eq!(
        build(on, None),
        plain,
        "GL なし(renderer = cpu)では包まない"
    );
    assert_eq!(build("[vrm]\nenabled = false\n", Some(4096)), plain);
    assert_eq!(
        build(on, Some(4096)),
        (vec!["status", "photo"], true),
        "画面の構成は変えず、場面を持つ"
    );
    assert_eq!(
        build(&format!("{on}screens = [\"agents\"]\n"), Some(4096)),
        plain,
        "写真を載せない設定"
    );
}

#[cfg(target_os = "linux")]
mod gpu {
    use super::*;
    use crate::app::screens::photo::PhotoScreen;
    use crate::config::Config;
    use crate::photos::Source;
    use crate::runtime::{FrameParts, scene_frame};
    use crate::screen::Provider;
    use miharashi_gpu::{Offscreen, SceneOutcome};
    use std::path::Path;

    fn offscreen() -> Option<Offscreen> {
        match Offscreen::open(Path::new("/dev/dri/renderD128")) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                None
            }
        }
    }

    /// 写真 + VRM を、持続する Compositor で 3 フレーム描く(1 枚目は写真を渡し、2 枚目以降は GPU 側のものと
    /// 前の重ね絵を使う)。どのフレームも写真と重ね絵が残り、領域の中に VRM が重なる。
    #[test]
    fn photo_and_vrm_keep_the_photo_and_overlay_across_frames() {
        use miharashi_gpu::compose::Compositor;
        let Some(off) = offscreen() else { return };
        let (dir, p) = super::super::tests::test_photo();
        let photo = PhotoScreen::new(&Config::default(), SIZE, Some(4096), Source::File(p, None));
        let feed: Box<dyn Provider> = Box::new(photo.feed_provider());
        let mut mgr = ScreenManager::new(
            vec![Box::new(WithScene::new(
                photo,
                ready_scene(),
                StageRegion::Left,
                30,
            ))],
            "photo",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            text_engine(""),
            Duration::from_secs(60),
        )
        .with_gl_available(true)
        .with_provider(feed);
        let end = Instant::now() + Duration::from_secs(5);
        let t0 = Instant::now();
        while mgr.panel_view().unwrap().photo.is_none() && Instant::now() < end {
            mgr.tick(t0);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(mgr.panel_view().unwrap().photo.is_some());
        let region = WithScene::<PhotoScreen>::region(StageRegion::Left, SIZE);
        let frames = off
            .with_target(SIZE, |gl, fbo| {
                let mut comp = Compositor::new(gl, SIZE).unwrap();
                let mut scratch = Vec::new();
                let mut out = Vec::new();
                for k in 0..3u64 {
                    mgr.tick(t0 + Duration::from_millis(100 * k));
                    let mut c = miharashi_render::Canvas::new(SIZE.0, SIZE.1);
                    let comp_frame = mgr.render_composite(&mut c);
                    let mut parts = FrameParts::default();
                    parts.fill(&comp_frame);
                    let Composite::Gl(g) = comp_frame else {
                        panic!("Composite::Gl ではない")
                    };
                    let mut sf = scene_frame(g, &c, &parts);
                    let o = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
                    assert_eq!(o, SceneOutcome::Drawn, "{k} 枚目");
                    let mut img = vec![0; (SIZE.0 * SIZE.1 * 4) as usize];
                    miharashi_gpu::read_rgba_top_down(gl, fbo, SIZE, &mut img, &mut scratch);
                    out.push(img);
                }
                comp.destroy(gl);
                out
            })
            .unwrap();
        let px = |img: &[u8], x: u32, y: u32| -> [u8; 4] {
            let i = ((y * SIZE.0 + x) * 4) as usize;
            img[i..i + 4].try_into().unwrap()
        };
        for (k, img) in frames.iter().enumerate() {
            // 領域の外の写真(緑)が、どのフレームでも出る
            let outside = px(img, SIZE.0 - 5, SIZE.1 / 2);
            assert_eq!(outside[3], 255, "{k} 枚目");
            // 領域の中に VRM が重なり、写真だけの絵と変わる
            let inside = (0..region.h)
                .flat_map(|y| (0..region.w).map(move |x| (x, y)))
                .filter(|&(x, y)| px(img, x, y) != px(&frames[0], SIZE.0 - 5, 5))
                .count();
            assert!(
                inside > 200,
                "{k} 枚目: 領域に VRM が重なっていない {inside}"
            );
        }
        // 写真の画素(領域の外)は、1 枚目と 2・3 枚目で同じ(GPU 側の写真と重ね絵が残っている)
        for k in 1..3 {
            for (x, y) in [
                (SIZE.0 - 5, 10),
                (SIZE.0 - 5, SIZE.1 / 2),
                (SIZE.0 - 40, SIZE.1 - 10),
            ] {
                assert_eq!(
                    px(&frames[k], x, y),
                    px(&frames[0], x, y),
                    "{k} 枚目 ({x},{y})"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
