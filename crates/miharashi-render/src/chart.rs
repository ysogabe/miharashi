use crate::canvas::rgba;
use crate::{Canvas, TextEngine};

/// 時系列折れ線グラフ(値は 0..=max)。
pub fn line_chart(c: &mut Canvas, t: &mut TextEngine, x: f32, y: f32, w: f32, h: f32, title: &str, data: &[f32], max: f32) {
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, 1.0, rgba(60, 80, 105, 255));
    t.draw(c, title, x + 14.0, y + 8.0, 22.0, [180, 200, 220, 255]);
    let (px, py, pw, ph) = (x + 56.0, y + 46.0, w - 76.0, h - 66.0);
    for i in 0..=4 {
        let gy = py + ph * i as f32 / 4.0;
        c.hline(px, gy, pw, rgba(50, 66, 86, 255));
        let label = format!("{:>3}", (max * (4 - i) as f32 / 4.0) as i32);
        t.draw_right(c, &label, px - 8.0, gy - 10.0, 16.0, [130, 150, 170, 255]);
    }
    if data.len() < 2 {
        return;
    }
    let pts: Vec<(f32, f32)> = data
        .iter()
        .enumerate()
        .map(|(i, v)| (px + pw * i as f32 / (data.len() - 1) as f32, py + ph * (1.0 - (v / max).clamp(0.0, 1.0))))
        .collect();
    c.fill_under(&pts, py + ph, rgba(64, 200, 255, 50));
    c.polyline(&pts, 2.5, rgba(64, 200, 255, 255));
}
