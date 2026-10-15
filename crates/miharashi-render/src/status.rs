//! 状況画面(Server Status)。OVERVIEW と DETAILS の 2 ページ。
use crate::canvas::rgba;
use crate::chart::{self, Scale, Series};
use crate::screens::Snapshot;
use crate::{Canvas, TextEngine};

const FG: [u8; 4] = [225, 235, 245, 255];
const DIM: [u8; 4] = [140, 160, 180, 255];
const WARN: [u8; 4] = [255, 170, 60, 255];
const CRIT: [u8; 4] = [255, 90, 80, 255];
pub const CPU_COLOR: [u8; 4] = [64, 200, 255, 255];
pub const GPU_COLOR: [u8; 4] = [255, 170, 60, 255];
/// GPU ごとの色。GPU0 は GPU_COLOR、以降は順に使う。
const GPU_COLORS: [[u8; 4]; 4] = [
    GPU_COLOR,
    [255, 110, 170, 255],
    [140, 220, 110, 255],
    [190, 150, 255, 255],
];

/// `i` 番目の GPU の色。
pub fn gpu_color(i: usize) -> [u8; 4] {
    GPU_COLORS[i % GPU_COLORS.len()]
}

const HEAD: f32 = 72.0;
const FOOT: f32 = 64.0;
const PAD: f32 = 32.0;
const GAP: f32 = 20.0;
const TILE_H: f32 = 250.0;

type Rect = (f32, f32, f32, f32);
type Row = (String, String, [u8; 4]);

/// 状況画面のページ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusPage {
    #[default]
    Overview,
    Details,
}

impl StatusPage {
    /// もう一方のページ。
    pub fn toggled(self) -> Self {
        match self {
            Self::Overview => Self::Details,
            Self::Details => Self::Overview,
        }
    }
}

fn line() -> tiny_skia::Color {
    rgba(60, 80, 105, 255)
}

/// 値の大きさで色を変える(warn 以上でオレンジ、crit 以上で赤)。
fn level_color(v: f32, warn: f32, crit: f32) -> [u8; 4] {
    if v >= crit {
        CRIT
    } else if v >= warn {
        WARN
    } else {
        FG
    }
}

fn temp_color(v: f32) -> [u8; 4] {
    level_color(v, 80.0, 90.0)
}

fn pct(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.0}%"))
}

fn deg(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.0}°C"))
}

fn watt(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.0} W"))
}

fn kwh(wh: Option<f64>) -> String {
    wh.map_or("--".to_string(), |v| format!("{:.2} kWh", v / 1000.0))
}

fn mbps(v: Option<f32>) -> String {
    v.map_or("--".to_string(), |v| format!("{v:.1} Mbps"))
}

fn num(v: Option<u32>) -> String {
    v.map_or("--".to_string(), |v| v.to_string())
}

fn swap_text(s: &Snapshot) -> String {
    match (s.swap_used_gb, s.swap_total_gb) {
        (Some(u), Some(t)) => format!("{u:.1} / {t:.1} GB"),
        (Some(u), None) => format!("{u:.1} GB"),
        _ => "--".to_string(),
    }
}

/// 幅 `max_w` に収まる文字列と文字サイズ。まず 65% まで縮め、それでも入らなければ末尾を「…」で切る。
fn fit(t: &mut TextEngine, text: &str, size: f32, max_w: f32) -> (String, f32) {
    let w = t.measure(text, size);
    if w <= max_w {
        return (text.to_string(), size);
    }
    let small = (size * max_w / w).max(size * 0.65);
    if t.measure(text, small) <= max_w {
        return (text.to_string(), small);
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.len() > 1 {
        chars.pop();
        let s: String = chars.iter().collect::<String>() + "…";
        if t.measure(&s, small) <= max_w {
            return (s, small);
        }
    }
    ("…".to_string(), small)
}

/// 左寄せで描き、幅に収まらなければ縮める(縦は元のサイズの中央に揃える)。描いた幅を返す。
#[allow(clippy::too_many_arguments)]
fn fit_left(
    c: &mut Canvas,
    t: &mut TextEngine,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    max_w: f32,
    color: [u8; 4],
) -> f32 {
    let (s, sz) = fit(t, text, size, max_w);
    t.draw(c, &s, x, y + (size - sz) * 0.65, sz, color);
    t.measure(&s, sz)
}

/// 右寄せ版の `fit_left`。
#[allow(clippy::too_many_arguments)]
fn fit_right(
    c: &mut Canvas,
    t: &mut TextEngine,
    text: &str,
    right: f32,
    y: f32,
    size: f32,
    max_w: f32,
    color: [u8; 4],
) -> f32 {
    let (s, sz) = fit(t, text, size, max_w);
    let w = t.measure(&s, sz);
    t.draw(c, &s, right - w, y + (size - sz) * 0.65, sz, color);
    w
}

fn tile(c: &mut Canvas, t: &mut TextEngine, (x, y, w, h): Rect, label: &str) {
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, 1.0, line());
    t.draw(c, label, x + 20.0, y + 14.0, 24.0, DIM);
}

/// タイル内の 1 行: 左に項目名(薄い色)、右に値。値を先に描き、項目名は残りの幅に収める。
#[allow(clippy::too_many_arguments)]
fn kv_row(
    c: &mut Canvas,
    t: &mut TextEngine,
    x: f32,
    y: f32,
    w: f32,
    label: &str,
    value: &str,
    color: [u8; 4],
) {
    let vw = fit_right(c, t, value, x + w - 20.0, y, 34.0, (w - 40.0) * 0.65, color);
    fit_left(
        c,
        t,
        label,
        x + 20.0,
        y + 6.0,
        26.0,
        (w - 40.0 - vw - 12.0).max(20.0),
        DIM,
    );
}

/// 項目名・値・値の色の行を並べるタイル(最大 4 行)。
fn rows_tile(c: &mut Canvas, t: &mut TextEngine, rect: Rect, title: &str, rows: &[Row]) {
    tile(c, t, rect, title);
    let (x, y, w, _) = rect;
    if rows.is_empty() {
        t.draw(c, "--", x + 20.0, y + 70.0, 48.0, DIM);
    }
    for (i, (label, value, color)) in rows.iter().take(4).enumerate() {
        kv_row(c, t, x, y + 54.0 + i as f32 * 46.0, w, label, value, *color);
    }
}

fn row(label: &str, value: String, color: [u8; 4]) -> Row {
    (label.to_string(), value, color)
}

fn header(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, page: StatusPage) {
    let w = c.width() as f32;
    t.draw(c, "SERVER STATUS", PAD, 16.0, 36.0, FG);
    let tw = t.measure("SERVER STATUS", 36.0);
    let indicator = match page {
        StatusPage::Overview => "1/2 OVERVIEW",
        StatusPage::Details => "2/2 DETAILS",
    };
    t.draw(c, indicator, PAD + tw + 28.0, 24.0, 28.0, CPU_COLOR);
    t.draw_right(
        c,
        &format!("{}  {}", s.date, s.time),
        w - PAD,
        22.0,
        30.0,
        DIM,
    );
    c.hline(0.0, HEAD, w, line());
}

fn footer(c: &mut Canvas, t: &mut TextEngine, keys: &str, note: Option<&str>) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let fy = h - FOOT;
    c.hline(0.0, fy, w, line());
    t.draw(c, keys, PAD, fy + 16.0, 28.0, DIM);
    if let Some(n) = note {
        t.draw_right(c, n, w - PAD, fy + 20.0, 24.0, DIM);
    }
}

/// 5 枚のタイル(上段)の領域。
fn tile_rect(w: f32, i: usize) -> Rect {
    let tw = (w - PAD * 2.0 - GAP * 4.0) / 5.0;
    (PAD + (tw + GAP) * i as f32, HEAD + 24.0, tw, TILE_H)
}

/// 左右 2 つのグラフ(箱)の (x, 幅)。
fn halves(w: f32) -> [(f32, f32); 2] {
    let cw = (w - PAD * 2.0 - GAP) / 2.0;
    [(PAD, cw), (PAD + cw + GAP, cw)]
}

fn series<'a>(name: &'a str, data: &'a [f32], color: [u8; 4]) -> Series<'a> {
    Series { name, data, color }
}

/// 状況画面。`page` で OVERVIEW / DETAILS を切り替える。
pub fn status(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, page: StatusPage) {
    c.clear(rgba(8, 12, 20, 255));
    header(c, t, s, page);
    match page {
        StatusPage::Overview => overview(c, t, s),
        StatusPage::Details => details(c, t, s),
    }
}

/// CPU / GPU タイル: 使用率 + 温度 + 現在の W + 今日の kWh。
#[allow(clippy::too_many_arguments)]
fn power_tile(
    c: &mut Canvas,
    t: &mut TextEngine,
    rect: Rect,
    name: &str,
    usage: Option<f32>,
    temp: Option<f32>,
    power: String,
    energy: Option<f64>,
    color: [u8; 4],
) {
    tile(c, t, rect, name);
    let (x, y, w, h) = rect;
    c.fill_rect(x, y, 6.0, h, rgba(color[0], color[1], color[2], 255));
    t.draw(c, &pct(usage), x + 20.0, y + 46.0, 68.0, FG);
    fit_right(
        c,
        t,
        &deg(temp),
        x + w - 20.0,
        y + 66.0,
        40.0,
        w * 0.4,
        temp.map_or(DIM, temp_color),
    );
    fit_left(c, t, &power, x + 20.0, y + 132.0, 44.0, w - 40.0, FG);
    let e = format!("今日 {}", kwh(energy));
    fit_left(c, t, &e, x + 20.0, y + 192.0, 30.0, w - 40.0, DIM);
}

/// GPU が 2 台以上のときの GPU タイル: GPU ごとに使用率と温度を 1 行ずつ、最後に監視 GPU の W と今日の kWh。
fn multi_gpu_tile(c: &mut Canvas, t: &mut TextEngine, rect: Rect, s: &Snapshot) {
    tile(c, t, rect, "GPU");
    let (x, y, w, h) = rect;
    c.fill_rect(
        x,
        y,
        6.0,
        h,
        rgba(GPU_COLOR[0], GPU_COLOR[1], GPU_COLOR[2], 255),
    );
    // 行の領域は y+50..y+176。台数が多ければ行の間隔と文字を縮める
    let step = (126.0 / s.gpus.len() as f32).min(46.0);
    let k = step / 46.0;
    for (i, g) in s.gpus.iter().enumerate() {
        let ry = y + 50.0 + i as f32 * step;
        let right = x + w - 20.0;
        let tw = fit_right(
            c,
            t,
            &deg(g.temp),
            right,
            ry + 4.0 * k,
            30.0 * k,
            w * 0.3,
            g.temp.map_or(DIM, temp_color),
        );
        let uw = fit_right(
            c,
            t,
            &pct(g.usage),
            right - tw - 20.0,
            ry,
            36.0 * k,
            w * 0.3,
            FG,
        );
        fit_left(
            c,
            t,
            g.display_name(),
            x + 20.0,
            ry + 6.0 * k,
            26.0 * k,
            (w - 40.0 - tw - uw - 40.0).max(20.0),
            gpu_color(i),
        );
    }
    let e = format!(
        "{}  今日 {}",
        watt(s.gpu_power_w),
        kwh(s.gpu_energy_today_wh)
    );
    fit_left(c, t, &e, x + 20.0, y + 192.0, 30.0, w - 40.0, DIM);
}

/// USAGE % のグラフの系列。GPU が 2 台以上なら GPU ごとに分ける。
fn usage_series(s: &Snapshot) -> Vec<Series<'_>> {
    let mut v = vec![series("CPU", &s.cpu_history, CPU_COLOR)];
    if s.gpus.len() >= 2 {
        for (i, g) in s.gpus.iter().enumerate() {
            v.push(series(g.display_name(), &g.history, gpu_color(i)));
        }
    } else {
        v.push(series("GPU", &s.gpu_history, GPU_COLOR));
    }
    v
}

fn overview(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot) {
    let (w, h) = (c.width() as f32, c.height() as f32);

    power_tile(
        c,
        t,
        tile_rect(w, 0),
        "CPU",
        s.cpu,
        s.cpu_temp,
        s.cpu_power_w
            .map_or("--".to_string(), |v| format!("{v:.0} W*")),
        s.cpu_energy_today_wh,
        CPU_COLOR,
    );
    if s.gpus.len() >= 2 {
        multi_gpu_tile(c, t, tile_rect(w, 1), s);
    } else {
        power_tile(
            c,
            t,
            tile_rect(w, 1),
            "GPU",
            s.gpu,
            s.gpu_temp,
            watt(s.gpu_power_w),
            s.gpu_energy_today_wh,
            GPU_COLOR,
        );
    }

    // MEMORY: 使用量 / 全体、使用率、スワップ
    let rect = tile_rect(w, 2);
    let (mx, ty, mw, _) = rect;
    tile(c, t, rect, "MEMORY");
    let used = s.mem_used.map_or("--".to_string(), |v| format!("{v:.1}"));
    let uw = t.measure(&used, 68.0);
    t.draw(c, &used, mx + 20.0, ty + 46.0, 68.0, FG);
    let total = s
        .mem_total
        .map_or("/ -- GB".to_string(), |v| format!("/ {v:.0} GB"));
    fit_left(
        c,
        t,
        &total,
        mx + 20.0 + uw + 12.0,
        ty + 66.0,
        36.0,
        (mw - 40.0 - uw - 12.0).max(40.0),
        DIM,
    );
    let ratio = match (s.mem_used, s.mem_total) {
        (Some(u), Some(tt)) if tt > 0.0 => Some(u / tt * 100.0),
        _ => None,
    };
    kv_row(c, t, mx, ty + 132.0, mw, "使用率", &pct(ratio), FG);
    kv_row(c, t, mx, ty + 184.0, mw, "スワップ", &swap_text(s), FG);

    // SSD: ドライブごとの温度 / FAN
    let ssd: Vec<Row> = s
        .ssd_temps
        .iter()
        .map(|(n, v)| (n.clone(), format!("{v:.0}°C"), temp_color(*v)))
        .collect();
    rows_tile(c, t, tile_rect(w, 3), "SSD TEMP", &ssd);
    let fans: Vec<Row> = s
        .fans
        .iter()
        .map(|(n, v)| (n.clone(), v.clone(), FG))
        .collect();
    rows_tile(c, t, tile_rect(w, 4), "FAN", &fans);

    // 下段: 使用率と電力の時系列
    let sum_h = 64.0;
    let sum_y = h - FOOT - sum_h;
    let cy = tile_rect(w, 0).1 + TILE_H + GAP;
    let ch = sum_y - 16.0 - cy;
    let [l, r] = halves(w);
    chart::line_chart_ex(
        c,
        t,
        l.0,
        cy,
        l.1,
        ch,
        "USAGE %",
        &usage_series(s),
        Scale::Fixed(100.0),
        "%",
    );
    chart::line_chart_ex(
        c,
        t,
        r.0,
        cy,
        r.1,
        ch,
        "POWER W",
        &[
            series("CPU*", &s.cpu_power_history, CPU_COLOR),
            series("GPU", &s.gpu_power_history, GPU_COLOR),
        ],
        Scale::Auto { min: 50.0 },
        " W",
    );

    summary(c, t, s, sum_y, sum_h);
    footer(
        c,
        t,
        "5: 詳細    4/6: 画面の切替    0: メニュー",
        Some("* CPU の W は推定値(RAPL)。CPU+GPU のみ"),
    );
}

/// 電力のまとめ行(現在・今日・電気代・ピーク)。幅に収まらなければ全体を縮める。
fn summary(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot, y: f32, h: f32) {
    let w = c.width() as f32;
    c.hline(0.0, y, w, line());
    let now = match (s.cpu_power_w, s.gpu_power_w) {
        (Some(a), Some(b)) => watt(Some(a + b)),
        _ => "--".to_string(),
    };
    let today_wh = match (s.cpu_energy_today_wh, s.gpu_energy_today_wh) {
        (Some(a), Some(b)) => Some(a + b),
        _ => None,
    };
    let mut segs: Vec<(String, String)> =
        vec![("現在 CPU+GPU".into(), now), ("今日".into(), kwh(today_wh))];
    if let Some(price) = s.price_per_kwh {
        let cost = today_wh.map_or("--".to_string(), |wh| {
            format!("約{:.0}円", wh / 1000.0 * price as f64)
        });
        segs.push(("電気代".into(), cost));
    }
    if let Some((label, val, time)) = s.peaks_today.first() {
        segs.push((format!("最大 {label}"), format!("{val} ({time})")));
    }
    let (lsz, vsz, inner, outer) = (26.0, 38.0, 12.0, 56.0);
    let mut total = -outer;
    for (l, v) in &segs {
        total += t.measure(l, lsz) + inner + t.measure(v, vsz) + outer;
    }
    let k = ((w - PAD * 2.0) / total).min(1.0);
    let mut x = PAD;
    for (l, v) in &segs {
        let lw = t.measure(l, lsz * k);
        t.draw(c, l, x, y + (h - lsz * k * 1.3) / 2.0, lsz * k, DIM);
        x += lw + inner * k;
        let vw = t.measure(v, vsz * k);
        t.draw(c, v, x, y + (h - vsz * k * 1.3) / 2.0, vsz * k, FG);
        x += vw + outer * k;
    }
}

fn details(c: &mut Canvas, t: &mut TextEngine, s: &Snapshot) {
    let (w, h) = (c.width() as f32, c.height() as f32);

    // MEM PRESSURE
    let load = s.load_avg.map_or("--".to_string(), |l| {
        format!("{:.1} {:.1} {:.1}", l[0], l[1], l[2])
    });
    let psi_col = |v: Option<f32>| v.map_or(DIM, |v| level_color(v, 30.0, 60.0));
    rows_tile(
        c,
        t,
        tile_rect(w, 0),
        "MEM PRESSURE",
        &[
            row("PSI CPU", pct(s.psi_cpu), psi_col(s.psi_cpu)),
            row("PSI MEM", pct(s.psi_mem), psi_col(s.psi_mem)),
            row("スワップ", swap_text(s), FG),
            row("LOAD", load, FG),
        ],
    );

    // GPU DETAIL
    let vram = format!("{} / {} MiB", num(s.vram_used_mib), num(s.vram_total_mib));
    let vram_pct = match (s.vram_used_mib, s.vram_total_mib) {
        (Some(u), Some(tt)) if tt > 0 => Some(u as f32 / tt as f32 * 100.0),
        _ => None,
    };
    let clock = format!(
        "{} / {} MHz",
        num(s.gpu_clock_mhz),
        num(s.gpu_clock_max_mhz)
    );
    rows_tile(
        c,
        t,
        tile_rect(w, 1),
        "GPU DETAIL",
        &[
            row("VRAM", vram, FG),
            row("VRAM %", pct(vram_pct), FG),
            row("CLOCK", clock, FG),
            row(
                "P-STATE",
                s.gpu_pstate.clone().unwrap_or_else(|| "--".into()),
                FG,
            ),
        ],
    );

    // NETWORK
    rows_tile(
        c,
        t,
        tile_rect(w, 2),
        "NETWORK",
        &[
            row("IF", s.net_iface.clone().unwrap_or_else(|| "--".into()), FG),
            row("受信 RX", mbps(s.net_rx_mbps), CPU_COLOR),
            row("送信 TX", mbps(s.net_tx_mbps), GPU_COLOR),
        ],
    );

    // DISK: ルートの使用率と容量、動いているデバイス上位 2 つ
    let gb = s
        .disk_root_used_total_gb
        .map_or("--".to_string(), |(u, tt)| format!("{u:.0} / {tt:.0} GB"));
    let mut disks: Vec<&(String, f32, f32)> = s.disk_io.iter().collect();
    disks.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)));
    let used_col = s
        .disk_root_used_pct
        .map_or(DIM, |v| level_color(v, 80.0, 90.0));
    let mut disk_rows = vec![
        row("/ 使用率", pct(s.disk_root_used_pct), used_col),
        row("/ 容量", gb, FG),
    ];
    for (name, r, wr) in disks.into_iter().take(2) {
        disk_rows.push(row(name, format!("R {r:.1}  W {wr:.1}"), FG));
    }
    rows_tile(c, t, tile_rect(w, 3), "DISK (MB/s)", &disk_rows);

    // HEALTH
    let feed_col = match s.feed_status.as_deref() {
        Some("online") => FG,
        Some(_) => WARN,
        None => DIM,
    };
    let failed_col = s
        .failed_units
        .map_or(DIM, |n| if n > 0 { CRIT } else { FG });
    rows_tile(
        c,
        t,
        tile_rect(w, 4),
        "HEALTH",
        &[
            row("failed unit", num(s.failed_units), failed_col),
            row(
                "写真フィード",
                s.feed_status.clone().unwrap_or_else(|| "--".into()),
                feed_col,
            ),
        ],
    );

    // 下段: 時系列 2 つ + 箱 2 つ
    let box_h = 270.0;
    let by = h - FOOT - 20.0 - box_h;
    let cy = tile_rect(w, 0).1 + TILE_H + GAP;
    let ch = by - GAP - cy;
    let [l, r] = halves(w);
    chart::line_chart_ex(
        c,
        t,
        l.0,
        cy,
        l.1,
        ch,
        "NETWORK Mbps",
        &[
            series("RX", &s.net_rx_history, CPU_COLOR),
            series("TX", &s.net_tx_history, GPU_COLOR),
        ],
        Scale::Auto { min: 8.0 },
        "",
    );
    chart::line_chart_ex(
        c,
        t,
        r.0,
        cy,
        r.1,
        ch,
        "PRESSURE %",
        &[
            series("CPU", &s.psi_cpu_history, CPU_COLOR),
            series("MEM", &s.psi_mem_history, GPU_COLOR),
        ],
        Scale::Fixed(100.0),
        "%",
    );

    // GPU PROCESSES: 名前 / PID / VRAM
    let (bx, bw) = l;
    tile(c, t, (bx, by, bw, box_h), "GPU PROCESSES");
    if s.gpu_processes.is_empty() {
        t.draw(c, "--", bx + 20.0, by + 60.0, 36.0, DIM);
    }
    for (i, (name, pid, vram)) in s.gpu_processes.iter().take(5).enumerate() {
        let ry = by + 52.0 + i as f32 * 42.0;
        let right = bx + bw - 20.0;
        fit_right(c, t, &format!("{vram} MiB"), right, ry, 30.0, 170.0, FG);
        fit_right(
            c,
            t,
            &format!("PID {pid}"),
            right - 190.0,
            ry + 4.0,
            26.0,
            150.0,
            DIM,
        );
        let name_w = bw - 40.0 - 190.0 - 170.0;
        fit_left(c, t, name, bx + 20.0, ry, 30.0, name_w, FG);
    }

    // TODAY PEAKS: 項目 / 値 / 時刻
    let (bx, bw) = r;
    tile(c, t, (bx, by, bw, box_h), "TODAY PEAKS");
    if s.peaks_today.is_empty() {
        t.draw(c, "--", bx + 20.0, by + 60.0, 36.0, DIM);
    }
    for (i, (label, val, time)) in s.peaks_today.iter().take(5).enumerate() {
        let ry = by + 52.0 + i as f32 * 42.0;
        let right = bx + bw - 20.0;
        fit_right(c, t, time, right, ry + 4.0, 26.0, 110.0, DIM);
        let vw = fit_right(c, t, val, right - 130.0, ry, 30.0, 220.0, FG);
        let label_w = (bw - 40.0 - 130.0 - vw - 16.0).max(40.0);
        fit_left(c, t, label, bx + 20.0, ry, 30.0, label_w, FG);
    }

    footer(c, t, "5: 概要    4/6: 画面の切替    0: メニュー", None);
}
