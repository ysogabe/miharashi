//! 室内環境と天気の画面(docs/DESIGN-screens.md §2.1)。左に室内センサーのタイル(値と過去 24 時間の折れ線)、
//! 右に気象庁の府県予報(今日・明日)と週間予報、下の帯に自宅の市町村の警報・注意報。
//! 値の文字列や取得の状態は画面ロジック(miharashi-app)が作り、ここは並べて描くだけ。
use crate::canvas::{Canvas, rgba};
use crate::photo_layout::scale;
use crate::status::{fit, fit_left, fit_right};
use crate::text::TextEngine;

const FG: [u8; 4] = [225, 235, 245, 255];
const DIM: [u8; 4] = [140, 160, 180, 255];
const WARN: [u8; 4] = [255, 200, 60, 255];
const CRIT: [u8; 4] = [255, 90, 80, 255];
const ACCENT: [u8; 4] = [64, 200, 255, 255];
const HIGH: [u8; 4] = [255, 150, 90, 255];
const LOW: [u8; 4] = [110, 175, 255, 255];
const SAT: [u8; 4] = [120, 170, 255, 255];
const SUN: [u8; 4] = [255, 120, 110, 255];

/// 室内センサーのタイル 1 枚。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SensorTile {
    pub label: String,
    /// 値("23.5")。取れなければ "--"
    pub value: String,
    pub unit: String,
    /// 値の右に出す補足("35 %"、"↘ 下降 -1.2"、"やや高い")
    pub note: String,
    /// 0 = ふつう、1 = 注意(黄)、2 = 警告(赤)
    pub level: u8,
    /// 過去の値(古い順・等間隔)。NaN は欠け
    pub history: Vec<f32>,
}

/// 府県予報の 1 日(今日・明日)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForecastDay {
    /// "今日 15日(水)"
    pub label: String,
    /// 天気("くもり時々晴れ")。アイコンもこの文字から決める
    pub weather: String,
    pub max: Option<i32>,
    pub min: Option<i32>,
    /// 6 時間ごとの降水確率 [%](0〜6 時、6〜12 時、12〜18 時、18〜24 時)。過ぎた時間帯は None
    pub pops: [Option<u8>; 4],
}

/// 週間予報の 1 日。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WeekDay {
    /// 日("16")
    pub day: String,
    /// 曜日(0 = 月 … 6 = 日)
    pub weekday: usize,
    /// 天気("晴時々曇")
    pub weather: String,
    pub max: Option<i32>,
    pub min: Option<i32>,
    pub pop: Option<u8>,
}

/// 天気予報。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Weather {
    /// 予報の地域("千葉県北西部")
    pub area: String,
    pub days: Vec<ForecastDay>,
    pub week: Vec<WeekDay>,
}

/// 警報・注意報 1 つ。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Warning {
    pub name: String,
    /// 警戒レベル(注意報 2、警報 3、危険警報 4、特別警報 5)
    pub level: u8,
}

/// 取得の状態("HA 16:20 取得"、"最終 16:20 取得 · HA に接続できません")。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Feed {
    pub text: String,
    /// 最新でない(取得に失敗している)なら true。黄色で出す
    pub stale: bool,
}

/// 画面に出す内容。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RoomView {
    /// "2026/10/15 (水)  16:20"
    pub clock: String,
    pub tiles: Vec<SensorTile>,
    pub env: Feed,
    /// まだ予報を一度も取れていなければ None
    pub weather: Option<Weather>,
    pub weather_feed: Feed,
    /// 自宅の市町村("流山市")。分からなければ空
    pub warn_area: String,
    /// 発表中の警報・注意報(レベルの高い順)。まだ取れていなければ None
    pub warnings: Option<Vec<Warning>>,
    pub warn_feed: Feed,
}

/// タイルの最大の枚数(2 列 × 3 段)。
pub const MAX_TILES: usize = 6;

fn rgb(c: [u8; 4], a: u8) -> tiny_skia::Color {
    rgba(c[0], c[1], c[2], a)
}

fn panel(c: &mut Canvas, (x, y, w, h): (f32, f32, f32, f32), k: f32) {
    c.fill_rect(x, y, w, h, rgba(16, 24, 36, 255));
    c.stroke_rect(x, y, w, h, k.max(1.0), rgba(60, 80, 105, 255));
}

fn level_color(level: u8) -> [u8; 4] {
    match level {
        0 => FG,
        1 => WARN,
        _ => CRIT,
    }
}

fn feed_color(f: &Feed) -> [u8; 4] {
    if f.stale { WARN } else { DIM }
}

fn temp(v: Option<i32>) -> String {
    v.map_or("--".into(), |v| format!("{v}℃"))
}

/// 室内環境と天気の画面。
pub fn room_screen(c: &mut Canvas, t: &mut TextEngine, view: &RoomView) {
    let k = scale(c.height());
    let (w, h) = (c.width() as f32, c.height() as f32);
    let pad = 32.0 * k;
    let head = 72.0 * k;
    let band = 110.0 * k;
    c.clear(rgba(8, 12, 20, 255));
    // 見出し
    t.draw(c, "ROOM & WEATHER", pad, 16.0 * k, 36.0 * k, FG);
    t.draw_right(c, &view.clock, w - pad, 22.0 * k, 30.0 * k, DIM);
    c.hline(0.0, head, w, rgba(60, 80, 105, 255));
    // 本体(左: 室内、右: 天気)
    let (top, bottom) = (head + 24.0 * k, h - band - 24.0 * k);
    let lw = ((w - pad * 2.0 - pad) * 0.47).round();
    let rx = pad + lw + pad;
    room_side(c, t, view, (pad, top, lw, bottom - top), k);
    weather_side(c, t, view, (rx, top, w - pad - rx, bottom - top), k);
    // 下の帯(警報・注意報)
    warnings_band(c, t, view, h - band, k);
}

fn section_head(
    c: &mut Canvas,
    t: &mut TextEngine,
    (x, y, w): (f32, f32, f32),
    title: &str,
    feed: &Feed,
    k: f32,
) {
    let tw = fit_left(c, t, title, x, y, 30.0 * k, w * 0.55, FG);
    fit_right(
        c,
        t,
        &feed.text,
        x + w,
        y + 6.0 * k,
        22.0 * k,
        (w - tw - 24.0 * k).max(40.0 * k),
        feed_color(feed),
    );
}

fn room_side(
    c: &mut Canvas,
    t: &mut TextEngine,
    view: &RoomView,
    (x, y, w, h): (f32, f32, f32, f32),
    k: f32,
) {
    section_head(c, t, (x, y, w), "室内環境", &view.env, k);
    let top = y + 56.0 * k;
    if view.tiles.is_empty() {
        t.draw(
            c,
            "室内センサーが設定されていません([room] tiles)",
            x,
            top + 20.0 * k,
            28.0 * k,
            DIM,
        );
        return;
    }
    let gap = 20.0 * k;
    let tw = (w - gap) / 2.0;
    let th = (y + h - top - gap * 2.0) / 3.0;
    for (i, tile) in view.tiles.iter().take(MAX_TILES).enumerate() {
        let (col, row) = ((i % 2) as f32, (i / 2) as f32);
        sensor_tile(
            c,
            t,
            tile,
            (x + col * (tw + gap), top + row * (th + gap), tw, th),
            k,
        );
    }
}

fn sensor_tile(
    c: &mut Canvas,
    t: &mut TextEngine,
    tile: &SensorTile,
    rect: (f32, f32, f32, f32),
    k: f32,
) {
    let (x, y, w, h) = rect;
    panel(c, rect, k);
    let color = level_color(tile.level);
    if tile.level > 0 {
        c.fill_rect(x, y, 6.0 * k, h, rgb(color, 255));
    }
    let inner = w - 40.0 * k;
    // Noto Sans CJK には下付きの数字が無い(CO₂ が豆腐になる)
    let label: String = tile
        .label
        .chars()
        .map(|ch| match ch {
            '₀'..='₉' => char::from(b'0' + (ch as u32 - '₀' as u32) as u8),
            _ => ch,
        })
        .collect();
    fit_left(
        c,
        t,
        &label,
        x + 20.0 * k,
        y + 12.0 * k,
        24.0 * k,
        inner,
        DIM,
    );
    // 値と単位。補足は右寄せで、残りの幅に収める
    let vw = fit_left(
        c,
        t,
        &tile.value,
        x + 20.0 * k,
        y + 40.0 * k,
        60.0 * k,
        inner * 0.6,
        color,
    );
    let uw = fit_left(
        c,
        t,
        &tile.unit,
        x + 20.0 * k + vw + 8.0 * k,
        y + 70.0 * k,
        26.0 * k,
        inner * 0.25,
        DIM,
    );
    let used = vw + 8.0 * k + uw + 16.0 * k;
    if !tile.note.is_empty() && inner - used > 30.0 * k {
        let nc = if tile.level > 0 { color } else { DIM };
        fit_right(
            c,
            t,
            &tile.note,
            x + w - 20.0 * k,
            y + 66.0 * k,
            28.0 * k,
            inner - used,
            nc,
        );
    }
    let gy = y + 124.0 * k;
    let gh = y + h - 14.0 * k - gy;
    if gh > 24.0 * k {
        sparkline(c, t, &tile.history, (x + 20.0 * k, gy, inner, gh), k);
    }
}

/// 過去の値の折れ線。右端に最大と最小の値を小さく出す。
fn sparkline(
    c: &mut Canvas,
    t: &mut TextEngine,
    data: &[f32],
    (x, y, w, h): (f32, f32, f32, f32),
    k: f32,
) {
    let finite = || data.iter().copied().filter(|v| v.is_finite());
    let (Some(lo), Some(hi)) = (finite().reduce(f32::min), finite().reduce(f32::max)) else {
        t.draw(c, "履歴なし", x, y + h / 2.0 - 12.0 * k, 20.0 * k, DIM);
        return;
    };
    let decimals = if hi - lo < 10.0 && hi.abs() < 100.0 {
        1
    } else {
        0
    };
    let label = |v: f32| format!("{v:.decimals$}");
    let lab_w = t
        .measure(&label(hi), 18.0 * k)
        .max(t.measure(&label(lo), 18.0 * k))
        + 10.0 * k;
    let pw = (w - lab_w).max(10.0);
    let span = (hi - lo).max(f32::EPSILON);
    // 平らな値(変化なし)は真ん中に引く
    let pos = |i: usize, v: f32| {
        let fx = x + pw * i as f32 / (data.len().max(2) - 1) as f32;
        let fy = if (hi - lo).abs() < f32::EPSILON {
            y + h / 2.0
        } else {
            y + h * (1.0 - (v - lo) / span)
        };
        (fx, fy)
    };
    c.hline(x, y + h, pw, rgba(50, 66, 86, 255));
    // 欠けで区切った区間ごとに描く
    let mut seg: Vec<(f32, f32)> = Vec::new();
    let flush = |c: &mut Canvas, seg: &mut Vec<(f32, f32)>| {
        if seg.len() >= 2 {
            c.fill_under(seg, y + h, rgb(ACCENT, 36));
            c.polyline(seg, 2.0 * k, rgb(ACCENT, 255));
        } else if let Some(&(px, py)) = seg.first() {
            c.fill_circle(px, py, 2.0 * k, rgb(ACCENT, 255));
        }
        seg.clear();
    };
    for (i, v) in data.iter().enumerate() {
        if v.is_finite() {
            seg.push(pos(i, *v));
        } else {
            flush(c, &mut seg);
        }
    }
    flush(c, &mut seg);
    t.draw_right(c, &label(hi), x + w, y - 6.0 * k, 18.0 * k, DIM);
    t.draw_right(c, &label(lo), x + w, y + h - 20.0 * k, 18.0 * k, DIM);
}

fn weather_side(
    c: &mut Canvas,
    t: &mut TextEngine,
    view: &RoomView,
    (x, y, w, h): (f32, f32, f32, f32),
    k: f32,
) {
    let title = match &view.weather {
        Some(wx) if !wx.area.is_empty() => format!("天気  {}", wx.area),
        _ => "天気".to_string(),
    };
    section_head(c, t, (x, y, w), &title, &view.weather_feed, k);
    let top = y + 56.0 * k;
    let Some(wx) = &view.weather else {
        t.draw(c, "予報はまだありません", x, top + 20.0 * k, 32.0 * k, DIM);
        return;
    };
    let gap = 24.0 * k;
    let dh = 320.0 * k;
    let dw = (w - gap) / 2.0;
    for (i, d) in wx.days.iter().take(2).enumerate() {
        day_panel(c, t, d, (x + i as f32 * (dw + gap), top, dw, dh), k);
    }
    let wy = top + dh + gap;
    if !wx.week.is_empty() && y + h - wy > 200.0 * k {
        week_panel(c, t, &wx.week, (x, wy, w, y + h - wy), k);
    }
}

fn day_panel(
    c: &mut Canvas,
    t: &mut TextEngine,
    d: &ForecastDay,
    rect: (f32, f32, f32, f32),
    k: f32,
) {
    let (x, y, w, _) = rect;
    panel(c, rect, k);
    fit_left(
        c,
        t,
        &d.label,
        x + 20.0 * k,
        y + 12.0 * k,
        26.0 * k,
        w - 40.0 * k,
        DIM,
    );
    let icon = 120.0 * k;
    weather_icon(
        c,
        &d.weather,
        (x + 20.0 * k + icon / 2.0, y + 60.0 * k + icon / 2.0),
        icon,
    );
    let tx = x + 40.0 * k + icon;
    let tw = (x + w - 20.0 * k - tx).max(20.0 * k);
    // 気象庁の文は長いことがある(「くもり昼前から夕方晴れ」)。縮めて、それでも入らなければ切る
    fit_left(c, t, &d.weather, tx, y + 64.0 * k, 34.0 * k, tw, FG);
    let hw = fit_left(
        c,
        t,
        &format!("最高 {}", temp(d.max)),
        tx,
        y + 120.0 * k,
        32.0 * k,
        tw / 2.0,
        HIGH,
    );
    fit_left(
        c,
        t,
        &format!("最低 {}", temp(d.min)),
        tx + hw + 24.0 * k,
        y + 120.0 * k,
        32.0 * k,
        (tw - hw - 24.0 * k).max(20.0 * k),
        LOW,
    );
    // 6 時間ごとの降水確率
    let py = y + 196.0 * k;
    t.draw(c, "降水確率", x + 20.0 * k, py, 22.0 * k, DIM);
    let cells = ["0-6時", "6-12時", "12-18時", "18-24時"];
    let cw = (w - 40.0 * k) / 4.0;
    for (i, (name, pop)) in cells.iter().zip(d.pops).enumerate() {
        let cx = x + 20.0 * k + cw * i as f32 + cw / 2.0;
        let nw = t.measure(name, 20.0 * k);
        t.draw(c, name, cx - nw / 2.0, py + 34.0 * k, 20.0 * k, DIM);
        let v = pop.map_or("--".to_string(), |p| format!("{p}%"));
        let col = match pop {
            Some(p) if p >= 50 => LOW,
            Some(_) => FG,
            None => DIM,
        };
        let vw = t.measure(&v, 34.0 * k);
        t.draw(c, &v, cx - vw / 2.0, py + 62.0 * k, 34.0 * k, col);
    }
}

fn week_panel(
    c: &mut Canvas,
    t: &mut TextEngine,
    week: &[WeekDay],
    rect: (f32, f32, f32, f32),
    k: f32,
) {
    let (x, y, w, h) = rect;
    panel(c, rect, k);
    t.draw(c, "週間予報", x + 20.0 * k, y + 12.0 * k, 24.0 * k, DIM);
    let n = week.len().min(7);
    let cw = (w - 40.0 * k) / n as f32;
    // 縦に余る分は上下に分ける
    let body = 300.0 * k;
    let y0 = y + 48.0 * k + ((h - 48.0 * k - body) / 2.0).max(0.0);
    let center = |t: &mut TextEngine, c: &mut Canvas, s: &str, cx: f32, yy: f32, size: f32, col| {
        let (s, sz) = fit(t, s, size, cw - 12.0 * k);
        let sw = t.measure(&s, sz);
        t.draw(c, &s, cx - sw / 2.0, yy + (size - sz) * 0.65, sz, col);
    };
    for (i, d) in week.iter().take(n).enumerate() {
        let cx = x + 20.0 * k + cw * i as f32 + cw / 2.0;
        if i > 0 {
            c.fill_rect(cx - cw / 2.0, y0, k.max(1.0), body, rgba(40, 54, 72, 255));
        }
        let wd = ["月", "火", "水", "木", "金", "土", "日"][d.weekday % 7];
        let col = match d.weekday % 7 {
            5 => SAT,
            6 => SUN,
            _ => FG,
        };
        center(t, c, &format!("{}({wd})", d.day), cx, y0, 28.0 * k, col);
        let icon = (78.0 * k).min(cw * 0.8);
        weather_icon(c, &d.weather, (cx, y0 + 50.0 * k + icon / 2.0), icon);
        center(t, c, &d.weather, cx, y0 + 140.0 * k, 22.0 * k, DIM);
        // 最高 / 最低
        let hi = d.max.map_or("--".into(), |v| v.to_string());
        let lo = d.min.map_or("--".into(), |v| v.to_string());
        let sz = 30.0 * k;
        let (hw, sw, lw) = (t.measure(&hi, sz), t.measure(" / ", sz), t.measure(&lo, sz));
        let mut tx = cx - (hw + sw + lw) / 2.0;
        let ty = y0 + 186.0 * k;
        t.draw(c, &hi, tx, ty, sz, HIGH);
        tx += hw;
        t.draw(c, " / ", tx, ty, sz, DIM);
        tx += sw;
        t.draw(c, &lo, tx, ty, sz, LOW);
        let pop = d.pop.map_or("--".to_string(), |p| format!("{p}%"));
        let pc = match d.pop {
            Some(p) if p >= 50 => LOW,
            Some(_) => FG,
            None => DIM,
        };
        center(t, c, &pop, cx, y0 + 240.0 * k, 28.0 * k, pc);
    }
}

/// 天気の種類(アイコンの絵柄)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sky {
    Sun,
    Cloud,
    Rain,
    Snow,
}

/// 天気の文から (主な天気, 次の天気, 「後」か)。"晴時々曇" → (晴, 曇, false)、"くもり後雨" → (曇, 雨, true)。
pub fn sky_of(text: &str) -> (Option<Sky>, Option<Sky>, bool) {
    let mut found: Vec<(usize, Sky)> = Vec::new();
    for (i, ch) in text.char_indices() {
        let s = match ch {
            '晴' => Sky::Sun,
            '曇' => Sky::Cloud,
            '雨' => Sky::Rain,
            '雪' => Sky::Snow,
            'く' if text[i..].starts_with("くもり") => Sky::Cloud,
            _ => continue,
        };
        found.push((i, s));
    }
    let Some(&(i0, first)) = found.first() else {
        return (None, None, false);
    };
    let Some(&(i1, second)) = found.iter().find(|(_, s)| *s != first) else {
        return (Some(first), None, false);
    };
    let between = &text[i0..i1];
    let after = between.contains('後') || between.contains("のち");
    (Some(first), Some(second), after)
}

fn draw_sky(c: &mut Canvas, sky: Sky, (x, y): (f32, f32), s: f32) {
    let cloud = |c: &mut Canvas, dy: f32, col| {
        let y = y + dy;
        c.fill_circle(x - 0.18 * s, y + 0.06 * s, 0.17 * s, col);
        c.fill_circle(x + 0.02 * s, y - 0.06 * s, 0.24 * s, col);
        c.fill_circle(x + 0.22 * s, y + 0.07 * s, 0.16 * s, col);
        c.fill_rect(x - 0.18 * s, y + 0.06 * s, 0.40 * s, 0.17 * s, col);
    };
    match sky {
        Sky::Sun => {
            let col = rgba(255, 185, 50, 255);
            c.fill_circle(x, y, 0.24 * s, col);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let (sn, cs) = a.sin_cos();
                c.polyline(
                    &[
                        (x + cs * 0.33 * s, y + sn * 0.33 * s),
                        (x + cs * 0.45 * s, y + sn * 0.45 * s),
                    ],
                    0.06 * s,
                    col,
                );
            }
        }
        Sky::Cloud => cloud(c, 0.0, rgba(185, 195, 210, 255)),
        Sky::Rain => {
            cloud(c, -0.1 * s, rgba(150, 160, 178, 255));
            for i in 0..3 {
                let dx = (i as f32 - 1.0) * 0.18 * s;
                c.polyline(
                    &[
                        (x + dx + 0.03 * s, y + 0.2 * s),
                        (x + dx - 0.04 * s, y + 0.38 * s),
                    ],
                    0.06 * s,
                    rgba(90, 160, 255, 255),
                );
            }
        }
        Sky::Snow => {
            cloud(c, -0.1 * s, rgba(170, 180, 198, 255));
            for i in 0..3 {
                let dx = (i as f32 - 1.0) * 0.18 * s;
                let dy = if i == 1 { 0.36 } else { 0.26 };
                c.fill_circle(x + dx, y + dy * s, 0.05 * s, rgba(240, 245, 255, 255));
            }
        }
    }
}

/// 天気の文から絵を描く。次の天気があれば、主な天気を左上に、次の天気を右下に小さく描く。
fn weather_icon(c: &mut Canvas, text: &str, (x, y): (f32, f32), size: f32) {
    let (first, second, after) = sky_of(text);
    let Some(first) = first else {
        return;
    };
    let Some(second) = second else {
        return draw_sky(c, first, (x, y), size);
    };
    draw_sky(c, first, (x - size * 0.17, y - size * 0.12), size * 0.7);
    draw_sky(c, second, (x + size * 0.26, y + size * 0.24), size * 0.48);
    // 「後」は矢印、「時々・一時」は斜めの線
    let col = rgba(200, 210, 225, 255);
    let (ax, ay) = (x + size * 0.02, y + size * 0.3);
    if after {
        let r = size * 0.08;
        c.polyline(&[(ax - r, ay), (ax + r, ay)], size * 0.03, col);
        c.polyline(
            &[
                (ax + r * 0.4, ay - r * 0.6),
                (ax + r, ay),
                (ax + r * 0.4, ay + r * 0.6),
            ],
            size * 0.03,
            col,
        );
    } else {
        c.polyline(
            &[
                (ax - size * 0.06, ay + size * 0.1),
                (ax + size * 0.06, ay - size * 0.1),
            ],
            size * 0.03,
            col,
        );
    }
}

/// 警戒レベルの色と文字の色(気象庁: 注意報は黄、警報は赤、危険警報は紫、特別警報は黒)。
fn warning_colors(level: u8) -> ([u8; 4], [u8; 4]) {
    match level {
        0..=2 => ([242, 231, 0, 255], [20, 20, 20, 255]),
        3 => ([255, 40, 0, 255], [255, 255, 255, 255]),
        4 => ([170, 0, 170, 255], [255, 255, 255, 255]),
        _ => ([12, 0, 12, 255], [255, 255, 255, 255]),
    }
}

fn warnings_band(c: &mut Canvas, t: &mut TextEngine, view: &RoomView, y: f32, k: f32) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let pad = 32.0 * k;
    c.fill_rect(0.0, y, w, h - y, rgba(12, 18, 28, 255));
    c.hline(0.0, y, w, rgba(60, 80, 105, 255));
    let area = if view.warn_area.is_empty() {
        "自宅の市町村"
    } else {
        &view.warn_area
    };
    let title = format!("{area}の警報・注意報");
    let cy = y + (h - y) / 2.0;
    let tw = fit_left(c, t, &title, pad, cy - 18.0 * k, 28.0 * k, w * 0.25, DIM);
    // 右端に取得の状態。チップはその手前まで
    let fw = fit_right(
        c,
        t,
        &view.warn_feed.text,
        w - pad,
        cy - 13.0 * k,
        22.0 * k,
        w * 0.3,
        feed_color(&view.warn_feed),
    );
    let mut x = pad + tw + 32.0 * k;
    let right = w - pad - fw - 32.0 * k;
    let Some(list) = &view.warnings else {
        t.draw(c, "--", x, cy - 20.0 * k, 32.0 * k, DIM);
        return;
    };
    if list.is_empty() {
        t.draw(c, "発表なし", x, cy - 20.0 * k, 32.0 * k, FG);
        return;
    }
    let (chip_h, size) = (56.0 * k, 30.0 * k);
    for (i, wn) in list.iter().enumerate() {
        let cw = t.measure(&wn.name, size) + 40.0 * k;
        if x + cw > right {
            let rest = format!("ほか {} 件", list.len() - i);
            t.draw(c, &rest, x, cy - 18.0 * k, 28.0 * k, FG);
            break;
        }
        let (bg, fg) = warning_colors(wn.level);
        c.fill_rect(x, cy - chip_h / 2.0, cw, chip_h, rgb(bg, 255));
        if wn.level >= 5 {
            c.stroke_rect(
                x,
                cy - chip_h / 2.0,
                cw,
                chip_h,
                2.0 * k,
                rgba(255, 255, 255, 255),
            );
        }
        t.draw(c, &wn.name, x + 20.0 * k, cy - size * 0.68, size, fg);
        x += cw + 16.0 * k;
    }
}

/// 見本(`png` の画像とテスト)。
pub fn demo_view() -> RoomView {
    let wave = |base: f32, amp: f32, n: usize| -> Vec<f32> {
        (0..n)
            .map(|i| base + amp * (i as f32 / n as f32 * std::f32::consts::TAU).sin())
            .collect()
    };
    let tile = |label: &str, value: &str, unit: &str, note: &str, level, history| SensorTile {
        label: label.into(),
        value: value.into(),
        unit: unit.into(),
        note: note.into(),
        level,
        history,
    };
    let mut co2 = wave(700.0, 250.0, 144);
    co2[40] = f32::NAN;
    co2[41] = f32::NAN;
    let day = |label: &str, weather: &str, max, min, pops| ForecastDay {
        label: label.into(),
        weather: weather.into(),
        max,
        min,
        pops,
    };
    let week = [
        ("16", 3, "晴時々曇", Some(24), Some(15), Some(10)),
        ("17", 4, "曇", Some(22), Some(16), Some(30)),
        ("18", 5, "雨", Some(19), Some(15), Some(70)),
        ("19", 6, "曇一時雨", Some(21), Some(14), Some(40)),
        ("20", 0, "晴", Some(23), Some(15), Some(10)),
        ("21", 1, "曇後晴", Some(24), Some(16), Some(10)),
        ("22", 2, "雪", None, None, None),
    ]
    .map(|(day, weekday, weather, max, min, pop)| WeekDay {
        day: day.into(),
        weekday,
        weather: weather.into(),
        max,
        min,
        pop,
    })
    .to_vec();
    RoomView {
        clock: "2026/10/14 (火)  08:42".into(),
        tiles: vec![
            tile("温度", "23.5", "℃", "", 0, wave(23.5, 1.2, 144)),
            tile("湿度", "56", "%", "", 0, wave(55.0, 6.0, 144)),
            tile("CO₂", "1120", "ppm", "やや高い", 1, co2),
            tile("VOC", "2708", "μg/m³", "", 0, wave(1500.0, 900.0, 144)),
            tile(
                "気圧",
                "1011.4",
                "hPa",
                "↘ 下降 -1.8",
                0,
                wave(1012.0, 2.0, 144),
            ),
            tile("玄関", "23.4", "℃", "35 %", 0, Vec::new()),
        ],
        env: Feed {
            text: "HA 08:42 取得".into(),
            stale: false,
        },
        weather: Some(Weather {
            area: "千葉県北西部".into(),
            days: vec![
                day(
                    "今日 14日(火)",
                    "晴れ時々くもり",
                    Some(24),
                    None,
                    [None, Some(10), Some(20), Some(10)],
                ),
                day(
                    "明日 15日(水)",
                    "くもり昼前から夕方晴れ所により明け方まで雨",
                    Some(21),
                    Some(16),
                    [Some(30), Some(60), Some(50), Some(20)],
                ),
            ],
            week,
        }),
        weather_feed: Feed {
            text: "気象庁 05:00 発表".into(),
            stale: false,
        },
        warn_area: "流山市".into(),
        warnings: Some(vec![
            Warning {
                name: "大雨警報".into(),
                level: 3,
            },
            Warning {
                name: "雷注意報".into(),
                level: 2,
            },
        ]),
        warn_feed: Feed {
            text: "08:30 発表".into(),
            stale: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_from_text() {
        assert_eq!(sky_of("晴"), (Some(Sky::Sun), None, false));
        assert_eq!(
            sky_of("晴時々曇"),
            (Some(Sky::Sun), Some(Sky::Cloud), false)
        );
        assert_eq!(
            sky_of("くもり後雨"),
            (Some(Sky::Cloud), Some(Sky::Rain), true)
        );
        assert_eq!(
            sky_of("くもり　昼前　から　夕方　晴れ"),
            (Some(Sky::Cloud), Some(Sky::Sun), false)
        );
        assert_eq!(
            sky_of("雨のち晴れ"),
            (Some(Sky::Rain), Some(Sky::Sun), true)
        );
        assert_eq!(sky_of("雪か雨"), (Some(Sky::Snow), Some(Sky::Rain), false));
        assert_eq!(sky_of("暴風雨"), (Some(Sky::Rain), None, false));
        assert_eq!(sky_of(""), (None, None, false));
    }

    /// 見本・空・取得前のどれでも、どの大きさでも描ける(パニックしない)。
    #[test]
    fn draws_any_view_at_any_size() {
        let mut t = TextEngine::new("Noto Sans CJK JP");
        let demo = demo_view();
        let empty = RoomView::default();
        let mut many = demo.clone();
        many.tiles = (0..9).map(|_| demo.tiles[0].clone()).collect();
        many.warnings = Some(
            (0..20)
                .map(|i| Warning {
                    name: format!("注意報{i}"),
                    level: 2 + (i % 4) as u8,
                })
                .collect(),
        );
        for (w, h) in [(2560, 1080), (1920, 1080), (640, 270)] {
            let mut c = Canvas::new(w, h);
            for v in [&demo, &empty, &many] {
                room_screen(&mut c, &mut t, v);
            }
        }
    }
}
