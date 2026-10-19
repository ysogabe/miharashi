//! キオスクの実行ループ。画面ロジックとは `screen::App` だけでつながり、具体的な型は知らない。
//! seat/VT の有効・無効、SIGINT/SIGTERM、入力、描画、画面の電源(DPMS)、状態ログを扱う。
//! 状態ログは共通部分(t / rss / fds / frames / worst_frame)に `App::stats()` の `k=v` を続けて出す。
use crate::config::Config;
use crate::panel::{DisplayControl, DisplayStatus, Panel, PanelInput};
use crate::screen::{App, Composite, GlComposite, GlScene, KeyPress, PhotoData, PhotoRef, Top};
use crate::screen_power::{Mode as PowerMode, ScreenPower};
use miharashi_gpu::{
    AgentGpuCard, Frame, GlesDisplay, PhotoLayer, PhotoSource, Placement, Radar, SceneError,
    SceneFrame, SceneOutcome, SceneTop, SourceImage,
};
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::input::{Action, Keys};
use miharashi_linux::kms::Display;
use miharashi_linux::seat::{SeatMode, Session, SessionEvent};
use miharashi_linux::vt::VtGuard;
use miharashi_render::Canvas;
use miharashi_render::aircraft::{self, BlipKind, RadarGeom};
use miharashi_render::photo_layout::{self, Mode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / 1024 / 1024)
}

fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

/// 状態ログ 1 行を作る。共通部分のあとに画面ロジックの `k=v` を並べる(tools/soak_summary.py が読む)。
fn format_stats(
    t: u64,
    rss_mb: u64,
    fds: usize,
    frames: u64,
    worst_ms: u128,
    extra: &[(&'static str, String)],
) -> String {
    let mut line =
        format!("stats: t={t}s rss={rss_mb}MB fds={fds} frames={frames} worst_frame={worst_ms}ms");
    for (k, v) in extra {
        line.push_str(&format!(" {k}={v}"));
    }
    line
}

/// 描画の間隔の上限(`App::frame_interval`)のとき、表示の完了後に待つ時間。
/// 表示の完了待ち(vsync)は vsync の境界で終わるので、間隔いっぱいを待つと次のフレームが 1 つあとの vsync に
/// ずれる(30fps が 20fps になる)。vsync 半分だけ早く起きて、描画を次の vsync の手前に間に合わせる。
/// 表示できるのは vsync の境界だけなので、間隔は vsync の整数倍に切り上げて上限を守る
/// (60Hz で 50fps は 2 vsync = 30fps。半分だけ引くやり方のままだと次の vsync に間に合って 60fps になる)。
fn frame_wait(interval: Duration, refresh_hz: u32) -> Duration {
    let vsyncs = display_vsyncs(interval, refresh_hz);
    if vsyncs <= 1.0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64((vsyncs - 0.5) / f64::from(refresh_hz.max(1)))
}

/// 間隔を表示できる vsync の数(間隔を vsync の整数倍に切り上げた値、最小 1)。`frame_wait` と遅れの判定で共通。
fn display_vsyncs(interval: Duration, refresh_hz: u32) -> f64 {
    let hz = f64::from(refresh_hz.max(1));
    // 30fps と 60Hz のような割り切れる組が、浮動小数点の誤差で 1 vsync 多くならないように少し引く
    (interval.as_secs_f64() * hz - 1e-6).ceil().max(1.0)
}

/// 間隔の上限があるときの「遅れ」の閾値 [ms]。切り上げた後の表示間隔に半フレームの余裕を足す
/// (60Hz で 50fps は表示間隔が 33.3ms なので、設定の間隔 20ms を基にすると正常なフレームが遅れになる)。
fn late_limit_ms(interval: Duration, refresh_hz: u32) -> f32 {
    let hz = f64::from(refresh_hz.max(1));
    ((display_vsyncs(interval, refresh_hz) + 0.5) / hz * 1000.0) as f32
}

/// 区間内のフレーム時間 [ms] を "平均/p95" にする(PoC-005 のベースライン用)。空なら "-"。
fn summarize_ms(samples: &mut Vec<f32>) -> String {
    if samples.is_empty() {
        return "-".into();
    }
    samples.sort_by(f32::total_cmp);
    let avg = samples.iter().sum::<f32>() / samples.len() as f32;
    let p95 = samples[((samples.len() as f32 * 0.95).ceil() as usize).clamp(1, samples.len()) - 1];
    samples.clear();
    format!("{avg:.1}/{p95:.1}")
}

/// 画面ロジックの写真を GPU の合成器の形にする。`screen_h` は画面の高さ(影の間隔の倍率に使う)。
pub fn photo_layer(p: PhotoRef<'_>, screen_h: u32) -> PhotoLayer<'_> {
    let src = match p.data {
        PhotoData::Frame(f) => PhotoSource::Frame(f.data()),
        PhotoData::Source { image, layout } => PhotoSource::Image(SourceImage {
            pixels: image.as_bytes(),
            size: (image.width(), image.height()),
            channels: image.color().channel_count() as u32,
            place: Placement {
                cover: layout.mode == Mode::Cover,
                rect: (
                    layout.photo.x,
                    layout.photo.y,
                    layout.photo.w,
                    layout.photo.h,
                ),
                scale: photo_layout::scale(screen_h),
            },
        }),
        PhotoData::Resident => PhotoSource::Resident,
    };
    PhotoLayer { id: p.id, src }
}

/// レーダーの点を GPU の合成器の形 (x, y, 種類) にする。
pub fn radar_blips(g: &RadarGeom) -> Vec<[f32; 3]> {
    g.blips
        .iter()
        .map(|b| {
            let kind = match b.kind {
                BlipKind::Other => 0.0,
                BlipKind::Focus => 1.0,
                BlipKind::Ground => 2.0,
            };
            [b.x, b.y, kind]
        })
        .collect()
}

/// 航空機の画面のレーダーを GPU の合成器の形にする。`blips` は `radar_blips` の結果。
pub fn radar_layer<'a>(
    g: &RadarGeom,
    sweep: f32,
    trail: f32,
    screen_h: u32,
    blips: &'a [[f32; 3]],
) -> Radar<'a> {
    Radar {
        center: g.center,
        radius: g.radius,
        sweep,
        trail,
        scale: photo_layout::scale(screen_h),
        blips,
    }
}

/// `Composite` を GPU の合成器の `Frame` にするとき、`Frame` が借りるもの(レーダーの点・Agent のカード)の置き場。
/// `fill` で詰めてから `frame_of` / `scene_frame` に渡す。
#[derive(Default)]
pub struct FrameParts<'a> {
    blips: Vec<[f32; 3]>,
    cards: Vec<AgentGpuCard<'a>>,
}

impl<'a> FrameParts<'a> {
    /// `comp`(`Gl` なら下地)の分を詰める。
    pub fn fill(&mut self, comp: &Composite<'a>) {
        match comp {
            Composite::Radar { geom, .. } => self.blips = radar_blips(geom),
            Composite::Agents { cards, .. } => {
                self.cards.extend(cards.iter().map(|c| AgentGpuCard {
                    id: c.id,
                    revision: c.revision,
                    pixels: c.surface.data(),
                    size: (c.surface.width(), c.surface.height()),
                    rect: (c.pose.x, c.pose.y, c.pose.width, c.pose.height),
                    alpha: c.pose.alpha,
                }));
            }
            Composite::Gl(g) => {
                if let Some(base) = &g.base {
                    self.fill(base);
                }
            }
            Composite::Canvas | Composite::Photo { .. } => {}
        }
    }
}

/// `Gl` 以外の `Composite` を GPU の合成器の `Frame` にする(`Gl` は None)。`canvas` は、完成画面か、
/// 写真の重ね絵・レーダーやカードの背景を描いたもの。
pub fn frame_of<'a>(
    comp: &Composite<'a>,
    canvas: &'a Canvas,
    parts: &'a FrameParts<'a>,
) -> Option<Frame<'a>> {
    let h = canvas.height();
    Some(match comp {
        Composite::Canvas => Frame::Image(canvas.pixmap.data()),
        Composite::Photo {
            photo,
            old,
            overlay_changed,
        } => Frame::Photo {
            photo: photo_layer(*photo, h),
            old: old.map(|(p, t)| (photo_layer(p, h), t)),
            overlay: overlay_changed.then(|| canvas.pixmap.data()),
        },
        Composite::Radar {
            background_changed,
            geom,
            sweep,
        } => Frame::Radar {
            background: background_changed.then(|| canvas.pixmap.data()),
            radar: radar_layer(geom, *sweep, aircraft::TRAIL, h, &parts.blips),
        },
        Composite::Agents {
            background_changed,
            scrim_alpha,
            focus_card_id,
            ..
        } => Frame::Agents {
            background: background_changed.then(|| canvas.pixmap.data()),
            cards: &parts.cards,
            scrim_alpha: *scrim_alpha,
            focus_card_id: *focus_card_id,
        },
        Composite::Gl(_) => return None,
    })
}

/// `Composite::Gl` を GPU の `present_scene` の入力にする。下地は `frame_of` で `Frame` にする。
/// `Gl` の入れ子は呼び出し側の誤り: `debug_assert!` で検出し、リリースでは領域を空にして
/// `present_scene` の失敗(その場面の `release` と、3D 抜きでの完成)として扱う(`Err` で実行ループを抜けない)。
pub fn scene_frame<'a>(
    g: GlComposite<'a>,
    canvas: &'a Canvas,
    parts: &'a FrameParts<'a>,
) -> SceneFrame<'a> {
    let GlComposite {
        base,
        scene,
        mut region,
        top,
    } = g;
    let base = base.as_deref();
    if base.is_some_and(|b| matches!(b, Composite::Gl(_))) {
        debug_assert!(false, "Composite::Gl の base に Gl は入れられない");
        region.w = 0;
        region.h = 0;
    }
    SceneFrame {
        base: base.and_then(|b| frame_of(b, canvas, parts)),
        scene,
        region,
        top: match top {
            Top::Off => SceneTop::Off,
            Top::Keep => SceneTop::Keep,
            Top::Upload(p) => SceneTop::Upload(p.data()),
        },
    }
}

/// 表示経路。CPU は Canvas を dumb buffer へ変換コピーし、GLES は表示カードの GPU で
/// テクスチャとして描いて page flip する(PoC-005 PR2。見た目は同じ)。
// 1 つしか作らないので、variant の大きさの差(glow のコンテキストが約 6KB)は気にしない
#[allow(clippy::large_enum_variant)]
enum Presenter {
    Cpu(Display),
    Gles(GlesDisplay),
}

impl Presenter {
    /// `renderer` は auto / cpu / gles。auto で GLES が使えなければ理由をログに出して CPU に戻る。
    fn open(
        fd: &std::os::fd::OwnedFd,
        renderer: &str,
        connector: &str,
        mode: ModeSel,
    ) -> Result<Self, String> {
        let dup = || fd.try_clone().map_err(|e| e.to_string());
        if renderer != "cpu" {
            match GlesDisplay::open(dup()?, connector, mode) {
                Ok(d) => {
                    eprintln!("renderer: gles ({})", d.describe());
                    return Ok(Self::Gles(d));
                }
                Err(e) if renderer == "auto" => {
                    eprintln!("renderer: GLES が使えない: {e}(cpu に戻る)")
                }
                Err(e) => return Err(format!("renderer=gles: {e}")),
            }
        }
        eprintln!("renderer: cpu (dumb buffer)");
        Ok(Self::Cpu(Display::open(dup()?, connector, mode)?))
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Cpu(d) => d.size(),
            Self::Gles(d) => d.size(),
        }
    }

    fn refresh(&self) -> u32 {
        match self {
            Self::Cpu(d) => d.refresh(),
            Self::Gles(d) => d.refresh(),
        }
    }

    /// GLES なら、GPU で扱える写真の最大の幅と高さ(写真の合成を GPU に任せる)。
    fn gpu_max_texture(&self) -> Option<u32> {
        match self {
            Self::Cpu(_) => None,
            Self::Gles(d) => Some(d.max_texture()),
        }
    }

    /// 前回から今までに写真を GPU で作った時間 [ms]。CPU 経路は空。
    fn take_photo_build_ms(&mut self) -> Vec<f32> {
        match self {
            Self::Cpu(_) => Vec::new(),
            Self::Gles(d) => d.take_build_ms(),
        }
    }

    fn suspend(&mut self) {
        match self {
            Self::Cpu(d) => d.suspend(),
            Self::Gles(d) => d.suspend(),
        }
    }

    fn resume(&mut self) -> Result<(), String> {
        match self {
            Self::Cpu(d) => d.resume(),
            Self::Gles(d) => d.resume(),
        }
    }

    fn set_power(&mut self, on: bool) -> Result<(), String> {
        match self {
            Self::Cpu(d) => d.set_power(on),
            Self::Gles(d) => d.set_power(on),
        }
    }

    /// 1 枚描いて表示する。戻り値は (描画, 転送) の時間 [ms]。
    /// CPU は Canvas に完成画面を描いて変換コピーする。GLES は写真の合成を GPU に任せ(PoC-005 PR3)、
    /// Canvas には重ね絵だけを、変わったときだけ描く。
    fn draw(&mut self, app: &mut dyn App, canvas: &mut Canvas) -> Result<(f32, f32), String> {
        let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f32() * 1000.0;
        match self {
            Self::Cpu(d) => {
                let t0 = Instant::now();
                app.render(canvas);
                let t1 = Instant::now();
                d.present(|buf, pitch| canvas.write_xrgb8888(buf, pitch))?;
                Ok((ms(t0, t1), ms(t1, Instant::now())))
            }
            Self::Gles(d) => draw_gles(d, app, canvas),
        }
    }

    /// 持っている `GlScene` を、GL が使えるうちに解放する(終了時。`drop` の前に呼ぶ)。CPU 経路は何もしない。
    fn release_scenes(&mut self, scenes: &mut [&mut dyn GlScene]) -> Vec<String> {
        match self {
            Self::Cpu(_) => Vec::new(),
            Self::Gles(d) => d.release_scenes(scenes),
        }
    }
}

/// GLES 経路が `draw_gles` に求めること(試験で DRM なしの表示に差し替えるため)。
pub(crate) trait GlesSurface {
    fn present_scene(&mut self, f: &mut SceneFrame<'_>) -> Result<SceneOutcome, SceneError>;
    fn present_frame(&mut self, f: &Frame<'_>) -> Result<(), String>;
}

impl GlesSurface for GlesDisplay {
    fn present_scene(&mut self, f: &mut SceneFrame<'_>) -> Result<SceneOutcome, SceneError> {
        GlesDisplay::present_scene(self, f)
    }

    fn present_frame(&mut self, f: &Frame<'_>) -> Result<(), String> {
        GlesDisplay::present_frame(self, f)
    }
}

/// GLES 経路の 1 枚。戻り値は (描画, 転送) の時間 [ms]。
/// 3D の失敗は `Err` でなく `SceneFailed`(表示は続く)。場面の失敗は、そのあとに致命的な失敗(最上段の重ね絵・
/// swap・page flip)が起きて `Err` になるときも、必ず `app.report_scene_failure` に届ける。届けないと、`release` 済みの
/// 場面が `app.gl_scenes()` に残って終了時にもう一度 `release` される。
pub(crate) fn draw_gles(
    d: &mut impl GlesSurface,
    app: &mut dyn App,
    canvas: &mut Canvas,
) -> Result<(f32, f32), String> {
    let ms = |a: Instant, b: Instant| b.duration_since(a).as_secs_f32() * 1000.0;
    let t0 = Instant::now();
    let comp = app.render_composite(canvas);
    let t1 = Instant::now();
    let mut parts = FrameParts::default();
    parts.fill(&comp);
    // `comp` が `app` を借りているので、報告は借用が終わってから
    let (shown, scene_failed) = match comp {
        Composite::Gl(g) => {
            let mut frame = scene_frame(g, canvas, &parts);
            match d.present_scene(&mut frame) {
                Ok(SceneOutcome::Drawn) => (Ok(()), None),
                Ok(SceneOutcome::SceneFailed(msg)) => (Ok(()), Some(msg)),
                Err(e) => (Err(e.message), e.scene_failed),
            }
        }
        other => {
            let frame = frame_of(&other, canvas, &parts).expect("Gl 以外");
            (d.present_frame(&frame), None)
        }
    };
    let times = (ms(t0, t1), ms(t1, Instant::now()));
    if let Some(msg) = scene_failed {
        app.report_scene_failure(&msg);
    }
    shown.map(|()| times)
}

/// 実行ループが表示に求めること(試験で DRM なしの表示に差し替えるため)。
pub(crate) trait FrameSink {
    fn draw(&mut self, app: &mut dyn App, canvas: &mut Canvas) -> Result<(f32, f32), String>;
}

impl FrameSink for Presenter {
    fn draw(&mut self, app: &mut dyn App, canvas: &mut Canvas) -> Result<(f32, f32), String> {
        Presenter::draw(self, app, canvas)
    }
}

/// 実行ループの 1 枚。描画の失敗(下地・EGL・page flip など)は、`?` で関数を抜けずに `run_err` へ入れて
/// None を返す。呼び出し側(ループ)は `break` して、場面の解放を通る終了処理(`end_run`)へ進む。
pub(crate) fn draw_frame(
    display: &mut impl FrameSink,
    app: &mut dyn App,
    canvas: &mut Canvas,
    run_err: &mut Option<String>,
) -> Option<(f32, f32)> {
    match display.draw(app, canvas) {
        Ok(times) => Some(times),
        Err(e) => {
            eprintln!("描画に失敗: {e}");
            *run_err = Some(e);
            None
        }
    }
}

/// 画面ロジックを作るために必要な、表示先が決まったあとの情報。
/// 操作パネルの待ち受けに失敗したとき、試し直すまでの間隔。
const PANEL_RETRY: Duration = Duration::from_secs(30);
/// キーボードが 1 台も無いとき、挿されていないか見直す間隔。
const KEYS_RETRY: Duration = Duration::from_secs(10);

/// 操作パネルに見せるモニターの状態。
fn display_status(screen_on: bool, power: Option<&ScreenPower>) -> DisplayStatus {
    DisplayStatus {
        on: screen_on,
        mode: power.and_then(ScreenPower::mode).map(PowerMode::as_str),
    }
}

pub struct Output {
    pub size: (u32, u32),
    /// GLES なら Some(GPU で扱える写真の最大の幅と高さ)。auto で CPU に戻ったときは None
    pub gpu_max_texture: Option<u32>,
}

/// 実行ループ。`make_app` には確定した表示サイズが渡される。
pub fn run(cfg: &Config, make_app: impl FnOnce(&Output) -> Box<dyn App>) -> Result<(), String> {
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }

    let seat_mode = SeatMode::parse(&cfg.runtime.seat)?;
    let mut session = Session::open(seat_mode)?;
    eprintln!(
        "seat: {} ({})",
        session.name(),
        if session.is_libseat() {
            "libseat"
        } else {
            "直接オープン"
        }
    );

    // 表示先のカードとコネクターを決める。/dev/dri/card0 には固定しない。
    let cards = drm_select::scan();
    let card = drm_select::choose_card(&cards, &CardSel::parse(&cfg.display.card))?;
    let connector = drm_select::choose_connector(card, &cfg.display.connector)?;
    eprintln!(
        "display: {} driver={} pci={} connector={}",
        card.path.display(),
        card.driver,
        card.pci.as_deref().unwrap_or("-"),
        connector.name
    );
    let drm_dev = session.open_device(&card.path)?;
    let mode = ModeSel::parse(&cfg.display.mode)?;
    let mut display = Presenter::open(&drm_dev.fd, &cfg.display.renderer, &connector.name, mode)?;
    let (w, h) = display.size();
    eprintln!("mode: {w}x{h}@{}Hz", display.refresh());
    // vrefresh が 0 のモードでも fps の上限と遅れの判定が効くよう、60Hz とみなす
    let refresh_hz = match display.refresh() {
        0 => {
            eprintln!("mode: リフレッシュレートが 0Hz なので 60Hz とみなす");
            60
        }
        hz => hz,
    };

    let vt = VtGuard::enter();
    eprintln!(
        "vt: {}",
        if vt.is_vt() {
            "KD_GRAPHICS"
        } else {
            "標準入力は VT ではない"
        }
    );
    // 操作パネルがあればキーボードは無くてもよい(後から挿されたら KEYS_RETRY ごとに拾う)
    let mut keys = if cfg.panel.enabled {
        Keys::open_any(&mut session, &cfg.input.devices)
    } else {
        Keys::open(&mut session, &cfg.input.devices)?
    };
    if keys.is_empty() {
        eprintln!("input: キーボードが無い(操作パネルで操作する)");
    } else {
        eprintln!("input: {:?}", keys.device_names());
    }
    let mut keys_retry = Instant::now() + KEYS_RETRY;

    let mut app = make_app(&Output {
        size: (w, h),
        gpu_max_texture: display.gpu_max_texture(),
    });
    let mut canvas = Canvas::new(w, h);

    let start = Instant::now();
    let max = (cfg.runtime.max_seconds > 0).then(|| Duration::from_secs(cfg.runtime.max_seconds));
    let stats_every = Duration::from_secs(cfg.runtime.stats_interval_secs.max(1));
    let mut next_stats = Instant::now() + stats_every;
    let (mut worst_ms, mut frames) = (0u128, 0u64);
    // 区間ごとの CPU 使用率と、描画(app.render)・転送(CPU: dumb buffer への変換コピー、GLES: テクスチャ転送と page flip の完了待ち)の時間
    let mut self_cpu = miharashi_linux::sysinfo::SelfCpu::default();
    self_cpu.poll(Instant::now());
    let (mut render_ms, mut present_ms) = (Vec::new(), Vec::new());
    // 写真 1 枚ごとの、GPU で完成フレームを作った CPU 時間(PoC-005 PR3b)
    let mut photo_ms = Vec::new();
    // 動かし続ける画面(レーダー)の、前のフレームからの間隔。(フレーム数, 遅れた数, 前回の表示時刻)
    let (mut anim_frames, mut anim_slow, mut anim_last) = (0u32, 0u32, None::<Instant>);
    let mut redraw = true;
    let mut resume_pending = false;
    // 画面の電源。消している間は描かない(GLES は page flip が終わらないことがある)。
    // `power_tried` は最後に切り替えを試みた値。失敗しても同じ値では 20ms ごとに再試行しない
    // 操作パネルのスレッドからも手動オーバーライドを切り替えるので共有する
    let power = ScreenPower::start(&cfg.home_assistant, &cfg.screen_power).map(Arc::new);
    let mut screen_on = true;
    let mut power_tried = true;
    let mut published_display = DisplayStatus::default();
    // 手元の操作パネルの API。待ち受けられなくてもキオスクは動かし続け(状態ログは panel=error)、
    // PANEL_RETRY ごとに試し直す(起動直後で LAN の IP がまだ無いときなど)。同じ失敗は 1 回だけログに出す
    let mut panel: Option<Panel> = None;
    let mut panel_err: Option<String> = None;
    let mut panel_retry = Instant::now();

    // ループが描画の失敗で終わったときのエラー(終了処理のあとに返す)
    let mut run_err = None;
    let reason = loop {
        if term.load(Ordering::Relaxed) {
            break "シグナルを受信";
        }
        if max.is_some_and(|m| start.elapsed() >= m) {
            break "max_seconds に到達";
        }

        for ev in session.dispatch() {
            match ev {
                SessionEvent::Disabled => {
                    // VT が非アクティブ: 描画・入力・DRM master を手放してから応答する。
                    keys.close(&mut session);
                    display.suspend();
                    app.set_display_on(false, Instant::now());
                    session.ack_disable();
                    eprintln!("seat: 無効化(描画を停止)");
                }
                SessionEvent::Enabled => {
                    let n = keys.reopen(&mut session);
                    eprintln!("input: {n} デバイスを開き直した");
                    resume_pending = true;
                    eprintln!("seat: 有効化(復帰処理)");
                }
            }
        }
        if !session.is_enabled() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if resume_pending {
            match display.resume() {
                Ok(()) => {
                    resume_pending = false;
                    redraw = true;
                    eprintln!("seat: 表示を復元");
                    // CRTC を設定し直すと画面が点くので、消しておくべきなら消し直す
                    if !screen_on {
                        screen_on = true;
                        power_tried = true;
                    }
                    app.set_display_on(screen_on, Instant::now());
                }
                Err(e) => {
                    eprintln!("{e}(再試行)");
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }

        if keys.is_empty() && Instant::now() >= keys_retry {
            keys_retry = Instant::now() + KEYS_RETRY;
            // 入力デバイスが増減したときだけ開き直す(毎回開くと描画が数百 ms 止まる)
            if keys.reopen_if_changed(&mut session).is_some_and(|n| n > 0) {
                eprintln!("input: {:?}", keys.device_names());
            }
        }
        let mut quit = false;
        for a in keys.poll() {
            let key = match a {
                Action::Quit => {
                    quit = true;
                    break;
                }
                Action::Prev => KeyPress::Prev,
                Action::Select => KeyPress::Select,
                Action::Next => KeyPress::Next,
                Action::Up => KeyPress::Up,
                Action::Down => KeyPress::Down,
                Action::Menu => KeyPress::Menu,
            };
            eprintln!("key: {a:?}");
            if let Some(p) = &power {
                p.on_key();
            }
            // 消えている画面へのキーは点けるだけにする(見えないまま画面を切り替えない)
            if screen_on {
                redraw |= app.on_key(key);
            }
        }
        if quit {
            break "キー操作(Esc/q)";
        }
        if cfg.panel.enabled && panel.is_none() && Instant::now() >= panel_retry {
            panel_retry = Instant::now() + PANEL_RETRY;
            // モニターの切り替えは、HA の手動オーバーライド(mode エンティティ)があるときだけ
            let control = power
                .clone()
                .filter(|p| p.mode().is_some())
                .map(|p| p as Arc<dyn DisplayControl>);
            match Panel::start(&cfg.panel, control) {
                Ok(p) => {
                    eprintln!("panel: {} で待ち受け", p.addr());
                    published_display = display_status(screen_on, power.as_deref());
                    p.publish(app.panel_view(), published_display);
                    panel = Some(p);
                    panel_err = None;
                }
                Err(e) => {
                    if panel_err.as_ref() != Some(&e) {
                        eprintln!("panel: 起動できない(パネルなしで続け、再試行する): {e}");
                    }
                    panel_err = Some(e);
                }
            }
        }
        // パネルのキーもキーボードと同じに扱う(消えている画面では点けるだけ)
        for input in panel.iter().flat_map(Panel::inputs) {
            eprintln!("key: panel {input:?}");
            // 巡回の切り替えは画面を選ぶ操作ではない(`/api/v1/display` と同じ)。点けず、消えていても反映する
            if let PanelInput::Rotation(on) = input {
                redraw |= app.set_rotation(on);
                continue;
            }
            if let Some(p) = &power {
                p.on_key();
            }
            if screen_on {
                redraw |= match input {
                    PanelInput::Key(key) => app.on_key(key),
                    PanelInput::Screen(id) => app.show_screen(id),
                    PanelInput::Rotation(_) => unreachable!("上で扱った"),
                };
            }
        }
        let tick_t0 = Instant::now();
        redraw |= app.tick(tick_t0);
        let tick_ms = tick_t0.elapsed().as_secs_f32() * 1000.0;

        // アラートは画面を消していても点けて見せる(tick で届いたものを同じ周期で反映する)
        let want_on = app.wants_screen() || power.as_ref().is_none_or(|p| p.want_on());
        if want_on == screen_on {
            power_tried = screen_on;
        } else if want_on != power_tried {
            power_tried = want_on;
            match display.set_power(want_on) {
                Ok(()) => {
                    screen_on = want_on;
                    app.set_display_on(screen_on, Instant::now());
                    redraw |= want_on;
                    eprintln!("display: {}", if want_on { "点灯" } else { "消灯" });
                }
                Err(e) => eprintln!("display: 電源を切り替えられない: {e}"),
            }
        }

        // 画面を消していてもパネルの表示は追従させる(モニターの点灯・消灯も知らせる)
        let ds = display_status(screen_on, power.as_deref());
        // 消えているあいだのキーのラベル(点けるだけ / force_off なら何もしない)に使う。
        // パネルが無くても毎周期渡す(後からパネルを起動したときの最初の送信に間に合わせる)。
        // mode が変われば ds も変わるので、下でパネルへ送り直す
        app.set_display_forced_off(ds.mode == Some(PowerMode::ForceOff.as_str()));
        if let Some(p) = &panel
            && (redraw || ds != published_display)
        {
            p.publish(app.panel_view(), ds);
            published_display = ds;
        }

        let mut drew = false;
        if redraw && screen_on {
            let t0 = Instant::now();
            // 描画の失敗(下地・EGL・page flip など)でも、場面の解放を通る終了処理へ進む(`end_run`)
            let Some((r, p)) = draw_frame(&mut display, app.as_mut(), &mut canvas, &mut run_err)
            else {
                break "描画エラー";
            };
            render_ms.push(r);
            present_ms.push(p);
            photo_ms.extend(display.take_photo_build_ms());
            redraw = false;
            frames += 1;
            worst_ms = worst_ms.max(t0.elapsed().as_millis());
            // 60Hz で 1 枚飛ぶと間隔が 25ms を超える。どこで時間を使ったかを分けて残す(1 分に 10 件まで)
            drew = true;
            if app.animating() {
                let now = Instant::now();
                if let Some(last) = anim_last {
                    let gap = now.duration_since(last).as_secs_f32() * 1000.0;
                    anim_frames += 1;
                    // 間隔の上限があるときは、vsync 単位に切り上げた表示間隔に半フレームの余裕を足した値を超えたら遅れ
                    let limit = app
                        .frame_interval()
                        .map_or(25.0, |d| late_limit_ms(d, refresh_hz));
                    if gap > limit {
                        anim_slow += 1;
                        if anim_slow <= 10 {
                            eprintln!(
                                "anim: 遅れ {gap:.1}ms(tick {tick_ms:.1} / 描画 {r:.1} / 転送と表示待ち {p:.1})"
                            );
                        }
                    }
                }
                anim_last = Some(now);
            } else {
                anim_last = None;
            }
        }

        if Instant::now() >= next_stats {
            // 画面ロジックの値のあとに、PoC-005 の計測値を足す(既存の並びは変えない)
            let mut extra = app.stats();
            extra.push((
                "cpu_self",
                self_cpu
                    .poll(Instant::now())
                    .map_or("-".into(), |v| format!("{v:.1}")),
            ));
            extra.push(("render_ms", summarize_ms(&mut render_ms)));
            extra.push(("present_ms", summarize_ms(&mut present_ms)));
            extra.push(("photo_gpu_ms", summarize_ms(&mut photo_ms)));
            // レーダーのフレーム数と、間隔が 25ms を超えた数(PoC-005 PR4a)
            if anim_frames > 0 {
                extra.push(("anim", format!("{anim_frames}/{anim_slow}")));
                (anim_frames, anim_slow) = (0, 0);
            }
            if cfg.panel.enabled {
                extra.push(("panel", panel.as_ref().map_or("error", Panel::stat).into()));
            }
            eprintln!(
                "{}",
                format_stats(
                    start.elapsed().as_secs(),
                    rss_mb(),
                    fd_count(),
                    frames,
                    worst_ms,
                    &extra
                )
            );
            worst_ms = 0;
            next_stats += stats_every;
        }
        // 動かし続ける画面は、表示の完了待ち(vsync)だけで間隔をとる(20ms 待つと 30fps に落ち、
        // 背景を描き直すフレームで 1 枚飛んで引っかかって見える)
        // 間隔の上限がある画面(キューブ)は、描いた直後に残りを待つ
        match app.frame_interval().filter(|_| drew && screen_on) {
            Some(d) => std::thread::sleep(frame_wait(d, refresh_hz)),
            None if !(screen_on && app.animating()) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {}
        }
    };

    eprintln!("終了: {reason}");
    // 消したまま終わると、元のコンソールも見えなくなる
    if !screen_on && session.is_enabled() {
        let _ = display.set_power(true);
    }
    // 3D の GL オブジェクトは、表示を閉じる前に(GL が使えるうちに)消す
    let result = end_run(run_err, app.as_mut(), |scenes| {
        display.release_scenes(scenes)
    });
    // 表示(元の CRTC を復元)→ 入力 → デバイスの順に解放する。VT は VtGuard が TEXT に戻す。
    drop(display);
    keys.close(&mut session);
    session.close_device(drm_dev);
    drop(vt);
    result
}

/// 実行ループを抜けたあとの、場面の解放。`release_scenes` は GL が使えるうちに(表示を閉じる前に)呼ぶ。
/// 描画の失敗で抜けたとき(`run_err`)も呼ぶ。解放はベストエフォートで、panic はログに出すだけ。元のエラーを返す。
/// 3D が失敗した画面の場面は `app.gl_scenes()` に入らない(失敗したその場で `release` 済み)。
pub(crate) fn end_run(
    run_err: Option<String>,
    app: &mut dyn App,
    release_scenes: impl FnOnce(&mut [&mut dyn GlScene]) -> Vec<String>,
) -> Result<(), String> {
    for panic in release_scenes(&mut app.gl_scenes()) {
        eprintln!("3D: release が panic した: {panic}");
    }
    run_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn extra(ticks: u64, cpu: &str) -> Vec<(&'static str, String)> {
        vec![
            ("ticks", ticks.to_string()),
            ("keys", "0".into()),
            ("screen", "Status(Overview)".into()),
            ("photo", "1".into()),
            ("feed", "online".into()),
            ("cpu", cpu.into()),
            ("gpu", "Some(45.0)".into()),
        ]
    }

    #[test]
    fn stats_line_matches_legacy_format() {
        let line = format_stats(60, 81, 26, 62, 19, &extra(61, "Some(13.0)"));
        assert_eq!(
            line,
            "stats: t=60s rss=81MB fds=26 frames=62 worst_frame=19ms ticks=61 keys=0 screen=Status(Overview) photo=1 feed=online cpu=Some(13.0) gpu=Some(45.0)"
        );
        eprintln!("{line}");
    }

    /// 間隔の上限の待ちは、間隔を vsync の整数倍に切り上げて vsync 半分を引いた時間(1 vsync 以下なら 0)。
    #[test]
    fn frame_wait_is_the_interval_minus_half_a_vsync() {
        let w = frame_wait(Duration::from_secs_f64(1.0 / 30.0), 60);
        assert!(
            (w.as_secs_f64() - (1.0 / 30.0 - 1.0 / 120.0)).abs() < 1e-9,
            "{w:?}"
        );
        assert_eq!(frame_wait(Duration::from_millis(2), 60), Duration::ZERO);
        assert_eq!(frame_wait(Duration::from_millis(33), 0), Duration::ZERO);
    }

    /// 設定の fps と実際の表示間隔(待ち + vsync 半分 = vsync の整数倍)。上限を超えて速くならない。
    #[test]
    fn frame_wait_keeps_the_fps_cap_for_any_refresh_rate() {
        let shown = |fps: u32, hz: u32| -> f64 {
            let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
            let wait = frame_wait(interval, hz).as_secs_f64();
            // 待ち明けの直後の vsync で表示される(描画は一瞬で終わるとする。待ちが 0 なら毎 vsync)
            let vsync = 1.0 / f64::from(hz);
            (wait / vsync).ceil().max(1.0) * vsync
        };
        let fps = |f: u32, hz: u32| 1.0 / shown(f, hz);
        // (設定の fps, Hz, 実際の fps)
        for (set, hz, want) in [
            (30, 60, 30.0),
            (50, 60, 30.0),
            (60, 60, 60.0),
            (20, 60, 20.0),
            (45, 60, 30.0),
            (30, 75, 25.0),
            (25, 75, 25.0),
            (30, 144, 28.8),
            (60, 144, 48.0),
        ] {
            let got = fps(set, hz);
            assert!((got - want).abs() < 0.01, "{set}fps {hz}Hz: {got}");
            assert!(
                got <= f64::from(set) + 1e-6,
                "{set}fps {hz}Hz が上限を超えた: {got}"
            );
        }
        // 0(制限なし)は呼ばれないが、1 vsync 以下の間隔は待たない
        assert_eq!(
            frame_wait(Duration::from_secs_f64(1.0 / 120.0), 60),
            Duration::ZERO
        );
        assert_eq!(
            frame_wait(Duration::from_secs_f64(1.0 / 60.0), 60),
            Duration::ZERO
        );
    }

    /// 遅れの閾値は切り上げ後の表示間隔 + 半フレーム。正常な間隔は遅れにならず、1 vsync 落ちた間隔は遅れになる。
    #[test]
    fn late_limit_follows_the_displayed_interval() {
        // (設定の fps, Hz, 正常な表示間隔 [ms])
        for (set, hz, shown_ms) in [
            (30, 60, 1000.0 / 30.0),
            (50, 60, 1000.0 / 30.0),
            (60, 60, 1000.0 / 60.0),
            (30, 75, 40.0),
            (60, 144, 1000.0 / 48.0),
        ] {
            let limit = late_limit_ms(Duration::from_secs_f64(1.0 / f64::from(set)), hz);
            let vsync_ms = 1000.0 / hz as f32;
            assert!(
                shown_ms <= limit,
                "{set}fps {hz}Hz: 正常な間隔が遅れになる {limit}"
            );
            assert!(
                shown_ms + vsync_ms > limit,
                "{set}fps {hz}Hz: 1 vsync 落ちても遅れにならない {limit}"
            );
        }
        // 従来の 60Hz・30fps と同じ
        let old = 1000.0 / 30.0 + 500.0 / 60.0;
        let l = late_limit_ms(Duration::from_secs_f64(1.0 / 30.0), 60);
        assert!((l - old).abs() < 0.01, "{l}");
    }

    #[test]
    fn summarize_ms_gives_avg_and_p95() {
        let mut v: Vec<f32> = (1..=20).map(|x| x as f32).collect();
        assert_eq!(summarize_ms(&mut v), "10.5/19.0");
        assert!(v.is_empty(), "区間ごとに空にする");
        assert_eq!(summarize_ms(&mut v), "-");
        assert_eq!(summarize_ms(&mut vec![4.0]), "4.0/4.0");
    }

    #[test]
    fn stats_lines_are_readable_by_soak_summary() {
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/soak_summary.py");
        if Command::new("python3").arg("--version").output().is_err() {
            eprintln!("python3 がないので skip");
            return;
        }
        let prefix = "2026-10-13T08:00:00+09:00 host miharashi[1]: ";
        let mut text = String::new();
        for i in 0..3u64 {
            // 今の並び: 画面ロジックの値(aircraft を含む)+ gpus + PoC-005 の計測値 + パネル(有効なときだけ)
            let mut e = extra(61 + 60 * i, "Some(13.0)");
            e.insert(4, ("aircraft", "false".to_string()));
            e.extend([
                ("gpus", "GPU0:0,GPU1:0".to_string()),
                ("cpu_self", format!("{}.5", i + 1)),
                ("render_ms", "12.0/20.0".to_string()),
                ("present_ms", "3.0/4.0".to_string()),
                ("panel", "idle".to_string()),
            ]);
            let line = format_stats(60 * (i + 1), 81, 26, 62 + i, 19, &e);
            text.push_str(&format!("{prefix}{line}\n"));
        }
        let path = std::env::temp_dir().join(format!("miharashi-soak-{}.log", std::process::id()));
        std::fs::write(&path, text).unwrap();
        let out = Command::new("python3")
            .arg(&script)
            .stdin(Stdio::from(std::fs::File::open(&path).unwrap()))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(stdout.contains("stats 3 行"), "{stdout}");
        assert!(stdout.contains("CPU(自プロセス, %)"), "{stdout}");
    }
}
