use std::path::Path;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

/// premultiplied RGBA8、上から下、stride = width * 4。
pub struct Canvas {
    pub pixmap: Pixmap,
}

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self { pixmap: Pixmap::new(width, height).expect("invalid canvas size") }
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    pub fn clear(&mut self, color: Color) {
        self.pixmap.fill(color);
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            self.pixmap.fill_rect(r, &paint(color), Transform::identity(), None);
        }
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: Color) {
        let mut pb = PathBuilder::new();
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            pb.push_rect(r);
        }
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
        }
    }

    pub fn hline(&mut self, x: f32, y: f32, w: f32, color: Color) {
        self.fill_rect(x, y, w, 1.0, color);
    }

    pub fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Color) {
        if points.len() < 2 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for p in &points[1..] {
            pb.line_to(p.0, p.1);
        }
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width, line_join: tiny_skia::LineJoin::Round, ..Stroke::default() };
            self.pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
        }
    }

    /// 折れ線の下を塗りつぶす(グラフ用)。
    pub fn fill_under(&mut self, points: &[(f32, f32)], baseline: f32, color: Color) {
        if points.len() < 2 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, baseline);
        for p in points {
            pb.line_to(p.0, p.1);
        }
        pb.line_to(points[points.len() - 1].0, baseline);
        pb.close();
        if let Some(path) = pb.finish() {
            self.pixmap.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
        }
    }

    /// 他の Pixmap を (x, y) に合成する。
    pub fn draw_pixmap(&mut self, src: &Pixmap, x: i32, y: i32) {
        self.pixmap.draw_pixmap(x, y, src.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    }

    /// 不透明度 `opacity`(0.0..=1.0)で他の Pixmap を合成する(クロスフェード用)。
    pub fn draw_pixmap_opacity(&mut self, src: &Pixmap, x: i32, y: i32, opacity: f32) {
        let paint = PixmapPaint { opacity: opacity.clamp(0.0, 1.0), ..PixmapPaint::default() };
        self.pixmap.draw_pixmap(x, y, src.as_ref(), &paint, Transform::identity(), None);
    }

    /// 1 画素を src-over で合成する(文字描画用)。color は straight RGBA。
    pub fn blend_pixel(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
        if x < 0 || y < 0 || x >= self.width() as i32 || y >= self.height() as i32 || a == 0 {
            return;
        }
        let idx = (y as usize * self.width() as usize + x as usize) * 4;
        let data = self.pixmap.data_mut();
        let sa = a as u32;
        let inv = 255 - sa;
        let mul = |c: u8| (c as u32 * sa + 127) / 255;
        data[idx] = (mul(r) + (data[idx] as u32 * inv + 127) / 255) as u8;
        data[idx + 1] = (mul(g) + (data[idx + 1] as u32 * inv + 127) / 255) as u8;
        data[idx + 2] = (mul(b) + (data[idx + 2] as u32 * inv + 127) / 255) as u8;
        data[idx + 3] = (sa + (data[idx + 3] as u32 * inv + 127) / 255) as u8;
    }

    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        self.pixmap.save_png(path).map_err(|e| e.to_string())
    }

    /// KMS 用: XRGB8888(メモリ上 B,G,R,X)へ変換して dst の各行(pitch バイト)へ書く。
    pub fn write_xrgb8888(&self, dst: &mut [u8], pitch: usize) {
        let w = self.width() as usize;
        for (y, row) in self.pixmap.data().chunks_exact(w * 4).enumerate() {
            let out = &mut dst[y * pitch..y * pitch + w * 4];
            for (s, d) in row.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
            }
        }
    }
}
