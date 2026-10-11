use crate::canvas::rgba;
use crate::{Canvas, TextEngine};

pub struct Series<'a> {
    pub name: &'a str,
    pub data: &'a [f32],
    pub color: [u8; 4],
}

/// 複数系列の時系列折れ線グラフ(値は 0..=max)。凡例は右上。
pub fn line_chart(c: &mut Canvas, t: &mut TextEngine, x: f32, y: f32, w: f32, h: f32, title: &str, series: &[Series], max: f32) {
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, 1.0, rgba(60, 80, 105, 255));
    t.draw(c, title, x + 16.0, y + 10.0, 24.0, [180, 200, 220, 255]);

    let mut lx = x + w - 16.0;
    for s in series.iter().rev() {
        let tw = t.measure(s.name, 22.0);
        lx -= tw;
        t.draw(c, s.name, lx, y + 12.0, 22.0, s.color);
        lx -= 22.0;
        c.fill_rect(lx, y + 21.0, 14.0, 6.0, rgba(s.color[0], s.color[1], s.color[2], 255));
        lx -= 28.0;
    }

    let (px, py, pw, ph) = (x + 64.0, y + 56.0, w - 88.0, h - 80.0);
    for i in 0..=4 {
        let gy = py + ph * i as f32 / 4.0;
        c.hline(px, gy, pw, rgba(50, 66, 86, 255));
        let label = format!("{:>3}%", (max * (4 - i) as f32 / 4.0) as i32);
        t.draw_right(c, &label, px - 8.0, gy - 11.0, 18.0, [130, 150, 170, 255]);
    }
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        let pts: Vec<(f32, f32)> = s
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| (px + pw * i as f32 / (s.data.len() - 1) as f32, py + ph * (1.0 - (v / max).clamp(0.0, 1.0))))
            .collect();
        c.fill_under(&pts, py + ph, rgba(s.color[0], s.color[1], s.color[2], 36));
        c.polyline(&pts, 2.5, rgba(s.color[0], s.color[1], s.color[2], 255));
    }
}
