//! 画面ロジック。`ScreenManager`(manager.rs)が登録した画面(screens/)を切り替え、runtime からは `App` として見える。
//! 画面を足すときは `kiosk` の登録に足す(docs/DESIGN-screen-interface.md)。
pub mod manager;
pub mod screens {
    pub mod agents;
    pub mod aircraft;
    pub mod cube;
    pub mod gl_check;
    pub mod photo;
    pub mod room;
    pub mod status;
    pub mod with_scene;
}

use crate::config::Config;
use crate::photos::Source;
use manager::ScreenManager;
use miharashi_render::TextEngine;
use miharashi_render::screens::Snapshot;
use screens::aircraft::AircraftScreen;
use screens::cube::CubeScreen;
use screens::gl_check::{Dirt, DirtyScreen};
use screens::photo::PhotoScreen;
use screens::room::{RoomFeeds, RoomScreen};
use screens::status::StatusScreen;
use screens::with_scene::WithScene;
use std::time::Duration;

/// データ収集(実測値)の抽象。実行ループも画面も具体的な収集方法を知らない。
pub trait DataSource {
    /// 1 秒ごとなど、更新の時期に呼ぶ。
    fn update(&mut self, snap: &mut Snapshot);
}

pub fn text_engine(font: &str) -> TextEngine {
    let path = std::path::Path::new(font);
    if !font.is_empty() && path.exists() {
        match TextEngine::from_font_file("Noto Sans CJK JP", path) {
            Ok(t) => return t,
            Err(e) => eprintln!("font {font}: {e}。システムフォントを使う"),
        }
    }
    TextEngine::new("Noto Sans CJK JP")
}

pub fn set_clock(s: &mut Snapshot) {
    set_clock_at(s, chrono::Local::now());
}

/// 日付・時刻を `now` にする(`png` の fixture が、実時刻に依らない絵を作るため)。
pub fn set_clock_at(s: &mut Snapshot, now: chrono::DateTime<chrono::Local>) {
    use chrono::Datelike;
    let wd =
        ["月", "火", "水", "木", "金", "土", "日"][now.weekday().num_days_from_monday() as usize];
    s.date = format!("{} ({wd})", now.format("%Y/%m/%d"));
    s.time = now.format("%H:%M:%S").to_string();
}

/// `[vrm]` の設定どおり、画面 `screen`(id = `id`)に VRM を載せる。載せないときはそのまま返す。
/// 載せるのは、有効で、その画面が `vrm.screens` にあり、GL が使えるときだけ(`gl` は `gpu_max.is_some()`)。
/// `renderer = cpu` や `auto` で GLES を開けなかったときに包んでも、ScreenManager が 3D を外して元の画面を出すが、
/// 読み込みの別スレッドを起こしてモデルを RAM に置いてしまう。包まなければ、無効のときと同じ動きになる。
/// 読み込みは画面が最初に出るまで始めない(`VrmScene::lazy`)ので、巡回に出ない画面の分のモデルは読まない。
fn decorated<S: crate::screen::Screen + 'static>(
    cfg: &Config,
    gl: bool,
    id: &str,
    screen: S,
) -> Box<dyn crate::screen::Screen> {
    let v = &cfg.vrm;
    if !(v.enabled && gl && v.screens.iter().any(|s| s == id)) {
        return Box::new(screen);
    }
    let count = std::rc::Rc::new(miharashi_glscene::ObjectCount::default());
    Box::new(WithScene::new(
        screen,
        miharashi_vrm::VrmScene::lazy(v.model.clone().into(), count),
        v.stage_region(),
        v.fps,
    ))
}

/// 管理ページの `feeds` に出す、警報の取得への参照。`room` とは無関係で、`[alerts] disaster` と HA の url で決まる。
/// `warnings` は `disaster::start` の戻り値。
pub fn disaster_source(
    cfg: &Config,
    warnings: Option<crate::disaster::HomeShared>,
) -> crate::panel::Source<crate::disaster::HomeShared> {
    if !cfg.alerts.disaster {
        return Err("alerts.disaster = false".into());
    }
    if cfg.home_assistant.url.is_empty() {
        return Err("home_assistant.url が空".into());
    }
    warnings.ok_or_else(|| "警報の取得を起動できなかった".into())
}

/// 同じく航空機の取得への参照。`health` は `aircraft::start` が返したもの。
pub fn aircraft_source(
    cfg: &Config,
    health: Option<crate::aircraft::HealthShared>,
) -> crate::panel::Source<crate::aircraft::HealthShared> {
    if !cfg.aircraft.enabled {
        return Err("aircraft.enabled = false".into());
    }
    if cfg.aircraft.url.is_empty() {
        return Err("aircraft.url が空".into());
    }
    health.ok_or_else(|| "航空機の取得を起動できなかった".into())
}

/// 室内環境・天気の取得への参照。`room` は `room_feeds` の戻り値(画面を出さないときは None)。
fn room_sources(
    cfg: &Config,
    room: Option<&RoomFeeds>,
) -> (
    crate::panel::Source<crate::env::Shared>,
    crate::panel::Source<crate::weather::Shared>,
) {
    let off = |why: &str| -> String { why.into() };
    if !cfg.room.enabled {
        return (
            Err(off("room.enabled = false")),
            Err(off("room.enabled = false")),
        );
    }
    let ha_off = if cfg.home_assistant.url.is_empty() {
        "home_assistant.url が空"
    } else {
        "room.tiles が空、または取得を起動できなかった"
    };
    let weather_off = "予報の地域が決まらない(home_assistant.url も room.forecast_area も空)";
    (
        room.and_then(|r| r.env.clone()).ok_or_else(|| off(ha_off)),
        room.and_then(|r| r.weather.clone())
            .ok_or_else(|| off(weather_off)),
    )
}

/// キオスクの画面を登録する。並び順が切り替わる順になる(docs/DESIGN-screens.md §2)。
/// `gpu_max` は GPU 経路のとき Some(GPU で扱える写真の最大の幅と高さ)。室内環境と天気の取得が無い(`room` が None)なら
/// その画面は、航空機の取得が無効(`aircraft` が None)なら航空機の画面は登録しない。
#[allow(clippy::too_many_arguments)]
pub fn kiosk(
    cfg: &Config,
    size: (u32, u32),
    gpu_max: Option<u32>,
    source: Source,
    data: Box<dyn DataSource>,
    snap: Snapshot,
    room: Option<RoomFeeds>,
    aircraft: Option<crate::aircraft::Shared>,
) -> ScreenManager {
    let geoframe = match &source {
        Source::Geoframe(_) => None,
        Source::File(..) => Some("写真ファイルを直接指定している(GEOFRAME を使わない)"),
        Source::Demo { .. } => Some("動画の見本の写真(GEOFRAME を使わない)"),
    };
    let photo = PhotoScreen::new(cfg, size, gpu_max, source);
    let feed = photo.feed_provider();
    let (home_assistant, weather) = room_sources(cfg, room.as_ref());
    let admin_geoframe = match geoframe {
        None => Ok(photo.link()),
        Some(why) => Err(why.to_string()),
    };
    let gl = gpu_max.is_some();
    let mut screens: Vec<Box<dyn crate::screen::Screen>> = vec![
        Box::new(StatusScreen::new()),
        decorated(cfg, gl, "photo", photo),
    ];
    if let Some(feeds) = room {
        screens.push(Box::new(RoomScreen::new(cfg.room.tiles.clone(), feeds)));
    }
    if let Some(shared) = aircraft {
        screens.push(Box::new(AircraftScreen::new(shared, gpu_max.is_some())));
    }
    let agent_shared = cfg
        .agents
        .enabled
        .then(|| crate::agents::start(&cfg.agents));
    if cfg.agents.enabled {
        screens.push(decorated(
            cfg,
            gl,
            "agents",
            AgentScreen::new(size, gl, cfg.agents.stale_secs),
        ));
    }
    // 3D の画面(既定は無効。GL が使えないときは案内を出し、自動巡回では飛ばす)
    if cfg.cube.enabled {
        let (full, third) = cfg.cube.layouts();
        for (on, is_third) in [(full, false), (third, true)] {
            if on {
                screens.push(Box::new(CubeScreen::new(is_third, cfg.cube.fps)));
            }
        }
    }
    if cfg.gl_check.enabled {
        screens.push(Box::new(DirtyScreen::new(Dirt::A)));
        screens.push(Box::new(DirtyScreen::new(Dirt::B)));
    }
    let mut m = ScreenManager::new(
        screens,
        &cfg.ui.start_screen,
        data,
        snap,
        text_engine(&cfg.ui.font),
        Duration::from_secs(cfg.alerts.show_minutes * 60),
    )
    .with_gl_available(gpu_max.is_some())
    .with_provider(Box::new(feed));
    let admin_agents = match &agent_shared {
        Some(a) => Ok(a.clone()),
        None => Err("agents.enabled = false".to_string()),
    };
    if let Some(shared) = agent_shared {
        m = m.with_provider(Box::new(crate::agents::AgentProvider::new(shared)));
    }
    m = m.with_admin(|a| {
        a.geoframe = admin_geoframe;
        a.home_assistant = home_assistant;
        a.weather = weather;
        a.agents = admin_agents;
    });
    m = if cfg.rotation.enabled {
        m.with_rotation(
            &cfg.rotation.steps,
            Duration::from_secs(cfg.rotation.pause_after_input_secs),
        )
    } else {
        m
    };
    if cfg.ui.refresh_minutes > 0 {
        m.with_refresh(
            Duration::from_secs(cfg.ui.refresh_minutes * 60),
            cfg.ui.refresh_screens.clone(),
        )
    } else {
        m
    }
}

use screens::agents::AgentScreen;

#[cfg(test)]
mod cube_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod with_scene_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DummyData, dummy_snapshot};
    use crate::screen::{App, Composite, KeyPress, PhotoData};
    use miharashi_render::Canvas;
    use std::time::Instant;

    /// テスト用の写真(緑の 4:3)を置いたディレクトリ。
    pub(crate) fn test_photo() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "miharashi-app-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        image::RgbaImage::from_pixel(64, 48, image::Rgba([30, 120, 60, 255]))
            .save(&p)
            .unwrap();
        (dir, p)
    }

    fn app_with_photo(size: (u32, u32)) -> (ScreenManager, std::path::PathBuf) {
        app_with_photo_on(size, None)
    }

    /// `gpu_max` が Some なら GPU 経路(写真はデコードしただけで届く)。
    fn app_with_photo_on(
        size: (u32, u32),
        gpu_max: Option<u32>,
    ) -> (ScreenManager, std::path::PathBuf) {
        let (dir, p) = test_photo();
        let app = kiosk(
            &Config::default(),
            size,
            gpu_max,
            Source::File(p, Some("テスト".into())),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            None,
        );
        (app, dir)
    }

    /// 警報の取得は room の画面とは別。`room.enabled = false` でも `ok` になりうる。
    #[test]
    fn ac15_admin_sources_follow_the_config_not_the_screens() {
        use crate::panel::FeedState;
        let mut cfg = Config::default();
        cfg.room.enabled = false;
        cfg.alerts.disaster = true;
        cfg.home_assistant.url = "http://192.0.2.10:8123".into();
        cfg.agents.enabled = false;
        cfg.aircraft.enabled = false;
        let (dir, p) = test_photo();
        let warnings = crate::disaster::HomeShared::default();
        warnings.lock().unwrap().status.ok();
        let app = kiosk(
            &cfg,
            (64, 36),
            None,
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            None,
        )
        .with_admin(|a| {
            a.disaster = disaster_source(&cfg, Some(warnings.clone()));
            a.aircraft = aircraft_source(&cfg, None);
        });
        let feeds = App::admin_sources(&app).feeds();
        let by = |id: &str| feeds.iter().find(|f| f.id == id).unwrap().clone();
        assert_eq!(
            by("disaster").state,
            FeedState::Ok,
            "room が無効でも警報は動く"
        );
        assert!(by("disaster").last_ok.is_some());
        for id in ["home_assistant", "weather"] {
            assert_eq!(by(id).state, FeedState::NotRunning, "{id}");
            assert_eq!(by(id).error.as_deref(), Some("room.enabled = false"));
        }
        assert_eq!(
            by("aircraft").error.as_deref(),
            Some("aircraft.enabled = false")
        );
        assert_eq!(
            by("agents").error.as_deref(),
            Some("agents.enabled = false")
        );
        // 写真をファイルで指定しているので GEOFRAME は動いていない
        assert_eq!(by("geoframe").state, FeedState::NotRunning);
        // 警報の理由: 無効、HA の url が空、起動できなかった
        cfg.alerts.disaster = false;
        assert_eq!(
            disaster_source(&cfg, None).unwrap_err(),
            "alerts.disaster = false"
        );
        cfg.alerts.disaster = true;
        cfg.home_assistant.url.clear();
        assert_eq!(
            disaster_source(&cfg, Some(warnings)).unwrap_err(),
            "home_assistant.url が空"
        );
        cfg.home_assistant.url = "http://192.0.2.10:8123".into();
        assert!(disaster_source(&cfg, None).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    /// room が有効で HA の url があれば、room の取得への参照が入る。
    #[test]
    fn admin_sources_hold_the_room_feeds_when_the_room_runs() {
        let mut cfg = Config::default();
        cfg.room.enabled = true;
        cfg.home_assistant.url = "http://192.0.2.10:8123".into();
        let feeds = RoomFeeds {
            env: Some(Default::default()),
            weather: None,
            warnings: None,
        };
        let (env, weather) = room_sources(&cfg, Some(&feeds));
        assert!(env.is_ok());
        assert!(weather.unwrap_err().contains("予報の地域"));
        cfg.home_assistant.url.clear();
        let (env, _) = room_sources(&cfg, None);
        assert_eq!(env.unwrap_err(), "home_assistant.url が空");
    }

    fn photo_position(app: &ScreenManager) -> usize {
        app.panel_view().unwrap().photo.map_or(0, |p| p.0)
    }

    /// (画面の id, 状況画面のページ)
    fn screen(app: &ScreenManager) -> (&'static str, Option<&'static str>) {
        let v = app.panel_view().unwrap();
        (app.screen_id(), v.page)
    }

    fn pump_until(app: &mut ScreenManager, cond: impl Fn(&ScreenManager) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            app.tick(Instant::now());
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    const OVERVIEW: (&str, Option<&str>) = ("status", Some("OVERVIEW"));
    const DETAILS: (&str, Option<&str>) = ("status", Some("DETAILS"));
    const PHOTO: (&str, Option<&str>) = ("photo", None);

    /// 4/6 は画面の巡回(端で折り返す)、5・8/2 は表示中の画面の中の操作。
    #[test]
    fn keys_cycle_screens_and_act_inside_them() {
        let (mut app, dir) = app_with_photo((320, 180));
        assert_eq!(screen(&app), OVERVIEW);
        app.on_key(KeyPress::Up);
        app.on_key(KeyPress::Down);
        assert_eq!(screen(&app), OVERVIEW, "状況画面の 8/2 は何もしない");
        app.on_key(KeyPress::Select);
        assert_eq!(screen(&app), DETAILS, "状況画面の 5 でページ切替");
        app.on_key(KeyPress::Next);
        assert_eq!(screen(&app), PHOTO, "6 で次の画面");
        app.on_key(KeyPress::Next);
        assert_eq!(
            screen(&app),
            DETAILS,
            "最後の画面の 6 は先頭へ。最後に見ていたページ"
        );
        app.on_key(KeyPress::Prev);
        assert_eq!(screen(&app), PHOTO, "先頭の画面の 4 は最後へ");
        assert!(
            pump_until(&mut app, |a| photo_position(a) == 1),
            "写真が読み込まれる"
        );
        app.on_key(KeyPress::Down);
        app.on_key(KeyPress::Up);
        app.on_key(KeyPress::Select);
        assert_eq!(screen(&app), PHOTO, "写真画面の 8/2・5 は画面を変えない");
        assert!(
            pump_until(&mut app, |a| photo_position(a) == 1),
            "写真切替後も位置が有効"
        );
        assert_eq!(app.keys, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 何も描かない画面(3 つ目の画面としてメニューと巡回を確かめる)。
    struct Blank;

    impl crate::screen::Screen for Blank {
        fn id(&self) -> crate::screen::ScreenId {
            "blank"
        }
        fn title(&self) -> &'static str {
            "BLANK"
        }
        fn render(&mut self, ctx: &mut crate::screen::RenderContext<'_>) {
            ctx.canvas
                .clear(miharashi_render::canvas::rgba(0, 0, 0, 255));
        }
    }

    fn three_screens() -> ScreenManager {
        ScreenManager::new(
            vec![
                Box::new(StatusScreen::new()),
                Box::new(Blank),
                Box::new(Blank2),
            ],
            "status",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            text_engine(""),
            Duration::from_secs(60),
        )
    }

    struct Blank2;

    impl crate::screen::Screen for Blank2 {
        fn id(&self) -> crate::screen::ScreenId {
            "blank2"
        }
        fn title(&self) -> &'static str {
            "BLANK 2"
        }
        fn render(&mut self, ctx: &mut crate::screen::RenderContext<'_>) {
            ctx.canvas
                .clear(miharashi_render::canvas::rgba(0, 0, 40, 255));
        }
    }

    /// 出すものが無いかを外から切り替えられる画面(自動巡回の skip_if_empty)。
    struct Maybe(std::rc::Rc<std::cell::Cell<bool>>);

    impl crate::screen::Screen for Maybe {
        fn id(&self) -> crate::screen::ScreenId {
            "maybe"
        }
        fn title(&self) -> &'static str {
            "MAYBE"
        }
        fn render(&mut self, ctx: &mut crate::screen::RenderContext<'_>) {
            ctx.canvas
                .clear(miharashi_render::canvas::rgba(40, 0, 0, 255));
        }
        fn is_empty(&self) -> bool {
            self.0.get()
        }
    }

    fn step(screen: &str, secs: u64, skip_if_empty: bool) -> crate::config::RotationStep {
        crate::config::RotationStep {
            screen: screen.into(),
            secs,
            skip_if_empty,
        }
    }

    /// status・blank・maybe の 3 画面で巡回する。`empty` で maybe を空にできる。
    fn rotating(
        steps: &[crate::config::RotationStep],
        pause_secs: u64,
    ) -> (ScreenManager, std::rc::Rc<std::cell::Cell<bool>>) {
        let empty = std::rc::Rc::new(std::cell::Cell::new(false));
        let app = ScreenManager::new(
            vec![
                Box::new(StatusScreen::new()),
                Box::new(Blank),
                Box::new(Maybe(empty.clone())),
            ],
            "status",
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            text_engine(""),
            Duration::from_secs(60),
        )
        .with_rotation(steps, Duration::from_secs(pause_secs));
        (app, empty)
    }

    /// 1 秒ごとに tick して、`secs` 秒後の画面。
    fn run_for(app: &mut ScreenManager, t: &mut Instant, secs: u64) -> &'static str {
        for _ in 0..secs {
            *t += Duration::from_secs(1);
            app.tick(*t);
        }
        app.screen_id()
    }

    /// 最初の手順の画面から始め、手順の秒数ごとに次へ。skip_if_empty の画面は空なら飛ばす。
    /// 登録に無い画面の手順は捨てる。巡回はキーの数に入れない。
    #[test]
    fn rotation_follows_the_steps_and_skips_empty_screens() {
        let steps = [
            step("blank", 10, false),
            step("aircraft", 10, false),
            step("maybe", 20, true),
            step("status", 5, false),
        ];
        let (mut app, empty) = rotating(&steps, 300);
        let mut t = Instant::now();
        assert_eq!(app.screen_id(), "blank", "start_screen ではなく最初の手順");
        assert_eq!(run_for(&mut app, &mut t, 9), "blank");
        assert_eq!(
            run_for(&mut app, &mut t, 1),
            "maybe",
            "無い画面(aircraft)は飛ばす"
        );
        assert_eq!(run_for(&mut app, &mut t, 20), "status");
        assert_eq!(run_for(&mut app, &mut t, 5), "blank", "最後の次は先頭");
        empty.set(true);
        assert_eq!(
            run_for(&mut app, &mut t, 10),
            "status",
            "空の maybe を飛ばす"
        );
        assert_eq!(app.keys, 0);
        let stats = app.stats();
        assert_eq!(stats.last(), Some(&("rotation", "on".to_string())));
    }

    /// どの手順も飛ばす状態なら、今の画面のまま待つ。
    #[test]
    fn rotation_stays_when_every_step_is_skipped() {
        let (mut app, empty) = rotating(&[step("maybe", 10, true)], 300);
        empty.set(true);
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 35), "maybe");
        let (none, _) = rotating(&[step("radar", 10, false)], 300);
        assert_eq!(
            none.rotation_state(),
            None,
            "回せる画面が無ければ巡回しない"
        );
        assert_eq!(none.screen_id(), "status");
    }

    /// キー・操作パネルで止まり、最後の操作から pause 秒たったら次の手順から戻る。
    #[test]
    fn input_pauses_the_rotation_then_it_resumes_at_the_next_step() {
        let steps = [
            step("status", 10, false),
            step("blank", 10, false),
            step("maybe", 10, false),
        ];
        let (mut app, _) = rotating(&steps, 60);
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 10), "blank");
        app.on_key(KeyPress::Prev);
        assert_eq!(
            (app.screen_id(), app.rotation_state()),
            ("status", Some("paused"))
        );
        let mut t = Instant::now();
        assert_eq!(
            run_for(&mut app, &mut t, 59),
            "status",
            "止めている間は動かない"
        );
        assert_eq!(
            run_for(&mut app, &mut t, 1),
            "maybe",
            "blank の次の手順から"
        );
        assert_eq!(app.rotation_state(), Some("on"));
        app.show_screen("blank");
        assert_eq!(app.rotation_state(), Some("paused"), "操作パネルでも止まる");
        assert_eq!(
            app.stats().last(),
            Some(&("rotation", "paused".to_string()))
        );
    }

    /// 操作パネルに見せる巡回の状態("on" / "paused" / "off")。
    fn panel_rotation(app: &ScreenManager) -> Option<&'static str> {
        app.panel_view().unwrap().rotation.map(|r| match r {
            crate::screen::PanelRotation::On => "on",
            crate::screen::PanelRotation::Paused { .. } => "paused",
            crate::screen::PanelRotation::Off => "off",
        })
    }

    /// 止めている間にもう一度操作すると、戻る時刻が延びる(操作パネルへ送り直す)。
    #[test]
    fn another_input_extends_the_pause() {
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        let until = |app: &ScreenManager| match app.panel_view().unwrap().rotation {
            Some(crate::screen::PanelRotation::Paused { until }) => until,
            r => panic!("止まっていない: {r:?}"),
        };
        app.on_key(KeyPress::Next);
        let first = until(&app);
        assert!(first <= Instant::now() + Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(5));
        app.on_key(KeyPress::Next);
        assert!(until(&app) > first);
    }

    /// 操作パネルの `/status` の rotation。runtime は tick が再描画を求めた周期に `panel_view()` を送るので、
    /// 戻ったときに画面が変わらなくても(次の手順が同じ画面・メニューを開いている)、その周期の tick は true。
    #[test]
    fn panel_view_follows_the_rotation_state() {
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        assert_eq!(panel_rotation(&app), Some("on"));

        // 次の手順の画面(blank)へ操作パネルで移って止める。戻っても画面は blank のまま
        app.show_screen("blank");
        assert_eq!(panel_rotation(&app), Some("paused"));
        let mut t = Instant::now();
        run_for(&mut app, &mut t, 59);
        assert_eq!(panel_rotation(&app), Some("paused"));
        t += Duration::from_secs(1);
        assert!(app.tick(t), "戻った周期は再描画(パネルへ送る)");
        assert_eq!(app.screen_id(), "blank");
        assert_eq!(panel_rotation(&app), Some("on"));

        // メニューを開いたまま戻る(キーは実時刻で止めるので、時計を合わせるため作り直す)
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        app.on_key(KeyPress::Menu);
        assert_eq!(panel_rotation(&app), Some("paused"));
        let mut t = Instant::now();
        run_for(&mut app, &mut t, 59);
        t += Duration::from_secs(1);
        assert!(app.tick(t), "メニュー中に戻った周期も再描画");
        assert!(app.menu_open());
        assert_eq!(panel_rotation(&app), Some("on"));
    }

    /// アラートの間は手順の時間を進めない(メニューは開くキーで巡回が止まる)。
    #[test]
    fn alerts_freeze_the_rotation_clock() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 0);
        let mut app = app.with_alerts(Some(rx));
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 5), "status");
        tx.send(miharashi_render::alert::AlertView::Disaster(
            crate::disaster::demo_view(),
        ))
        .unwrap();
        assert_eq!(run_for(&mut app, &mut t, 59), "status", "アラート中");
        assert!(app.alert_active());
        assert_eq!(run_for(&mut app, &mut t, 2), "status", "閉じた");
        assert!(!app.alert_active());
        assert_eq!(run_for(&mut app, &mut t, 4), "blank", "残りの 5 秒で次へ");
    }

    /// 操作パネルから止めたままにすると、時間がたっても画面は変わらない。
    /// 状態が変わったときだけ true(パネルへ送り直す)。キーの数に入れない。
    #[test]
    fn rotation_off_keeps_the_screen() {
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 5), "status");
        assert!(app.set_rotation(false), "on から off");
        assert!(!app.set_rotation(false), "off のままなら何もしない");
        assert_eq!(run_for(&mut app, &mut t, 600), "status");
        assert_eq!(app.rotation_state(), Some("off"));
        assert_eq!(panel_rotation(&app), Some("off"));
        assert_eq!(app.stats().last(), Some(&("rotation", "off".to_string())));
        assert_eq!(app.keys, 0);
    }

    /// 止めたままの間にキー・操作パネルで画面を選んでも off のまま(paused にしない)。pause 秒たっても動かない。
    #[test]
    fn input_while_off_keeps_it_off() {
        let steps = [
            step("status", 10, false),
            step("blank", 10, false),
            step("maybe", 10, false),
        ];
        let (mut app, _) = rotating(&steps, 60);
        assert!(app.set_rotation(false));
        app.on_key(KeyPress::Next);
        assert_eq!(
            (app.screen_id(), app.rotation_state()),
            ("blank", Some("off"))
        );
        app.show_screen("maybe");
        assert_eq!(
            app.rotation_state(),
            Some("off"),
            "操作パネルでも off のまま"
        );
        // キーは実時刻で数えるので、時計を合わせ直す
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 120), "maybe");
        assert_eq!(panel_rotation(&app), Some("off"));
    }

    /// off から on にすると、次の tick ですぐ次の手順の画面へ移る。
    #[test]
    fn turning_rotation_on_moves_to_the_next_step_at_once() {
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        let mut t = Instant::now();
        assert!(app.set_rotation(false));
        assert_eq!(run_for(&mut app, &mut t, 30), "status");
        assert!(app.set_rotation(true), "off から on");
        assert!(!app.set_rotation(true), "on のままなら何もしない");
        assert_eq!(panel_rotation(&app), Some("on"));
        assert_eq!(run_for(&mut app, &mut t, 1), "blank");
        assert_eq!(
            run_for(&mut app, &mut t, 10),
            "status",
            "そのあとは手順どおり"
        );
        assert_eq!(app.keys, 0);
    }

    /// 操作で止めている間に on にすると、すぐ次の手順へ移る。off にすると止めたままになる。
    #[test]
    fn turning_rotation_on_while_paused_resumes_at_once() {
        let steps = [
            step("status", 10, false),
            step("blank", 10, false),
            step("maybe", 10, false),
        ];
        let (mut app, _) = rotating(&steps, 60);
        let mut t = Instant::now();
        assert_eq!(run_for(&mut app, &mut t, 10), "blank");
        app.show_screen("status");
        assert_eq!(app.rotation_state(), Some("paused"));
        let mut t = Instant::now();
        assert!(app.set_rotation(true), "paused から on");
        assert_eq!(app.rotation_state(), Some("on"));
        assert_eq!(
            run_for(&mut app, &mut t, 1),
            "maybe",
            "blank の次の手順から"
        );

        app.on_key(KeyPress::Prev);
        assert_eq!(app.rotation_state(), Some("paused"));
        let mut t = Instant::now();
        assert!(app.set_rotation(false), "paused から off");
        assert_eq!(
            run_for(&mut app, &mut t, 120),
            "blank",
            "pause 秒たっても戻らない"
        );
        assert_eq!(app.rotation_state(), Some("off"));
    }

    /// メニューを開いたまま再開しても、メニューの間は移らない。閉じるキーで一時停止になり、
    /// あとは操作のあとと同じく pause 秒で次の手順から戻る。
    #[test]
    fn turning_rotation_on_during_the_menu_does_not_move_it() {
        let (mut app, _) = rotating(&[step("status", 10, false), step("blank", 10, false)], 60);
        app.set_rotation(false);
        app.on_key(KeyPress::Menu);
        assert!(app.menu_open());
        let mut t = Instant::now();
        assert!(app.set_rotation(true));
        assert_eq!(
            run_for(&mut app, &mut t, 30),
            "status",
            "メニュー中は移らない"
        );
        app.on_key(KeyPress::Menu);
        assert!(!app.menu_open());
        // メニューを閉じるキーで一時停止になる(on のときのキーと同じ)
        assert_eq!(app.rotation_state(), Some("paused"));
    }

    /// 巡回が無効なら、操作パネルから止める・再開するは何もしない。
    #[test]
    fn set_rotation_does_nothing_without_rotation() {
        let mut app = three_screens();
        assert!(!app.set_rotation(false));
        assert!(!app.set_rotation(true));
        assert_eq!(app.rotation_state(), None);
        assert_eq!(panel_rotation(&app), None);
        assert_eq!(app.keys, 0);
    }

    /// 巡回が無効なら状態ログに rotation を出さない。
    #[test]
    fn no_rotation_by_default() {
        let app = three_screens();
        assert_eq!(app.rotation_state(), None);
        assert_eq!(panel_rotation(&app), None);
        assert!(app.stats().iter().all(|(k, _)| *k != "rotation"));
    }

    /// 0 でメニューを開き、8/2(4/6 も)で選んで 5 で移る。0 で閉じる。メニュー中は画面に操作を渡さない。
    #[test]
    fn menu_selects_a_screen() {
        let mut app = three_screens();
        app.on_key(KeyPress::Next);
        app.on_key(KeyPress::Next);
        app.on_key(KeyPress::Next);
        assert_eq!(app.screen_id(), "status", "3 画面を巡回して戻る");
        app.on_key(KeyPress::Menu);
        assert!(app.menu_open());
        app.on_key(KeyPress::Down);
        app.on_key(KeyPress::Down);
        app.on_key(KeyPress::Down);
        // 0 → 0(3 つ進んで一周)→ 2(戻ると最後へ折り返す)→ 0 → 2
        app.on_key(KeyPress::Up);
        app.on_key(KeyPress::Next);
        app.on_key(KeyPress::Prev);
        assert_eq!(app.screen_id(), "status", "選んでいる間は移らない");
        app.on_key(KeyPress::Select);
        assert!(!app.menu_open());
        assert_eq!(app.screen_id(), "blank2");
        app.on_key(KeyPress::Prev);
        app.on_key(KeyPress::Prev);
        app.on_key(KeyPress::Menu);
        app.on_key(KeyPress::Select);
        assert_eq!(
            screen(&app),
            OVERVIEW,
            "開いたときは表示中の画面を選んでいる。メニューの 5 はページを切り替えない"
        );
        app.on_key(KeyPress::Menu);
        app.on_key(KeyPress::Down);
        app.on_key(KeyPress::Menu);
        assert!(!app.menu_open());
        assert_eq!(app.screen_id(), "status", "0 で閉じると移らない");
    }

    /// 操作パネルの `/status` の menu。開く・選ぶ・閉じるが `panel_view()` に出る(#49)。
    #[test]
    fn panel_view_follows_the_menu() {
        let mut app = three_screens();
        let menu = |app: &ScreenManager| app.panel_view().unwrap().menu;
        assert_eq!(menu(&app), None);
        app.on_key(KeyPress::Next);
        app.on_key(KeyPress::Menu);
        assert_eq!(menu(&app), Some(1), "開いたときは表示中の画面");
        app.on_key(KeyPress::Down);
        assert_eq!(menu(&app), Some(2));
        app.on_key(KeyPress::Down);
        assert_eq!(menu(&app), Some(0), "下の端で一周");
        app.on_key(KeyPress::Up);
        assert_eq!(menu(&app), Some(2), "上の端で一周");
        app.on_key(KeyPress::Prev);
        assert_eq!(menu(&app), Some(1), "4 でも動く");
        app.on_key(KeyPress::Next);
        assert_eq!(menu(&app), Some(2), "6 でも動く");
        app.on_key(KeyPress::Select);
        assert_eq!(menu(&app), None, "5 で閉じる");
        assert_eq!(app.panel_view().unwrap().current, 2, "選んだ画面へ移る");
        app.on_key(KeyPress::Menu);
        app.on_key(KeyPress::Up);
        app.on_key(KeyPress::Menu);
        assert_eq!(menu(&app), None, "0 でも閉じる");
        assert_eq!(app.panel_view().unwrap().current, 2, "0 では移らない");
        app.on_key(KeyPress::Menu);
        assert_eq!(menu(&app), Some(2));
        app.show_screen("status");
        assert_eq!(menu(&app), None, "POST /api/v1/screen でも閉じる");
    }

    /// 操作パネルの `/status` の keys(#51)。アラート → メニュー → 画面の順に決まり、`on_key` と同じ意味になる。
    #[test]
    fn panel_view_keys_follow_the_alert_menu_and_screen() {
        use crate::screen::PanelKeys;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = three_screens().with_alerts(Some(rx));
        let keys = |app: &ScreenManager| app.panel_view().unwrap().keys;
        assert_eq!(
            keys(&app),
            PanelKeys {
                prev: Some("前の画面"),
                next: Some("次の画面"),
                select: Some("詳細"),
                up: None,
                down: None,
                menu: Some("メニュー"),
            },
            "状況の画面(概要)"
        );
        app.on_key(KeyPress::Select);
        assert_eq!(
            keys(&app).select,
            Some("概要"),
            "5 で詳細へ移ると意味が変わる"
        );
        app.on_key(KeyPress::Next);
        assert_eq!(
            (keys(&app).select, keys(&app).up, keys(&app).down),
            (None, None, None),
            "画面の中の操作が無い画面"
        );
        app.on_key(KeyPress::Menu);
        assert_eq!(
            keys(&app),
            PanelKeys {
                prev: Some("上を選ぶ"),
                next: Some("下を選ぶ"),
                select: Some("移動"),
                up: Some("上を選ぶ"),
                down: Some("下を選ぶ"),
                menu: Some("閉じる"),
            },
            "メニュー中"
        );
        tx.send(miharashi_render::alert::AlertView::Disaster(
            crate::disaster::demo_view(),
        ))
        .unwrap();
        app.tick(Instant::now());
        assert!(app.alert_active());
        let close = Some("閉じる");
        assert_eq!(
            keys(&app),
            PanelKeys {
                prev: close,
                next: close,
                select: close,
                up: close,
                down: close,
                menu: close,
            },
            "アラート中はどのキーも閉じるだけ"
        );
        app.on_key(KeyPress::Down);
        assert_eq!(
            keys(&app).select,
            Some("移動"),
            "アラートを閉じるとメニューに戻る"
        );
    }

    /// 操作パネルの `/status` の keys(#53)。モニターが消えているあいだ、runtime はキーを `on_key` に渡さず
    /// 点けるだけにする(force_off なら何もしない)ので、アラートやメニューより先にそれを出す。点けば元に戻る。
    #[test]
    fn panel_view_keys_follow_the_display_power() {
        use crate::screen::PanelKeys;
        let mut app = three_screens();
        let keys = |app: &ScreenManager| app.panel_view().unwrap().keys;
        let all = |label: Option<&'static str>| PanelKeys {
            prev: label,
            next: label,
            select: label,
            up: label,
            down: label,
            menu: label,
        };
        app.on_key(KeyPress::Menu);
        let menu = keys(&app);
        assert_eq!(menu.select, Some("移動"));

        app.set_display_on(false, Instant::now());
        assert_eq!(
            keys(&app),
            all(Some("画面を点ける")),
            "自動で消えている: どのキーも点けるだけ(メニュー中でも)"
        );
        app.set_display_forced_off(true);
        assert_eq!(keys(&app), all(None), "常に消灯: どのキーも何もしない");

        app.set_display_forced_off(false);
        app.set_display_on(true, Instant::now());
        assert_eq!(keys(&app), menu, "点いたら元のラベルに戻る");

        app.set_display_forced_off(true);
        assert_eq!(
            keys(&app),
            menu,
            "force_off でも点いているあいだ(アラートで点けたときなど)は、ふつうのラベル"
        );
    }

    /// 操作パネルの `POST /api/v1/screen`: 登録されている id だけに移り、メニューとアラートは閉じる。
    #[test]
    fn show_screen_by_id() {
        let mut app = three_screens();
        app.on_key(KeyPress::Menu);
        assert!(app.show_screen("blank2"));
        assert_eq!(app.screen_id(), "blank2");
        assert!(!app.menu_open());
        assert!(!app.show_screen("room"), "登録されていない");
        assert_eq!(app.screen_id(), "blank2");
    }

    #[test]
    fn panel_view_follows_the_screen() {
        let (mut app, dir) = app_with_photo((320, 180));
        let v = app.panel_view().unwrap();
        let ids: Vec<_> = v.screens.iter().map(|s| (s.id, s.title)).collect();
        assert_eq!(ids, [("status", "STATUS"), ("photo", "PHOTO FRAME")]);
        assert_eq!((v.current, v.page), (0, Some("OVERVIEW")));
        assert!(
            v.feed_online,
            "最初の tick より前から、今までどおり接続中として出す"
        );
        app.on_key(KeyPress::Select);
        assert_eq!(app.panel_view().unwrap().page, Some("DETAILS"));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        let v = app.panel_view().unwrap();
        assert_eq!((v.current, v.page, v.photo), (1, None, Some((1, 1))));
        assert!(v.feed_online);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_screen_picks_the_registered_id() {
        let (app, dir) = app_with_photo((320, 180));
        assert_eq!(screen(&app), OVERVIEW);
        let _ = std::fs::remove_dir_all(&dir);
        let (dir, p) = test_photo();
        let cfg = Config {
            ui: crate::config::Ui {
                start_screen: "photo".into(),
                ..Config::default().ui
            },
            ..Config::default()
        };
        let app = kiosk(
            &cfg,
            (320, 180),
            None,
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            None,
        );
        assert_eq!(screen(&app), PHOTO);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状態ログ: tools/soak_summary.py が読む順に並び、screen= は画面の id。
    #[test]
    fn stats_use_screen_ids_in_soak_summary_order() {
        let (dir, p) = test_photo();
        let planes: crate::aircraft::Shared = std::sync::Arc::new(std::sync::Mutex::new(Some((
            Instant::now(),
            crate::aircraft::demo_view(),
        ))));
        let mut app = kiosk(
            &Config::default(),
            (320, 180),
            None,
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            Some(planes),
        );
        app.tick(Instant::now());
        let keys: Vec<_> = app.stats().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            [
                "ticks",
                "keys",
                "screen",
                "photo",
                "aircraft",
                "feed",
                "cpu",
                "gpu",
                "gpus",
                "page",
                "refreshes"
            ]
        );
        let get = |app: &ScreenManager, k: &str| {
            app.stats()
                .into_iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v)
        };
        assert_eq!(get(&app, "screen").as_deref(), Some("status"));
        assert_eq!(get(&app, "page").as_deref(), Some("overview"));
        assert_eq!(get(&app, "feed").as_deref(), Some("online"));
        assert_eq!(
            get(&app, "aircraft").as_deref(),
            Some("5"),
            "半径内の機体の数"
        );
        app.on_key(KeyPress::Next);
        assert_eq!(get(&app, "screen").as_deref(), Some("photo"));
        assert_eq!(get(&app, "page"), None);
        app.on_key(KeyPress::Next);
        assert_eq!(
            get(&app, "screen").as_deref(),
            Some("aircraft"),
            "3 つ目の画面"
        );
        app.on_key(KeyPress::Next);
        assert_eq!(get(&app, "screen").as_deref(), Some("status"));
        let ids: Vec<_> = app
            .panel_view()
            .unwrap()
            .screens
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["status", "photo", "aircraft"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 焼き付き対策: 対象の画面を続けて出していると、間隔ごとにグリッチ → 暗転 → 再表示の演出が入り、
    /// 約 1.6 秒で終わる。キーで打ち切る。対象でない画面や、アラート中には入れない。
    #[test]
    fn refresh_effect_on_listed_screens_only() {
        let (dir, p) = test_photo();
        let planes: crate::aircraft::Shared = std::sync::Arc::new(std::sync::Mutex::new(Some((
            Instant::now(),
            crate::aircraft::demo_view(),
        ))));
        let cfg = Config {
            ui: crate::config::Ui {
                start_screen: "aircraft".into(),
                refresh_minutes: 0,
                ..Config::default().ui
            },
            ..Config::default()
        };
        let mut app = kiosk(
            &cfg,
            (1280, 540),
            None,
            Source::File(p, None),
            Box::new(DummyData::new()),
            dummy_snapshot(0.0),
            None,
            Some(planes),
        )
        .with_refresh(Duration::from_secs(60), vec!["aircraft".into()]);
        let t0 = Instant::now();
        app.tick(t0);
        assert!(!app.refreshing());
        let center = |app: &mut ScreenManager| {
            let mut c = Canvas::new(1280, 540);
            match app.render_composite(&mut c) {
                Composite::Radar {
                    background_changed,
                    geom,
                    ..
                } => (background_changed, geom.center),
                _ => panic!("航空機の画面は Radar"),
            }
        };
        let (_, before) = center(&mut app);
        assert!(app.tick(t0 + Duration::from_secs(61)), "間隔が来たら演出");
        assert!(
            app.refreshing() && app.animating(),
            "演出中は毎フレーム描く"
        );
        let mut c = Canvas::new(1280, 540);
        assert!(matches!(app.render_composite(&mut c), Composite::Canvas));
        let mut normal = Canvas::new(1280, 540);
        app.render(&mut normal);
        app.tick(t0 + Duration::from_secs(63));
        assert!(!app.refreshing(), "1.6 秒で終わる");
        let (redrawn, after) = center(&mut app);
        assert!(redrawn, "終わったら GPU 経路の背景を描き直す");
        assert_eq!(
            (after.0 - before.0, after.1 - before.1),
            (3.0, 0.0),
            "演出のたびに画面全体を 6px(1080p 換算)ずらす"
        );
        // キーで打ち切る(画面はキーのとおりに動く)
        app.tick(t0 + Duration::from_secs(130));
        assert!(app.refreshing());
        app.on_key(KeyPress::Down);
        assert!(!app.refreshing());
        assert_eq!(app.screen_id(), "aircraft");
        // 対象でない画面では入れない
        app.on_key(KeyPress::Next);
        assert_eq!(app.screen_id(), "status");
        app.tick(t0 + Duration::from_secs(400));
        assert!(!app.refreshing(), "状況画面は対象でない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn px(c: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * c.width() + x) * 4) as usize;
        c.pixmap.data()[i..i + 4].try_into().unwrap()
    }

    /// ScreenManager を通して描いた状況画面は、描画関数を直接呼んだものと 1 画素も違わない。
    #[test]
    fn status_render_matches_the_render_function() {
        for size in [(1920u32, 1080u32), (2560, 1080)] {
            let (mut app, dir) = app_with_photo(size);
            app.tick(Instant::now());
            let mut text = text_engine("");
            for page in [
                miharashi_render::screens::StatusPage::Overview,
                miharashi_render::screens::StatusPage::Details,
            ] {
                let mut got = Canvas::new(size.0, size.1);
                app.render(&mut got);
                let mut want = Canvas::new(size.0, size.1);
                miharashi_render::screens::status(&mut want, &mut text, app.world(), page);
                assert!(got.pixmap.data() == want.pixmap.data(), "{size:?} {page:?}");
                app.on_key(KeyPress::Select);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn both_screens_render_expected_regions_at_1080p() {
        let (mut app, dir) = app_with_photo((1920, 1080));
        let mut c = Canvas::new(1920, 1080);
        app.render(&mut c);
        // 状況画面: CPU タイルの左端にアクセント色、背景は暗色。
        assert_eq!(
            px(&c, 34, 150),
            [64, 200, 255, 255],
            "CPU タイルのアクセント"
        );
        assert_eq!(px(&c, 5, 540), [8, 12, 20, 255], "背景");
        // グラフ領域にシリーズ色(CPU のシアン)が描かれている。
        let mut found = false;
        for y in 400..900 {
            for x in 100..1880 {
                if px(&c, x, y) == [64, 200, 255, 255] {
                    found = true;
                }
            }
        }
        assert!(found, "CPU 使用率の折れ線");

        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        // 写真が拡大されて全面に出る。フェードが終わるまで待って確認する。
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut c2 = Canvas::new(1920, 1080);
        app.render(&mut c2);
        // テスト写真は 4:3 なので 1080p では左右どちらかに高さいっぱいで置かれる(中央 x=960 は必ず写真内)。
        let center = px(&c2, 960, 300);
        assert_eq!(center, [30, 120, 60, 255], "写真(緑)が表示される");
        let outside = px(&c2, 1900, 1070).min(px(&c2, 20, 1070));
        assert!(
            outside[1] < 70 && outside[1] > 20,
            "余白は暗いぼかし背景: {outside:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GPU 経路の合成(写真 + 透明背景の重ね絵)を CPU で再現し、CPU 経路の `render` と比べる。
    /// 重ね絵は変わったときだけ描き直すことも確かめる。
    #[test]
    fn composite_photo_plus_overlay_matches_cpu_render() {
        let size = (640u32, 270u32);
        let (mut app, dir) = app_with_photo(size);
        let mut c = Canvas::new(size.0, size.1);
        assert!(
            matches!(app.render_composite(&mut c), Composite::Canvas),
            "状況画面は Canvas が完成画面"
        );
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());

        let mut cpu = Canvas::new(size.0, size.1);
        app.render(&mut cpu);
        let mut overlay = Canvas::new(size.0, size.1);
        let (frame, changed) = match app.render_composite(&mut overlay) {
            Composite::Photo {
                photo,
                old,
                overlay_changed,
            } => {
                assert!(old.is_none(), "フェードは終わっている");
                let PhotoData::Frame(f) = photo.data else {
                    panic!("CPU 経路の写真は完成フレーム");
                };
                (f.data().to_vec(), overlay_changed)
            }
            _ => panic!("写真画面は Photo になる"),
        };
        assert!(changed, "最初は重ね絵を描く");
        assert!(over_diff(&overlay, &frame, &cpu) <= 2, "CPU 経路との差");

        // 時刻などが変わらなければ描き直さない。キーを押すと描き直す
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: false,
                ..
            }
        ));
        app.on_key(KeyPress::Down);
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: true,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写真画面の 5 で写真の情報を消し、もう一度押すと戻す。GPU 経路の重ね絵にも反映する。
    #[test]
    fn select_toggles_the_photo_info() {
        let size = (640u32, 270u32);
        let (mut app, dir) = app_with_photo(size);
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let draw = |app: &mut ScreenManager| {
            let mut c = Canvas::new(size.0, size.1);
            app.render(&mut c);
            c.pixmap.data().to_vec()
        };
        let with_info = draw(&mut app);
        app.on_key(KeyPress::Select);
        let without = draw(&mut app);
        assert!(with_info != without, "情報が消える");
        let mut overlay = Canvas::new(size.0, size.1);
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: true,
                ..
            }
        ));
        app.on_key(KeyPress::Select);
        assert!(draw(&mut app) == with_info, "もう一度押すと戻る");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// メニューは写真の重ね絵の上に描き、GPU 経路でも CPU 経路と同じ絵になる。
    #[test]
    fn menu_is_drawn_over_the_photo_overlay() {
        let size = (640u32, 270u32);
        let (mut app, dir) = app_with_photo(size);
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut overlay = Canvas::new(size.0, size.1);
        app.render_composite(&mut overlay);
        app.on_key(KeyPress::Menu);
        let mut cpu = Canvas::new(size.0, size.1);
        app.render(&mut cpu);
        let frame = match app.render_composite(&mut overlay) {
            Composite::Photo {
                photo,
                overlay_changed: true,
                ..
            } => match photo.data {
                PhotoData::Frame(f) => f.data().to_vec(),
                _ => panic!("CPU 経路の写真は完成フレーム"),
            },
            _ => panic!("メニューを開いたら重ね絵を描き直す"),
        };
        assert!(
            over_diff(&overlay, &frame, &cpu) <= 2,
            "メニューも重ね絵に入る"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重ね絵(premultiplied)を写真のフレームに source-over で重ねたものと、CPU の完成画面との最大の差。
    fn over_diff(overlay: &Canvas, frame: &[u8], cpu: &Canvas) -> u8 {
        let mut max = 0u8;
        for ((o, f), want) in overlay
            .pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(frame.as_chunks::<4>().0)
            .zip(cpu.pixmap.data().as_chunks::<4>().0)
        {
            for i in 0..3 {
                let v = o[i] as f32 + f[i] as f32 * (1.0 - o[3] as f32 / 255.0);
                max = max.max((v.round() as i32 - want[i] as i32).unsigned_abs() as u8);
            }
        }
        max
    }

    /// アラートは写真の重ね絵の上に描き、開いたとき・閉じたときに重ね絵を描き直させる。
    /// キーを押すと閉じる。CPU 経路の完成画面とも一致する。
    #[test]
    fn alert_layer_redraws_the_photo_overlay() {
        let size = (640u32, 270u32);
        let (app, dir) = app_with_photo(size);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = app.with_alerts(Some(rx));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut overlay = Canvas::new(size.0, size.1);
        app.render_composite(&mut overlay);
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: false,
                ..
            }
        ));
        tx.send(miharashi_render::alert::AlertView::Quake(
            crate::quake::demo_view(),
        ))
        .unwrap();
        assert!(app.tick(Instant::now()), "アラートが来たら描き直す");
        assert!(app.wants_screen(), "アラート中は画面を点ける");
        let mut cpu = Canvas::new(size.0, size.1);
        app.render(&mut cpu);
        let frame = match app.render_composite(&mut overlay) {
            Composite::Photo {
                photo,
                overlay_changed: true,
                ..
            } => match photo.data {
                PhotoData::Frame(f) => f.data().to_vec(),
                _ => panic!("CPU 経路の写真は完成フレーム"),
            },
            _ => panic!("アラートが来たら重ね絵を描き直す"),
        };
        assert!(
            over_diff(&overlay, &frame, &cpu) <= 2,
            "アラートも重ね絵に入る"
        );
        app.on_key(KeyPress::Next);
        assert!(!app.wants_screen(), "キーで閉じる");
        assert_eq!(screen(&app), PHOTO, "閉じるだけで、写真は進めない");
        assert!(matches!(
            app.render_composite(&mut overlay),
            Composite::Photo {
                overlay_changed: true,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GPU 経路: 写真はデコードしただけで 1 回だけ渡し、次からは GPU 側のフレームを使う
    /// (CPU 側の画素は捨てる)。CPU で描くときは、その場で完成フレームを作る。
    #[test]
    fn gpu_path_sends_source_once_then_resident() {
        let size = (640u32, 270u32);
        let kind = |app: &mut ScreenManager| {
            let mut c = Canvas::new(size.0, size.1);
            match app.render_composite(&mut c) {
                Composite::Photo { photo, .. } => match photo.data {
                    PhotoData::Frame(_) => "frame",
                    PhotoData::Source { .. } => "source",
                    PhotoData::Resident => "resident",
                },
                _ => "canvas",
            }
        };
        let (mut app, dir) = app_with_photo_on(size, Some(16384));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        assert_eq!(kind(&mut app), "source", "1 回目はデコードしただけの写真");
        assert_eq!(kind(&mut app), "resident", "2 回目からは GPU 側のもの");
        let _ = std::fs::remove_dir_all(&dir);

        // 上限より大きい写真は、取得スレッドで完成フレームまで作る
        let (mut app, dir) = app_with_photo_on(size, Some(32));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        assert_eq!(kind(&mut app), "frame");
        let _ = std::fs::remove_dir_all(&dir);

        // GPU 経路の写真でも CPU の render は描ける
        let (mut app, dir) = app_with_photo_on(size, Some(16384));
        app.on_key(KeyPress::Next);
        assert!(pump_until(&mut app, |a| photo_position(a) == 1));
        std::thread::sleep(Duration::from_millis(900));
        app.tick(Instant::now());
        let mut c = Canvas::new(size.0, size.1);
        app.render(&mut c);
        assert_eq!(px(&c, 320, 100), [30, 120, 60, 255], "写真(緑)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn has_color(
        c: &Canvas,
        color: [u8; 4],
        x: std::ops::Range<u32>,
        y: std::ops::Range<u32>,
    ) -> bool {
        y.into_iter()
            .any(|y| x.clone().any(|x| px(c, x, y) == color))
    }

    #[test]
    fn status_pages_render_at_1080p_and_21_9() {
        for size in [(1920u32, 1080u32), (2560, 1080)] {
            let (mut app, dir) = app_with_photo(size);
            for page in [OVERVIEW, DETAILS] {
                assert_eq!(screen(&app), page);
                let mut c = Canvas::new(size.0, size.1);
                app.render(&mut c);
                assert_eq!(px(&c, 5, 540), [8, 12, 20, 255], "背景 {size:?}");
                if page == OVERVIEW {
                    // CPU / GPU タイルの左端のアクセント色。
                    assert_eq!(px(&c, 3 + 32, 150), [64, 200, 255, 255], "CPU {size:?}");
                    let gx = 32 + ((size.0 - 64 - 80) / 5 + 20) + 3;
                    assert_eq!(px(&c, gx, 150), [255, 170, 60, 255], "GPU {size:?}");
                    assert!(has_color(
                        &c,
                        [64, 200, 255, 255],
                        100..size.0 - 100,
                        400..900
                    ));
                } else {
                    // DETAILS にはアクセント色の帯がない。タイル枠内は暗色のまま。
                    assert_ne!(px(&c, 35, 150), [64, 200, 255, 255], "{size:?}");
                    assert_eq!(px(&c, 40, 330), [16, 24, 36, 255], "タイル背景 {size:?}");
                    // ページ指示のシアン色の文字が見出しにある。
                    assert!(has_color(&c, [64, 200, 255, 255], 300..700, 20..60));
                }
                app.on_key(KeyPress::Select);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
