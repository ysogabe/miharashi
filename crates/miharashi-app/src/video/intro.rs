//! 紹介動画(10 / 30 / 60 秒)。各画面の見本(`DemoRun`)を仮想時刻で描き、1920x1080 のレイアウトに合成して、
//! 中間の動画を経由せずに書き出す。構成と文言は `docs/intro-video/edit.json`(編集の原本)。
//! 長さはフレーム単位(300 / 900 / 1800)に正確に合わせる。
use super::demo::{AgentScript, DemoRun, DemoScreen};
use miharashi_render::canvas::rgba;
use miharashi_render::{Canvas, Pixmap, TextEngine};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// 編集の原本(リポジトリの `docs/intro-video/edit.json`)。`--edit` で別のファイルも使える。
const EDIT_JSON: &str = include_str!("../../../../docs/intro-video/edit.json");

const BG: [u8; 3] = [7, 19, 28];
const FG: [u8; 4] = [231, 240, 244, 255];
const ACCENT: [u8; 4] = [114, 226, 197, 255];
const MUTED: [u8; 4] = [143, 168, 181, 255];
const DIM: [u8; 4] = [95, 120, 134, 255];
const LINE: [u8; 4] = [29, 51, 65, 255];

/// シーンの切り替えのフェード(フレーム数)
const FADE_FRAMES: u32 = 6;

#[derive(Deserialize, Clone)]
struct Edit {
    fps: u32,
    size: (u32, u32),
    screen_size: (u32, u32),
    text: Text,
    versions: BTreeMap<String, Vec<SceneDef>>,
}

#[derive(Deserialize, Clone)]
struct Text {
    title: String,
    tagline: Vec<String>,
    description: Vec<String>,
    badge_intro: String,
    badge_sample: String,
    tags: String,
    thumbs_label: String,
    thumbs_caption: String,
    tabs: Vec<String>,
    controls_guide: String,
    keys: Vec<KeyRow>,
    outro_lead: String,
    outro_url: String,
    outro_repo: String,
    outro_footer: String,
}

#[derive(Deserialize, Clone)]
struct KeyRow {
    keys: Vec<String>,
    label: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Intro,
    Feature,
    Controls,
    Outro,
}

#[derive(Deserialize, Clone)]
struct AgentsDef {
    new_at: f64,
    done_at: Option<f64>,
}

#[derive(Deserialize, Clone)]
struct SceneDef {
    kind: Kind,
    /// 画面の見本(status / photo / travel / room / aircraft / agents)。feature と controls だけ
    shot: Option<String>,
    duration: f64,
    /// 強調するタブ(0〜4)
    index: Option<usize>,
    title: Option<String>,
    subtitle: Option<String>,
    agents: Option<AgentsDef>,
}

fn shot(name: &str) -> Result<DemoScreen, String> {
    match name {
        "travel" => Ok(DemoScreen::Trip),
        n => DemoScreen::parse(n),
    }
}

/// 3 つのフォント(通常の日本語、太字、等幅)。
struct Fonts {
    sans: TextEngine,
    bold: TextEngine,
    mono: TextEngine,
    mono_bold: TextEngine,
}

impl Fonts {
    fn new() -> Self {
        let jp = "Noto Sans CJK JP";
        let mono = "Noto Sans Mono";
        Self {
            sans: TextEngine::new(jp),
            bold: TextEngine::new(jp).with_weight(700),
            mono: TextEngine::new(mono),
            mono_bold: TextEngine::new(mono).with_weight(700),
        }
    }
}

fn put(t: &mut TextEngine, c: &mut Canvas, s: &str, x: f32, cy: f32, size: f32, col: [u8; 4]) {
    t.draw(c, s, x, cy - size * 0.65, size, col);
}

fn put_right(
    t: &mut TextEngine,
    c: &mut Canvas,
    s: &str,
    right: f32,
    cy: f32,
    size: f32,
    col: [u8; 4],
) {
    t.draw_right(c, s, right, cy - size * 0.65, size, col);
}

macro_rules! col {
    ($c:expr) => {{
        let c: [u8; 4] = $c;
        rgba(c[0], c[1], c[2], c[3])
    }};
}

fn background(size: (u32, u32)) -> Canvas {
    let mut c = Canvas::new(size.0, size.1);
    let (w, h) = (size.0 as usize, size.1 as usize);
    let data = c.pixmap.data_mut();
    for y in 0..h {
        for x in 0..w {
            // 右上に、ごく淡い青緑の光
            let (dx, dy) = (x as f32 / w as f32 - 0.9, y as f32 / h as f32 + 0.05);
            let glow = (-(dx * dx * 2.2 + dy * dy * 4.0) * 6.0).exp() * 0.07;
            let i = (y * w + x) * 4;
            for k in 0..3 {
                let v = BG[k] as f32 + (ACCENT[k] as f32 - BG[k] as f32) * glow;
                data[i + k] = v.round() as u8;
            }
            data[i + 3] = 255;
        }
    }
    c
}

/// 面積平均で縮小する(文字をつぶさない)。`src` は不透明。
pub(super) fn resample(src: &Pixmap, dw: u32, dh: u32) -> Pixmap {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (dw, dh) = (dw as usize, dh as usize);
    let s = src.data();
    // 横方向
    let mut tmp = vec![0f32; dw * sh * 3];
    let sx = sw as f32 / dw as f32;
    for x in 0..dw {
        let (x0, x1) = (x as f32 * sx, (x + 1) as f32 * sx);
        let (i0, i1) = (x0.floor() as usize, (x1.ceil() as usize).min(sw));
        for y in 0..sh {
            let mut acc = [0f32; 3];
            for i in i0..i1 {
                let w = (x1.min((i + 1) as f32) - x0.max(i as f32)).max(0.0);
                let p = (y * sw + i) * 4;
                for k in 0..3 {
                    acc[k] += w * s[p + k] as f32;
                }
            }
            for k in 0..3 {
                tmp[(y * dw + x) * 3 + k] = acc[k] / sx;
            }
        }
    }
    // 縦方向
    let mut out = Pixmap::new(dw as u32, dh as u32).expect("縮小先の大きさ");
    let sy = sh as f32 / dh as f32;
    let d = out.data_mut();
    for y in 0..dh {
        let (y0, y1) = (y as f32 * sy, (y + 1) as f32 * sy);
        let (j0, j1) = (y0.floor() as usize, (y1.ceil() as usize).min(sh));
        for x in 0..dw {
            let mut acc = [0f32; 3];
            for j in j0..j1 {
                let w = (y1.min((j + 1) as f32) - y0.max(j as f32)).max(0.0);
                for k in 0..3 {
                    acc[k] += w * tmp[(j * dw + x) * 3 + k];
                }
            }
            let p = (y * dw + x) * 4;
            for k in 0..3 {
                d[p + k] = (acc[k] / sy).round().clamp(0.0, 255.0) as u8;
            }
            d[p + 3] = 255;
        }
    }
    out
}

fn logo(c: &mut Canvas, f: &mut Fonts, badge: &str) {
    // ロゴ(棒グラフの印)と名前
    c.stroke_rect(96.0, 36.0, 32.0, 32.0, 2.0, col!(ACCENT));
    for (i, h) in [10.0, 18.0, 14.0].into_iter().enumerate() {
        c.fill_rect(104.0 + i as f32 * 7.0, 60.0 - h, 4.0, h, col!(ACCENT));
    }
    put(&mut f.bold, c, "Miharashi", 142.0, 52.0, 26.0, FG);
    put(&mut f.mono, c, "RUST / LINUX", 310.0, 53.0, 16.0, DIM);
    // 右上のバッジ
    let w = f.sans.measure(badge, 20.0);
    c.fill_circle(1824.0 - w - 18.0, 52.0, 5.0, col!(ACCENT));
    put_right(&mut f.sans, c, badge, 1824.0, 52.0, 20.0, MUTED);
}

const SCREEN_X: i32 = 96;
const SCREEN_Y: i32 = 240;

fn tabs(c: &mut Canvas, f: &mut Fonts, text: &Text, active: Option<usize>) {
    let cell = 1728.0 / 5.0;
    c.fill_rect(96.0, 214.0, 1728.0, 1.0, col!(LINE));
    for (i, name) in text.tabs.iter().enumerate() {
        let x = 96.0 + i as f32 * cell;
        let on = active == Some(i);
        let color = if on { FG } else { DIM };
        put(
            &mut f.mono,
            c,
            &format!("{:02}", i + 1),
            x,
            190.0,
            17.0,
            if on { ACCENT } else { DIM },
        );
        put(&mut f.mono, c, name, x + 40.0, 190.0, 17.0, color);
        if on {
            c.fill_rect(x, 212.0, cell - 8.0, 3.0, col!(ACCENT));
        }
    }
}

fn fit_wide(f: &mut Fonts, s: &str, size: f32, max: f32) -> f32 {
    let w = f.bold.measure(s, size);
    if w > max { size * max / w } else { size }
}

/// 1 シーンの静的な絵(背景・文字)と、画面の上に重ねる絵。
struct Layers {
    bg: Canvas,
    fg: Option<Canvas>,
}

fn keypad(c: &mut Canvas, f: &mut Fonts, rows: &[KeyRow]) {
    let size = 84.0;
    for (r, row) in rows.iter().enumerate() {
        let y = 380.0 + r as f32 * 166.0;
        for (i, k) in row.keys.iter().enumerate() {
            let x = 384.0 + i as f32 * 114.0;
            c.fill_rect(x, y, size, size, rgba(14, 44, 46, 255));
            c.stroke_rect(x, y, size, size, 3.0, col!(ACCENT));
            let w = f.mono_bold.measure(k, 46.0);
            put(
                &mut f.mono_bold,
                c,
                k,
                x + (size - w) / 2.0,
                y + size / 2.0,
                46.0,
                ACCENT,
            );
        }
        put(&mut f.sans, c, &row.label, 850.0, y + size / 2.0, 32.0, FG);
    }
}

fn thumb(c: &mut Canvas, src: &Pixmap, x: i32, y: i32, w: u32, shadow: bool) {
    let h = (w as f32 * src.height() as f32 / src.width() as f32).round() as u32;
    let small = resample(src, w, h);
    if shadow {
        c.fill_rect(
            x as f32 + 10.0,
            y as f32 + 14.0,
            w as f32,
            h as f32,
            rgba(0, 0, 0, 110),
        );
    }
    c.draw_pixmap(&small, x, y);
    c.stroke_rect(
        x as f32,
        y as f32,
        w as f32,
        h as f32,
        1.5,
        col!([42, 66, 81, 255]),
    );
}

fn hero_text(c: &mut Canvas, f: &mut Fonts, t: &Text, y0: f32) {
    put(&mut f.bold, c, &t.title, 96.0, y0, 104.0, FG);
    for (i, line) in t.tagline.iter().enumerate() {
        let color = if i == 0 { FG } else { ACCENT };
        put(
            &mut f.bold,
            c,
            line,
            96.0,
            y0 + 168.0 + i as f32 * 92.0,
            64.0,
            color,
        );
    }
}

struct Stills {
    map: BTreeMap<(&'static str, u32), Pixmap>,
    fps: u32,
}

impl Stills {
    /// 画面 `screen` の、動き出してから `frame` 番目のフレーム(2560x1080)。
    fn get(&mut self, screen: DemoScreen, frame: u32) -> Result<&Pixmap, String> {
        let name = DemoScreen::ALL
            .iter()
            .find(|(_, d)| *d == screen)
            .map_or("", |(n, _)| *n);
        if !self.map.contains_key(&(name, frame)) {
            let mut run = DemoRun::new(
                screen,
                (2560, 1080),
                self.fps,
                AgentScript::default(),
                std::time::Duration::ZERO,
            )?;
            for i in 0..frame {
                run.frame(i);
            }
            self.map
                .insert((name, frame), run.frame(frame).pixmap.clone());
        }
        Ok(&self.map[&(name, frame)])
    }
}

fn layers(
    def: &SceneDef,
    edit: &Edit,
    f: &mut Fonts,
    stills: &mut Stills,
) -> Result<Layers, String> {
    let t = &edit.text;
    let mut bg = background(edit.size);
    let mut fg = None;
    match def.kind {
        Kind::Intro => {
            logo(&mut bg, f, &t.badge_intro);
            hero_text(&mut bg, f, t, 250.0);
            for (i, line) in t.description.iter().enumerate() {
                put(
                    &mut f.sans,
                    &mut bg,
                    line,
                    96.0,
                    648.0 + i as f32 * 50.0,
                    30.0,
                    MUTED,
                );
            }
            bg.fill_rect(96.0, 765.0, 470.0, 1.0, col!(LINE));
            put(&mut f.mono, &mut bg, &t.tags, 96.0, 806.0, 18.0, DIM);
            put(
                &mut f.mono,
                &mut bg,
                &t.thumbs_label,
                954.0,
                232.0,
                16.0,
                ACCENT,
            );
            let status = stills.get(DemoScreen::Status, 45)?.clone();
            let photo = stills.get(DemoScreen::Photo, 35)?.clone();
            let radar = stills.get(DemoScreen::Aircraft, 30)?.clone();
            thumb(&mut bg, &status, 954, 268, 866, false);
            thumb(&mut bg, &photo, 954, 684, 425, false);
            thumb(&mut bg, &radar, 1395, 684, 425, false);
            put(
                &mut f.sans,
                &mut bg,
                &t.thumbs_caption,
                954.0,
                916.0,
                24.0,
                MUTED,
            );
        }
        Kind::Feature | Kind::Controls => {
            logo(&mut bg, f, &t.badge_sample);
            let title = def.title.as_deref().unwrap_or("");
            let size = fit_wide(f, title, 52.0, 1728.0);
            put(&mut f.bold, &mut bg, title, 96.0, 128.0, size, FG);
            if def.kind == Kind::Feature {
                tabs(&mut bg, f, t, def.index);
            } else {
                bg.fill_rect(96.0, 214.0, 1728.0, 1.0, col!(LINE));
                put(
                    &mut f.mono,
                    &mut bg,
                    &t.controls_guide,
                    96.0,
                    190.0,
                    17.0,
                    ACCENT,
                );
                // 画面を暗くして、キーを重ねる
                let mut o = Canvas::new(edit.size.0, edit.size.1);
                o.fill_rect(
                    SCREEN_X as f32,
                    SCREEN_Y as f32,
                    edit.screen_size.0 as f32,
                    edit.screen_size.1 as f32,
                    rgba(7, 19, 28, 200),
                );
                keypad(&mut o, f, &t.keys);
                fg = Some(o);
            }
            let sub = def.subtitle.as_deref().unwrap_or("");
            put(&mut f.sans, &mut bg, sub, 96.0, 1012.0, 26.0, MUTED);
            // 画面の枠
            bg.stroke_rect(
                SCREEN_X as f32 - 1.0,
                SCREEN_Y as f32 - 1.0,
                edit.screen_size.0 as f32 + 2.0,
                edit.screen_size.1 as f32 + 2.0,
                1.5,
                col!([42, 66, 81, 255]),
            );
        }
        Kind::Outro => {
            logo(&mut bg, f, &t.badge_intro);
            hero_text(&mut bg, f, t, 232.0);
            put(
                &mut f.sans,
                &mut bg,
                &t.outro_lead,
                96.0,
                607.0,
                28.0,
                MUTED,
            );
            bg.fill_rect(96.0, 654.0, 940.0, 100.0, col!(ACCENT));
            put(
                &mut f.mono_bold,
                &mut bg,
                &t.outro_url,
                128.0,
                704.0,
                42.0,
                BG.map(|v| v).into_rgba(),
            );
            put(
                &mut f.mono,
                &mut bg,
                &t.outro_repo,
                96.0,
                808.0,
                28.0,
                MUTED,
            );
            put(
                &mut f.mono,
                &mut bg,
                "RUST / LINUX",
                96.0,
                936.0,
                16.0,
                ACCENT,
            );
            put(
                &mut f.sans,
                &mut bg,
                &t.outro_footer,
                96.0,
                990.0,
                22.0,
                DIM,
            );
            let status = stills.get(DemoScreen::Status, 45)?.clone();
            let radar = stills.get(DemoScreen::Aircraft, 30)?.clone();
            let agents = stills.get(DemoScreen::Agents, 15)?.clone();
            thumb(&mut bg, &status, 1090, 190, 700, true);
            thumb(&mut bg, &radar, 1130, 480, 700, true);
            thumb(&mut bg, &agents, 1170, 770, 700, true);
        }
    }
    Ok(Layers { bg, fg })
}

trait IntoRgba {
    fn into_rgba(self) -> [u8; 4];
}

impl IntoRgba for [u8; 3] {
    fn into_rgba(self) -> [u8; 4] {
        [self[0], self[1], self[2], 255]
    }
}

/// 実行中の 1 シーン。
struct Running {
    def: SceneDef,
    layers: Layers,
    run: Option<DemoRun>,
    /// 縮小した画面と、その元の描き直しの回数
    scaled: Option<(u64, Pixmap)>,
}

fn start_scene(
    def: &SceneDef,
    start_frame: u32,
    edit: &Edit,
    f: &mut Fonts,
    stills: &mut Stills,
) -> Result<Running, String> {
    let layers = layers(def, edit, f, stills)?;
    let run = match def.kind {
        Kind::Feature | Kind::Controls => {
            let name = def
                .shot
                .as_deref()
                .ok_or("feature / controls に shot がない")?;
            let script = def
                .agents
                .as_ref()
                .map_or_else(AgentScript::default, |a| AgentScript {
                    new_at: a.new_at,
                    done_at: a.done_at,
                });
            // 画面の壁時計は、動画の時間に合わせて進める(シーンが変わっても時計が戻らない)
            let offset =
                std::time::Duration::from_secs_f64(f64::from(start_frame) / f64::from(edit.fps));
            Some(DemoRun::new(
                shot(name)?,
                (2560, 1080),
                edit.fps,
                script,
                offset,
            )?)
        }
        _ => None,
    };
    Ok(Running {
        def: def.clone(),
        layers,
        run,
        scaled: None,
    })
}

/// 各シーンの (開始, 終了) フレーム。累積時刻をフレームに丸めるので、合計は秒数 × fps に正確に合う。
fn frame_ranges(scenes: &[SceneDef], fps: u32) -> Vec<(u32, u32)> {
    let mut cum = 0.0f64;
    let mut start = 0u32;
    scenes
        .iter()
        .map(|s| {
            cum += s.duration;
            let end = (cum * f64::from(fps)).round() as u32;
            let r = (start, end);
            start = end;
            r
        })
        .collect()
}

fn compose(
    rt: &mut Running,
    local: u32,
    global: u32,
    total: u32,
    edit: &Edit,
    prev: Option<&Canvas>,
    out: &mut Canvas,
) {
    out.pixmap
        .data_mut()
        .copy_from_slice(rt.layers.bg.pixmap.data());
    if let Some(run) = rt.run.as_mut() {
        run.frame(local);
        let rev = run.revision();
        if rt.scaled.as_ref().is_none_or(|(r, _)| *r != rev) {
            let s = resample(&run.canvas().pixmap, edit.screen_size.0, edit.screen_size.1);
            rt.scaled = Some((rev, s));
        }
        if let Some((_, s)) = &rt.scaled {
            out.draw_pixmap(s, SCREEN_X, SCREEN_Y);
        }
    }
    if let Some(fg) = &rt.layers.fg {
        out.draw_pixmap(&fg.pixmap, 0, 0);
    }
    // 進行線(動画全体のどこか)
    if matches!(rt.def.kind, Kind::Feature | Kind::Controls) {
        out.fill_rect(96.0, 1058.0, 1728.0, 3.0, col!(LINE));
        let w = 1728.0 * f64::from(global + 1) as f32 / total as f32;
        out.fill_rect(96.0, 1058.0, w, 3.0, col!(ACCENT));
    }
    // シーンの頭は、前のシーンの最後の絵から短くフェードする
    if let Some(prev) = prev
        && local < FADE_FRAMES
    {
        let op = 1.0 - (local + 1) as f32 / (FADE_FRAMES + 1) as f32;
        out.draw_pixmap_opacity(&prev.pixmap, 0, 0, op);
    }
}

/// 1 つの版(`10` / `30` / `60`)を書き出す。戻り値: フレーム数。
fn write_version(
    edit: &Edit,
    name: &str,
    out_path: &Path,
    overwrite: bool,
    stop: &AtomicBool,
) -> Result<u32, String> {
    let scenes = edit
        .versions
        .get(name)
        .ok_or_else(|| format!("版 {name} がない"))?;
    let secs: u32 = name.parse().map_err(|_| "版は数字(10 / 30 / 60)")?;
    let fps = edit.fps;
    let ranges = frame_ranges(scenes, fps);
    let total = ranges.last().map_or(0, |r| r.1);
    if total != secs * fps {
        return Err(format!(
            "版 {name} の長さが合わない: {total} フレーム(期待 {})",
            secs * fps
        ));
    }
    let settings = super::record_settings(out_path, edit.size, fps, overwrite);
    miharashi_vrm::record::check_output(out_path, overwrite, "--overwrite")?;
    let mut fonts = Fonts::new();
    let mut stills = Stills {
        map: BTreeMap::new(),
        fps,
    };
    let mut sink = miharashi_vrm::record::Sink::spawn(&settings)?;
    let mut out = Canvas::new(edit.size.0, edit.size.1);
    let mut prev: Option<Canvas> = None;
    for (def, (start, end)) in scenes.iter().zip(&ranges) {
        super::check_stop(stop)?;
        let mut rt = start_scene(def, *start, edit, &mut fonts, &mut stills)?;
        for g in *start..*end {
            super::check_stop(stop)?;
            compose(&mut rt, g - start, g, total, edit, prev.as_ref(), &mut out);
            sink.write_frame(out.pixmap.data())?;
        }
        prev = Some(Canvas {
            pixmap: out.pixmap.clone(),
        });
    }
    sink.finish()?;
    Ok(total)
}

/// `intro-video`: `version` が `all` なら 10 / 30 / 60 秒版を `out_dir` に `miharashi-<N>s.mp4` で書き出す。
pub fn write(
    version: &str,
    out_dir: &Path,
    edit_file: Option<&Path>,
    overwrite: bool,
    stop: &AtomicBool,
) -> Result<Vec<(String, u32)>, String> {
    let json = match edit_file {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => EDIT_JSON.to_string(),
    };
    let edit: Edit = serde_json::from_str(&json).map_err(|e| format!("edit.json: {e}"))?;
    if edit.screen_size.0 > edit.size.0 || edit.text.tabs.len() != 5 {
        return Err("edit.json の screen_size / tabs が正しくない".into());
    }
    let names: Vec<String> = if version == "all" {
        edit.versions.keys().cloned().collect()
    } else {
        vec![version.to_string()]
    };
    // 新しい checkout でも書き出せるよう、出力先のディレクトリは作る
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("{} を作れない: {e}", out_dir.display()))?;
    let mut done = Vec::new();
    for n in names {
        let path = out_dir.join(format!("miharashi-{n}s.mp4"));
        let frames = write_version(&edit, &n, &path, overwrite, stop)?;
        println!("wrote {} ({frames} frames)", path.display());
        done.push((n, frames));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit() -> Edit {
        serde_json::from_str(EDIT_JSON).expect("edit.json")
    }

    /// 編集の原本: どの版も、累積時刻をフレームに丸めて 300 / 900 / 1800 フレームに合い、シーンが隙間なく並ぶ。
    #[test]
    fn versions_add_up_to_exact_frames() {
        let e = edit();
        for (name, frames) in [("10", 300), ("30", 900), ("60", 1800)] {
            let scenes = &e.versions[name];
            let r = frame_ranges(scenes, e.fps);
            assert_eq!(r.last().unwrap().1, frames, "{name}");
            assert_eq!(r[0].0, 0);
            for w in r.windows(2) {
                assert_eq!(w[0].1, w[1].0);
            }
            assert!(r.iter().all(|(s, e)| e > s), "長さ 0 のシーンがない");
            for s in scenes {
                if let Some(n) = &s.shot {
                    shot(n).expect("shot の名前");
                }
            }
        }
    }

    /// 端数の秒数でも、累積で丸めるので合計がずれない。
    #[test]
    fn fractional_durations_round_cumulatively() {
        let s = |d| SceneDef {
            kind: Kind::Intro,
            shot: None,
            duration: d,
            index: None,
            title: None,
            subtitle: None,
            agents: None,
        };
        let r = frame_ranges(
            &[s(1.25), s(1.25), s(1.25), s(1.25), s(1.5), s(1.5), s(2.0)],
            30,
        );
        assert_eq!(r.last().unwrap().1, 300);
    }

    #[test]
    fn resample_keeps_flat_colors_and_size() {
        let mut p = Pixmap::new(40, 20).unwrap();
        p.fill(rgba(10, 20, 30, 255));
        let r = resample(&p, 27, 13);
        assert_eq!((r.width(), r.height()), (27, 13));
        assert!(
            r.pixels()
                .iter()
                .all(|px| px.red() == 10 && px.alpha() == 255)
        );
    }
}
