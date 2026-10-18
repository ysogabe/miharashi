//! キューブ画面と、状態を汚す試験画面のテスト(Issue #74 T3、docs/DESIGN-screen-plugin-v2.md §3.3・§5.5・§10)。
//! 時計・間隔・設定のテストは GPU なしで動く。描画のテストは `gpu` モジュール(GPU がなければ skip)。
use super::manager::ScreenManager;
use super::screens::cube::CubeScreen;
use super::text_engine;
use crate::data::{DummyData, dummy_snapshot};
use crate::screen::{App, Screen, World};
use std::time::{Duration, Instant};

const SIZE: (u32, u32) = (480, 270);

fn manager(screens: Vec<Box<dyn Screen>>, start: &str, gl: bool) -> ScreenManager {
    ScreenManager::new(
        screens,
        start,
        Box::new(DummyData::new()),
        dummy_snapshot(0.0),
        text_engine(""),
        Duration::from_secs(60),
    )
    .with_gl_available(gl)
}

/// 回転の時間は、表示中かつ点灯中に経った分だけ進む。非表示・消灯の間は止まり、復帰でまとめて進まない。
#[test]
fn rotation_time_advances_only_while_visible_and_on() {
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);
    let w = World::default();
    let mut s = CubeScreen::new(false, 30);
    s.set_presentation(false, false, true, t0); // 非表示
    s.tick(&w, at(10));
    assert_eq!(s.active(), Duration::ZERO);
    assert!(!s.scene_animating());

    s.set_presentation(true, false, true, at(10)); // 表示
    assert!(s.scene_animating());
    s.tick(&w, at(12));
    assert_eq!(s.active(), Duration::from_secs(2));

    // アラート・メニューが上にあっても、下の 3D は動き続ける
    s.set_presentation(true, true, true, at(13));
    assert_eq!(s.active(), Duration::from_secs(3));

    // 消灯
    s.set_presentation(true, false, false, at(14));
    assert_eq!(s.active(), Duration::from_secs(4));
    assert!(!s.scene_animating());
    s.tick(&w, at(100));
    assert_eq!(s.active(), Duration::from_secs(4), "消灯中は止まる");

    // 復帰: 溜まった分を一度に進めない
    s.set_presentation(true, false, true, at(100));
    assert_eq!(s.active(), Duration::from_secs(4), "復帰で跳ばない");
    s.tick(&w, at(101));
    assert_eq!(s.active(), Duration::from_secs(5));

    // 非表示に戻っても止まる
    s.set_presentation(false, false, true, at(102));
    s.tick(&w, at(500));
    assert_eq!(s.active(), Duration::from_secs(6));
    s.set_presentation(true, false, true, at(500));
    s.tick(&w, at(500));
    assert_eq!(s.active(), Duration::from_secs(6), "非表示の間は進まない");
}

/// ScreenManager を通しても、消灯・非表示では動き続けず、`frame_interval` は動いているキューブの間だけ。
#[test]
fn manager_animates_and_limits_frames_only_while_the_cube_is_shown_and_on() {
    struct Plain;
    impl Screen for Plain {
        fn id(&self) -> crate::screen::ScreenId {
            "plain"
        }
        fn title(&self) -> &'static str {
            "PLAIN"
        }
        fn render(&mut self, _: &mut crate::screen::RenderContext<'_>) {}
    }
    let mut app = manager(
        vec![Box::new(Plain), Box::new(CubeScreen::new(false, 30))],
        "cube",
        true,
    );
    // 最初の tick で 1 秒ごとの更新を済ませておく(以降は 1 秒以内なので、時計の更新で再描画にならない)
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    app.tick(t0);
    assert!(App::animating(&app));
    assert!(app.tick(at(2)), "動いている 3D は毎周期描き直す");
    let interval = App::frame_interval(&app).expect("キューブは間隔の上限を持つ");
    assert!(
        (interval.as_secs_f64() - 1.0 / 30.0).abs() < 1e-9,
        "{interval:?}"
    );

    app.set_display_on(false, at(3));
    assert!(!App::animating(&app), "消灯中は動かさない");
    assert_eq!(App::frame_interval(&app), None);
    assert!(!app.tick(at(400)), "消灯中は再描画を要求しない");
    app.set_display_on(true, at(450));
    assert!(App::animating(&app));

    app.on_key(crate::screen::KeyPress::Prev); // 普通の画面へ
    app.tick(at(460));
    assert!(
        !App::animating(&app),
        "キューブが見えていなければ動かさない"
    );
    assert_eq!(App::frame_interval(&app), None, "ほかの画面は vsync のまま");

    // GL が使えなければ 3D は動かさず、間隔の上限も要らない
    let mut cpu = manager(vec![Box::new(CubeScreen::new(false, 30))], "cube", false);
    cpu.tick(at(1));
    assert!(!App::animating(&cpu));
    assert_eq!(App::frame_interval(&cpu), None);

    // fps = 0 は制限なし(vsync だけ)
    let mut free = manager(vec![Box::new(CubeScreen::new(true, 0))], "cube-third", true);
    free.tick(at(1));
    assert!(App::animating(&free));
    assert_eq!(App::frame_interval(&free), None);
}

/// 状態ログの `gl_objs` は、GL の場面を持つ画面があるときだけ出る(既存の並びは変えない)。
#[test]
fn stats_show_gl_objs_only_with_gl_scenes() {
    let plain = manager(
        vec![Box::new(crate::app::screens::status::StatusScreen::new())],
        "status",
        true,
    );
    assert!(!plain.stats().iter().any(|(k, _)| *k == "gl_objs"));
    let cube = manager(vec![Box::new(CubeScreen::new(false, 30))], "cube", true);
    assert!(
        cube.stats().contains(&("gl_objs", "0".to_string())),
        "まだ何も作っていない"
    );
}

/// 本番の既定の設定ではキューブも試験画面も登録しない。有効にすると、設定のとおりの画面が並ぶ。
#[test]
fn cube_and_dirty_screens_are_registered_only_when_enabled() {
    use crate::config::Config;
    use crate::photos::Source;
    let ids = |cfg: &Config| {
        let (dir, p) = super::tests::test_photo();
        let app = super::kiosk(
            cfg,
            SIZE,
            Some(4096),
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            None,
        );
        let _ = std::fs::remove_dir_all(dir);
        app.panel_view()
            .unwrap()
            .screens
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&Config::default()), ["status", "photo"]);
    let cfg = Config::parse("[cube]\nenabled = true\nlayout = \"third\"\nfps = 60\n").unwrap();
    assert_eq!(ids(&cfg), ["status", "photo", "cube-third"]);
    let cfg = Config::parse("[cube]\nenabled = true\n[gl_check]\nenabled = true\n").unwrap();
    assert_eq!(
        ids(&cfg),
        [
            "status",
            "photo",
            "cube",
            "cube-third",
            "dirty-a",
            "dirty-b"
        ]
    );
}

#[cfg(target_os = "linux")]
mod gpu {
    use super::*;
    use crate::agent_demo;
    use crate::aircraft;
    use crate::app::screens::agents::AgentScreen;
    use crate::app::screens::aircraft::AircraftScreen;
    use crate::app::screens::cube::{FACE_COLORS, THIRD_BAND, THIRD_BASE};
    use crate::app::screens::gl_check::{Dirt, DirtyScreen};
    use crate::app::screens::photo::PhotoScreen;
    use crate::config::Config;
    use crate::photos::Source;
    use crate::runtime::{FrameParts, frame_of, scene_frame};
    use crate::screen::{Composite, Provider, RenderContext};
    use glow::HasContext;
    use miharashi_gpu::compose::Compositor;
    use miharashi_gpu::{Offscreen, SceneOutcome, diff_rgb, read_rgba_top_down};
    use miharashi_render::Canvas;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn offscreen() -> Option<Offscreen> {
        match Offscreen::open(Path::new("/dev/dri/renderD128")) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                None
            }
        }
    }

    fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE.0 + x) * 4) as usize;
        img[i..i + 4].try_into().unwrap()
    }

    fn face(i: usize) -> [u8; 4] {
        let c = FACE_COLORS[i];
        [c[0], c[1], c[2], 255]
    }

    /// 画面 1 つを `render_composite` して、新しい Compositor で描き、読み戻す(3D の画面だけ)。
    fn draw_screen(off: &Offscreen, screen: &mut dyn Screen) -> Vec<u8> {
        let w = World::default();
        let mut text = text_engine("");
        let mut c = Canvas::new(SIZE.0, SIZE.1);
        let mut ctx = RenderContext {
            canvas: &mut c,
            text: &mut text,
            world: &w,
            covered: false,
            force_overlay: false,
        };
        let comp = screen.render_composite(&mut ctx);
        let mut parts = FrameParts::default();
        parts.fill(&comp);
        let Composite::Gl(g) = comp else {
            panic!("Composite::Gl ではない")
        };
        let mut sf = scene_frame(g, &c, &parts);
        let (outcome, img) = off.render_scene(&mut sf, SIZE).unwrap();
        assert_eq!(outcome, SceneOutcome::Drawn);
        img
    }

    fn shown_cube(third: bool) -> CubeScreen {
        let mut s = CubeScreen::new(third, 30);
        s.set_presentation(true, false, true, Instant::now());
        s
    }

    /// 3D が占める画素の数(下地と違う画素)。
    fn painted(img: &[u8], base: [u8; 4]) -> usize {
        img.as_chunks::<4>()
            .0
            .iter()
            .filter(|p| **p != base)
            .count()
    }

    /// 全面: 手前の面(+Z)が中央に見え、領域の隅は下地(黒)のまま。深度テストを外すと、あとから描く奥の面(-Z)が前に出る。
    #[test]
    fn full_cube_is_depth_tested_over_black() {
        let Some(off) = offscreen() else { return };
        let img = draw_screen(&off, &mut shown_cube(false));
        assert_eq!(
            px(&img, SIZE.0 / 2, SIZE.1 / 2),
            face(4),
            "中央は手前の +Z 面"
        );
        for (x, y) in [
            (0, 0),
            (SIZE.0 - 1, 0),
            (0, SIZE.1 - 1),
            (SIZE.0 - 1, SIZE.1 - 1),
        ] {
            assert_eq!(px(&img, x, y), [0, 0, 0, 255], "({x},{y}) は下地の黒");
        }
        let n = painted(&img, [0, 0, 0, 255]);
        assert!(
            n > (SIZE.0 * SIZE.1) as usize / 20 && n < (SIZE.0 * SIZE.1) as usize / 2,
            "立方体が画面の一部を占める: {n}"
        );

        let mut flat = CubeScreen::new(false, 30).without_depth();
        flat.set_presentation(true, false, true, Instant::now());
        let img = draw_screen(&off, &mut flat);
        assert_eq!(
            px(&img, SIZE.0 / 2, SIZE.1 / 2),
            face(5),
            "深度テストが無ければ、あとから描く奥の面が前に出る(試験の前提)"
        );
    }

    /// 1/3 幅: 領域の外は単色の下地のまま、領域の中に立方体が出る。
    #[test]
    fn third_cube_stays_inside_its_region_over_the_base() {
        let Some(off) = offscreen() else { return };
        let img = draw_screen(&off, &mut shown_cube(true));
        let r = CubeScreen::region(true, SIZE);
        assert_eq!((r.w, r.h), (SIZE.0 / 3, SIZE.1));
        assert_eq!(px(&img, SIZE.0 / 2, SIZE.1 / 2), face(4));
        let mut inside = 0;
        for y in 0..SIZE.1 {
            for x in 0..SIZE.0 {
                let p = px(&img, x, y);
                if x < r.x || x >= r.x + r.w {
                    assert_eq!(p, THIRD_BASE, "領域の外 ({x},{y}) は下地のまま");
                } else if p != THIRD_BAND {
                    inside += 1;
                }
            }
        }
        assert!(inside > 1000, "領域の中に立方体が出る: {inside}");
        assert_eq!(
            px(&img, r.x, 0),
            THIRD_BAND,
            "領域の隅は透けて、領域の帯の色"
        );
        // 全面とは別の絵になる(領域の幅に合わせて小さく出る)
        let full = draw_screen(&off, &mut shown_cube(false));
        assert!(painted(&full, [0, 0, 0, 255]) > inside);
    }

    /// 同じ時刻なら同じ絵(決定的)。時間が進めば角度が変わる。消灯 → 復帰で角度が跳ばない。非表示の間は進まない。
    #[test]
    fn same_time_gives_the_same_picture() {
        let Some(off) = offscreen() else { return };
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let w = World::default();
        let mut a = CubeScreen::new(false, 30);
        let mut b = CubeScreen::new(false, 30);
        for s in [&mut a, &mut b] {
            s.set_presentation(true, false, true, t0);
            s.tick(&w, at(1500));
        }
        let (ia, ib) = (draw_screen(&off, &mut a), draw_screen(&off, &mut b));
        assert_eq!(diff_rgb(&ia, &ib, 0), (0, 0), "同じ時間なら同じ絵");
        let start = draw_screen(&off, &mut shown_cube(false));
        assert_ne!(diff_rgb(&ia, &start, 0), (0, 0), "時間が進むと角度が変わる");

        // 消灯してずっとあと(復帰)でも、消灯の間の分は進まない
        a.set_presentation(true, false, false, at(2000));
        a.tick(&w, at(600_000));
        a.set_presentation(true, false, true, at(600_000));
        b.set_presentation(true, false, true, at(2000));
        b.tick(&w, at(2000));
        let (ia, ib) = (draw_screen(&off, &mut a), draw_screen(&off, &mut b));
        assert_eq!(diff_rgb(&ia, &ib, 0), (0, 0), "復帰で角度が跳ばない");

        // 非表示の間は進まない
        a.set_presentation(false, false, true, at(600_000));
        a.tick(&w, at(900_000));
        a.set_presentation(true, false, true, at(900_000));
        let ia = draw_screen(&off, &mut a);
        assert_eq!(diff_rgb(&ia, &ib, 0), (0, 0), "非表示の間に進まない");
    }

    /// 2D → 3D・3D → 2D の検証に使う、実際の 6 つの画面(写真・航空機・Agent・キューブ・試験画面 2 つ)。
    /// 写真が読み込まれるまで待つ。
    fn all_screens() -> (ScreenManager, std::path::PathBuf) {
        struct AgentFeed(crate::agents::AgentSnapshot);
        impl Provider for AgentFeed {
            fn update(&mut self, world: &mut World) {
                world.agents = self.0.clone();
            }
        }
        let (dir, p) = super::super::tests::test_photo();
        let photo = PhotoScreen::new(
            &Config::default(),
            SIZE,
            Some(4096),
            Source::File(p, Some("テスト".into())),
        );
        let feed = photo.feed_provider();
        let shared: aircraft::Shared =
            Arc::new(Mutex::new(Some((Instant::now(), aircraft::demo_view()))));
        let now = Instant::now();
        let wall = std::time::SystemTime::now();
        let (initial, _, _) = agent_demo::fixture_snapshots(wall, now);
        let mut app = manager(
            vec![
                Box::new(photo),
                Box::new(AircraftScreen::new(shared, true)),
                Box::new(AgentScreen::new(SIZE, true, 30)),
                Box::new(CubeScreen::new(false, 30)),
                Box::new(DirtyScreen::new(Dirt::A)),
                Box::new(DirtyScreen::new(Dirt::B)),
            ],
            "photo",
            true,
        )
        .with_provider(Box::new(feed))
        .with_provider(Box::new(AgentFeed(initial)));
        let end = Instant::now() + Duration::from_secs(5);
        while app.panel_view().unwrap().photo.is_none() && Instant::now() < end {
            app.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.panel_view().unwrap().photo.is_some(),
            "写真が読み込まれる"
        );
        (app, dir)
    }

    fn kind(c: &Composite<'_>) -> &'static str {
        match c {
            Composite::Canvas => "canvas",
            Composite::Photo { .. } => "photo",
            Composite::Radar { .. } => "radar",
            Composite::Agents { .. } => "agents",
            Composite::Gl(_) => "gl",
        }
    }

    /// 持続する Compositor に、画面を `ids` の順に出して描く。各フレームの (種類, 画像)。
    /// 2D のフレームは本番の `present_frame` と同じく framebuffer を bind するだけで描く。
    /// キューブの時間が進まないよう、画面を切り替えたあとは `tick` しない(2D の画面は切り替え前に tick 済み)。
    fn run(off: &Offscreen, app: &mut ScreenManager, ids: &[&str]) -> Vec<(&'static str, Vec<u8>)> {
        off.with_target(SIZE, |gl, fbo| {
            let mut comp = Compositor::new(gl, SIZE).unwrap();
            let mut scratch = Vec::new();
            let mut out = Vec::new();
            for id in ids {
                app.show_screen(id);
                let mut c = Canvas::new(SIZE.0, SIZE.1);
                let composite = app.render_composite(&mut c);
                let mut parts = FrameParts::default();
                parts.fill(&composite);
                let k = kind(&composite);
                match composite {
                    Composite::Gl(g) => {
                        let mut sf = scene_frame(g, &c, &parts);
                        let o = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
                        assert_eq!(o, SceneOutcome::Drawn, "{id}");
                    }
                    other => {
                        let frame = frame_of(&other, &c, &parts).unwrap();
                        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo)) };
                        comp.draw(gl, &frame).unwrap();
                    }
                }
                let mut img = vec![0; (SIZE.0 * SIZE.1 * 4) as usize];
                read_rgba_top_down(gl, fbo, SIZE, &mut img, &mut scratch);
                out.push((k, img));
            }
            comp.destroy(gl);
            out
        })
        .unwrap()
    }

    /// 2D の画面(写真・航空機・Agent)を実際に出す前に、1 周期 tick して状態を取り込ませる。
    fn warm(app: &mut ScreenManager) {
        let now = Instant::now();
        for id in ["aircraft", "agents", "photo"] {
            app.show_screen(id);
            app.tick(now);
        }
    }

    /// 2D → 3D: 写真・航空機・Agent の直後にキューブを描いた絵が、キューブだけの絵と一致する。
    #[test]
    fn cube_after_2d_screens_matches_the_cube_alone() {
        let Some(off) = offscreen() else { return };
        let (mut app, dir) = all_screens();
        warm(&mut app);
        let chain = run(&off, &mut app, &["photo", "aircraft", "agents", "cube"]);
        let kinds: Vec<_> = chain.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            ["photo", "radar", "agents", "gl"],
            "実際の画面の描き方"
        );
        let (mut alone, dir2) = all_screens();
        let cube = run(&off, &mut alone, &["cube"]);
        assert_eq!(
            diff_rgb(&chain[3].1, &cube[0].1, 0),
            (0, 0),
            "2D のあとのキューブは、キューブだけと同じ"
        );
        assert_eq!(px(&cube[0].1, SIZE.0 / 2, SIZE.1 / 2), face(4));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    /// 3D → 3D: 状態を汚す 2 種類の試験画面のあとにキューブを描いても、キューブだけの絵と一致する。
    /// 試験画面どうしを続けても、後ろの絵は前の汚れに影響されない。
    #[test]
    fn cube_after_dirty_screens_matches_the_cube_alone() {
        let Some(off) = offscreen() else { return };
        let (mut app, dir) = all_screens();
        let alone = run(&off, &mut app, &["cube"]).remove(0).1;
        for ids in [
            ["dirty-a", "cube"].as_slice(),
            &["dirty-b", "cube"],
            &["dirty-a", "dirty-b", "cube"],
            &["dirty-b", "dirty-a", "dirty-b", "cube"],
        ] {
            let (mut app, d) = all_screens();
            let chain = run(&off, &mut app, ids);
            let last = &chain.last().unwrap().1;
            assert_eq!(diff_rgb(last, &alone, 0), (0, 0), "{ids:?}");
            let _ = std::fs::remove_dir_all(d);
        }
        // 試験画面どうし(後ろの絵が前の汚れに影響されない)
        for (first, second) in [("dirty-a", "dirty-b"), ("dirty-b", "dirty-a")] {
            let (mut app, d) = all_screens();
            let chain = run(&off, &mut app, &[first, second]);
            let (mut app2, d2) = all_screens();
            let single = run(&off, &mut app2, &[second]);
            assert_eq!(
                diff_rgb(&chain[1].1, &single[0].1, 0),
                (0, 0),
                "{first} → {second}"
            );
            let _ = std::fs::remove_dir_all(d);
            let _ = std::fs::remove_dir_all(d2);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 3D の画面 1 つを、持続する Compositor に描く(`draw_scene`)。
    fn draw_3d(
        gl: &glow::Context,
        comp: &mut Compositor,
        fbo: glow::Framebuffer,
        s: &mut dyn Screen,
    ) {
        let w = World::default();
        let mut text = text_engine("");
        let mut c = Canvas::new(SIZE.0, SIZE.1);
        let mut ctx = RenderContext {
            canvas: &mut c,
            text: &mut text,
            world: &w,
            covered: false,
            force_overlay: false,
        };
        let composite = s.render_composite(&mut ctx);
        let mut parts = FrameParts::default();
        parts.fill(&composite);
        let Composite::Gl(g) = composite else {
            panic!("Composite::Gl ではない")
        };
        let mut sf = scene_frame(g, &c, &parts);
        let o = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
        assert_eq!(o, SceneOutcome::Drawn, "{}", s.id());
    }

    /// 3D → 2D: 汚す試験画面(とキューブ)の直後に写真・航空機(Radar)・Agent を描いても、汚さなかったときと一致する。
    /// レーダーの回る線のように、画面が時間で変わる絵と比べないよう、同じ `Frame` を新しい Compositor と、3D のあとの
    /// Compositor の両方に描く。
    #[test]
    fn two_d_screens_after_dirty_screens_match_clean_draws() {
        let Some(off) = offscreen() else { return };
        for (id, want) in [
            ("photo", "photo"),
            ("aircraft", "radar"),
            ("agents", "agents"),
        ] {
            let (mut app, dir) = all_screens();
            warm(&mut app);
            app.show_screen(id);
            let mut c = Canvas::new(SIZE.0, SIZE.1);
            let composite = app.render_composite(&mut c);
            assert_eq!(kind(&composite), want, "実際の画面の描き方");
            let mut parts = FrameParts::default();
            parts.fill(&composite);
            let frame = frame_of(&composite, &c, &parts).unwrap();
            let size = (SIZE.0 * SIZE.1 * 4) as usize;
            off.with_target(SIZE, |gl, fbo| {
                let read = |scratch: &mut Vec<u8>| {
                    let mut img = vec![0; size];
                    read_rgba_top_down(gl, fbo, SIZE, &mut img, scratch);
                    img
                };
                let mut scratch = Vec::new();
                let mut clean = Compositor::new(gl, SIZE).unwrap();
                unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo)) };
                clean.draw(gl, &frame).unwrap();
                let want_img = read(&mut scratch);
                clean.destroy(gl);
                let mut after = Compositor::new(gl, SIZE).unwrap();
                let mut screens: Vec<Box<dyn Screen>> = vec![
                    Box::new(DirtyScreen::new(Dirt::A)),
                    Box::new(DirtyScreen::new(Dirt::B)),
                    Box::new(shown_cube(false)),
                    Box::new(shown_cube(true)),
                ];
                for s in screens.iter_mut() {
                    draw_3d(gl, &mut after, fbo, s.as_mut());
                    // 本番の `present_frame` と同じく、framebuffer を bind するだけで 2D を描く
                    unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo)) };
                    after.draw(gl, &frame).unwrap();
                    let got = read(&mut scratch);
                    assert_eq!(
                        diff_rgb(&got, &want_img, 0),
                        (0, 0),
                        "{} → {id} は {id} だけと同じ",
                        s.id()
                    );
                }
                // 2 つ続けて汚したあとでも
                draw_3d(gl, &mut after, fbo, screens[0].as_mut());
                draw_3d(gl, &mut after, fbo, screens[1].as_mut());
                unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo)) };
                after.draw(gl, &frame).unwrap();
                assert_eq!(
                    diff_rgb(&read(&mut scratch), &want_img, 0),
                    (0, 0),
                    "A → B → {id}"
                );
                after.destroy(gl);
            })
            .unwrap();
            drop(composite);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 切り替えを数百回重ねても、場面が作ったオブジェクトの「作成 − 削除」が増えない(作るのは最初の 1 回だけ)。
    /// 状態ログの `gl_objs` にも出る。終了時の解放で 0 に戻る。
    #[test]
    fn switching_many_times_does_not_grow_gl_objects() {
        let Some(off) = offscreen() else { return };
        let (mut app, dir) = all_screens();
        let live = |app: &ScreenManager| -> String {
            app.stats()
                .into_iter()
                .find(|(k, _)| *k == "gl_objs")
                .map(|(_, v)| v)
                .expect("gl_objs が出る")
        };
        assert_eq!(live(&app), "0");
        let ids: Vec<&str> = (0..300)
            .flat_map(|i| {
                [
                    "cube",
                    "photo",
                    if i % 3 == 0 { "dirty-a" } else { "dirty-b" },
                    "photo",
                ]
            })
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        off.with_target(SIZE, |gl, fbo| {
            let mut comp = Compositor::new(gl, SIZE).unwrap();
            for (n, id) in ids.iter().enumerate() {
                app.show_screen(id);
                let mut c = Canvas::new(SIZE.0, SIZE.1);
                let composite = app.render_composite(&mut c);
                let mut parts = FrameParts::default();
                parts.fill(&composite);
                match composite {
                    Composite::Gl(g) => {
                        let mut sf = scene_frame(g, &c, &parts);
                        let o = comp.draw_scene(gl, Some(fbo), &mut sf).unwrap();
                        assert_eq!(o, SceneOutcome::Drawn, "{n} 回目 {id}");
                    }
                    other => {
                        let frame = frame_of(&other, &c, &parts).unwrap();
                        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo)) };
                        comp.draw(gl, &frame).unwrap();
                    }
                }
                if n >= 3 {
                    // 場面は 3 種類(キューブ 3 + 試験画面 A・B はそれぞれ 9)。最初の周回で作り終え、そのあとは増えない
                    seen.insert((n >= 12, live(&app)));
                }
            }
            // 終了時の解放(runtime の end_run と同じ口)
            let leftovers = Compositor::release_scenes(gl, SIZE, Some(fbo), &mut app.gl_scenes());
            assert!(leftovers.is_empty(), "{leftovers:?}");
            comp.destroy(gl);
        })
        .unwrap();
        // 周回の最初(n < 12)のあと、数は 1 つの値(= 3 + 9 + 9 = 21)に落ち着き、300 回目まで変わらない
        let steady: Vec<_> = seen.iter().filter(|(late, _)| *late).collect();
        assert_eq!(steady.len(), 1, "切り替えを重ねても増えない: {seen:?}");
        assert_eq!(steady[0].1, "21");
        assert_eq!(live(&app), "0", "解放で作成 − 削除が 0 に戻る");
        let _ = std::fs::remove_dir_all(dir);
    }
}
