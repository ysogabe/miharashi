//! T3 Agent card surfaces and CPU scene composition.
//!
//! Card pixels are rendered independently from their pose so callers can keep a
//! surface while it moves, scales, or fades. All pixel buffers are premultiplied
//! RGBA8, top-to-bottom.

use crate::{Canvas, TextEngine};
use std::sync::Arc;
use tiny_skia::{
    Color, FillRule, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Transform,
};

/// Card size at which the text scale is 1.0. Larger cards scale their text up
/// to `MAX_TEXT_SCALE` and use the remaining height for wrapped lines.
pub const BASE_CARD_WIDTH: f32 = 440.0;
pub const BASE_CARD_HEIGHT: f32 = 210.0;
const MAX_TEXT_SCALE: f32 = 1.6;
/// Height at which a one-line row has text scale 1.0.
pub const ROW_BASE_HEIGHT: f32 = 38.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentState {
    Running,
    Approval,
    Input,
    Error,
    Completed,
    Idle,
    Unknown,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "実行中",
            Self::Approval => "承認待ち",
            Self::Input => "入力・計画待ち",
            Self::Error => "異常",
            Self::Completed => "応答完了",
            Self::Idle => "待機",
            Self::Unknown => "不明",
        }
    }

    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Running => [64, 200, 255],
            Self::Approval => [255, 188, 102],
            Self::Input => [196, 160, 255],
            Self::Error => [255, 127, 140],
            Self::Completed => [103, 217, 173],
            Self::Idle | Self::Unknown => [143, 163, 183],
        }
    }

    fn tint(self) -> [u8; 3] {
        match self {
            Self::Running => [16, 33, 45],
            Self::Approval => [37, 30, 24],
            Self::Input => [32, 28, 48],
            Self::Error => [41, 27, 36],
            Self::Completed => [20, 37, 31],
            Self::Idle | Self::Unknown => [23, 31, 42],
        }
    }
}

/// Display-only card data. `progress` is a fraction only when a real plan
/// progress value exists; `None` deliberately draws no progress bar.
#[derive(Clone, Debug)]
pub struct AgentCardContent {
    /// Empty on the board, where the lane header names the project. The
    /// centered change card shows it because it is seen without its lane.
    pub project: String,
    pub title: String,
    pub state: AgentState,
    pub detail: String,
    pub provider_model: Option<String>,
    pub age: Option<String>,
    pub progress: Option<f32>,
    pub badge: Option<String>,
    /// A card behind others in a stack: only its top row is visible, so the
    /// state, title, and age share that row.
    pub compact: bool,
    /// A quiet child in a parent/child group, drawn as one low row: state dot,
    /// title, and age. The surface height is the row height (`ROW_BASE_HEIGHT`
    /// at text scale 1.0), so the caller chooses it by sizing the surface.
    pub one_line: bool,
    /// 複数の接続を監視しているときの、接続の短い目印(設定順の番号)。`None` なら従来どおり何も足さない。
    /// どのカードの種類(通常・重なった帯・1 行)でも、状態と経過時間より先に幅を確保して必ず描く。
    pub server: Option<String>,
}

/// Destination bounds in screen pixels. The center is kept stable by the
/// caller as the activity-driven width and height change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentPose {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub alpha: f32,
}

/// Cached card pixels and their current screen pose.
#[derive(Clone, Debug)]
pub struct AgentCardSurface {
    pub id: u64,
    /// Increment whenever card content or focus styling changes.
    pub revision: u64,
    pub surface: Arc<Pixmap>,
    pub pose: AgentPose,
}

/// A complete CPU scene. `background` may contain the header, legend, empty
/// state, central change message, and dimmed board. `None` reuses the prior
/// GPU background; CPU composition requires it to be present.
#[derive(Clone, Copy, Debug)]
pub struct AgentScene<'a> {
    pub size: (u32, u32),
    pub background: Option<&'a [u8]>,
    pub cards: &'a [AgentCardSurface],
    pub scrim_alpha: f32,
    pub focus_card_id: Option<u64>,
}

/// Rasterize a card once for a content revision and size. `focus_label` may be
/// used to render a central change heading; the caller controls its lifetime and pose.
pub fn render_card(content: &AgentCardContent, size: (u32, u32), text: &mut TextEngine) -> Pixmap {
    let (w, h) = (size.0.max(32), size.1.max(32));
    let mut pix = Pixmap::new(w, h).expect("invalid agent card dimensions");
    let (wf, hf) = (w as f32, h as f32);
    if content.one_line {
        return render_row(content, pix, text);
    }
    let s = (wf / BASE_CARD_WIDTH)
        .min(if content.compact {
            f32::MAX
        } else {
            hf / BASE_CARD_HEIGHT
        })
        .clamp(0.6, MAX_TEXT_SCALE);
    let radius = (14.0 * s).min(hf / 2.0);
    let [tr, tg, tb] = content.state.tint();
    let [cr, cg, cb] = content.state.color();
    let mut fill = Paint::default();
    fill.set_color(Color::from_rgba8(tr, tg, tb, 248));
    fill.anti_alias = true;
    let mut path = PathBuilder::new();
    path.move_to(radius, 0.0);
    path.line_to(wf - radius, 0.0);
    path.quad_to(wf, 0.0, wf, radius);
    path.line_to(wf, hf - radius);
    path.quad_to(wf, hf, wf - radius, hf);
    path.line_to(radius, hf);
    path.quad_to(0.0, hf, 0.0, hf - radius);
    path.line_to(0.0, radius);
    path.quad_to(0.0, 0.0, radius, 0.0);
    path.close();
    if let Some(path) = path.finish() {
        pix.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
        let mut edge = Paint::default();
        edge.set_color(Color::from_rgba8(cr, cg, cb, 255));
        edge.anti_alias = true;
        pix.stroke_path(
            &path,
            &edge,
            &tiny_skia::Stroke {
                width: 2.0,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }
    let mut canvas = Canvas { pixmap: pix };
    let state_color = [cr, cg, cb, 255];
    let muted = [143, 163, 183, 255];
    let bright = [223, 231, 240, 255];
    canvas.fill_rect(
        0.0,
        12.0 * s,
        7.0 * s,
        (hf - 24.0 * s).max(0.0),
        Color::from_rgba8(cr, cg, cb, 255),
    );
    let left = 26.0 * s;
    let right = wf - 22.0 * s;
    let inner = (right - left).max(20.0);
    let top = 12.0 * s;
    let label = content.state.label();
    let top_row = card_top_row(text, content, left, right, s);
    let row = top_row.size;
    if let Some(span) = top_row.age
        && let Some(age) = content.age.as_deref()
    {
        text.draw_right(&mut canvas, age, span.x + span.w, top, row, muted);
    }
    if let (Some(span), Some(tag)) = (top_row.tag, content.server.as_deref()) {
        text.draw(&mut canvas, tag, span.x, top, row, bright);
    }
    if let Some((span, project)) = &top_row.project {
        text.draw(&mut canvas, project, span.x, top, row, muted);
    }
    let x = top_row.state.x;
    text.draw(&mut canvas, label, x, top, row, state_color);
    if content.compact {
        let start = x + top_row.state.w + 18.0 * s;
        let age_w = top_row.age.map_or(0.0, |a| a.w);
        let room = right - age_w - 14.0 * s - start;
        if room > 24.0 {
            let title = fit_text(text, &content.title, 24.0 * s, room);
            text.draw(&mut canvas, &title, start, top, 24.0 * s, bright);
        }
        return canvas.pixmap;
    }

    // Bottom rows are anchored so taller cards spend their extra height on
    // wrapped title and detail lines.
    let bar_y = hf - 30.0 * s;
    let has_bar = content.progress.is_some_and(|p| p.is_finite());
    let meta_y = if has_bar {
        bar_y - 44.0 * s
    } else {
        hf - 52.0 * s
    };
    let text_bottom = if content.provider_model.is_some() {
        meta_y - 6.0 * s
    } else if has_bar {
        bar_y - 8.0 * s
    } else {
        hf - 14.0 * s
    };
    let mut y = 46.0 * s;
    if let Some(badge) = content.badge.as_deref() {
        let badge = fit_text(text, badge, 24.0 * s, inner);
        text.draw(&mut canvas, &badge, left, y, 24.0 * s, state_color);
        y += 34.0 * s;
    }
    let title_size = 31.0 * s;
    let title_line = 42.0 * s;
    let detail_size = 24.0 * s;
    let detail_line = 34.0 * s;
    let spare = ((text_bottom - y - title_line - detail_line) / title_line).floor();
    let title_lines = (1.0 + spare.clamp(0.0, 2.0)) as usize;
    for line in wrap_text(text, &content.title, title_size, inner, title_lines) {
        text.draw(&mut canvas, &line, left, y, title_size, bright);
        y += title_line;
    }
    y += 8.0 * s;
    let detail_lines = (((text_bottom - y) / detail_line).floor()).clamp(0.0, 3.0) as usize;
    for line in wrap_text(text, &content.detail, detail_size, inner, detail_lines) {
        text.draw(&mut canvas, &line, left, y, detail_size, bright);
        y += detail_line;
    }
    if let Some(meta) = content.provider_model.as_deref() {
        let meta = fit_text(text, meta, 21.0 * s, inner);
        text.draw(&mut canvas, &meta, left, meta_y, 21.0 * s, muted);
    }
    if let Some(p) = content.progress.filter(|p| p.is_finite()) {
        canvas.fill_rect(
            left,
            bar_y,
            inner,
            5.0 * s,
            Color::from_rgba8(48, 67, 87, 255),
        );
        canvas.fill_rect(
            left,
            bar_y,
            inner * p.clamp(0.0, 1.0),
            5.0 * s,
            Color::from_rgba8(cr, cg, cb, 255),
        );
    }
    canvas.pixmap
}

/// A one-line row: an accent bar, a state dot, the title, and the age on the right.
fn render_row(content: &AgentCardContent, mut pix: Pixmap, text: &mut TextEngine) -> Pixmap {
    let (wf, hf) = (pix.width() as f32, pix.height() as f32);
    let s = (hf / ROW_BASE_HEIGHT).clamp(0.6, MAX_TEXT_SCALE);
    let radius = (9.0 * s).min(hf / 2.0);
    let [tr, tg, tb] = content.state.tint();
    let [cr, cg, cb] = content.state.color();
    let mut fill = Paint::default();
    fill.set_color(Color::from_rgba8(tr, tg, tb, 248));
    fill.anti_alias = true;
    let mut path = PathBuilder::new();
    path.move_to(radius, 0.0);
    path.line_to(wf - radius, 0.0);
    path.quad_to(wf, 0.0, wf, radius);
    path.line_to(wf, hf - radius);
    path.quad_to(wf, hf, wf - radius, hf);
    path.line_to(radius, hf);
    path.quad_to(0.0, hf, 0.0, hf - radius);
    path.line_to(0.0, radius);
    path.quad_to(0.0, 0.0, radius, 0.0);
    path.close();
    if let Some(path) = path.finish() {
        pix.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
        let mut edge = Paint::default();
        edge.set_color(Color::from_rgba8(cr, cg, cb, 150));
        edge.anti_alias = true;
        pix.stroke_path(
            &path,
            &edge,
            &tiny_skia::Stroke {
                width: 1.5,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }
    let mut canvas = Canvas { pixmap: pix };
    let muted = [143, 163, 183, 255];
    let bright = [223, 231, 240, 255];
    let size = 21.0 * s;
    let top = (hf - size * 1.3) / 2.0;
    let left = 14.0 * s;
    canvas.fill_circle(
        left + 5.0 * s,
        hf / 2.0,
        5.0 * s,
        Color::from_rgba8(cr, cg, cb, 255),
    );
    let row = card_row(text, content, (wf, hf));
    if let (Some(span), Some(age)) = (row.age, content.age.as_deref()) {
        text.draw_right(&mut canvas, age, span.x + span.w, top, size * 0.9, muted);
    }
    if let (Some(span), Some(tag)) = (row.tag, content.server.as_deref()) {
        text.draw(&mut canvas, tag, span.x, top, size, bright);
    }
    if row.title_room > 24.0 {
        let title = fit_text(text, &content.title, size, row.title_room);
        text.draw(&mut canvas, &title, row.title_x, top, size, bright);
    }
    canvas.pixmap
}

/// 横方向の範囲(左端と幅)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub x: f32,
    pub w: f32,
}

impl Span {
    pub fn end(self) -> f32 {
        self.x + self.w
    }
}

/// 通常のカード・重なった帯の 1 行目(接続の目印 / 接続名・プロジェクト / 状態 / 経過時間)の並び。
#[derive(Clone, Debug, PartialEq)]
pub struct TopRow {
    /// 状態・目印・経過時間の文字の大きさ。幅が足りないときだけ小さくする。
    pub size: f32,
    pub tag: Option<Span>,
    /// 接続名・プロジェクトの文字(幅に収まるよう省略したもの)。足りなければ None(目印だけを残す)。
    pub project: Option<(Span, String)>,
    pub state: Span,
    /// 幅が足りなくなったときは、経過時間より先に目印と状態を残す。
    pub age: Option<Span>,
}

/// 1 行目の並びを実測で決める。`server` が None のときは、接続の目印を足す前の並びと同じ。
pub fn card_top_row(
    text: &mut TextEngine,
    content: &AgentCardContent,
    left: f32,
    right: f32,
    s: f32,
) -> TopRow {
    let inner = (right - left).max(20.0);
    let label = content.state.label();
    let age = content.age.as_deref().filter(|a| !a.is_empty());
    let mut size = 23.0 * s;
    let Some(tag) = content.server.as_deref().filter(|t| !t.is_empty()) else {
        // 接続の目印がない(1 台だけの表示): 従来の並び。
        let age_w = age.map_or(0.0, |a| text.measure(a, size));
        let label_w = text.measure(label, size);
        let mut x = left;
        let mut project = None;
        if !content.project.is_empty() {
            let room = inner - age_w - label_w - 40.0 * s;
            // 幅が足りないカード(列が多い盤面)では、状態と経過時間を優先して名前を省く(重ねて描かない)。
            if room >= 56.0 * s {
                let shown = fit_text(text, &content.project, size, room);
                let w = text.measure(&shown, size);
                project = Some((Span { x, w }, shown));
                x += w + 16.0 * s;
            }
        }
        return TopRow {
            size,
            tag: None,
            project,
            state: Span { x, w: label_w },
            age: age.map(|_| Span {
                x: right - age_w,
                w: age_w,
            }),
        };
    };
    // 目印・状態・経過時間が収まらないときは、まず文字を小さくし(下限 0.6 倍)、それでも足りなければ経過時間を省く。
    let floor = size * 0.6;
    let gap = 10.0 * s;
    let before_age = 14.0 * s;
    let mut with_age = age.is_some();
    let (tag_w, label_w, age_w) = loop {
        let tag_w = text.measure(tag, size);
        let label_w = text.measure(label, size);
        let age_w = match (with_age, age) {
            (true, Some(a)) => text.measure(a, size),
            _ => 0.0,
        };
        let need = tag_w + gap + label_w + if with_age { before_age + age_w } else { 0.0 };
        if need <= inner {
            break (tag_w, label_w, age_w);
        }
        if size > floor {
            size = (size * 0.92).max(floor);
        } else if with_age {
            with_age = false;
        } else {
            break (tag_w, label_w, age_w);
        }
    };
    let tag_span = Span { x: left, w: tag_w };
    let mut x = left + tag_w + gap;
    let need = tag_w + gap + label_w + if with_age { before_age + age_w } else { 0.0 };
    let mut project = None;
    if !content.project.is_empty() {
        let room = inner - need - 20.0 * s;
        if room >= 56.0 * s {
            let shown = fit_text(text, &content.project, size, room);
            let w = text.measure(&shown, size);
            project = Some((Span { x, w }, shown));
            x += w + 16.0 * s;
        }
    }
    TopRow {
        size,
        tag: Some(tag_span),
        project,
        state: Span { x, w: label_w },
        age: with_age.then_some(Span {
            x: right - age_w,
            w: age_w,
        }),
    }
}

/// 1 行のカードの並び(状態の点 / 接続の目印 / 題名 / 経過時間)。
#[derive(Clone, Debug, PartialEq)]
pub struct RowLayout {
    pub tag: Option<Span>,
    pub title_x: f32,
    /// 題名に使える幅(24 以下なら題名は描かない)。
    pub title_room: f32,
    pub age: Option<Span>,
}

/// 1 行のカード(`one_line`)の並びを実測で決める。
pub fn card_row(text: &mut TextEngine, content: &AgentCardContent, size: (f32, f32)) -> RowLayout {
    let (wf, hf) = size;
    let s = (hf / ROW_BASE_HEIGHT).clamp(0.6, MAX_TEXT_SCALE);
    let font = 21.0 * s;
    let left = 14.0 * s;
    let right = wf - 14.0 * s;
    let start = left + 20.0 * s;
    let tag_w = content
        .server
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(|t| text.measure(t, font));
    let mut age = content
        .age
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(|a| text.measure(a, font * 0.9));
    // 目印は残す。収まらないときだけ経過時間を省く。
    if let (Some(tw), Some(aw)) = (tag_w, age)
        && start + tw + 8.0 * s + aw + 12.0 * s > right
    {
        age = None;
    }
    let age_w = age.unwrap_or(0.0);
    let (tag, title_x) = match tag_w {
        Some(w) => (Some(Span { x: start, w }), start + w + 8.0 * s),
        None => (None, start),
    };
    RowLayout {
        tag,
        title_x,
        title_room: right - age_w - 12.0 * s - title_x,
        age: age.map(|w| Span { x: right - w, w }),
    }
}

/// A 1x1 premultiplied pixel. Scaled to any rectangle, it is a flat fill, so the
/// group frames and connector lines ride the same GPU/CPU card path as the cards
/// and follow a card's tweened pose frame by frame.
pub fn solid_surface(rgba: [u8; 4]) -> Pixmap {
    let mut pix = Pixmap::new(1, 1).expect("1x1 pixmap");
    pix.fill(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
    pix
}

/// Horizontal alignment of a text label inside its surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelAlign {
    Left,
    Right,
}

/// A transparent surface holding one line of text, ellipsized to fit `size`.
pub fn render_label(
    label: &str,
    size: (u32, u32),
    font: f32,
    color: [u8; 4],
    align: LabelAlign,
    text: &mut TextEngine,
) -> Pixmap {
    let (w, h) = (size.0.max(1), size.1.max(1));
    let mut canvas = Canvas {
        pixmap: Pixmap::new(w, h).expect("invalid label dimensions"),
    };
    let shown = fit_text(text, label, font, w as f32);
    let top = (h as f32 - font * 1.3) / 2.0;
    match align {
        LabelAlign::Left => text.draw(&mut canvas, &shown, 0.0, top, font, color),
        LabelAlign::Right => text.draw_right(&mut canvas, &shown, w as f32, top, font, color),
    }
    canvas.pixmap
}

/// Ellipsize `value` to `max_width` at `size` with the real font metrics.
pub fn fit_label_text(text: &mut TextEngine, value: &str, size: f32, max_width: f32) -> String {
    fit_text(text, value, size, max_width)
}

fn fit_text(text: &mut TextEngine, value: &str, size: f32, max_width: f32) -> String {
    if text.measure(value, size) <= max_width {
        return value.to_string();
    }
    let mut chars: Vec<char> = value.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut candidate: String = chars.iter().collect();
        candidate.push('…');
        if text.measure(&candidate, size) <= max_width {
            return candidate;
        }
    }
    "…".to_string()
}

/// Greedy per-character wrapping (Japanese has no word spaces); the last
/// allowed line is ellipsized when text remains.
fn wrap_text(
    text: &mut TextEngine,
    value: &str,
    size: f32,
    max_width: f32,
    max_lines: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    let mut start = 0;
    while start < chars.len() && lines.len() < max_lines {
        let rest: String = chars[start..].iter().collect();
        if lines.len() + 1 == max_lines || text.measure(&rest, size) <= max_width {
            lines.push(fit_text(text, rest.trim_start(), size, max_width));
            break;
        }
        let mut end = start + 1;
        while end < chars.len() {
            let candidate: String = chars[start..=end].iter().collect();
            if text.measure(&candidate, size) > max_width {
                break;
            }
            end += 1;
        }
        lines.push(
            chars[start..end]
                .iter()
                .collect::<String>()
                .trim_start()
                .to_string(),
        );
        start = end;
    }
    lines
}

/// Composite a scene with the same card surfaces and poses used by the GPU
/// path. This path is useful for alerts, menus, PNG review, and systems without
/// a GPU compositor.
pub fn render_scene_cpu(scene: &AgentScene<'_>) -> Result<Pixmap, String> {
    let (w, h) = scene.size;
    let bg = scene
        .background
        .ok_or_else(|| "agent CPU scene needs a background".to_string())?;
    if bg.len() != w as usize * h as usize * 4 {
        return Err("agent background size does not match scene".into());
    }
    let mut out = Pixmap::new(w, h).ok_or_else(|| "invalid agent scene dimensions".to_string())?;
    out.data_mut().copy_from_slice(bg);
    let shade = if scene.scrim_alpha.is_finite() {
        scene.scrim_alpha.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if shade > 0.0 {
        let alpha = (shade * 255.0).round() as u8;
        let mut black = Paint::default();
        black.set_color(Color::from_rgba8(0, 0, 0, alpha));
        out.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
            &black,
            Transform::identity(),
            None,
        );
    }
    for card in scene.cards {
        let (cw, ch) = (card.surface.width(), card.surface.height());
        if !card.pose.x.is_finite()
            || !card.pose.y.is_finite()
            || !card.pose.width.is_finite()
            || !card.pose.height.is_finite()
            || !card.pose.alpha.is_finite()
            || card.pose.width <= 0.0
            || card.pose.height <= 0.0
        {
            continue;
        }
        let scale_x = card.pose.width / cw as f32;
        let scale_y = card.pose.height / ch as f32;
        let transform =
            Transform::from_translate(card.pose.x, card.pose.y).pre_scale(scale_x, scale_y);
        let dim = if shade > 0.0 && scene.focus_card_id != Some(card.id) {
            1.0 - shade
        } else {
            1.0
        };
        out.draw_pixmap(
            0,
            0,
            card.surface.as_ref().as_ref(),
            &PixmapPaint {
                opacity: card.pose.alpha.clamp(0.0, 1.0) * dim,
                quality: FilterQuality::Bilinear,
                ..Default::default()
            },
            transform,
            None,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_and_label_surfaces_follow_the_card_path() {
        let mut text = TextEngine::new("Noto Sans CJK JP");
        let solid = Arc::new(solid_surface([52, 70, 92, 255]));
        let label = Arc::new(render_label(
            "エージェント画面の改善の子・続き",
            (160, 28),
            17.0,
            [143, 163, 183, 255],
            LabelAlign::Left,
            &mut text,
        ));
        assert_eq!((label.width(), label.height()), (160, 28));
        let bg = [8u8, 12, 20, 255].repeat(200 * 100);
        let layer = |id, surface: &Arc<Pixmap>, x, y, w, h| AgentCardSurface {
            id,
            revision: 1,
            surface: surface.clone(),
            pose: AgentPose {
                x,
                y,
                width: w,
                height: h,
                alpha: 1.0,
            },
        };
        let cards = [
            layer(1, &solid, 10.0, 10.0, 100.0, 2.0),
            layer(2, &solid, 10.0, 10.0, 2.0, 60.0),
            layer(3, &label, 20.0, 30.0, 160.0, 28.0),
        ];
        let frame = render_scene_cpu(&AgentScene {
            size: (200, 100),
            background: Some(&bg),
            cards: &cards,
            scrim_alpha: 0.0,
            focus_card_id: None,
        })
        .unwrap();
        let px = |x: usize, y: usize| frame.data()[(y * 200 + x) * 4..][..3].to_vec();
        assert_eq!(px(50, 10), [52, 70, 92]);
        assert_eq!(px(10, 50), [52, 70, 92]);
        assert_eq!(px(150, 90), [8, 12, 20]);
        let lit = (30..58)
            .flat_map(|y| (20..180).map(move |x| (x, y)))
            .filter(|&(x, y)| px(x, y)[0] > 60)
            .count();
        assert!(lit > 20, "the label text is composed: {lit}");
    }

    #[test]
    fn japanese_cards_compose_at_full_hd_and_ultrawide() {
        let mut text = TextEngine::new("Noto Sans CJK JP");
        let content = AgentCardContent {
            project: "miharashi 日本語プロジェクト名".into(),
            title: "Agent カードの GPU 合成を実装する".into(),
            state: AgentState::Approval,
            detail: "計画の確認を待っています。本文がカード内に収まることを確認します。".into(),
            provider_model: Some("Codex / 選択モデル名".into()),
            age: Some("最後の動きから 2 分".into()),
            progress: Some(0.5),
            badge: Some("状態が変わりました".into()),
            compact: false,
            one_line: false,
            server: None,
        };
        let card = Arc::new(render_card(&content, (600, 300), &mut text));
        let tall = render_card(&content, (480, 700), &mut text);
        assert_eq!((tall.width(), tall.height()), (480, 700));
        let strip = render_card(
            &AgentCardContent {
                compact: true,
                project: String::new(),
                ..content.clone()
            },
            (480, 210),
            &mut text,
        );
        assert_eq!((strip.width(), strip.height()), (480, 210));
        let row = render_card(
            &AgentCardContent {
                one_line: true,
                project: String::new(),
                ..content.clone()
            },
            (480, ROW_BASE_HEIGHT as u32),
            &mut text,
        );
        assert_eq!((row.width(), row.height()), (480, ROW_BASE_HEIGHT as u32));
        // 状態の点と題名が、1 行の高さの中に描かれる(余白だけではない)。
        let inked = row
            .data()
            .chunks(4)
            .filter(|p| p[0] > 150 && p[1] > 150 && p[2] > 150)
            .count();
        assert!(inked > 100, "the title text is drawn: {inked}");
        for (width, name) in [
            (1920, "agents-1920x1080.png"),
            (2560, "agents-2560x1080.png"),
        ] {
            let size = (width, 1080);
            let bg = [8u8, 12, 20, 255].repeat(size.0 as usize * size.1 as usize);
            let layer = AgentCardSurface {
                id: 7,
                revision: 1,
                surface: card.clone(),
                pose: AgentPose {
                    x: 120.0,
                    y: 240.0,
                    width: 600.0,
                    height: 300.0,
                    alpha: 1.0,
                },
            };
            let scene = AgentScene {
                size,
                background: Some(&bg),
                cards: &[layer],
                scrim_alpha: 0.0,
                focus_card_id: None,
            };
            let frame = render_scene_cpu(&scene).unwrap();
            assert_eq!((frame.width(), frame.height()), size);
            let offset = (280usize * width as usize + 160) * 4;
            assert_ne!(&frame.data()[offset..offset + 3], &[8, 12, 20]);
            let path = std::env::temp_dir().join(name);
            frame.save_png(&path).unwrap();
        }
    }

    fn sample(
        server: Option<&str>,
        project: &str,
        state: AgentState,
        age: &str,
    ) -> AgentCardContent {
        AgentCardContent {
            project: project.into(),
            title: "接続名の識別を確かめる長い題名のスレッド".into(),
            state,
            detail: "手元の T3 で承認".into(),
            provider_model: Some("Codex · 選択モデル".into()),
            age: Some(age.into()),
            progress: None,
            badge: None,
            compact: false,
            one_line: false,
            server: server.map(str::to_string),
        }
    }

    fn assert_disjoint(spans: &[(&str, Span)], right: f32, what: &str) {
        let mut v: Vec<&(&str, Span)> = spans.iter().collect();
        v.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
        for pair in v.windows(2) {
            assert!(
                pair[0].1.end() <= pair[1].1.x + 0.01,
                "{what}: {} ({:?}) overlaps {} ({:?})",
                pair[0].0,
                pair[0].1,
                pair[1].0,
                pair[1].1
            );
        }
        for (name, sp) in spans {
            assert!(sp.end() <= right + 0.01, "{what}: {name} {sp:?} > {right}");
        }
    }

    /// 接続の目印があるカードの 1 行目は、最小幅から広い幅まで、目印・接続名・状態・経過時間が重ならず、
    /// 目印と状態は必ず残る。目印がないときは従来の並び(目印なし・状態は左端)。
    #[test]
    fn top_row_keeps_tag_and_state_and_never_overlaps() {
        let mut text = TextEngine::new("Noto Sans CJK JP");
        let long = "とても長い接続名の接続とても長い接続名の接続とても / 日本語プロジェクト";
        for state in [AgentState::Input, AgentState::Approval, AgentState::Running] {
            for age in ["1分未満", "12時間"] {
                // 通常のカードの最小幅 (380 * 0.6) から、広い列まで。
                for w in [228.0f32, 260.0, 300.0, 380.0, 440.0, 600.0, 880.0] {
                    let s = (w / BASE_CARD_WIDTH).clamp(0.6, MAX_TEXT_SCALE);
                    let (left, right) = (26.0 * s, w - 22.0 * s);
                    for project in ["", long, "コンテナ"] {
                        let c = sample(Some("②"), project, state, age);
                        let row = card_top_row(&mut text, &c, left, right, s);
                        let tag = row.tag.expect("the tag is always kept");
                        assert!(tag.w > 1.0);
                        assert!(row.state.w > 1.0);
                        let mut spans = vec![("tag", tag), ("state", row.state)];
                        if let Some((sp, shown)) = &row.project {
                            assert!(!shown.is_empty());
                            spans.push(("project", *sp));
                        }
                        if let Some(a) = row.age {
                            spans.push(("age", a));
                        }
                        assert_disjoint(&spans, right, &format!("w={w} {project} {age}"));
                        assert!(tag.x >= left - 0.01);
                        // 経過時間を省くのは、目印と状態だけでも足りないほど狭いときだけ。
                        if row.age.is_none() {
                            let need = tag.w + 10.0 * s + row.state.w;
                            assert!(need > (right - left) * 0.5, "w={w}");
                        }
                    }
                }
            }
        }
        // 十分に広ければ接続名も出る。
        let s = 1.0;
        let c = sample(Some("②"), "コンテナ", AgentState::Approval, "3分");
        let wide = card_top_row(&mut text, &c, 26.0 * s, 440.0 - 22.0 * s, s);
        assert_eq!(
            wide.project.as_ref().map(|(_, t)| t.as_str()),
            Some("コンテナ")
        );
        // 目印がないときは従来どおり(目印なし、接続名がなければ状態が左端)。
        let c = sample(None, "", AgentState::Approval, "3分");
        let legacy = card_top_row(&mut text, &c, 26.0, 418.0, 1.0);
        assert!(legacy.tag.is_none() && legacy.project.is_none());
        assert_eq!(legacy.state.x, 26.0);
        assert_eq!(legacy.size, 23.0);
    }

    /// 1 行のカードも、目印を題名・経過時間より先に確保して重ならない。
    #[test]
    fn one_line_row_keeps_the_tag_and_never_overlaps() {
        let mut text = TextEngine::new("Noto Sans CJK JP");
        for w in [150.0f32, 200.0, 228.0, 300.0, 440.0, 800.0] {
            for age in ["1分未満", "12時間"] {
                let mut c = sample(Some("④"), "", AgentState::Running, age);
                c.one_line = true;
                let layout = card_row(&mut text, &c, (w, ROW_BASE_HEIGHT));
                let tag = layout.tag.expect("the tag is kept");
                let mut spans = vec![("tag", tag)];
                if let Some(a) = layout.age {
                    spans.push(("age", a));
                }
                if layout.title_room > 24.0 {
                    spans.push((
                        "title",
                        Span {
                            x: layout.title_x,
                            w: layout.title_room,
                        },
                    ));
                }
                assert_disjoint(&spans, w, &format!("row w={w} {age}"));
            }
        }
    }

    fn ink(p: &Pixmap, x0: u32, x1: u32, y0: u32, y1: u32) -> usize {
        (y0..y1.min(p.height()))
            .flat_map(|y| (x0..x1.min(p.width())).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let d = &p.data()[((y * p.width() + x) * 4) as usize..][..3];
                d[0] > 150 && d[1] > 150 && d[2] > 150
            })
            .count()
    }

    /// 目印は実際に描かれ(明るい字の画素が増える)、通常・帯・1 行のどれでも、目印の範囲に字が入る。
    #[test]
    fn the_tag_is_drawn_on_every_card_mode() {
        let mut text = TextEngine::new("Noto Sans CJK JP");
        for (label, compact, one_line, size) in [
            ("full", false, false, (300u32, 210u32)),
            ("strip", true, false, (300, 58)),
            ("row", false, true, (300, 38)),
        ] {
            let mut c = sample(Some("③"), "", AgentState::Approval, "3分");
            c.compact = compact;
            c.one_line = one_line;
            let with = render_card(&c, size, &mut text);
            c.server = None;
            let without = render_card(&c, size, &mut text);
            let (span, s) = if one_line {
                let l = card_row(
                    &mut text,
                    &sample(Some("③"), "", AgentState::Approval, "3分"),
                    (size.0 as f32, size.1 as f32),
                );
                (l.tag.unwrap(), 1.0)
            } else {
                let s = (size.0 as f32 / BASE_CARD_WIDTH).clamp(0.6, MAX_TEXT_SCALE);
                let mut c = sample(Some("③"), "", AgentState::Approval, "3分");
                c.compact = compact;
                let t = card_top_row(&mut text, &c, 26.0 * s, size.0 as f32 - 22.0 * s, s);
                (t.tag.unwrap(), s)
            };
            let _ = s;
            let (x0, x1) = (span.x.floor() as u32, span.end().ceil() as u32 + 1);
            let tagged_ink = ink(&with, x0, x1, 0, size.1.min(60));
            assert!(tagged_ink > 15, "{label}: the tag is inked: {tagged_ink}");
            if !one_line {
                // 目印がないときは、同じ範囲に状態の字が来る(目印の分だけ右へ動く)。
                assert_ne!(with.data(), without.data(), "{label}");
            }
        }
    }
}
