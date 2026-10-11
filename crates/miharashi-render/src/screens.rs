use crate::canvas::rgba;
use crate::{chart, Canvas, TextEngine};
use tiny_skia::Pixmap;

/// 画面に渡す(ダミー)データ。
pub struct Snapshot {
    pub clock: String,
    /// 取得できない項目は None(画面には "--" と表示する)。
    pub cpu: Option<f32>,
    pub cpu_temp: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub mem_used: Option<f32>,
    pub mem_total: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub photo_caption: String,
}

const FG: [u8; 4] = [225, 235, 245, 255];
const DIM: [u8; 4] = [140, 160, 180, 255];

/// 画面1: 情報ダッシュボード(左に写真、右にステータスとグラフ)。
pub fn dashboard(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, photo_left: Option<&Pixmap>) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    c.clear(rgba(8, 12, 20, 255));
    let (head, foot) = (72.0, 64.0);
    let body_h = h - head - foot;
    let split = (w * 0.5).round();

    t.draw(c, "SERVER DASHBOARD", 32.0, 16.0, 36.0, FG);
    t.draw_right(c, &s.clock, w - 32.0, 22.0, 30.0, DIM);
    c.hline(0.0, head, w, rgba(60, 80, 105, 255));

    match photo_left {
        Some(p) => c.draw_pixmap(p, 0, head as i32 + 1),
        None => c.fill_rect(0.0, head + 1.0, split, body_h, rgba(30, 40, 55, 255)),
    }

    let (x, mut y) = (split + 32.0, head + 24.0);
    t.draw(c, "SYSTEM STATUS", x, y, 28.0, DIM);
    y += 56.0;
    let pct = |v: Option<f32>| v.map_or("--".to_string(), |v| format!("{v:.0}%"));
    let deg = |v: Option<f32>| v.map_or("--".to_string(), |v| format!("{v:.0}°C"));
    let mem = match (s.mem_used, s.mem_total) {
        (Some(u), Some(t)) => format!("{u:.1} / {t:.0} GB"),
        _ => "--".to_string(),
    };
    let rows = [
        ("CPU", pct(s.cpu), deg(s.cpu_temp)),
        ("GPU", pct(s.gpu), deg(s.gpu_temp)),
        ("MEMORY", mem, String::new()),
    ];
    for (name, val, extra) in rows {
        t.draw(c, name, x, y, 32.0, FG);
        t.draw(c, &val, x + 220.0, y, 32.0, FG);
        t.draw(c, &extra, x + 480.0, y, 32.0, DIM);
        y += 50.0;
    }
    y += 16.0;
    let chart_h = head + body_h - y - 24.0;
    chart::line_chart(c, t, x, y, w - x - 32.0, chart_h, "CPU使用率 時系列グラフ", &s.cpu_history, 100.0);

    let fy = h - foot;
    c.hline(0.0, fy, w, rgba(60, 80, 105, 255));
    let cell = w / 3.0;
    for (i, label) in ["4: PREV", "5: DETAILS", "6: NEXT"].iter().enumerate() {
        let tw = t.measure(label, 28.0);
        t.draw(c, label, cell * i as f32 + (cell - tw) / 2.0, fy + 16.0, 28.0, DIM);
    }
}

/// 画面2: 全画面写真 + 半透明の情報帯。
pub fn photo_frame(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, photo: Option<&Pixmap>) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    c.clear(rgba(0, 0, 0, 255));
    if let Some(p) = photo {
        c.draw_pixmap(p, 0, 0);
    }
    let band = 120.0;
    c.fill_rect(0.0, h - band, w, band, rgba(0, 0, 0, 150));
    t.draw(c, &s.clock, 40.0, h - band + 14.0, 44.0, [255, 255, 255, 255]);
    t.draw(c, &s.photo_caption, 40.0, h - band + 70.0, 28.0, [220, 225, 230, 255]);
}
