//! 画面ロジック。`ScreenManager`(manager.rs)が登録した画面(screens/)を切り替え、runtime からは `App` として見える。
//! 画面を足すときは `kiosk` の登録に足す(docs/DESIGN-screen-interface.md)。
pub mod manager;
pub mod screens {
    pub mod photo;
    pub mod status;
}

use crate::config::Config;
use crate::photos::Source;
use manager::ScreenManager;
use miharashi_render::TextEngine;
use miharashi_render::screens::Snapshot;
use screens::photo::PhotoScreen;
use screens::status::StatusScreen;
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
    use chrono::Datelike;
    let now = chrono::Local::now();
    let wd =
        ["月", "火", "水", "木", "金", "土", "日"][now.weekday().num_days_from_monday() as usize];
    s.date = format!("{} ({wd})", now.format("%Y/%m/%d"));
    s.time = now.format("%H:%M:%S").to_string();
}

/// キオスクの画面を登録する。並び順が切り替わる順になる(docs/DESIGN-screens.md §2)。
/// `gpu_max` は GPU 経路のとき Some(GPU で扱える写真の最大の幅と高さ)。
pub fn kiosk(
    cfg: &Config,
    size: (u32, u32),
    gpu_max: Option<u32>,
    source: Source,
    data: Box<dyn DataSource>,
    snap: Snapshot,
    aircraft: Option<crate::aircraft::Shared>,
) -> ScreenManager {
    let photo = PhotoScreen::new(cfg, size, gpu_max, source).with_aircraft(aircraft);
    let feed = photo.feed_provider();
    ScreenManager::new(
        vec![Box::new(StatusScreen::new()), Box::new(photo)],
        &cfg.ui.start_screen,
        data,
        snap,
        text_engine(&cfg.ui.font),
        Duration::from_secs(cfg.alerts.show_minutes * 60),
    )
    .with_provider(Box::new(feed))
}

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
        );
        (app, dir)
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
        );
        assert_eq!(screen(&app), PHOTO);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状態ログ: tools/soak_summary.py が読む順に並び、screen= は画面の id。
    #[test]
    fn stats_use_screen_ids_in_soak_summary_order() {
        let (mut app, dir) = app_with_photo((320, 180));
        app.tick(Instant::now());
        let keys: Vec<_> = app.stats().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            [
                "ticks", "keys", "screen", "photo", "aircraft", "feed", "cpu", "gpu", "gpus",
                "page"
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
        app.on_key(KeyPress::Next);
        assert_eq!(get(&app, "screen").as_deref(), Some("photo"));
        assert_eq!(get(&app, "page"), None);
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
