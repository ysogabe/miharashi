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
    let row = 23.0 * s;
    let age = content.age.as_deref().unwrap_or("");
    let age_w = if age.is_empty() {
        0.0
    } else {
        text.measure(age, row)
    };
    if !age.is_empty() {
        text.draw_right(&mut canvas, age, right, top, row, muted);
    }
    let mut x = left;
    if !content.project.is_empty() {
        let room = (inner - age_w - text.measure(label, row) - 40.0 * s).max(40.0);
        let project = fit_text(text, &content.project, row, room);
        text.draw(&mut canvas, &project, x, top, row, muted);
        x += text.measure(&project, row) + 16.0 * s;
    }
    text.draw(&mut canvas, label, x, top, row, state_color);
    if content.compact {
        let start = x + text.measure(label, row) + 18.0 * s;
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
}
