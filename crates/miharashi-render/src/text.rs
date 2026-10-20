use crate::Canvas;
use cosmic_text::fontdb;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};

pub struct TextEngine {
    fonts: FontSystem,
    swash: SwashCache,
    family: String,
    weight: u16,
}

impl TextEngine {
    /// `family` は日本語対応フォント名(例: "Noto Sans CJK JP")。
    pub fn new(family: &str) -> Self {
        Self {
            fonts: FontSystem::new(),
            swash: SwashCache::new(),
            family: family.to_string(),
            weight: Weight::NORMAL.0,
        }
    }

    /// 字の太さ(400 = 標準、700 = 太字)。既定は標準。
    pub fn with_weight(mut self, weight: u16) -> Self {
        self.weight = weight;
        self
    }

    /// フォントファイルをヒープへ読み込んで使う。システムフォント(mmap)には依存しないので、
    /// ページキャッシュが破棄されても描画時間が揺れない。
    pub fn from_font_file(family: &str, path: &std::path::Path) -> std::io::Result<Self> {
        let mut db = fontdb::Database::new();
        db.load_font_data(std::fs::read(path)?);
        Ok(Self {
            fonts: FontSystem::new_with_locale_and_db("ja-JP".to_string(), db),
            swash: SwashCache::new(),
            family: family.to_string(),
            weight: Weight::NORMAL.0,
        })
    }

    fn buffer(&mut self, text: &str, size: f32) -> Buffer {
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(size, size * 1.3));
        buf.set_size(None, None);
        let attrs = Attrs::new()
            .family(Family::Name(&self.family))
            .weight(Weight(self.weight));
        buf.set_text(text, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.fonts, false);
        buf
    }

    pub fn measure(&mut self, text: &str, size: f32) -> f32 {
        let buf = self.buffer(text, size);
        buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    /// (x, y) を左上として描画。color は straight RGBA。
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: [u8; 4],
    ) {
        let mut buf = self.buffer(text, size);
        let c = Color::rgba(color[0], color[1], color[2], color[3]);
        let (x, y) = (x as i32, y as i32);
        buf.draw(
            &mut self.fonts,
            &mut self.swash,
            c,
            |gx, gy, gw, gh, col| {
                for dy in 0..gh as i32 {
                    for dx in 0..gw as i32 {
                        canvas.blend_pixel(
                            x + gx + dx,
                            y + gy + dy,
                            col.r(),
                            col.g(),
                            col.b(),
                            col.a(),
                        );
                    }
                }
            },
        );
    }

    pub fn draw_right(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right: f32,
        y: f32,
        size: f32,
        color: [u8; 4],
    ) {
        let w = self.measure(text, size);
        self.draw(canvas, text, right - w, y, size, color);
    }
}
