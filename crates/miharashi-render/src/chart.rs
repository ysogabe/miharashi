use crate::canvas::rgba;
use crate::{Canvas, TextEngine};

pub struct Series<'a> {
    pub name: &'a str,
    pub data: &'a [f32],
    pub color: [u8; 4],
}

/// 縦軸の決め方。
#[derive(Debug, Clone, Copy)]
pub enum Scale {
    /// 0..=max に固定
    Fixed(f32),
    /// 系列の最大値を切りのよい値へ切り上げる(最小値は `min`)
    Auto { min: f32 },
}

/// `v` 以上で、1/4 刻みが切りのよい値になる最大値(例: 237 -> 240、51 -> 60)。
pub fn nice_max(v: f32, min: f32) -> f32 {
    let v = v.max(min).max(f32::MIN_POSITIVE);
    let exp = 10f32.powf(v.log10().floor());
    for m in [1.0, 1.2, 1.6, 2.0, 2.4, 3.0, 4.0, 6.0, 8.0, 10.0] {
        if m * exp >= v * 0.9999 {
            return m * exp;
        }
    }
    10.0 * exp
}

/// 複数系列の時系列折れ線グラフ(値は 0..=max、軸ラベルは %)。凡例は右上。
#[allow(clippy::too_many_arguments)]
pub fn line_chart(
    c: &mut Canvas,
    t: &mut TextEngine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: &str,
    series: &[Series],
    max: f32,
) {
    line_chart_ex(c, t, x, y, w, h, title, series, Scale::Fixed(max), "%");
}

/// `line_chart` の拡張版。縦軸の自動調整と、軸ラベルの単位(`unit`)を指定できる。
/// データが 2 点未満なら、プロット領域に "--" を出す。
#[allow(clippy::too_many_arguments)]
pub fn line_chart_ex(
    c: &mut Canvas,
    t: &mut TextEngine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    title: &str,
    series: &[Series],
    scale: Scale,
    unit: &str,
) {
    let max = match scale {
        Scale::Fixed(m) => m,
        Scale::Auto { min } => {
            let peak = series
                .iter()
                .flat_map(|s| s.data.iter().copied())
                .filter(|v| v.is_finite())
                .fold(0.0, f32::max);
            nice_max(peak, min)
        }
    };
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, 1.0, rgba(60, 80, 105, 255));
    t.draw(c, title, x + 16.0, y + 10.0, 24.0, [180, 200, 220, 255]);

    // 凡例がタイトルまで届くときは、収まるまで文字と間隔を縮める(最小 60%)
    let legend_w: f32 = series.iter().map(|s| t.measure(s.name, 22.0) + 50.0).sum();
    let room = w - 16.0 - (16.0 + t.measure(title, 24.0) + 16.0);
    let k = (room / legend_w).clamp(0.6, 1.0);
    let mut lx = x + w - 16.0;
    for s in series.iter().rev() {
        let tw = t.measure(s.name, 22.0 * k);
        lx -= tw;
        t.draw(
            c,
            s.name,
            lx,
            y + 12.0 + 11.0 * (1.0 - k),
            22.0 * k,
            s.color,
        );
        lx -= 22.0 * k;
        c.fill_rect(
            lx,
            y + 21.0,
            14.0 * k,
            6.0,
            rgba(s.color[0], s.color[1], s.color[2], 255),
        );
        lx -= 28.0 * k;
    }

    let (px, py, pw, ph) = (x + 84.0, y + 56.0, w - 108.0, h - 80.0);
    for i in 0..=4 {
        let gy = py + ph * i as f32 / 4.0;
        c.hline(px, gy, pw, rgba(50, 66, 86, 255));
        let v = max * (4 - i) as f32 / 4.0;
        let label = if max < 10.0 {
            format!("{v:.1}{unit}")
        } else {
            format!("{v:.0}{unit}")
        };
        t.draw_right(c, &label, px - 8.0, gy - 11.0, 18.0, [130, 150, 170, 255]);
    }
    if series.iter().all(|s| s.data.len() < 2) {
        let mw = t.measure("--", 32.0);
        t.draw(
            c,
            "--",
            px + (pw - mw) / 2.0,
            py + ph / 2.0 - 20.0,
            32.0,
            [130, 150, 170, 255],
        );
    }
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        let pts: Vec<(f32, f32)> = s
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    px + pw * i as f32 / (s.data.len() - 1) as f32,
                    py + ph * (1.0 - (v / max).clamp(0.0, 1.0)),
                )
            })
            .collect();
        c.fill_under(&pts, py + ph, rgba(s.color[0], s.color[1], s.color[2], 36));
        c.polyline(&pts, 2.5, rgba(s.color[0], s.color[1], s.color[2], 255));
    }
}
