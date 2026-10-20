//! Agent 専用の紹介動画。概念図 → 連続する実 AgentScreen → 案内。
//! 編集原本は docs/agents-intro-video/edit.json。既存の総合紹介動画とは独立した版を持つ。
use super::demo::{AgentScript, DemoRun, DemoScreen};
use super::intro::resample;
use miharashi_render::canvas::rgba;
use miharashi_render::{Canvas, Pixmap, TextEngine};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const EDIT_JSON: &str = include_str!("../../../../docs/agents-intro-video/edit.json");
const SIZE: (u32, u32) = (1920, 1080);
const SCREEN: (u32, u32) = (1728, 729);
const FG: [u8; 4] = [231, 240, 244, 255];
const MINT: [u8; 4] = [114, 226, 197, 255];
const MUTED: [u8; 4] = [143, 168, 181, 255];
const FADE: u32 = 6;
/// 通常の中央通知: 700ms 導入 + 3s 保持 + 800ms 復帰。
const NOTICE_SECONDS: f64 = 4.5;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    fps: u32,
    size: (u32, u32),
    text: Text,
    versions: BTreeMap<String, Version>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    badge: String,
    diagram_title: String,
    source_title: String,
    source_subtitle: String,
    source_lines: Vec<String>,
    state_title: String,
    state_subtitle: String,
    state_lines: Vec<String>,
    display_title: String,
    display_subtitle: String,
    display_lines: Vec<String>,
    read_only: String,
    demo_note: String,
    outro_title: String,
    outro_lines: Vec<String>,
    url: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    diagram_seconds: f64,
    demo_seconds: f64,
    outro_seconds: f64,
    new_at: f64,
    done_at: f64,
    captions: Vec<Caption>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Caption {
    /// 実画面の開始からの秒。見出しを替えても DemoRun は作り直さない。
    at: f64,
    title: String,
}

impl Version {
    /// 各区間の累積終端。個別の秒数の丸めで総フレーム数がずれないようにする。
    fn boundaries(&self, fps: u32) -> [u32; 3] {
        let frame = |s: f64| (s * f64::from(fps)).round() as u32;
        [
            frame(self.diagram_seconds),
            frame(self.diagram_seconds + self.demo_seconds),
            frame(self.diagram_seconds + self.demo_seconds + self.outro_seconds),
        ]
    }

    fn caption_index(&self, frame: u32, fps: u32) -> usize {
        self.captions
            .iter()
            .rposition(|c| (c.at * f64::from(fps)).round() as u32 <= frame)
            .unwrap_or(0)
    }
}

fn parse(json: &str) -> Result<Edit, String> {
    let edit: Edit = serde_json::from_str(json).map_err(|e| format!("agents edit.json: {e}"))?;
    if edit.fps != 30 || edit.size != SIZE {
        return Err("Agent 紹介動画は 1920x1080 / 30fps で指定してください".into());
    }
    if edit.versions.len() != 2 || !["15", "30"].iter().all(|n| edit.versions.contains_key(*n)) {
        return Err("versions には 15 / 30 の両版が必要です".into());
    }
    for (name, v) in &edit.versions {
        let durations = [v.diagram_seconds, v.demo_seconds, v.outro_seconds];
        if durations.iter().any(|s| !s.is_finite() || *s <= 0.0)
            || v.boundaries(edit.fps)[2] != name.parse::<u32>().unwrap() * edit.fps
            || v.boundaries(edit.fps).windows(2).any(|w| w[0] >= w[1])
        {
            return Err(format!("版 {name}: 区間の長さが不正です(合計は {name} 秒)"));
        }
        if !v.new_at.is_finite()
            || !v.done_at.is_finite()
            || v.new_at <= 0.0
            || v.done_at < v.new_at + NOTICE_SECONDS
            || v.done_at + NOTICE_SECONDS >= v.demo_seconds
        {
            return Err(format!(
                "版 {name}: 通知の導入・保持・一覧への復帰(各4.5秒)が収まりません"
            ));
        }
        if v.captions.first().is_none_or(|c| c.at != 0.0)
            || v.captions
                .iter()
                .any(|c| !c.at.is_finite() || c.at < 0.0 || c.at >= v.demo_seconds)
            || v.captions.windows(2).any(|w| w[0].at >= w[1].at)
        {
            return Err(format!(
                "版 {name}: captions は 0 秒から昇順で実画面の範囲内に指定してください"
            ));
        }
    }
    Ok(edit)
}

struct Fonts {
    sans: TextEngine,
    bold: TextEngine,
    mono: TextEngine,
}

impl Fonts {
    fn new() -> Self {
        Self {
            sans: TextEngine::new("Noto Sans CJK JP"),
            bold: TextEngine::new("Noto Sans CJK JP").with_weight(700),
            mono: TextEngine::new("Noto Sans Mono"),
        }
    }
}

/// 原本の文言を編集しても横幅からはみ出さないように縮める。
fn put(
    t: &mut TextEngine,
    c: &mut Canvas,
    s: &str,
    x: f32,
    cy: f32,
    size: f32,
    width: f32,
    color: [u8; 4],
) {
    let size = size * (width / t.measure(s, size).max(1.0)).min(1.0);
    t.draw(c, s, x, cy - size * 0.65, size, color);
}

fn base(f: &mut Fonts, text: &Text) -> Canvas {
    let mut c = Canvas::new(SIZE.0, SIZE.1);
    c.clear(rgba(7, 19, 28, 255));
    c.stroke_rect(96.0, 36.0, 32.0, 32.0, 2.0, rgba(114, 226, 197, 255));
    for (i, h) in [10.0, 18.0, 14.0].into_iter().enumerate() {
        c.fill_rect(
            104.0 + i as f32 * 7.0,
            60.0 - h,
            4.0,
            h,
            rgba(114, 226, 197, 255),
        );
    }
    put(
        &mut f.bold,
        &mut c,
        "Miharashi",
        142.0,
        52.0,
        26.0,
        160.0,
        FG,
    );
    put(
        &mut f.mono,
        &mut c,
        "RUST / LINUX",
        310.0,
        53.0,
        16.0,
        300.0,
        MUTED,
    );
    let w = f.sans.measure(&text.badge, 20.0).min(600.0);
    put(
        &mut f.sans,
        &mut c,
        &text.badge,
        1824.0 - w,
        52.0,
        20.0,
        w,
        MINT,
    );
    c
}

fn diagram(f: &mut Fonts, t: &Text) -> Canvas {
    let mut c = base(f, t);
    put(
        &mut f.bold,
        &mut c,
        &t.diagram_title,
        96.0,
        196.0,
        64.0,
        1728.0,
        FG,
    );
    for (i, (title, sub, lines)) in [
        (&t.source_title, &t.source_subtitle, &t.source_lines),
        (&t.state_title, &t.state_subtitle, &t.state_lines),
        (&t.display_title, &t.display_subtitle, &t.display_lines),
    ]
    .into_iter()
    .enumerate()
    {
        let x = 96.0 + i as f32 * 610.0;
        c.fill_rect(x, 354.0, 508.0, 382.0, rgba(14, 34, 45, 255));
        c.stroke_rect(x, 354.0, 508.0, 382.0, 2.0, rgba(42, 66, 81, 255));
        put(
            &mut f.mono,
            &mut c,
            &format!("0{}", i + 1),
            x + 32.0,
            404.0,
            20.0,
            444.0,
            MINT,
        );
        put(&mut f.bold, &mut c, title, x + 32.0, 478.0, 42.0, 444.0, FG);
        put(&mut f.sans, &mut c, sub, x + 32.0, 545.0, 28.0, 444.0, MINT);
        // 行数が増えてもカードの下端内で均等配置する。
        let step = (100.0 / lines.len().max(1) as f32).min(48.0);
        for (j, line) in lines.iter().enumerate() {
            put(
                &mut f.sans,
                &mut c,
                line,
                x + 32.0,
                620.0 + j as f32 * step,
                step.min(26.0),
                444.0,
                MUTED,
            );
        }
    }
    // 情報の流れる方向のみ。Miharashi からの制御・書き戻しは描かない。
    for x in [614.0, 1224.0] {
        c.polyline(
            &[(x, 545.0), (x + 72.0, 545.0)],
            2.0,
            rgba(114, 226, 197, 140),
        );
        c.polyline(
            &[(x + 62.0, 535.0), (x + 72.0, 545.0), (x + 62.0, 555.0)],
            2.0,
            rgba(114, 226, 197, 255),
        );
    }
    put(
        &mut f.bold,
        &mut c,
        &t.read_only,
        96.0,
        866.0,
        36.0,
        1728.0,
        MINT,
    );
    put(
        &mut f.sans,
        &mut c,
        &t.demo_note,
        96.0,
        1006.0,
        24.0,
        1728.0,
        MUTED,
    );
    c
}

fn demo_layer(f: &mut Fonts, t: &Text, title: &str) -> Canvas {
    let mut c = base(f, t);
    put(&mut f.bold, &mut c, title, 96.0, 144.0, 52.0, 1728.0, FG);
    put(
        &mut f.sans,
        &mut c,
        &t.read_only,
        96.0,
        202.0,
        23.0,
        1728.0,
        MINT,
    );
    c.stroke_rect(95.0, 239.0, 1730.0, 731.0, 1.5, rgba(42, 66, 81, 255));
    put(
        &mut f.sans,
        &mut c,
        &t.demo_note,
        96.0,
        1012.0,
        24.0,
        1728.0,
        MUTED,
    );
    c
}

fn outro(f: &mut Fonts, t: &Text) -> Canvas {
    let mut c = base(f, t);
    put(
        &mut f.mono,
        &mut c,
        "T3 AGENTS",
        96.0,
        300.0,
        26.0,
        1728.0,
        MINT,
    );
    put(
        &mut f.bold,
        &mut c,
        &t.outro_title,
        96.0,
        436.0,
        72.0,
        1728.0,
        FG,
    );
    let step = (150.0 / t.outro_lines.len().max(1) as f32).min(62.0);
    for (i, line) in t.outro_lines.iter().enumerate() {
        put(
            &mut f.sans,
            &mut c,
            line,
            96.0,
            576.0 + i as f32 * step,
            step.min(34.0),
            1728.0,
            MUTED,
        );
    }
    c.fill_rect(96.0, 770.0, 1728.0, 112.0, rgba(114, 226, 197, 255));
    put(
        &mut f.mono,
        &mut c,
        &t.url,
        132.0,
        826.0,
        44.0,
        1656.0,
        [7, 19, 28, 255],
    );
    put(
        &mut f.sans,
        &mut c,
        &t.demo_note,
        96.0,
        1006.0,
        24.0,
        1728.0,
        MUTED,
    );
    c
}

/// 1 つの実画面を保ち、見出しの変更では時計・イベント・通知の途中経過を初期化しない。
struct Movie {
    demo: DemoRun,
    diagram: Canvas,
    captions: Vec<Canvas>,
    outro: Canvas,
    scaled: Option<(u64, Pixmap)>,
    previous: Option<Pixmap>,
    phase: usize,
}

impl Movie {
    fn new(e: &Edit, v: &Version) -> Result<Self, String> {
        let mut f = Fonts::new();
        Ok(Self {
            demo: DemoRun::new(
                DemoScreen::Agents,
                (2560, 1080),
                e.fps,
                AgentScript {
                    new_at: v.new_at,
                    done_at: Some(v.done_at),
                },
                Duration::from_secs_f64(v.diagram_seconds),
            )?,
            diagram: diagram(&mut f, &e.text),
            captions: v
                .captions
                .iter()
                .map(|c| demo_layer(&mut f, &e.text, &c.title))
                .collect(),
            outro: outro(&mut f, &e.text),
            scaled: None,
            previous: None,
            phase: 0,
        })
    }

    fn frame(&mut self, e: &Edit, v: &Version, g: u32, out: &mut Canvas) {
        let [demo_start, outro_start, total] = v.boundaries(e.fps);
        let (phase, local) = if g < demo_start {
            (0, g)
        } else if g < outro_start {
            (1, g - demo_start)
        } else {
            (2, g - outro_start)
        };
        // 区間境界だけフェードする。見出しだけの切替では実画面を覆わない。
        if phase != self.phase {
            self.previous = Some(out.pixmap.clone());
            self.phase = phase;
        }
        let layer = match phase {
            0 => &self.diagram,
            1 => &self.captions[v.caption_index(local, e.fps)],
            _ => &self.outro,
        };
        out.pixmap.data_mut().copy_from_slice(layer.pixmap.data());
        if phase == 0 {
            let p = (g as f32 / e.fps as f32 / 1.6).fract();
            for x in [614.0, 1224.0] {
                out.fill_circle(x + 72.0 * p, 545.0, 5.0, rgba(114, 226, 197, 255));
            }
        } else if phase == 1 {
            self.demo.frame(local);
            let rev = self.demo.revision();
            if self.scaled.as_ref().is_none_or(|(r, _)| *r != rev) {
                self.scaled = Some((
                    rev,
                    resample(&self.demo.canvas().pixmap, SCREEN.0, SCREEN.1),
                ));
            }
            out.draw_pixmap(&self.scaled.as_ref().unwrap().1, 96, 240);
        }
        out.fill_rect(96.0, 1058.0, 1728.0, 3.0, rgba(29, 51, 65, 255));
        out.fill_rect(
            96.0,
            1058.0,
            1728.0 * (g + 1) as f32 / total as f32,
            3.0,
            rgba(114, 226, 197, 255),
        );
        if local < FADE {
            if let Some(prev) = &self.previous {
                out.draw_pixmap_opacity(prev, 0, 0, 1.0 - (local + 1) as f32 / (FADE + 1) as f32);
            }
        }
    }
}

/// bitexact と組み合わせても SPS に BT.709 の三属性を残す。
/// 総合紹介動画の設定は変えず、Agent 専用出力にだけ付加する。
fn settings(path: &Path, e: &Edit, overwrite: bool) -> miharashi_vrm::record::Settings {
    let mut s = super::record_settings(path, e.size, e.fps, overwrite);
    s.extra_args.extend([
        "-bsf:v".into(),
        "h264_metadata=colour_primaries=1:transfer_characteristics=1:matrix_coefficients=1".into(),
    ]);
    s
}

pub fn write(
    version: &str,
    out_dir: &Path,
    edit_file: Option<&Path>,
    overwrite: bool,
    stop: &AtomicBool,
) -> Result<Vec<(String, u32)>, String> {
    if !matches!(version, "15" | "30" | "all") {
        return Err("--version は 15 / 30 / all".into());
    }
    let json = match edit_file {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => EDIT_JSON.into(),
    };
    let edit = parse(&json)?;
    let names = if version == "all" {
        vec!["15", "30"]
    } else {
        vec![version]
    };
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    // 両版を先に確認し、2 本目の既存ファイルを理由に片方だけ生成しないようにする。
    for name in &names {
        miharashi_vrm::record::check_output(
            &out_dir.join(format!("miharashi-agents-{name}s.mp4")),
            overwrite,
            "--overwrite",
        )?;
    }
    let mut done = Vec::new();
    for name in names {
        let v = &edit.versions[name];
        let total = v.boundaries(edit.fps)[2];
        let path = out_dir.join(format!("miharashi-agents-{name}s.mp4"));
        let mut movie = Movie::new(&edit, v)?;
        let mut sink = miharashi_vrm::record::Sink::spawn(&settings(&path, &edit, overwrite))?;
        let mut out = Canvas::new(SIZE.0, SIZE.1);
        for g in 0..total {
            super::check_stop(stop)?;
            movie.frame(&edit, v, g, &mut out);
            sink.write_frame(out.pixmap.data())?;
        }
        sink.finish()?;
        println!("wrote {} ({total} frames)", path.display());
        done.push((name.to_string(), total));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn originals_have_exact_frames_and_room_for_both_notices() {
        let e = parse(EDIT_JSON).unwrap();
        for (name, expected) in [("15", [90, 390, 450]), ("30", [180, 780, 900])] {
            let v = &e.versions[name];
            assert_eq!(v.boundaries(e.fps), expected);
            assert!(
                v.done_at > v.new_at + NOTICE_SECONDS,
                "一覧復帰後に次の通知"
            );
            assert!(
                v.demo_seconds - v.done_at - NOTICE_SECONDS >= 0.5,
                "完了一覧を保持"
            );
        }
        let v = &e.versions["30"];
        assert_eq!(v.caption_index(89, 30), 0);
        assert_eq!(v.caption_index(90, 30), 1);
        assert_eq!(v.caption_index(225, 30), 2);
        assert_eq!(v.caption_index(300, 30), 3);
        assert_eq!(v.caption_index(435, 30), 4);
    }

    /// 見出しの境界を通っても、独立した連続実行と同じ AgentScreen を保持する。
    /// 新着と完了の通知を分ける一覧復帰も、シナリオの同じスレッドを追っている。
    #[test]
    fn caption_changes_preserve_the_continuous_agent_screen() {
        let e = parse(EDIT_JSON).unwrap();
        let v = &e.versions["30"];
        let mut movie = Movie::new(&e, v).unwrap();
        let mut reference = DemoRun::new(
            DemoScreen::Agents,
            (2560, 1080),
            e.fps,
            AgentScript {
                new_at: v.new_at,
                done_at: Some(v.done_at),
            },
            Duration::from_secs_f64(v.diagram_seconds),
        )
        .unwrap();
        let mut out = Canvas::new(SIZE.0, SIZE.1);
        let [start, end, _] = v.boundaries(e.fps);
        for global in start..end {
            let local = global - start;
            movie.frame(&e, v, global, &mut out);
            reference.frame(local);
            if [89, 90, 224, 225, 299, 300, 434, 435, 599].contains(&local) {
                assert_eq!(
                    movie.demo.canvas().pixmap.data(),
                    reference.canvas().pixmap.data(),
                    "実画面の {local} フレーム"
                );
            }
        }
    }

    /// 実環境での短いエンコード確認。動く点→フェード→新着の導入・保持を含む
    /// 同じ 52 フレームを 2 回生成し、フォント・仮想時計・合成・Sink 全体の再現性を見る。
    #[test]
    #[ignore = "ffmpeg と Noto フォントが必要。cargo test --release -p miharashi-app encoded_excerpt_is_byte_identical -- --ignored"]
    fn encoded_excerpt_is_byte_identical() {
        let e = parse(EDIT_JSON).unwrap();
        let v = &e.versions["15"];
        let dir =
            std::env::temp_dir().join(format!("miharashi-agents-repeat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a", "b"] {
            let path = dir.join(format!("{name}.mp4"));
            let mut movie = Movie::new(&e, v).unwrap();
            let mut sink = miharashi_vrm::record::Sink::spawn(&settings(&path, &e, false)).unwrap();
            let mut out = Canvas::new(SIZE.0, SIZE.1);
            for frame in 84..136 {
                movie.frame(&e, v, frame, &mut out);
                sink.write_frame(out.pixmap.data()).unwrap();
            }
            sink.finish().unwrap();
        }
        let a = std::fs::read(dir.join("a.mp4")).unwrap();
        let b = std::fs::read(dir.join("b.mp4")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!a.is_empty());
        assert_eq!(a, b, "同じ条件の短い映像は MP4 のバイト列まで一致する");
    }

    #[test]
    fn rejects_overlapping_notices_and_out_of_range_captions() {
        // 検証と同じ JSON 入力経路を通す(Serialize を本体型へ追加しない)。
        let mut json: serde_json::Value = serde_json::from_str(EDIT_JSON).unwrap();
        json["versions"]["15"]["done_at"] = 4.0.into();
        assert!(parse(&json.to_string()).is_err());
        json["versions"]["15"]["done_at"] = 5.0.into();
        json["versions"]["30"]["captions"][2]["at"] = 21.0.into();
        assert!(parse(&json.to_string()).is_err());
    }
}
