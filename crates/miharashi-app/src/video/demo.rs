//! 各画面の見本を、フレーム番号から決まる仮想時刻で描く(`DemoRun`)。1 画面だけの `ScreenManager` に仮データを渡し、
//! フレームごとに壁時計と `Instant` を進めて `tick` → `render`(CPU)する。`demo-video` と `intro-video` が共有する。
use crate::agent_demo::{Scenario, Second};
use crate::aircraft::{self, Shared};
use crate::app::manager::ScreenManager;
use crate::app::screens::{
    agents::AgentScreen, aircraft::AircraftScreen, photo::PhotoScreen, room::RoomScreen,
    status::StatusScreen,
};
use crate::config::Config;
use crate::photos::{Catalog, Feed, Fixed, Grouping, Source};
use crate::screen::{Provider, Screen, World};
use miharashi_render::Canvas;
use miharashi_render::screens::Snapshot;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// 状況画面の見本の秒番号の起点(履歴が負の秒にならないよう、十分先から始める)
const STATUS_SECOND0: i64 = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DemoScreen {
    Status,
    Photo,
    /// 旅の区切りの地図カード
    Trip,
    Room,
    Aircraft,
    Agents,
}

impl DemoScreen {
    pub const ALL: [(&'static str, DemoScreen); 6] = [
        ("status", Self::Status),
        ("photo", Self::Photo),
        ("trip", Self::Trip),
        ("room", Self::Room),
        ("aircraft", Self::Aircraft),
        ("agents", Self::Agents),
    ];

    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .find(|(n, _)| *n == s)
            .map(|(_, d)| *d)
            .ok_or_else(|| {
                format!(
                    "--screen は {}(指定は {s})",
                    Self::ALL.map(|(n, _)| n).join(" / ")
                )
            })
    }
}

/// T3 Agents の見本の筋書きの時刻(画面の開始からの秒)。新しく検出 → (あれば)実行中から応答完了。
/// 2 つ目は、1 つ目の中央表示(約 4.5 秒)が終わってから起こす(同じ作業の変化は、表示中なら中に混ぜられるため)。
#[derive(Clone, Copy, Debug)]
pub struct AgentScript {
    pub new_at: f64,
    pub done_at: Option<f64>,
}

impl Default for AgentScript {
    fn default() -> Self {
        Self {
            new_at: 0.4,
            done_at: Some(5.2),
        }
    }
}

/// 状況画面の見本。仮想時刻の 1 周期(1 秒)ごとに 1 サンプル進む(`video::status_data`)。
struct VideoData {
    second: i64,
}

impl crate::app::DataSource for VideoData {
    fn update(&mut self, snap: &mut Snapshot) {
        *snap = super::status_data::snapshot(self.second);
        self.second += 1;
    }
}

/// 毎周期、筋書きの今の一覧を `World` に写す。
struct AgentsProvider {
    scenario: Scenario,
    elapsed_ns: Rc<Cell<u64>>,
}

impl Provider for AgentsProvider {
    fn update(&mut self, world: &mut World) {
        world.agents = self
            .scenario
            .at(Duration::from_nanos(self.elapsed_ns.get()));
    }
}

/// 航空機の見本。受信機の取得の代わりに、5 秒ごとに「取得した」ことにして機体の位置を進める。
struct AircraftFeed {
    shared: Shared,
    view: miharashi_render::aircraft::AircraftView,
    /// 最後に取得したことにした回(0 から)
    polled: Option<u64>,
}

pub struct DemoRun {
    mgr: ScreenManager,
    base: Instant,
    wall0: chrono::DateTime<chrono::Local>,
    fps: u32,
    canvas: Canvas,
    /// 画面を描いた回数(描き直したときだけ増える)
    revision: u64,
    next_frame: u32,
    agents_elapsed: Option<Rc<Cell<u64>>>,
    aircraft: Option<AircraftFeed>,
}

impl DemoRun {
    pub fn new(
        screen: DemoScreen,
        size: (u32, u32),
        fps: u32,
        script: AgentScript,
        wall_offset: Duration,
    ) -> Result<Self, String> {
        if size.0 == 0 || size.1 == 0 {
            return Err("--size は 1x1 以上".into());
        }
        if !(1..=120).contains(&fps) {
            return Err("--fps は 1〜120".into());
        }
        // 設定ファイルを探さず、通信もしない(見本の入力は全部ここで作る)
        let mut cfg = Config::default();
        cfg.geoframe.photo_interval_secs = 3;
        cfg.geoframe.card_secs = 8;
        // 壁時計の開始。紹介動画では、動画全体の時間に合わせてシーンごとにずらす(画面の時計が動画の中で戻らない)
        let wall0 =
            super::wall_start() + chrono::Duration::from_std(wall_offset).unwrap_or_default();
        // 仮想の Instant の起点。画面の生成時に実時刻で取った時刻より後にそろえ、生成の速さで絵が変わらないようにする
        let base = Instant::now() + Duration::from_secs(3600);
        let text = crate::app::text_engine(&cfg.ui.font);
        let alert_len = Duration::from_secs(60);
        let mut agents_elapsed = None;
        let mut aircraft_feed = None;
        let mut providers: Vec<Box<dyn Provider>> = Vec::new();
        let (screen_box, id): (Box<dyn Screen>, &str) = match screen {
            DemoScreen::Status => (Box::new(StatusScreen::new()), "status"),
            DemoScreen::Photo | DemoScreen::Trip => {
                let source = Source::Demo {
                    card_first: screen == DemoScreen::Trip,
                };
                let grouping = Grouping {
                    home: None,
                    radius_km: cfg.geoframe.home_radius_km,
                };
                let fixed = Fixed {
                    now: wall0.naive_local(),
                    seed: 1,
                };
                let catalog = Catalog::open_fixed(&source, size, &grouping, fixed)?;
                let photo = PhotoScreen::with_feed(&cfg, Feed::inline(catalog));
                providers.push(Box::new(photo.feed_provider()));
                (Box::new(photo), "photo")
            }
            DemoScreen::Room => {
                let view = crate::app::screens::room::demo_for(&wall0);
                let room = RoomScreen::new(Vec::new(), Default::default()).with_demo(view);
                (Box::new(room), "room")
            }
            DemoScreen::Aircraft => {
                let shared: Shared = Arc::new(Mutex::new(None));
                aircraft_feed = Some(AircraftFeed {
                    shared: shared.clone(),
                    view: aircraft::demo_view(),
                    polled: None,
                });
                (
                    Box::new(AircraftScreen::new(shared, false).with_spinning_sweep()),
                    "aircraft",
                )
            }
            DemoScreen::Agents => {
                let secs = |s: f64| Duration::from_secs_f64(s);
                let scenario = Scenario::new(
                    SystemTime::from(wall0),
                    base,
                    secs(script.new_at),
                    Second::Completed,
                    script.done_at.map(secs),
                );
                let elapsed = Rc::new(Cell::new(0));
                agents_elapsed = Some(elapsed.clone());
                providers.push(Box::new(AgentsProvider {
                    scenario,
                    elapsed_ns: elapsed,
                }));
                (
                    Box::new(AgentScreen::new(size, false, cfg.agents.stale_secs)),
                    "agents",
                )
            }
        };
        let mut mgr = ScreenManager::new(
            vec![screen_box],
            id,
            Box::new(VideoData {
                second: STATUS_SECOND0 + wall_offset.as_secs() as i64,
            }),
            super::status_data::snapshot(STATUS_SECOND0 + wall_offset.as_secs() as i64),
            text,
            alert_len,
        );
        for p in providers {
            mgr = mgr.with_provider(p);
        }
        mgr.set_clock(wall0);
        mgr.restart_at(base);
        Ok(Self {
            mgr,
            base,
            wall0,
            fps,
            canvas: Canvas::new(size.0, size.1),
            revision: 0,
            next_frame: 0,
            agents_elapsed,
            aircraft: aircraft_feed,
        })
    }

    /// 描き直した回数。値が変わらなければ、直前の `frame` と同じ絵。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 直前の `frame` の画面。
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// フレーム `index`(0 から順に 1 つずつ)の画面。
    pub fn frame(&mut self, index: u32) -> &Canvas {
        assert_eq!(index, self.next_frame, "フレームは順に呼ぶ");
        self.next_frame += 1;
        let ns = u64::from(index) * 1_000_000_000 / u64::from(self.fps);
        let elapsed = Duration::from_nanos(ns);
        let now = self.base + elapsed;
        let wall = self.wall0 + chrono::Duration::nanoseconds(ns as i64);
        self.mgr.set_clock(wall);
        if let Some(e) = &self.agents_elapsed {
            e.set(ns);
        }
        if let Some(a) = &mut self.aircraft {
            // 受信機は 5 秒ごとに取れる。取れた時刻の表示も仮想の壁時計から作る
            let k = elapsed.as_secs() / 5;
            if a.polled != Some(k) {
                let mut v = aircraft::advanced(&a.view, (k * 5) as f64);
                v.time = (self.wall0 + chrono::Duration::seconds((k * 5) as i64))
                    .format("%H:%M:%S")
                    .to_string();
                if let Ok(mut g) = a.shared.lock() {
                    *g = Some((self.base + Duration::from_secs(k * 5), v));
                }
                a.polled = Some(k);
            }
        }
        let redraw = self.mgr.tick(now);
        if redraw || self.revision == 0 {
            self.mgr.render(&mut self.canvas);
            self.revision += 1;
        }
        &self.canvas
    }

    /// 画面の RGBA8(上から下)。不透明なので、premultiplied のまま渡してよい。
    pub fn rgba(canvas: &Canvas) -> &[u8] {
        canvas.pixmap.data()
    }
}

/// `demo-video`: 1 画面の見本を `seconds` 秒、`fps` で書き出す。
pub fn write(
    screen: DemoScreen,
    size: (u32, u32),
    fps: u32,
    seconds: u32,
    out: &std::path::Path,
    overwrite: bool,
    stop: &AtomicBool,
) -> Result<u32, String> {
    if !(1..=3600).contains(&seconds) {
        return Err("--seconds は 1〜3600".into());
    }
    let frames = seconds * fps;
    let settings = super::record_settings(out, size, fps, overwrite);
    super::ensure_parent(out)?;
    miharashi_vrm::record::check_output(out, overwrite, "--overwrite")?;
    let mut run = DemoRun::new(screen, size, fps, AgentScript::default(), Duration::ZERO)?;
    let mut sink = miharashi_vrm::record::Sink::spawn(&settings)?;
    for i in 0..frames {
        super::check_stop(stop)?;
        let canvas = run.frame(i);
        sink.write_frame(DemoRun::rgba(canvas))?;
    }
    sink.finish()?;
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: (u32, u32) = (320, 135);
    /// Agents の中央表示と配置領域が画面内に収まる大きさ(小さい画面では領域が画面の外になる)
    const AGENTS_SIZE: (u32, u32) = (960, 405);

    fn run(screen: DemoScreen, size: (u32, u32), fps: u32, offset: Duration) -> DemoRun {
        DemoRun::new(screen, size, fps, AgentScript::default(), offset).unwrap()
    }

    fn frames(screen: DemoScreen, size: (u32, u32), n: u32) -> Vec<Vec<u8>> {
        let mut run = run(screen, size, 30, Duration::ZERO);
        (0..n)
            .map(|i| DemoRun::rgba(run.frame(i)).to_vec())
            .collect()
    }

    /// 直前のフレームから絵が変わったフレームの番号(0 は含めない)。画素は保持せず、前の 1 枚とだけ比べる。
    fn change_points(
        screen: DemoScreen,
        size: (u32, u32),
        fps: u32,
        offset: Duration,
        n: u32,
    ) -> Vec<u32> {
        let mut run = run(screen, size, fps, offset);
        let mut prev = DemoRun::rgba(run.frame(0)).to_vec();
        let mut points = Vec::new();
        for i in 1..n {
            let cur = DemoRun::rgba(run.frame(i));
            if cur != prev.as_slice() {
                points.push(i);
                prev = cur.to_vec();
            }
        }
        points
    }

    fn size_of(screen: DemoScreen) -> (u32, u32) {
        if screen == DemoScreen::Agents {
            AGENTS_SIZE
        } else {
            SMALL
        }
    }

    /// どの画面も、同じ引数なら全フレームの画素まで同じ(実時計・乱数・スレッドの速さに依らない)。
    #[test]
    fn every_screen_is_deterministic() {
        for (name, screen) in DemoScreen::ALL {
            let a = frames(screen, size_of(screen), 200);
            // 生成の速さで絵が変わらないことを見るため、2 回目の前に少し待つ
            std::thread::sleep(Duration::from_millis(40));
            let b = frames(screen, size_of(screen), 200);
            assert!(a == b, "{name}: 2 回の絵が違う");
        }
    }

    /// 写真は 3 秒ごとに次の写真へ(フェードを挟んで)替わり、航空機の回る線と状況画面は動く。
    #[test]
    fn screens_move_with_virtual_time() {
        let changed = |screen, a: usize, b: usize| {
            let f = frames(screen, size_of(screen), 200);
            f[a] != f[b]
        };
        assert!(changed(DemoScreen::Photo, 30, 150), "写真が替わる");
        assert!(!changed(DemoScreen::Photo, 30, 31), "替わる前は同じ絵");
        assert!(changed(DemoScreen::Aircraft, 10, 40), "回る線が進む");
        assert!(changed(DemoScreen::Status, 5, 45), "グラフが動く");
        assert!(changed(DemoScreen::Agents, 5, 60), "新着が中央に出る");
        assert!(
            !changed(DemoScreen::Trip, 5, 100),
            "地図カードは止まっている"
        );
    }

    /// 壁時計とデータは仮想時刻の 1 秒ごとに進む(同じ秒のあいだ、状況画面は動かない)。
    #[test]
    fn status_advances_once_per_virtual_second() {
        let p = change_points(DemoScreen::Status, SMALL, 30, Duration::ZERO, 100);
        assert_eq!(p, vec![30, 60, 90]);
        // fps が違っても、変わるのは秒の境界
        let p = change_points(DemoScreen::Status, SMALL, 25, Duration::ZERO, 80);
        assert_eq!(p, vec![25, 50, 75]);
    }

    /// 写真は fps に依らず、ちょうど 3 秒ごとに切り替わる(1 フレームずつ遅れて積み上がらない)。
    /// 切り替わりのフレームで新しい写真を適用し、フェード(800ms)が終わると動かなくなる。
    #[test]
    fn photo_changes_every_three_seconds_and_fade_ends() {
        for fps in [24, 25, 30, 60] {
            let p = change_points(DemoScreen::Photo, SMALL, fps, Duration::ZERO, fps * 10);
            let fade_frames = (0.8 * f64::from(fps)).ceil() as u32;
            for k in 1..=3 {
                let at = k * 3 * fps;
                if at >= fps * 10 {
                    break;
                }
                assert!(p.contains(&at), "fps {fps}: {at} で切り替わる({p:?})");
                assert!(!p.contains(&(at - 1)), "fps {fps}: 切り替え前は動かない");
                // フェード(800ms)のあいだは毎フレーム動き、終わったら次の秒の境界まで止まる
                for f in at + 1..at + fade_frames {
                    assert!(p.contains(&f), "fps {fps}: フェード中の {f}({p:?})");
                }
                assert!(
                    !p.iter().any(|&f| f >= at + fade_frames && f < at + fps),
                    "fps {fps}: フェードが終わったら止まる({p:?})"
                );
            }
        }
    }

    /// room は分が替わる瞬間にだけ描き直す(14:35:58 から始めると 2 秒後の 14:36:00)。
    #[test]
    fn room_redraws_at_the_minute_boundary() {
        let p = change_points(DemoScreen::Room, SMALL, 30, Duration::from_secs(58), 120);
        assert_eq!(p, vec![60]);
        let p = change_points(DemoScreen::Room, SMALL, 30, Duration::ZERO, 120);
        assert!(p.is_empty(), "分の途中では動かない: {p:?}");
    }

    /// 航空機の見本は 5 秒ごとに取得し直すので、30 秒を過ぎても古い結果(受信機の応答なし)にならない。
    #[test]
    fn aircraft_feed_never_goes_stale() {
        let mut run = run(DemoScreen::Aircraft, SMALL, 10, Duration::ZERO);
        let (base, stale) = (run.base, Duration::from_secs(30));
        for i in 0..800u32 {
            run.frame(i);
            let at = run
                .aircraft
                .as_ref()
                .unwrap()
                .shared
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .0;
            let age = (base + Duration::from_millis(u64::from(i) * 100)) - at;
            assert!(
                age < Duration::from_secs(5) && age < stale,
                "{i}: 取得から {age:?}"
            );
        }
    }

    /// 出力先の親ディレクトリは自動で作る。中断(終了フラグ)では出力先も一時ファイルも残さない。
    #[test]
    fn write_creates_the_parent_and_leaves_nothing_when_interrupted() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg が無いので skip");
            return;
        }
        let dir = std::env::temp_dir().join(format!("miharashi-demo-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = dir.join("new/dir/a.mp4");
        let go = AtomicBool::new(false);
        let frames = write(DemoScreen::Status, (320, 136), 5, 1, &out, false, &go).unwrap();
        assert_eq!(frames, 5);
        assert!(out.metadata().unwrap().len() > 0);

        let stop = AtomicBool::new(true);
        let out2 = dir.join("new/dir/b.mp4");
        let err = write(DemoScreen::Status, (320, 136), 5, 1, &out2, false, &stop).unwrap_err();
        assert!(err.contains("中断"), "{err}");
        let left: Vec<_> = std::fs::read_dir(dir.join("new/dir"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("a.mp4")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
