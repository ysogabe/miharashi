//! `vrm-view`: VRM 1.0 モデルを実際のモニター(DRM/KMS)に表示する PoC(Issue #71 B の下調べ)。
//! `--photos` で、写真画面と同じ GPU 経路の写真を背景に描き、その上の領域(`--region`)にバストアップなど
//! (`--frame`)のキャラクターを重ねて、身振り(`--gesture`)と口パク(`--lipsync`)の負荷を測れる。
//! `--agents` なら、写真の代わりに T3 Agent 画面のカード(合成のダミー。`Frame::Agents` の GPU 経路)を背景にして、
//! 同じようにキャラクターを重ねる。`--follow-cards` で、フォーカスが移ったカードの方へ視線と指さしを向ける。
//! キオスク(miharashi.service)とは DRM master を取り合うので、止めてから実行する。libseat は使わず、
//! カードを直接 open する。終了(`--seconds` の経過、SIGINT/SIGTERM)で元の画面に戻る。
//! モデルは利用条件を確認して各自で用意する(リポジトリには置かない)。`--synthetic` なら合成モデル。
//! `--record <out.mp4>` を付けると、実モニター(DRM/KMS)を使わず、render node のオフスクリーンに同じ場面を
//! 固定刻み(1/`--fps` 秒)で描いて `ffmpeg` で動画にする(紹介動画やレビュー用。キオスクを止めなくてよい)。
//! この録画モードの値は計測に使わない。
use glow::HasContext;
use miharashi_gpu::compose::Compositor;
use miharashi_gpu::{GlesDisplay, Offscreen};
use miharashi_linux::drm_select::{self, CardSel, ModeSel};
use miharashi_linux::sysinfo::SelfCpu;
use miharashi_render::TextEngine;
use miharashi_vrm::agent_stage::{AgentStage, AimTracker, StageStats};
use miharashi_vrm::demo::FaceSet;
use miharashi_vrm::photo_stage::{self, Slideshow};
use miharashi_vrm::record::{self, Codec};
use miharashi_vrm::synth;
use miharashi_vrm::{Framing, Level, Model, Renderer, StageRegion, VrmLayer, demo};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const USAGE: &str = "使い方: vrm-view (--model <path.vrm> | --synthetic) [--card driver:amdgpu] [--connector auto] \
[--mode preferred] [--seconds 60] [--expression blink] [--fps 30] [--feature-level spring] [--outline-scale 1] \
[--frame full|waist|bust] [--region left|center|right|full] [--gesture] [--lipsync] \
[--photos <dir|synthetic>] [--photo-interval 10] \
[--agents [--cards 8] [--card-interval 6] [--follow-cards]] [--no-vrm] \
[--record <out.mp4> [--size 2560x1080] [--pci 0000:10:00.0] [--record-codec libx264|h264_vaapi] [--record-overwrite]]";

/// fps などを標準エラーに出す間隔
const STATS_EVERY: Duration = Duration::from_secs(5);

struct Args {
    model: Option<PathBuf>,
    card: String,
    connector: String,
    mode: String,
    seconds: f64,
    expression: String,
    /// 指定すると、フレームの間隔をこの fps に保つ(既定は vsync のまま)
    fps: Option<f64>,
    level: Level,
    outline_scale: f32,
    frame: Framing,
    region: StageRegion,
    gesture: bool,
    lipsync: bool,
    /// 背景に写真を描く。`synthetic` か、JPEG の入ったディレクトリ(なければ合成写真)
    photos: Option<String>,
    photo_interval: f64,
    /// キャラクターを描かず、背景(写真かカード)だけを描く(VRM の上乗せを出すための基準)
    no_vrm: bool,
    /// 背景に T3 Agent 画面のカード(合成のダミー)を描く
    agents: bool,
    /// ダミーのカードの枚数
    cards: usize,
    /// ダミーのカードの状態が変わる間隔 [秒]
    card_interval: f64,
    /// フォーカスが移ったカードの方へ視線と指さしを向ける
    follow_cards: bool,
    /// 実モニターを使わず、この動画ファイルに書き出す(オフライン描画。`--fps` の既定は 30)
    record: Option<PathBuf>,
    /// 録画の画面の大きさ
    size: (u32, u32),
    /// 録画で描く GPU の PCI アドレス(render node)
    pci: String,
    record_codec: Codec,
    record_overwrite: bool,
    /// `--size` `--pci` `--record-codec` `--record-overwrite` のどれかが指定された(`--record` なしでは誤り)
    record_opts: bool,
}

/// "2560x1080" を (幅, 高さ) にする。
fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("--size は 幅x高さ(例 2560x1080)")?;
    let w: u32 = w.parse().map_err(|_| "--size の幅が整数でない")?;
    let h: u32 = h.parse().map_err(|_| "--size の高さが整数でない")?;
    if w < 64 || h < 64 || w > 4096 || h > 4096 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err("--size は 64〜4096 の偶数(yuv420p のため)".into());
    }
    Ok((w, h))
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        model: None,
        card: "driver:amdgpu".into(),
        connector: "auto".into(),
        mode: "preferred".into(),
        seconds: 60.0,
        expression: "blink".into(),
        fps: None,
        level: Level::Spring,
        outline_scale: 1.0,
        frame: Framing::Full,
        region: StageRegion::Full,
        gesture: false,
        lipsync: false,
        photos: None,
        photo_interval: 10.0,
        no_vrm: false,
        agents: false,
        cards: 8,
        card_interval: 6.0,
        follow_cards: false,
        record: None,
        size: (2560, 1080),
        pci: "0000:10:00.0".into(),
        record_codec: Codec::X264,
        record_overwrite: false,
        record_opts: false,
    };
    let mut synthetic = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} に値がない\n{USAGE}"));
        match flag.as_str() {
            "--model" => a.model = Some(value()?.into()),
            "--synthetic" => synthetic = true,
            "--card" => a.card = value()?,
            "--connector" => a.connector = value()?,
            "--mode" => a.mode = value()?,
            "--seconds" => a.seconds = value()?.parse().map_err(|_| "--seconds が数でない")?,
            "--expression" => a.expression = value()?,
            "--fps" => a.fps = Some(value()?.parse().map_err(|_| "--fps が数でない")?),
            "--feature-level" => a.level = Level::parse(&value()?)?,
            "--outline-scale" => {
                a.outline_scale = value()?.parse().map_err(|_| "--outline-scale が数でない")?
            }
            "--frame" => a.frame = Framing::parse(&value()?)?,
            "--region" => a.region = StageRegion::parse(&value()?)?,
            "--gesture" => a.gesture = true,
            "--lipsync" => a.lipsync = true,
            "--photos" => a.photos = Some(value()?),
            "--photo-interval" => {
                a.photo_interval = value()?
                    .parse()
                    .map_err(|_| "--photo-interval が数でない")?
            }
            "--no-vrm" => a.no_vrm = true,
            "--agents" => a.agents = true,
            "--cards" => a.cards = value()?.parse().map_err(|_| "--cards が整数でない")?,
            "--card-interval" => {
                a.card_interval = value()?.parse().map_err(|_| "--card-interval が数でない")?
            }
            "--follow-cards" => a.follow_cards = true,
            "--record" => a.record = Some(value()?.into()),
            "--size" => {
                a.size = parse_size(&value()?)?;
                a.record_opts = true;
            }
            "--pci" => {
                a.pci = value()?;
                a.record_opts = true;
            }
            "--record-codec" => {
                a.record_codec = Codec::parse(&value()?)?;
                a.record_opts = true;
            }
            "--record-overwrite" => {
                a.record_overwrite = true;
                a.record_opts = true;
            }
            _ => return Err(format!("不明な引数 {flag}\n{USAGE}")),
        }
    }
    if a.agents && a.photos.is_some() {
        return Err("--agents と --photos は一緒に使えない(背景はどちらか一方)".into());
    }
    if a.no_vrm && a.photos.is_none() && !a.agents {
        return Err("--no-vrm は --photos か --agents と一緒に使う(背景だけを描く)".into());
    }
    if a.follow_cards && !a.agents {
        return Err("--follow-cards は --agents と一緒に使う".into());
    }
    if a.follow_cards && a.no_vrm {
        return Err("--follow-cards は VRM を描くとき(--no-vrm なし)だけ".into());
    }
    // カードに合わせて腕を動かすには、身振りが要る
    a.gesture |= a.follow_cards;
    if !(1..=24).contains(&a.cards) {
        return Err("--cards は 1〜24".into());
    }
    if !a.card_interval.is_finite() || !(0.5..=3600.0).contains(&a.card_interval) {
        return Err("--card-interval は 0.5〜3600 の数".into());
    }
    if !a.photo_interval.is_finite() || !(1.0..=3600.0).contains(&a.photo_interval) {
        return Err("--photo-interval は 1〜3600 の数".into());
    }
    if a.model.is_none() && !synthetic && !a.no_vrm {
        return Err(format!("--model か --synthetic が要る\n{USAGE}"));
    }
    if a.seconds.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err("--seconds は 0 より大きい数".into());
    }
    if a.fps
        .is_some_and(|f| f.partial_cmp(&1.0).is_none_or(|o| o.is_lt()))
    {
        return Err("--fps は 1 以上の数".into());
    }
    if a.record.is_none() && a.record_opts {
        return Err(
            "--size / --pci / --record-codec / --record-overwrite は --record と一緒に使う".into(),
        );
    }
    if a.record.is_some() && a.fps.is_some_and(|f| f > 240.0) {
        return Err("--fps は 240 以下(録画)".into());
    }
    Ok(a)
}

/// 発行時間(壁時計)の内訳 [ms]の合計(窓ごと・全体)。
#[derive(Default, Clone)]
struct Sum {
    frames: u32,
    frame: Duration,
    pose: f64,
    spring: f64,
    /// 表情の合成(`set_expressions` + 頂点の合成・転送)
    face: f64,
    /// 背景の合成(`Compositor::draw` の命令の発行まで。写真は1 枚目の完成フレーム作りを含む)
    compose: f64,
    /// VRM の描画命令の発行(層への描画と合成)
    vrm_draw: f64,
    /// Agent 画面の組み立て(配置・pose・カードの面と背景の CPU 描画。転送の準備まで)
    cards: f64,
    /// 写真 1 枚ごとの「完成フレームを作る」CPU 時間
    builds: Vec<f32>,
    /// `--agents`: カードの面・背景の描き直しと、GPU へ転送した量
    stage: StageStats,
    uploaded: u64,
}

impl Sum {
    fn add(&mut self, o: &Sum) {
        self.frames += o.frames;
        self.frame += o.frame;
        self.pose += o.pose;
        self.spring += o.spring;
        self.face += o.face;
        self.compose += o.compose;
        self.vrm_draw += o.vrm_draw;
        self.cards += o.cards;
        self.builds.extend_from_slice(&o.builds);
        self.stage.add(&o.stage);
        self.uploaded += o.uploaded;
    }

    fn line(&self, secs: f64, cpu: Option<f32>, agents: bool) -> String {
        let n = self.frames.max(1) as f64;
        let build = if self.builds.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{}枚 平均{:.1}/最大{:.1}ms",
                self.builds.len(),
                self.builds.iter().sum::<f32>() / self.builds.len() as f32,
                self.builds.iter().copied().fold(0.0, f32::max)
            )
        };
        let mut line = format!(
            "{:.1} fps  frame {:.2}ms  cpu {}  | 発行時間 ms(壁時計): pose {:.3} spring {:.3} face {:.3} compose {:.3} vrm_draw {:.3}",
            self.frames as f64 / secs,
            self.frame.as_secs_f64() * 1e3 / n,
            cpu.map_or("-".into(), |c| format!("{c:.1}%")),
            self.pose / n,
            self.spring / n,
            self.face / n,
            self.compose / n,
            self.vrm_draw / n,
        );
        if agents {
            let st = &self.stage;
            let avg = |ms: f64, k: u32| if k == 0 { 0.0 } else { ms / k as f64 };
            line += &format!(
                " cards {:.3}  | カード面 {}回(1回 {:.2}ms) 背景 {}回(1回 {:.2}ms) 組み立て {}フレーム 転送 {:.1}MB",
                self.cards / n,
                st.cards_redrawn,
                avg(st.cards_ms, st.cards_redrawn),
                st.bg_redrawn,
                avg(st.bg_ms, st.bg_redrawn),
                st.scenes,
                self.uploaded as f64 / 1e6,
            );
        } else {
            line += &format!("  | 写真の準備 {build}");
        }
        line
    }
}

/// VRM を描くのに要るもの(`--no-vrm` では持たない)。
struct Vrm {
    renderer: Renderer,
    layer: VrmLayer,
    faces: FaceSet,
}

/// 画面の背景(VRM の下に描くもの)。
enum Backdrop {
    /// 暗い単色(従来)
    None,
    Photo(Box<Slideshow>),
    /// T3 Agent 画面のカード(合成のダミー)。文字を描く `TextEngine` を持つ
    Agents(Box<AgentStage>, Box<TextEngine>),
}

/// 構図ごとの、頭の画面上の位置(領域の上端からの割合)。視線と指さしの向きの起点にする近似。
fn head_fraction(frame: Framing) -> f32 {
    match frame {
        Framing::Bust => 0.22,
        Framing::Waist => 0.17,
        Framing::Full => 0.10,
    }
}

/// 画面(または録画)を始める前に読むもの。
struct Assets {
    model: Option<Model>,
    photos: Option<Vec<image::RgbImage>>,
    text: Option<Box<TextEngine>>,
}

fn load_assets(a: &Args) -> Result<Assets, String> {
    let model = if a.no_vrm {
        None
    } else {
        let t_load = Instant::now();
        let model = match &a.model {
            Some(p) => Model::from_path(p)?,
            None => Model::from_glb(&synth::build_vrm_glb(&synth::Opts::default()))?,
        };
        let st = model.stats();
        eprintln!(
            "model: {} (VRM {}) meshes={} vertices={} triangles={} textures={} morph_targets={} ({:.0}ms)",
            a.model
                .as_deref()
                .map_or("synthetic".into(), |p| p.display().to_string()),
            model.vrm.spec_version,
            st.meshes,
            st.vertices,
            st.triangles,
            st.images.len(),
            st.morph_targets,
            t_load.elapsed().as_secs_f64() * 1e3
        );
        if model.vrm.expression(&a.expression).is_none() {
            return Err(format!("expression {} がない", a.expression));
        }
        if a.lipsync && FaceSet::of(&model.vrm).no_vowels() {
            return Err("口パクに使う表情(aa/ih/ou/ee/oh)がモデルにない".into());
        }
        Some(model)
    };
    let photos = match &a.photos {
        Some(p) => {
            let t = Instant::now();
            let dir = (p != "synthetic").then(|| Path::new(p));
            let (imgs, jpegs) = photo_stage::load(dir)?;
            eprintln!(
                "photos: {}枚({}) {:.0}ms",
                imgs.len(),
                if jpegs == 0 {
                    "合成写真".to_string()
                } else {
                    format!("JPEG {jpegs} 枚")
                },
                t.elapsed().as_secs_f64() * 1e3
            );
            Some(imgs)
        }
        None => None,
    };
    let text = a
        .agents
        .then(|| Box::new(TextEngine::new("Noto Sans CJK JP")));
    Ok(Assets {
        model,
        photos,
        text,
    })
}

/// VRM を描く Renderer と、領域の大きさの層を作る(`--no-vrm` なら None)。失敗したら作った分は消す。
fn init_vrm(
    gl: &glow::Context,
    model: Option<Model>,
    a: &Args,
    region_size: (u32, u32),
) -> Result<Option<Vrm>, String> {
    unsafe {
        eprintln!(
            "gl: GL_RENDERER={} GL_VERSION={}",
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VERSION)
        );
    }
    let Some(model) = model else { return Ok(None) };
    let faces = FaceSet::of(&model.vrm);
    let layer = VrmLayer::new(gl, region_size)?;
    match Renderer::with_level(gl, model, a.level) {
        Ok(mut r) => {
            r.outline_scale = a.outline_scale;
            // 透明な層に描いて、写真(または暗い背景)に重ねる
            r.clear_color = [0.0; 4];
            if a.gesture {
                if let Err(e) = r.set_gesture(true) {
                    r.destroy(gl);
                    layer.destroy(gl);
                    return Err(e);
                }
                let (arms, fingers) = r.rig().gesture_bones();
                eprintln!("gesture: 腕 {arms} 本、指の関節 {fingers} 個");
            }
            Ok(Some(Vrm {
                renderer: r,
                layer,
                faces,
            }))
        }
        Err(e) => {
            layer.destroy(gl);
            Err(e)
        }
    }
}

fn make_backdrop(
    a: &Args,
    photos: Option<Vec<image::RgbImage>>,
    text: Option<Box<TextEngine>>,
    size: (u32, u32),
    now0: Instant,
) -> Backdrop {
    match (photos, text) {
        (Some(imgs), _) => Backdrop::Photo(Box::new(Slideshow::new(
            imgs,
            size,
            Duration::from_secs_f64(a.photo_interval),
            now0,
        ))),
        (None, Some(text)) => {
            let stage = AgentStage::new(
                size,
                a.cards,
                Duration::from_secs_f64(a.card_interval),
                now0,
            );
            Backdrop::Agents(Box::new(stage), text)
        }
        (None, None) => Backdrop::None,
    }
}

/// Agent 画面なら、配置を一度決めて、キャラクターの領域(`rx, ry, rw, rh`)との重なりを知らせる。
fn report_agents(a: &Args, backdrop: &mut Backdrop, now0: Instant, rect: (u32, u32, u32, u32)) {
    let (rx, ry, rw, rh) = rect;
    if let Backdrop::Agents(stage, text) = backdrop {
        stage.update(now0, text);
        let region = (rx as f32, ry as f32, rw as f32, rh as f32);
        let (any, half, all) = stage.overlap(region);
        let f = stage.focus_rect();
        let ov = ((region.0 + region.2).min(f.0 + f.2) - region.0.max(f.0)).max(0.0);
        eprintln!(
            "agents: カード {all} 枚 / プロジェクト {} 個。領域 {} と重なるカード {any} 枚(半分以上隠れる {half} 枚)、\
拡大カード {:.0},{:.0} {:.0}x{:.0}(領域と重なる幅 {ov:.0}px)",
            stage.project_count(),
            a.region.name(),
            f.0,
            f.1,
            f.2,
            f.3,
        );
    }
}

fn run(a: &Args) -> Result<(), String> {
    let term = Arc::new(AtomicBool::new(false));
    // 1 回目はフラグで正常終了(画面を元に戻す)、2 回目は即終了。条件付きの登録を先にすること
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register_conditional_shutdown(sig, 128 + sig, term.clone())
            .map_err(|e| format!("signal: {e}"))?;
        signal_hook::flag::register(sig, term.clone()).map_err(|e| format!("signal: {e}"))?;
    }

    // 録画の出力先は、重いモデルの読み込みの前に確かめる(既存ファイルなどはすぐ分かるように)
    if let Some(out) = &a.record {
        record::check_output(out, a.record_overwrite)?;
    }
    // モデルと写真は画面を取る前に読む(読めなければキオスクの画面を触らずに終わる)
    let assets = load_assets(a)?;
    if a.record.is_some() {
        return record_run(a, assets, &term);
    }
    let Assets {
        model,
        photos,
        text,
    } = assets;

    // 表示先を決めて、カードを直接 open する
    let cards = drm_select::scan();
    let card = drm_select::choose_card(&cards, &CardSel::parse(&a.card))?;
    let connector = drm_select::choose_connector(card, &a.connector)?;
    eprintln!(
        "display: {} driver={} pci={} connector={}",
        card.path.display(),
        card.driver,
        card.pci.as_deref().unwrap_or("-"),
        connector.name
    );
    let mode = ModeSel::parse(&a.mode)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&card.path)
        .map_err(|e| format!("{}: {e}", card.path.display()))?;
    let mut display = GlesDisplay::open(file.into(), &connector.name, mode)?;
    let size = display.size();
    eprintln!(
        "mode: {}x{}@{}Hz  gles: {}",
        size.0,
        size.1,
        display.refresh(),
        display.describe()
    );
    let (rx, ry, rw, rh) = a.region.rect(size);
    eprintln!(
        "composition: frame={} region={} ({rw}x{rh} at {rx},{ry}) gesture={} lipsync={} photos={} agents={} follow={} vrm={}",
        a.frame.name(),
        a.region.name(),
        a.gesture,
        a.lipsync,
        a.photos.as_deref().unwrap_or("なし"),
        if a.agents {
            a.cards.to_string()
        } else {
            "なし".into()
        },
        a.follow_cards,
        if a.no_vrm { "なし" } else { "あり" },
    );

    // GL の初期化(Renderer と描画先)
    let init = display.with_gl(|gl, _| init_vrm(gl, model, a, (rw, rh)));
    let mut vrm = init?;
    let now0 = Instant::now();
    let mut backdrop = make_backdrop(a, photos, text, size, now0);
    report_agents(a, &mut backdrop, now0, (rx, ry, rw, rh));

    // 描画ループ。エラーでも後始末(destroy → GlesDisplay の drop)を通るよう、結果を持ち越す
    let result = view_loop(a, &mut display, vrm.as_mut(), &mut backdrop, &term);

    display.with_gl(|gl, _| {
        if let Some(v) = vrm {
            v.renderer.destroy(gl);
            v.layer.destroy(gl);
        }
        unsafe { gl.finish() };
    });
    // display の drop で元の CRTC に戻る
    drop(display);
    result
}

/// 構図から決まる、領域と視線・指さしの起点。
struct Geom {
    /// キャラクターの領域(画素。左下原点の `StageRegion::rect`)
    rect: (u32, u32, u32, u32),
    /// 視線と指さしの起点(頭の画面上の位置)
    head: (f32, f32),
    /// 画面の面までの仮想の距離
    depth: f32,
    /// 全身・全画面・背景なし・身振りなしのときだけ、従来どおりカメラを左右に振る
    sway: bool,
}

fn geom(a: &Args, size: (u32, u32), backdrop: &Backdrop) -> Geom {
    let rect = a.region.rect(size);
    let (rx, ry, rw, rh) = rect;
    Geom {
        rect,
        head: (
            rx as f32 + rw as f32 / 2.0,
            ry as f32 + rh as f32 * head_fraction(a.frame),
        ),
        depth: size.1 as f32 * 0.5,
        sway: a.frame == Framing::Full
            && a.region == StageRegion::Full
            && !a.gesture
            && matches!(backdrop, Backdrop::None),
    }
}

/// 時刻 `t` [秒](`now` はその時刻の `Instant`)に、視線と指さし・身振り・SpringBone を進める。
fn pose_vrm(
    a: &Args,
    v: &mut Vrm,
    backdrop: &Backdrop,
    tracker: &mut AimTracker,
    g: &Geom,
    now: Instant,
    (t, dt): (f32, f32),
) {
    if let (true, Backdrop::Agents(stage, _)) = (a.follow_cards, backdrop) {
        // フォーカス中のカードの方へ視線と指さしを向ける(重みは滑らかに出し入れ)
        let aim = tracker.step(now, stage.aim_target(), g.head, g.depth);
        v.renderer.set_aim(aim);
    }
    v.renderer.animate(t, dt);
}

/// 時刻 `t` の表情(口の母音・まばたき・笑顔)を設定する。
fn face_vrm(
    a: &Args,
    v: &mut Vrm,
    t: f32,
    face: &mut Vec<(&'static str, f32)>,
) -> Result<(), String> {
    if a.lipsync {
        // 口の母音・まばたき・笑顔を同時に合成する
        demo::face(t, &v.faces, face);
        v.renderer.set_expressions(face)
    } else {
        v.renderer
            .set_expression(&a.expression, demo::expression_weight(&a.expression, t))
    }
}

/// 層へ描いて、背景(または暗い単色)の上の領域に重ねる(`target` は画面なら None)。
/// 戻りは (更新の内訳, 描画命令の発行 [ms])。
fn draw_vrm(
    gl: &glow::Context,
    v: &mut Vrm,
    a: &Args,
    g: &Geom,
    target: Option<glow::Framebuffer>,
    yaw: f32,
    with_bg: bool,
) -> (miharashi_vrm::UpdateTimes, f64) {
    let (rx, ry, rw, rh) = g.rect;
    let times = v.renderer.update(gl);
    let t_draw = Instant::now();
    if with_bg {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            gl.clear_color(0.08, 0.09, 0.12, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }
    let cam = v.renderer.framing_camera(a.frame, (rw, rh), yaw);
    v.layer.bind(gl);
    v.renderer.render_camera(gl, (rw, rh), &cam.0, cam.1);
    v.layer.composite(gl, target, (rx as i32, ry as i32));
    (times, t_draw.elapsed().as_secs_f64() * 1e3)
}

fn view_loop(
    a: &Args,
    display: &mut GlesDisplay,
    mut vrm: Option<&mut Vrm>,
    backdrop: &mut Backdrop,
    term: &AtomicBool,
) -> Result<(), String> {
    let size = display.size();
    let g = geom(a, size, backdrop);
    let mut tracker = AimTracker::default();
    let start = Instant::now();
    let run_for = Duration::from_secs_f64(a.seconds);
    let mut cpu = SelfCpu::default();
    let mut cpu_total = SelfCpu::default();
    cpu.poll(start);
    cpu_total.poll(start);
    let mut next_stats = start + STATS_EVERY;
    let (mut win, mut total) = (Sum::default(), Sum::default());
    let mut last = Instant::now();
    let mut total_frames = 0u64;
    let mut prev_t = 0.0f32;
    let mut prev_uploaded = display.agent_uploaded();
    let mut face: Vec<(&'static str, f32)> = Vec::new();
    let agents = matches!(backdrop, Backdrop::Agents(..));
    while !term.load(Ordering::Relaxed) && start.elapsed() < run_for {
        if let Some(fps) = a.fps {
            // 次のフレームの時刻まで待つ(遅れていれば待たない)。そのあと present が vsync を待つ
            let next = start + Duration::from_secs_f64(total_frames as f64 / fps);
            // SIGTERM に反応できるよう、100ms ずつに分けて待つ
            while !term.load(Ordering::Relaxed) {
                let Some(d) = next.checked_duration_since(Instant::now()) else {
                    break;
                };
                std::thread::sleep(d.min(Duration::from_millis(100)));
            }
        }
        let now = Instant::now();
        let t = (now - start).as_secs_f32();
        let t0 = now;
        let mut sum = Sum::default();
        // 背景を進める(Agent 画面は、変化のあるフレームだけ組み立てる)
        match &mut *backdrop {
            Backdrop::Photo(s) => {
                s.update(now);
            }
            Backdrop::Agents(stage, text) => {
                let c0 = Instant::now();
                stage.update(now, text);
                sum.cards = c0.elapsed().as_secs_f64() * 1e3;
                sum.stage = stage.take_stats();
            }
            Backdrop::None => {}
        }
        if let Some(v) = vrm.as_deref_mut() {
            pose_vrm(a, v, backdrop, &mut tracker, &g, now, (t, t - prev_t));
            let f0 = Instant::now();
            face_vrm(a, v, t, &mut face)?;
            sum.face = f0.elapsed().as_secs_f64() * 1e3;
        }
        prev_t = t;
        let yaw = if g.sway { demo::camera_yaw(t) } else { 0.0 };
        let enter = Instant::now();
        // 層へ描いて、背景(または暗い単色)の上の領域に重ねる。GL を借りた中で測る
        let mut draw = |gl: &glow::Context, sum: &mut Sum, with_bg: bool| {
            let Some(v) = vrm.as_deref_mut() else { return };
            let (times, vrm_ms) = draw_vrm(gl, v, a, &g, None, yaw, with_bg);
            sum.pose = times.pose_ms;
            sum.spring = times.spring_ms;
            sum.face += times.morph_ms;
            sum.vrm_draw = vrm_ms;
        };
        // present が page flip の完了まで待つので、フレームの間隔は vsync でとれる
        let agent_cards = match &*backdrop {
            Backdrop::Agents(stage, _) => stage.gpu_cards(),
            _ => Vec::new(),
        };
        let frame = match &*backdrop {
            Backdrop::Photo(s) => Some(s.frame(now)),
            Backdrop::Agents(stage, _) => Some(stage.frame(&agent_cards)),
            Backdrop::None => None,
        };
        match frame {
            Some(frame) => {
                // --no-vrm も同じ present_frame_with で、flip の前に compose を測る(draw_vrm は VRM がなければ何もしない)
                display.present_frame_with(&frame, |gl, _| {
                    sum.compose = enter.elapsed().as_secs_f64() * 1e3;
                    draw(gl, &mut sum, false);
                })?;
            }
            None => {
                display.present_with(|gl, _| draw(gl, &mut sum, true))?;
            }
        }
        drop(agent_cards);
        match &mut *backdrop {
            Backdrop::Photo(s) => s.mark_sent(),
            Backdrop::Agents(stage, _) => stage.mark_sent(),
            Backdrop::None => {}
        }
        sum.builds = display.take_build_ms();
        let uploaded = display.agent_uploaded();
        sum.uploaded = uploaded - prev_uploaded;
        prev_uploaded = uploaded;
        sum.frame = t0.elapsed();
        sum.frames = 1;
        win.add(&sum);
        total_frames += 1;
        let now = Instant::now();
        if now >= next_stats {
            let dt = now.duration_since(last).as_secs_f64();
            eprintln!("{}", win.line(dt, cpu.poll(now), agents));
            total.add(&win);
            win = Sum::default();
            last = now;
            next_stats = now + STATS_EVERY;
        }
    }
    total.add(&win);
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "終了: {total_frames} フレーム / {secs:.1}秒{}",
        if term.load(Ordering::Relaxed) {
            "(シグナル)"
        } else {
            ""
        }
    );
    eprintln!(
        "全体の平均: {}",
        total.line(secs, cpu_total.poll(Instant::now()), agents)
    );
    Ok(())
}

/// 録画の進み具合を出す間隔
const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// `--record`: 実モニターを使わず、render node のオフスクリーンに固定刻みで描いて ffmpeg に流す。
/// 各フレームの時刻 = フレーム番号 / fps(実時間とは無関係)。写真のフェード・カードの動き・身振り・口パク・
/// SpringBone は、すべてこの仮想時刻で進む。画面の合成は実モニターと同じ `draw_frame_with` を通す。
fn record_run(a: &Args, assets: Assets, term: &AtomicBool) -> Result<(), String> {
    let out = a.record.as_ref().ok_or("--record がない")?;
    let fps = a.fps.unwrap_or(30.0);
    let size = a.size;
    let total = ((a.seconds * fps).round() as u64).max(1);
    let node = miharashi_gpu::render_node_for_pci(&a.pci);
    let off = Offscreen::open(&node)?;
    let (rx, ry, rw, rh) = a.region.rect(size);
    eprintln!(
        "record: {} -> {}x{} @{fps}fps {total} フレーム({:.1}秒) codec={:?}{}  gles: {}",
        node.display(),
        size.0,
        size.1,
        total as f64 / fps,
        a.record_codec,
        if a.record_overwrite { " 上書き" } else { "" },
        off.describe()
    );
    eprintln!(
        "composition: frame={} region={} ({rw}x{rh} at {rx},{ry}) gesture={} lipsync={} photos={} agents={} follow={} vrm={}",
        a.frame.name(),
        a.region.name(),
        a.gesture,
        a.lipsync,
        a.photos.as_deref().unwrap_or("なし"),
        if a.agents {
            a.cards.to_string()
        } else {
            "なし".into()
        },
        a.follow_cards,
        if a.no_vrm { "なし" } else { "あり" },
    );
    let settings = record::Settings {
        out: out.clone(),
        size,
        fps,
        codec: a.record_codec,
        overwrite: a.record_overwrite,
        vaapi_device: node,
    };
    let Assets {
        model,
        photos,
        text,
    } = assets;
    let wall = Instant::now();
    let result = off.with_target(size, |gl, fbo| -> Result<(u64, bool), String> {
        let mut vrm = init_vrm(gl, model, a, (rw, rh))?;
        // 仮想時刻の基準。以後の時刻は base + フレーム番号 / fps
        let base = Instant::now();
        let mut backdrop = make_backdrop(a, photos, text, size, base);
        report_agents(a, &mut backdrop, base, (rx, ry, rw, rh));
        let mut comp = match Compositor::new(gl, size) {
            Ok(c) => c,
            Err(e) => {
                if let Some(v) = vrm {
                    v.renderer.destroy(gl);
                    v.layer.destroy(gl);
                }
                return Err(e);
            }
        };
        let r = record::Sink::spawn(&settings).and_then(|sink| {
            record_frames(
                gl,
                fbo,
                a,
                sink,
                total,
                base,
                &mut comp,
                vrm.as_mut(),
                &mut backdrop,
                term,
            )
        });
        comp.destroy(gl);
        if let Some(v) = vrm {
            v.renderer.destroy(gl);
            v.layer.destroy(gl);
        }
        unsafe { gl.finish() };
        r
    });
    // 失敗したときの書きかけは Sink が一時ファイルごと消す(出力先には成功したときだけ移す)
    let (frames, interrupted) = result.and_then(|r| r)?;
    let secs = wall.elapsed().as_secs_f64();
    eprintln!(
        "録画: {frames} フレーム({:.1}秒分)を {secs:.1}秒で書き出した{} -> {}({} バイト)",
        frames as f64 / fps,
        if interrupted {
            "(シグナルで中断。ここまでを保存)"
        } else {
            ""
        },
        out.display(),
        out.metadata().map_or(0, |m| m.len()),
    );
    eprintln!("注意: 録画モードの値(書き出し時間など)は計測に使わない");
    Ok(())
}

/// 録画のフレームのループ。戻りは (書いたフレーム数, シグナルで中断したか)。
#[allow(clippy::too_many_arguments)]
fn record_frames(
    gl: &glow::Context,
    fbo: glow::Framebuffer,
    a: &Args,
    mut sink: record::Sink,
    total: u64,
    base: Instant,
    comp: &mut Compositor,
    mut vrm: Option<&mut Vrm>,
    backdrop: &mut Backdrop,
    term: &AtomicBool,
) -> Result<(u64, bool), String> {
    let (size, fps) = (a.size, a.fps.unwrap_or(30.0));
    let g = geom(a, size, backdrop);
    let target = Some(fbo);
    let mut tracker = AimTracker::default();
    let mut face: Vec<(&'static str, f32)> = Vec::new();
    let mut rgba = vec![0u8; size.0 as usize * size.1 as usize * 4];
    let mut scratch = Vec::new();
    let start = Instant::now();
    let mut next_progress = start + PROGRESS_EVERY;
    let mut prev_t = 0.0f32;
    let mut done = 0u64;
    let mut interrupted = false;
    while done < total {
        if term.load(Ordering::Relaxed) {
            interrupted = true;
            break;
        }
        let secs = done as f64 / fps;
        let now = base + Duration::from_secs_f64(secs);
        let t = secs as f32;
        match &mut *backdrop {
            Backdrop::Photo(s) => {
                s.update(now);
            }
            Backdrop::Agents(stage, text) => {
                stage.update(now, text);
                stage.take_stats();
            }
            Backdrop::None => {}
        }
        if let Some(v) = vrm.as_deref_mut() {
            pose_vrm(a, v, backdrop, &mut tracker, &g, now, (t, t - prev_t));
            face_vrm(a, v, t, &mut face)?;
        }
        prev_t = t;
        let yaw = if g.sway { demo::camera_yaw(t) } else { 0.0 };
        let agent_cards = match &*backdrop {
            Backdrop::Agents(stage, _) => stage.gpu_cards(),
            _ => Vec::new(),
        };
        let frame = match &*backdrop {
            Backdrop::Photo(s) => Some(s.frame(now)),
            Backdrop::Agents(stage, _) => Some(stage.frame(&agent_cards)),
            Backdrop::None => None,
        };
        let mut draw = |gl: &glow::Context, _: (u32, u32)| {
            if let Some(v) = vrm.as_deref_mut() {
                draw_vrm(gl, v, a, &g, target, yaw, frame.is_none());
            }
        };
        match &frame {
            Some(f) => miharashi_gpu::draw_frame_with(comp, gl, size, target, f, &mut draw)?,
            None => miharashi_gpu::draw_with(gl, size, target, &mut draw),
        }
        drop(agent_cards);
        match &mut *backdrop {
            Backdrop::Photo(s) => s.mark_sent(),
            Backdrop::Agents(stage, _) => stage.mark_sent(),
            Backdrop::None => {}
        }
        miharashi_gpu::read_rgba_top_down(gl, fbo, size, &mut rgba, &mut scratch);
        sink.write_frame(&rgba)?;
        done += 1;
        let wall = Instant::now();
        if wall >= next_progress {
            let el = wall.duration_since(start).as_secs_f64();
            let left = el / done as f64 * (total - done) as f64;
            eprintln!(
                "録画 {done}/{total} フレーム({:.0}%) 経過 {el:.0}秒 残り約 {left:.0}秒(録画モードの値は計測に使わない)",
                done as f64 * 100.0 / total as f64,
            );
            next_progress = wall + PROGRESS_EVERY;
        }
    }
    sink.finish()?;
    Ok((done, interrupted))
}

fn main() {
    let result = parse_args().and_then(|a| run(&a));
    if let Err(e) = result {
        eprintln!("vrm-view: {e}");
        std::process::exit(1);
    }
}
