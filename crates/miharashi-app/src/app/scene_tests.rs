//! 3D の画面(`Composite::Gl`)の仕組みのテスト(Issue #74 T2、docs/DESIGN-screen-plugin-v2.md)。
//! 場面と画面は `scene_check` の試験用のもの。GPU を使うテストは `gpu` モジュール(GPU がなければ skip)。
use super::manager::ScreenManager;
use super::text_engine;
use crate::config::RotationStep;
use crate::data::{DummyData, dummy_snapshot};
use crate::scene_check::{BASE_COLOR, Base, CheckScreen, Probe};
use crate::screen::{App, Composite, KeyPress, RenderContext, Screen, ScreenId, Top};
use miharashi_render::alert::{self, AlertView};
use miharashi_render::canvas::rgba;
use miharashi_render::{Canvas, notice};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const SIZE: (u32, u32) = (320, 180);

/// 3D を持たない普通の画面。
struct Plain(ScreenId);

impl Screen for Plain {
    fn id(&self) -> ScreenId {
        self.0
    }
    fn title(&self) -> &'static str {
        "PLAIN"
    }
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        ctx.canvas.clear(rgba(0, 0, 0, 255));
    }
}

fn manager(
    screens: Vec<Box<dyn Screen>>,
    start: &str,
    gl: bool,
) -> (ScreenManager, Sender<AlertView>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let app = ScreenManager::new(
        screens,
        start,
        Box::new(DummyData::new()),
        dummy_snapshot(0.0),
        text_engine(""),
        Duration::from_secs(60),
    )
    .with_gl_available(gl)
    .with_alerts(Some(rx));
    (app, tx)
}

fn check(id: ScreenId, gl_only: bool, base: Base, probe: &Rc<Probe>) -> Box<dyn Screen> {
    Box::new(CheckScreen::new(id, gl_only, base, probe.clone()))
}

fn quake() -> AlertView {
    AlertView::Quake(crate::quake::demo_view())
}

/// アラートだけを透明な Canvas に描いたもの(最上段の層の期待値)。
fn alert_layer(view: &AlertView) -> Canvas {
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    c.clear(rgba(0, 0, 0, 0));
    alert::overlay(&mut c, &mut text_engine(""), view);
    c
}

/// `Gl` なら最上段の指示("off" / "keep" / "upload")。
fn top_kind(comp: &Composite<'_>) -> &'static str {
    match comp {
        Composite::Gl(g) => match g.top {
            Top::Off => "off",
            Top::Keep => "keep",
            Top::Upload(_) => "upload",
        },
        _ => panic!("Composite::Gl ではない"),
    }
}

fn frame_top(app: &mut ScreenManager) -> &'static str {
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    top_kind(&app.render_composite(&mut c))
}

/// 最上段の層はアラートを透明背景に描いたもの(`Upload` のとき)。`Gl` の下地(画面の Canvas)にはアラートを描かない。
/// `Off` の次は必ず `Upload`、`Keep` は前回が出したときだけ。消灯・復帰をはさんでも `Keep` に頼らない。
#[test]
fn top_is_upload_after_off_and_keep_only_after_upload() {
    let probe = Rc::new(Probe::default());
    let (mut app, tx) = manager(
        vec![check("check", false, Base::Canvas, &probe)],
        "check",
        true,
    );
    let now = Instant::now();
    assert_eq!(frame_top(&mut app), "off", "アラートもメニューも無い");

    tx.send(quake()).unwrap();
    app.tick(now);
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    let comp = app.render_composite(&mut c);
    match &comp {
        Composite::Gl(g) => match g.top {
            Top::Upload(p) => assert_eq!(p.data(), alert_layer(&quake()).pixmap.data()),
            _ => panic!("最初は Upload"),
        },
        _ => panic!("Gl"),
    }
    drop(comp);
    assert_eq!(
        &c.pixmap.data()[..4],
        &BASE_COLOR,
        "下地の Canvas にアラートは描かない(3D の下に入るので)"
    );
    assert_eq!(frame_top(&mut app), "keep", "変わっていなければ前回のまま");

    app.on_key(KeyPress::Select); // アラートを閉じる
    assert_eq!(frame_top(&mut app), "off");
    tx.send(quake()).unwrap();
    app.tick(now);
    assert_eq!(frame_top(&mut app), "upload", "Off の次は必ず Upload");
    assert_eq!(frame_top(&mut app), "keep");
    app.set_display_on(false, now);
    app.set_display_on(true, now);
    assert_eq!(
        frame_top(&mut app),
        "upload",
        "消灯・復帰をはさむと GPU 側の重ね絵が最後のものか分からない"
    );
}

/// 写真の下地(重ね絵は `overlay_changed` のときだけ描き直す)に 3D を重ねた画面で、アラート表示中に 3D が失敗する。
/// 次のフレームからは `Gl` でなく `Photo` になるので、重ね絵を描き直させてアラートが消えないこと。
#[test]
fn alert_stays_visible_when_the_scene_fails_over_a_photo() {
    let probe = Rc::new(Probe::default());
    let (mut app, tx) = manager(
        vec![check("check", false, Base::Photo, &probe)],
        "check",
        true,
    );
    let now = Instant::now();
    tx.send(quake()).unwrap();
    app.tick(now);
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert_eq!(top_kind(&app.render_composite(&mut c)), "upload");
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    match app.render_composite(&mut c) {
        Composite::Gl(g) => match g.base.as_deref() {
            Some(Composite::Photo {
                overlay_changed, ..
            }) => assert!(
                !overlay_changed,
                "変わっていなければ写真の重ね絵は描き直さない"
            ),
            _ => panic!("下地は写真"),
        },
        _ => panic!("Gl"),
    }

    let invalidated = probe.invalidates.get();
    app.report_scene_failure("試験");
    assert_eq!(
        probe.invalidates.get(),
        invalidated + 1,
        "画面の描き直しを要求する"
    );
    assert!(app.stats().contains(&("scene_fail", "1".to_string())));

    // 失敗の次のフレーム: 写真の重ね絵を描き直し(force_overlay)、その上にアラートを描く
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    match app.render_composite(&mut c) {
        Composite::Photo {
            overlay_changed, ..
        } => assert!(overlay_changed, "アラートが消えないよう重ね絵を描き直す"),
        _ => panic!("3D が失敗した画面は下地だけになる"),
    }
    let mut expected = Canvas::new(SIZE.0, SIZE.1);
    expected.clear(rgba(0, 0, 0, 0));
    expected.fill_rect(4.0, 4.0, 24.0, 12.0, rgba(255, 255, 255, 255));
    alert::overlay(&mut expected, &mut text_engine(""), &quake());
    assert_eq!(
        c.pixmap.data(),
        expected.pixmap.data(),
        "写真の重ね絵にアラートが載る"
    );
}

/// 画面の Canvas が完成画面(下地が `Canvas`)の 3D 画面でも、失敗の次のフレームからアラートが見え続ける。
#[test]
fn alert_stays_visible_when_the_scene_fails_over_a_canvas() {
    let probe = Rc::new(Probe::default());
    let (mut app, tx) = manager(
        vec![check("check", false, Base::Canvas, &probe)],
        "check",
        true,
    );
    tx.send(quake()).unwrap();
    app.tick(Instant::now());
    assert_eq!(frame_top(&mut app), "upload");
    app.report_scene_failure("試験");
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Canvas));
    let mut expected = Canvas::new(SIZE.0, SIZE.1);
    expected.clear(rgba(BASE_COLOR[0], BASE_COLOR[1], BASE_COLOR[2], 255));
    alert::overlay(&mut expected, &mut text_engine(""), &quake());
    assert_eq!(c.pixmap.data(), expected.pixmap.data());
}

/// 3D の失敗はその画面だけ。ほかの 3D の画面は出し続ける。失敗した画面は戻らない(v1)。
#[test]
fn failure_affects_only_that_screen_and_stays() {
    let (a, b) = (Rc::new(Probe::default()), Rc::new(Probe::default()));
    let (mut app, _tx) = manager(
        vec![
            check("a", false, Base::Canvas, &a),
            check("b", false, Base::Canvas, &b),
        ],
        "a",
        true,
    );
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Gl(_)));
    app.report_scene_failure("試験");
    for _ in 0..3 {
        assert!(matches!(app.render_composite(&mut c), Composite::Canvas));
    }
    app.on_key(KeyPress::Next);
    assert!(
        matches!(app.render_composite(&mut c), Composite::Gl(_)),
        "b は出る"
    );
    app.on_key(KeyPress::Prev);
    assert!(
        matches!(app.render_composite(&mut c), Composite::Canvas),
        "a は戻らない"
    );
}

/// GL が使えないとき(renderer = cpu、GL なしの auto)は、装飾は下地だけを出し、3D の分の再描画を求めない。
/// 動かない写真画面を毎フレーム CPU で描き直さない。GL が使えて失敗していなければ求める。失敗後は求めない。
#[test]
fn scene_animation_is_requested_only_when_gl_works() {
    let drive = |gl: bool, fail: bool| {
        let probe = Rc::new(Probe::default());
        probe.scene_animating.set(true);
        let (mut app, _tx) = manager(
            vec![check("check", false, Base::Photo, &probe)],
            "check",
            gl,
        );
        let mut c = Canvas::new(SIZE.0, SIZE.1);
        let first = matches!(app.render_composite(&mut c), Composite::Gl(_));
        if fail {
            app.report_scene_failure("試験");
        }
        let t = Instant::now();
        app.tick(t); // 時計の更新
        (first, app.tick(t), app.animating())
    };
    assert_eq!(
        drive(true, false),
        (true, true, true),
        "3D が動く間は描き直す"
    );
    assert_eq!(
        drive(false, false),
        (false, false, false),
        "GL なしは下地だけ。描き直さない"
    );
    assert_eq!(
        drive(true, true),
        (true, false, false),
        "失敗後は描き直さない"
    );
}

/// 消灯中は 3D の再描画を求めない。画面には消灯・点灯が伝わる(演出の時計を止められる)。復帰で戻る。
#[test]
fn scene_does_not_animate_while_the_display_is_off() {
    let probe = Rc::new(Probe::default());
    probe.scene_animating.set(true);
    let (mut app, _tx) = manager(
        vec![check("check", false, Base::Canvas, &probe)],
        "check",
        true,
    );
    let t = Instant::now();
    app.tick(t);
    assert!(app.tick(t) && app.animating());
    assert_eq!(probe.display_on.get(), Some(true));
    app.set_display_on(false, t);
    assert!(!app.tick(t), "消灯中は描き直しを求めない");
    assert!(!app.animating());
    assert_eq!(probe.display_on.get(), Some(false), "画面に消灯が伝わる");
    app.set_display_on(true, t);
    assert!(app.tick(t) && app.animating(), "復帰したら戻る");
    assert_eq!(probe.display_on.get(), Some(true));
}

/// 3D だけの画面は、GL が使えないと CPU の案内(`render`)を出し、`render_composite` は呼ばない。
/// 手動では選べる。GL が使えれば `Composite::Gl`。
#[test]
fn gl_only_screen_shows_the_notice_without_gl() {
    let probe = Rc::new(Probe::default());
    let (mut app, _tx) = manager(
        vec![
            Box::new(Plain("plain")),
            check("cube", true, Base::None, &probe),
        ],
        "plain",
        false,
    );
    assert!(app.show_screen("cube"), "手動では選べる");
    assert_eq!(app.screen_id(), "cube");
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Canvas));
    let mut expected = Canvas::new(SIZE.0, SIZE.1);
    notice::gpu_required(&mut expected, &mut text_engine(""), "CHECK");
    assert_eq!(c.pixmap.data(), expected.pixmap.data(), "案内が完成画面");
    assert_eq!((probe.renders.get(), probe.composites.get()), (1, 0));
    let mut cpu = Canvas::new(SIZE.0, SIZE.1);
    app.render(&mut cpu);
    assert_eq!(
        cpu.pixmap.data(),
        expected.pixmap.data(),
        "CPU 経路も同じ案内"
    );
    // 案内は動かない画面: 描き直しを求めない(scene_animating を足さない)
    probe.scene_animating.set(true);
    let t = Instant::now();
    app.tick(t);
    assert!(!app.tick(t) && !app.animating());

    let (mut app, _tx) = manager(vec![check("cube", true, Base::None, &probe)], "cube", true);
    assert!(matches!(app.render_composite(&mut c), Composite::Gl(_)));
}

/// 3D だけの画面が失敗したら、同じ案内になる。
#[test]
fn gl_only_screen_shows_the_notice_after_failure() {
    let probe = Rc::new(Probe::default());
    let (mut app, _tx) = manager(vec![check("cube", true, Base::None, &probe)], "cube", true);
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Gl(_)));
    app.report_scene_failure("試験");
    assert!(matches!(app.render_composite(&mut c), Composite::Canvas));
    let mut expected = Canvas::new(SIZE.0, SIZE.1);
    notice::gpu_required(&mut expected, &mut text_engine(""), "CHECK");
    assert_eq!(c.pixmap.data(), expected.pixmap.data());
}

fn step(screen: &str, secs: u64) -> RotationStep {
    RotationStep {
        screen: screen.into(),
        secs,
        skip_if_empty: false,
    }
}

fn run_for(app: &mut ScreenManager, t: &mut Instant, secs: u64) -> &'static str {
    for _ in 0..secs {
        *t += Duration::from_secs(1);
        app.tick(*t);
    }
    app.screen_id()
}

/// 自動巡回は、GL が使えない・3D が失敗した 3D だけの画面を飛ばす(`skip_if_empty` に関係なく)。
/// 装飾(`needs_gl() = false`)は、GL が使えなくても巡回に残す。
#[test]
fn rotation_skips_gl_only_screens_without_gl() {
    let build = |gl: bool, steps: &[RotationStep]| {
        let probe = Rc::new(Probe::default());
        let (app, _tx) = manager(
            vec![
                Box::new(Plain("a")),
                check("cube", true, Base::None, &probe),
                check("deco", false, Base::Canvas, &probe),
                Box::new(Plain("b")),
            ],
            "a",
            gl,
        );
        app.with_rotation(steps, Duration::from_secs(60))
    };
    let steps = [
        step("a", 10),
        step("cube", 10),
        step("deco", 10),
        step("b", 10),
    ];

    let mut app = build(true, &steps);
    let mut t = Instant::now();
    assert_eq!(run_for(&mut app, &mut t, 10), "cube");
    assert_eq!(run_for(&mut app, &mut t, 10), "deco");

    let mut app = build(false, &steps);
    let mut t = Instant::now();
    assert_eq!(app.screen_id(), "a");
    assert_eq!(
        run_for(&mut app, &mut t, 10),
        "deco",
        "cube は飛ばす。装飾は残す"
    );
    assert_eq!(run_for(&mut app, &mut t, 10), "b");
    assert_eq!(run_for(&mut app, &mut t, 10), "a");

    // 最初の手順が飛ばす画面なら、次の手順から始める
    let app = build(false, &[step("cube", 10), step("b", 10)]);
    assert_eq!(app.screen_id(), "b");
    let app = build(true, &[step("cube", 10), step("b", 10)]);
    assert_eq!(app.screen_id(), "cube");

    // 失敗した 3D だけの画面も飛ばす(出していたその場面は、時間が来るまで案内のまま)
    let mut app = build(true, &steps);
    let mut t = Instant::now();
    assert_eq!(run_for(&mut app, &mut t, 10), "cube");
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Gl(_)));
    app.report_scene_failure("試験");
    assert_eq!(run_for(&mut app, &mut t, 10), "deco");
    assert_eq!(run_for(&mut app, &mut t, 10), "b");
    assert_eq!(run_for(&mut app, &mut t, 10), "a");
    assert_eq!(run_for(&mut app, &mut t, 10), "deco", "cube は戻らない");
}

/// 巡回の手順がすべて GL が要る画面で GL が使えないときは、案内の画面に切り替えず、今の画面のまま巡回しない。
/// (手動で GL が要る画面を選んだときの案内は、そのまま出る)
#[test]
fn rotation_does_not_start_when_every_step_needs_gl() {
    let probe = Rc::new(Probe::default());
    let (app, _tx) = manager(
        vec![
            Box::new(Plain("a")),
            check("cube", true, Base::None, &probe),
            check("cube2", true, Base::None, &probe),
        ],
        "a",
        false,
    );
    let mut app = app.with_rotation(
        &[step("cube", 10), step("cube2", 10)],
        Duration::from_secs(60),
    );
    assert_eq!(app.screen_id(), "a", "今の画面のまま");
    assert_eq!(app.rotation_state(), None, "巡回を始めない");
    let mut t = Instant::now();
    assert_eq!(run_for(&mut app, &mut t, 30), "a");

    // 手動で選んだときの案内は変わらない
    assert!(app.show_screen("cube"));
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Canvas));

    // GL が使えるなら、先頭から巡回する
    let (app, _tx) = manager(
        vec![
            Box::new(Plain("a")),
            check("cube", true, Base::None, &probe),
        ],
        "a",
        true,
    );
    let app = app.with_rotation(&[step("cube", 10)], Duration::from_secs(60));
    assert_eq!(app.screen_id(), "cube");
    assert_eq!(app.rotation_state(), Some("on"));
}

/// 終了時の解放用に、全画面の `GlScene` を集められる。
#[test]
fn gl_scenes_collects_every_scene() {
    let probe = Rc::new(Probe::default());
    let (mut app, _tx) = manager(
        vec![
            check("a", true, Base::None, &probe),
            Box::new(Plain("plain")),
            check("b", false, Base::Canvas, &probe),
        ],
        "plain",
        true,
    );
    assert_eq!(App::gl_scenes(&mut app).len(), 2);
}

/// 3D が失敗した画面の場面は、その場で `release` 済みなので、終了時の解放の対象から外す。
#[test]
fn gl_scenes_skips_the_failed_screen() {
    let probe = Rc::new(Probe::default());
    let (mut app, _tx) = manager(
        vec![
            check("a", true, Base::None, &probe),
            Box::new(Plain("plain")),
            check("b", false, Base::Canvas, &probe),
        ],
        "a",
        true,
    );
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    assert!(matches!(app.render_composite(&mut c), Composite::Gl(_)));
    app.report_scene_failure("試験");
    assert_eq!(App::gl_scenes(&mut app).len(), 1, "a を外し、b だけ");
}

/// `Composite::Gl` の下地に `Gl` は入れられない(呼び出し側の誤り)。デバッグビルドでは検出する。
#[cfg(all(target_os = "linux", debug_assertions))]
#[test]
#[should_panic(expected = "Gl は入れられない")]
fn nested_gl_is_detected() {
    use crate::runtime::{FrameParts, scene_frame};
    use crate::scene_check::CheckScene;
    use crate::screen::GlComposite;
    let (mut s1, mut s2) = (
        CheckScene::new(Rc::default()),
        CheckScene::new(Rc::default()),
    );
    let region = CheckScreen::region(SIZE);
    let inner = Composite::Gl(GlComposite {
        base: None,
        scene: &mut s2,
        region,
        top: Top::Off,
    });
    let outer = GlComposite {
        base: Some(Box::new(inner)),
        scene: &mut s1,
        region,
        top: Top::Off,
    };
    let canvas = Canvas::new(SIZE.0, SIZE.1);
    let parts = FrameParts::default();
    let _ = scene_frame(outer, &canvas, &parts);
}

#[cfg(target_os = "linux")]
mod gpu {
    use super::*;
    use crate::runtime::{
        FrameParts, FrameSink, GlesSurface, draw_frame, draw_gles, end_run, frame_of, scene_frame,
    };
    use crate::scene_check::{BACK_COLOR, CheckScreen, FRONT_COLOR};
    use crate::screen::App;
    use miharashi_gpu::compose::Compositor;
    use miharashi_gpu::{
        Frame, Offscreen, SceneError, SceneFrame, SceneOutcome, read_rgba_top_down,
    };
    use miharashi_render::menu;
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

    /// `Composite` を 1 フレームだけ、新しい Compositor で描いて読み戻す。`Gl` 以外は `present_frame` と同じ `Frame` で描く。
    fn draw_once(off: &Offscreen, app: &mut ScreenManager) -> (Option<SceneOutcome>, Vec<u8>) {
        let mut c = Canvas::new(SIZE.0, SIZE.1);
        let comp = app.render_composite(&mut c);
        let mut parts = FrameParts::default();
        parts.fill(&comp);
        match comp {
            Composite::Gl(g) => {
                let mut sf = scene_frame(g, &c, &parts);
                let (outcome, img) = off.render_scene(&mut sf, SIZE).unwrap();
                (Some(outcome), img)
            }
            other => {
                let frame = frame_of(&other, &c, &parts).unwrap();
                (None, off.render_frames(&[frame], SIZE).unwrap())
            }
        }
    }

    fn px(img: &[u8], (x, y): (u32, u32)) -> [u8; 4] {
        let i = ((y * SIZE.0 + x) * 4) as usize;
        img[i..i + 4].try_into().unwrap()
    }

    /// 領域の中の相対位置(0〜1)の画素。
    fn at(img: &[u8], fx: f32, fy: f32) -> [u8; 4] {
        let r = CheckScreen::region(SIZE);
        px(
            img,
            (
                r.x + (r.w as f32 * fx) as u32,
                r.y + (r.h as f32 * fy) as u32,
            ),
        )
    }

    /// 上(premultiplied)を下に重ねる(`ONE, ONE_MINUS_SRC_ALPHA`)。
    fn over(top: &[u8], below: &[u8]) -> Vec<u8> {
        top.as_chunks::<4>()
            .0
            .iter()
            .zip(below.as_chunks::<4>().0.iter())
            .flat_map(|(t, b)| {
                let inv = 255 - t[3] as u32;
                (0..4).map(move |i| (t[i] as u32 + (b[i] as u32 * inv + 127) / 255).min(255) as u8)
            })
            .collect()
    }

    fn assert_close(a: &[u8], b: &[u8], what: &str) {
        let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        assert!(worst <= 2, "{what}: 最大差 {worst}");
    }

    /// 3D だけの画面(下地なし)は黒の上に 3D。深度テストが効いて重なりは手前の赤。
    #[test]
    fn gl_only_screen_draws_the_scene_over_black() {
        let Some(off) = offscreen() else { return };
        let probe = Rc::new(Probe::default());
        let (mut app, _tx) = manager(vec![check("cube", true, Base::None, &probe)], "cube", true);
        let (outcome, img) = draw_once(&off, &mut app);
        assert_eq!(outcome, Some(SceneOutcome::Drawn));
        assert_eq!(px(&img, (0, 0)), [0, 0, 0, 255], "領域の外は黒");
        assert_eq!(at(&img, 0.5, 0.5), FRONT_COLOR, "重なりは手前(深度テスト)");
        assert_eq!(at(&img, 0.8, 0.5), BACK_COLOR);
        assert_eq!(
            at(&img, 0.5, 0.05),
            [0, 0, 0, 255],
            "領域の中でも四角の外は下地(黒)"
        );
    }

    /// 1 フレームが 下地 → 3D → アラート(メニュー)の順で出る。アラートの下に 3D が見える(アルファ)。
    #[test]
    fn frame_is_base_then_scene_then_menu() {
        let Some(off) = offscreen() else { return };
        let probe = Rc::new(Probe::default());
        let (mut app, _tx) = manager(
            vec![check("check", false, Base::Canvas, &probe)],
            "check",
            true,
        );
        let (outcome, plain) = draw_once(&off, &mut app);
        assert_eq!(outcome, Some(SceneOutcome::Drawn));
        assert_eq!(px(&plain, (0, 0)), BASE_COLOR, "領域の外は下地");
        assert_eq!(at(&plain, 0.5, 0.5), FRONT_COLOR);
        assert_eq!(at(&plain, 0.8, 0.5), BACK_COLOR);
        assert_eq!(at(&plain, 0.5, 0.05), BASE_COLOR, "3D の透明な所は下地");

        app.on_key(KeyPress::Menu);
        let (outcome, with_menu) = draw_once(&off, &mut app);
        assert_eq!(outcome, Some(SceneOutcome::Drawn));
        let mut layer = Canvas::new(SIZE.0, SIZE.1);
        layer.clear(rgba(0, 0, 0, 0));
        menu::menu(&mut layer, &mut text_engine(""), &["CHECK"], 0, 0);
        let want = over(layer.pixmap.data(), &plain);
        assert_close(&with_menu, &want, "下地 → 3D → メニュー");
        let alpha = |fx, fy| at(layer.pixmap.data(), fx, fy)[3];
        assert!(
            (1..255).contains(&alpha(0.5, 0.5)),
            "メニューは半透明で、下の 3D が見える(試験の前提)"
        );
        assert_ne!(
            at(&with_menu, 0.5, 0.5),
            FRONT_COLOR,
            "3D の手前にメニューが出る"
        );
        assert_ne!(
            at(&with_menu, 0.5, 0.5),
            at(layer.pixmap.data(), 0.5, 0.5),
            "3D が透けて見える"
        );
    }

    /// 最上段の層の `Upload` / `Keep` / `Off` を、1 つの Compositor に続けて渡しても、期待どおりの絵になる。
    /// `Off` のあとは `Upload` に戻るので、古い重ね絵が残らない。
    #[test]
    fn top_layer_follows_upload_keep_off_on_one_compositor() {
        let Some(off) = offscreen() else { return };
        let probe = Rc::new(Probe::default());
        let (mut app, _tx) = manager(
            vec![check("check", false, Base::Canvas, &probe)],
            "check",
            true,
        );
        let (mut kinds, mut images) = (Vec::new(), Vec::new());
        let keys = [
            Some(KeyPress::Menu), // upload
            None,                 // keep
            Some(KeyPress::Menu), // off(閉じる)
            Some(KeyPress::Menu), // upload(開く)
            None,                 // keep
        ];
        off.with_target(SIZE, |gl, fbo| {
            let mut comp = Compositor::new(gl, SIZE).unwrap();
            let mut scratch = Vec::new();
            for key in keys {
                if let Some(k) = key {
                    app.on_key(k);
                }
                let mut c = Canvas::new(SIZE.0, SIZE.1);
                let comp_frame = app.render_composite(&mut c);
                kinds.push(top_kind(&comp_frame));
                let mut parts = FrameParts::default();
                parts.fill(&comp_frame);
                let Composite::Gl(g) = comp_frame else {
                    panic!("Gl")
                };
                let mut sf = scene_frame(g, &c, &parts);
                let outcome = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
                assert_eq!(outcome, SceneOutcome::Drawn);
                let mut img = vec![0; (SIZE.0 * SIZE.1 * 4) as usize];
                read_rgba_top_down(gl, fbo, SIZE, &mut img, &mut scratch);
                images.push(img);
            }
            comp.destroy(gl);
        })
        .unwrap();
        assert_eq!(kinds, ["upload", "keep", "off", "upload", "keep"]);
        assert_eq!(images[1], images[0], "Keep は前回の重ね絵のまま");
        assert_ne!(images[2], images[0], "Off で重ね絵が消える");
        assert_eq!(at(&images[2], 0.5, 0.5), FRONT_COLOR);
        assert_eq!(images[3], images[0], "Off の次は Upload し直す");
        assert_eq!(images[4], images[0]);
    }

    /// 描画の失敗 → 画面の切り替え → 終了、まで通しても、失敗した場面の `release` は 1 回だけ
    /// (失敗したその場で呼ばれ、終了時には呼ばれない)。ほかの場面は終了時に 1 回ずつ。
    #[test]
    fn failed_scene_is_released_exactly_once_through_exit() {
        let Some(off) = offscreen() else { return };
        let (a, b) = (Rc::new(Probe::default()), Rc::new(Probe::default()));
        let (mut app, _tx) = manager(
            vec![
                check("a", false, Base::Canvas, &a),
                check("b", false, Base::Canvas, &b),
            ],
            "a",
            true,
        );
        a.fail_draw.set(true);
        off.with_target(SIZE, |gl, fbo| {
            let mut comp = Compositor::new(gl, SIZE).unwrap();
            let mut c = Canvas::new(SIZE.0, SIZE.1);
            let comp_frame = app.render_composite(&mut c);
            let mut parts = FrameParts::default();
            parts.fill(&comp_frame);
            let Composite::Gl(g) = comp_frame else {
                panic!("Gl")
            };
            let mut sf = scene_frame(g, &c, &parts);
            let outcome = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
            let SceneOutcome::SceneFailed(msg) = outcome else {
                panic!("3D が失敗する: {outcome:?}")
            };
            app.report_scene_failure(&msg);
            assert_eq!(
                (a.draws.get(), a.releases.get()),
                (1, 1),
                "その場で release"
            );
            app.on_key(KeyPress::Next); // 失敗した画面から切り替える
            end_run(None, &mut app, |scenes| {
                Compositor::release_scenes(gl, SIZE, Some(fbo), scenes)
            })
            .unwrap();
            comp.destroy(gl);
        })
        .unwrap();
        assert_eq!(a.releases.get(), 1, "失敗した場面は終了時に呼ばない");
        assert_eq!(b.releases.get(), 1, "ほかの場面は終了時に 1 回");
    }

    /// 表示を差し替えた実行ループ用の表示。本物の Compositor(オフスクリーン)で描き、`GlesDisplay::present_scene` と
    /// 同じく、描いたあとに `flip_err` があれば(swap・page flip の失敗)場面の失敗を持たせた `Err` にする。
    struct FakeDisplay<'a> {
        gl: &'a glow::Context,
        comp: &'a mut Compositor,
        target: Option<glow::Framebuffer>,
        flip_err: Option<&'static str>,
    }

    impl GlesSurface for FakeDisplay<'_> {
        fn present_scene(&mut self, f: &mut SceneFrame<'_>) -> Result<SceneOutcome, SceneError> {
            let outcome = self.comp.draw_scene(self.gl, self.target, f)?;
            match self.flip_err.take() {
                Some(e) => Err(SceneError::after(&outcome, e.into())),
                None => Ok(outcome),
            }
        }

        fn present_frame(&mut self, _: &Frame<'_>) -> Result<(), String> {
            unreachable!("3D の画面しか使わない")
        }
    }

    impl FrameSink for FakeDisplay<'_> {
        fn draw(&mut self, app: &mut dyn App, canvas: &mut Canvas) -> Result<(f32, f32), String> {
            draw_gles(self, app, canvas)
        }
    }

    /// 実行ループの 1 枚が表示の致命的なエラー(swap・page flip)で失敗しても、ループは `draw_frame` から抜け、
    /// 場面の失敗(`release` 済み)を反映してから終了処理に入る。解放は場面ごとに 1 回で、元のエラーを返す。
    /// 場面が先に失敗していたなら、終了時に `release` し直さない(二重に呼ばない)。
    #[test]
    fn display_error_after_a_scene_failure_still_releases_once() {
        let Some(off) = offscreen() else { return };
        for scene_fails in [false, true] {
            let probe = Rc::new(Probe::default());
            let (mut app, _tx) = manager(
                vec![check("check", false, Base::Canvas, &probe)],
                "check",
                true,
            );
            probe.fail_draw.set(scene_fails);
            let result = off
                .with_target(SIZE, |gl, fbo| {
                    let mut comp = Compositor::new(gl, SIZE).unwrap();
                    let mut display = FakeDisplay {
                        gl,
                        comp: &mut comp,
                        target: Some(fbo),
                        flip_err: Some("page flip に失敗"),
                    };
                    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
                    let mut run_err = None;
                    let times = draw_frame(&mut display, &mut app, &mut canvas, &mut run_err);
                    assert!(times.is_none(), "描画エラーならループを抜ける");
                    assert_eq!(run_err.as_deref(), Some("page flip に失敗"));
                    assert_eq!(
                        probe.releases.get(),
                        u32::from(scene_fails),
                        "失敗した場面はその場で release"
                    );
                    assert_eq!(
                        App::gl_scenes(&mut app).len(),
                        usize::from(!scene_fails),
                        "release 済みの場面は終了時の解放の対象から外れる"
                    );
                    let result = end_run(run_err, &mut app, |scenes| {
                        Compositor::release_scenes(gl, SIZE, Some(fbo), scenes)
                    });
                    comp.destroy(gl);
                    result
                })
                .unwrap();
            assert_eq!(result, Err("page flip に失敗".to_string()));
            assert_eq!(
                probe.releases.get(),
                1,
                "scene_fails={scene_fails}: 場面は 1 回だけ release する"
            );
        }
    }

    /// アラート表示中に 3D が失敗しても、そのフレームも次のフレームからも、アラートが見え続ける。
    /// 失敗した場面は `release` され、その後 `draw` は呼ばれない。
    #[test]
    fn alert_stays_visible_when_the_scene_fails() {
        let Some(off) = offscreen() else { return };
        let probe = Rc::new(Probe::default());
        let (mut app, tx) = manager(
            vec![check("check", false, Base::Canvas, &probe)],
            "check",
            true,
        );
        tx.send(quake()).unwrap();
        app.tick(Instant::now());
        probe.fail_draw.set(true);
        let (outcome, failing) = draw_once(&off, &mut app);
        let Some(SceneOutcome::SceneFailed(msg)) = outcome else {
            panic!("3D が失敗する: {outcome:?}")
        };
        app.report_scene_failure(&msg);
        assert_eq!(
            (probe.draws.get(), probe.releases.get()),
            (1, 1),
            "その場で release"
        );

        let mut base = Canvas::new(SIZE.0, SIZE.1);
        base.clear(rgba(BASE_COLOR[0], BASE_COLOR[1], BASE_COLOR[2], 255));
        let want = over(alert_layer(&quake()).pixmap.data(), base.pixmap.data());
        assert_close(
            &failing,
            &want,
            "失敗したフレーム: 下地とアラートで完成する",
        );
        for n in 0..3 {
            let (outcome, img) = draw_once(&off, &mut app);
            assert_eq!(outcome, None, "次のフレームからは 3D 抜き");
            assert_close(&img, &want, &format!("{n} 枚目: アラートが見え続ける"));
        }
        assert_eq!(
            (probe.draws.get(), probe.releases.get()),
            (1, 1),
            "もう draw しない"
        );
    }
}
