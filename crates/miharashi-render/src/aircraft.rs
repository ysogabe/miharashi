//! 航空機の画面。左半分に受信機を中心にしたレーダー、右半分にいちばん近い機体の諸元とほかの機体。
//! 管制レーダー風に、回る線と残光、線が通ると光る点を描く。GPU 経路では背景(地図、文字)を CPU で
//! 変わったときだけ描き、回る線と点は `miharashi-gpu` のシェーダーが毎フレーム描く(PoC-005 PR4a)。
use crate::canvas::{Canvas, rgba};
use crate::geo::{overlaps, prefs, projector};
use crate::photo_layout::scale;
use crate::text::TextEngine;

/// 機体 1 機。取れなかった項目は None か空文字。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plane {
    pub hex: String,
    pub callsign: String,
    pub lat: f64,
    pub lon: f64,
    /// 気圧高度(ft)。地上なら None で `ground`
    pub alt_ft: Option<i32>,
    pub ground: bool,
    /// 対地速度(kt)
    pub gs_kt: Option<f64>,
    /// 進行方向(度。北 0、時計回り)
    pub track: Option<f64>,
    /// 昇降率(ft/分)
    pub vrate: Option<i32>,
    /// 受信機からの距離(km)と方位(度)
    pub dist_km: f64,
    pub bearing: f64,
    /// ICAO の型式("B789" など)
    pub type_code: String,
    /// 機種("Boeing 787 9" など)
    pub model: String,
    pub reg: String,
    /// 運航会社か所有者
    pub owner: String,
    /// 出発地と到着地("HND 東京" など)
    pub from: String,
    pub to: String,
}

/// 画面に出す内容。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AircraftView {
    /// 受信機 (緯度, 経度)
    pub receiver: (f64, f64),
    pub radius_km: f64,
    /// 半径内の機体(近い順)
    pub planes: Vec<Plane>,
    /// 受信している機体の総数(半径外も含む)
    pub total: usize,
    /// 時刻("22:41:05")
    pub time: String,
    /// 注目する機体(`planes` の添字)。0 = いちばん近い機体
    pub focus: usize,
    /// 機体がいないときに出す受信の状態("受信機から応答がありません" など)。None なら「半径内に機体はありません」
    pub notice: Option<String>,
}

// 管制レーダー風の配色(暗い背景に緑)。注目する機体だけ琥珀色にする
const FG: [u8; 4] = [170, 245, 190, 255];
const DIM: [u8; 4] = [80, 150, 105, 255];
const FOCUS: [u8; 3] = [255, 190, 60];
const OTHER: [u8; 3] = [110, 255, 150];
const GROUND: [u8; 3] = [90, 150, 115];
const BG: [u8; 3] = [2, 8, 5];
const LINE: [u8; 3] = [20, 60, 35];

/// 回る線が 1 周する時間 [秒]。
pub const SWEEP_PERIOD_SECS: f32 = 4.0;
/// 残光の長さ(明るさが 1/e になるまでの角度 [rad])。
pub const TRAIL: f32 = 0.9;
/// CPU で描くとき(静止画)の回る線の角度 [rad]。北 0、時計回り
pub const STILL_SWEEP: f32 = 2.6;

/// レーダーの点の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlipKind {
    /// 注目する機体
    Focus,
    Other,
    Ground,
}

/// レーダーの点 1 つ(画面の px)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blip {
    pub x: f32,
    pub y: f32,
    pub kind: BlipKind,
}

/// レーダーの円と点の位置(画面の px)。背景(CPU)と回る線(GPU)で同じものを使う。
#[derive(Debug, Clone, PartialEq)]
pub struct RadarGeom {
    pub center: (f32, f32),
    pub radius: f32,
    pub blips: Vec<Blip>,
}

fn dash(s: &str) -> &str {
    if s.is_empty() { "—" } else { s }
}

fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// 高度の表記("32,000 ft" / "地上" / "—")。
pub fn altitude_label(p: &Plane) -> String {
    match (p.ground, p.alt_ft) {
        (true, _) => "地上".into(),
        (false, Some(a)) => format!("{} ft", thousands(a as i64)),
        (false, None) => "—".into(),
    }
}

/// 方位(度)を 8 方位の日本語にする。
pub fn compass(deg: f64) -> &'static str {
    const N: [&str; 8] = ["北", "北東", "東", "南東", "南", "南西", "西", "北西"];
    N[(((deg.rem_euclid(360.0) + 22.5) / 45.0) as usize) % 8]
}

fn rgb(c: [u8; 3], a: u8) -> tiny_skia::Color {
    rgba(c[0], c[1], c[2], a)
}

/// 左半分のレーダーの範囲(画面の px)。
fn radar_rect(size: (u32, u32)) -> (f32, f32, f32, f32) {
    (0.0, 0.0, size.0 as f32 / 2.0, size.1 as f32)
}

/// 地図の範囲(経度 西, 東, 緯度 南, 北)。半径の 1.12 倍を南北の半分にとる(円の外に方位の文字を置く余白)
fn map_bounds(view: &AircraftView) -> (f64, f64, f64, f64) {
    let (lat, lon) = view.receiver;
    let half = view.radius_km.max(1.0) * 1.12 / 111.0;
    let half_lon = half / lat.to_radians().cos().max(0.2);
    (lon - half_lon, lon + half_lon, lat - half, lat + half)
}

/// 受信機を中心にしたレーダーの円と、機体の点の位置。
pub fn radar_geometry(view: &AircraftView, size: (u32, u32)) -> RadarGeom {
    let (lat, lon) = view.receiver;
    let proj = projector(map_bounds(view), radar_rect(size));
    let (cx, cy) = proj(lon, lat);
    let px_per_km = cy - proj(lon, lat + 1.0 / 111.0).1;
    let blips = view
        .planes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = proj(p.lon, p.lat);
            let kind = if i == view.focus {
                BlipKind::Focus
            } else if p.ground {
                BlipKind::Ground
            } else {
                BlipKind::Other
            };
            Blip { x, y, kind }
        })
        .collect();
    RadarGeom {
        center: (cx, cy),
        radius: view.radius_km.max(1.0) as f32 * px_per_km,
        blips,
    }
}

/// CPU で描く完成画面(`renderer = cpu` と静止画)。回る線は `STILL_SWEEP` の角度で止めて描く。
pub fn aircraft_screen(c: &mut Canvas, t: &mut TextEngine, view: &AircraftView) {
    aircraft_background(c, t, view);
    let g = radar_geometry(view, (c.width(), c.height()));
    sweep_cpu(c, &g, STILL_SWEEP, TRAIL);
}

/// 回る線と点を除いた画面(GPU 経路の背景)。機体の情報が変わったときだけ描き直す。
pub fn aircraft_background(c: &mut Canvas, t: &mut TextEngine, view: &AircraftView) {
    aircraft_background_cached(c, t, view, &mut None);
}

/// 地図、距離の輪、目盛りだけを描いたレーダーの下地(左半分)。受信機の位置、半径、画面の大きさで決まる。
pub struct RadarBase {
    key: ((f64, f64), f64, (u32, u32)),
    canvas: Canvas,
}

/// `aircraft_background` と同じ画面を、レーダーの下地を `base` に取っておいて描く。
/// 機体が動くたびの描き直しが、下地の写しと文字だけになる(地図の塗りがいちばん重い)。
pub fn aircraft_background_cached(
    c: &mut Canvas,
    t: &mut TextEngine,
    view: &AircraftView,
    base: &mut Option<RadarBase>,
) {
    let size = (c.width(), c.height());
    let (w, h) = (size.0 as f32, size.1 as f32);
    let k = scale(c.height());
    let key = (view.receiver, view.radius_km, size);
    if base.as_ref().is_none_or(|b| b.key != key) {
        *base = Some(RadarBase {
            key,
            canvas: radar_base(t, view, size, k),
        });
    }
    let base = base.as_ref().expect("上で作った");
    // 左半分はレーダーで覆うので、右半分だけ塗る
    c.fill_rect(w / 2.0, 0.0, w / 2.0, h, rgb(BG, 255));
    let mut m = Canvas {
        pixmap: base.canvas.pixmap.clone(),
    };
    radar_labels(&mut m, t, view, size, k);
    let (rx, ry, _, _) = radar_rect(size);
    c.draw_pixmap(&m.pixmap, rx as i32, ry as i32);
    let half = w / 2.0;
    c.fill_rect(half, 0.0, 2.0 * k, h, rgb(LINE, 255));
    details(
        c,
        t,
        view,
        (half + 56.0 * k, 0.0, w - half - 112.0 * k, h),
        k,
    );
}

/// レーダーの下地: 地図、距離の輪、目盛り、方位の文字、受信機の位置。
fn radar_base(t: &mut TextEngine, view: &AircraftView, size: (u32, u32), k: f32) -> Canvas {
    let (rx, ry, rw, rh) = radar_rect(size);
    let r_km = view.radius_km.max(1.0);
    let b = map_bounds(view);
    let g = radar_geometry(view, size);
    let mut m = Canvas::new(rw.max(1.0) as u32, rh.max(1.0) as u32);
    m.clear(rgb(BG, 255));
    let proj = projector(b, (0.0, 0.0, rw, rh));
    for r in prefs().iter().filter(|r| overlaps(r.bbox, b)) {
        let rings: Vec<Vec<(f32, f32)>> = r
            .rings
            .iter()
            .map(|ring| ring.iter().map(|&(x, y)| proj(x, y)).collect())
            .collect();
        m.fill_rings(&rings, rgba(5, 20, 12, 255));
        m.stroke_rings(&rings, 1.2 * k, rgba(25, 70, 42, 255));
    }
    let (cx, cy) = (g.center.0 - rx, g.center.1 - ry);
    let outer = g.radius;
    let px_per_km = outer / r_km as f32;
    // 距離の輪と十字
    for ring in [10.0, 25.0, 50.0, 100.0] {
        if ring > r_km + 0.1 {
            continue;
        }
        let rr = ring as f32 * px_per_km;
        m.stroke_circle(cx, cy, rr, 1.5 * k, rgba(40, 140, 75, 200));
        // 東の軸の上、輪の内側に距離を書く
        let label = format!("{ring:.0}km");
        let lw = t.measure(&label, 22.0 * k);
        t.draw(
            &mut m,
            &label,
            cx + rr - lw - 6.0 * k,
            cy - 28.0 * k,
            22.0 * k,
            DIM,
        );
    }
    m.stroke_circle(cx, cy, outer, 2.0 * k, rgba(50, 170, 90, 230));
    // 30° ごとの目盛り
    for i in 0..12 {
        let a = (i as f32 * 30.0).to_radians();
        let (s, co) = (a.sin(), -a.cos());
        let inner = if i % 3 == 0 { 0.0 } else { outer * 0.94 };
        m.polyline(
            &[
                (cx + s * inner, cy + co * inner),
                (cx + s * outer, cy + co * outer),
            ],
            1.0 * k,
            rgba(40, 140, 75, if i % 3 == 0 { 120 } else { 200 }),
        );
    }
    for (label, dx, dy) in [
        ("N", 0.0, -1.0),
        ("E", 1.0, 0.0),
        ("S", 0.0, 1.0),
        ("W", -1.0, 0.0),
    ] {
        let s = 30.0 * k;
        let x = cx + dx * (outer + 26.0 * k) - t.measure(label, s) / 2.0;
        let y = cy + dy * (outer + 26.0 * k) - s * 0.62;
        t.draw(&mut m, label, x, y, s, [OTHER[0], OTHER[1], OTHER[2], 255]);
    }
    m.fill_circle(cx, cy, 4.0 * k, rgb(OTHER, 255));
    t.draw(
        &mut m,
        &format!("航空機  受信機から半径{:.0}km", r_km),
        24.0 * k,
        20.0 * k,
        34.0 * k,
        FG,
    );
    m
}

/// 下地の上に、機体の名前と高度、時刻を描く(点そのものは回る線と一緒に描く)。
fn radar_labels(m: &mut Canvas, t: &mut TextEngine, view: &AircraftView, size: (u32, u32), k: f32) {
    let (rx, ry, rw, _) = radar_rect(size);
    let g = radar_geometry(view, size);
    // 遠いものから描き、注目する機体を最後に重ねる
    let mut order: Vec<usize> = (0..view.planes.len()).rev().collect();
    order.sort_by_key(|&i| i == view.focus);
    for (p, blip) in order.iter().map(|&i| (&view.planes[i], &g.blips[i])) {
        let (x, y) = (blip.x - rx, blip.y - ry);
        let color = match blip.kind {
            BlipKind::Focus => FOCUS,
            BlipKind::Other => OTHER,
            BlipKind::Ground => GROUND,
        };
        let off = 28.0 * k;
        if blip.kind == BlipKind::Focus {
            // 選択中の機体を角かっこで囲む
            let s = 16.0 * k;
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let (ex, ey) = (x + sx * s, y + sy * s);
                m.polyline(
                    &[(ex - sx * s * 0.5, ey), (ex, ey), (ex, ey - sy * s * 0.5)],
                    2.0 * k,
                    rgb(FOCUS, 255),
                );
            }
        }
        let name = if p.callsign.is_empty() {
            dash(&p.reg).to_string()
        } else {
            p.callsign.clone()
        };
        let ink = [color[0], color[1], color[2], 255];
        t.draw(m, &name, x + off, y - 32.0 * k, 28.0 * k, ink);
        t.draw(m, &altitude_label(p), x + off, y + 2.0 * k, 24.0 * k, DIM);
    }
    t.draw_right(m, &view.time, rw - 24.0 * k, 20.0 * k, 34.0 * k, DIM);
}

/// 線が通ってからの角度 [0, 2π)。`a` は点の方位(北 0、時計回り)
fn behind(sweep: f32, a: f32) -> f32 {
    (sweep - a).rem_euclid(std::f32::consts::TAU)
}

/// 画面の点の方位(北 0、時計回り。画面の y は下向き)。
fn bearing_px(center: (f32, f32), x: f32, y: f32) -> f32 {
    (x - center.0).atan2(-(y - center.1))
}

/// 回る線、残光、点を CPU で描く(GPU 経路では `miharashi-gpu` のシェーダーが同じものを描く)。
pub fn sweep_cpu(c: &mut Canvas, g: &RadarGeom, sweep: f32, trail: f32) {
    let k = scale(c.height());
    let (cx, cy) = g.center;
    let r = g.radius;
    let at = |a: f32, rr: f32| (cx + a.sin() * rr, cy - a.cos() * rr);
    // 残光: 線の後ろを細い扇形に分け、離れるほど薄くする
    let step = 0.04;
    for i in 0..((3.0 * trail / step) as usize) {
        let a0 = sweep - i as f32 * step;
        let alpha = (0.45 * (-(i as f32 * step) / trail).exp() * 255.0) as u8;
        if alpha == 0 {
            break;
        }
        let ring = vec![
            (cx, cy),
            at(a0, r),
            at(a0 - step * 0.5, r),
            at(a0 - step, r),
        ];
        c.fill_rings(&[ring], rgba(40, 255, 110, alpha));
    }
    c.polyline(&[(cx, cy), at(sweep, r)], 3.0 * k, rgba(150, 255, 180, 255));
    for b in &g.blips {
        let life =
            (-behind(sweep, bearing_px(g.center, b.x, b.y)) / (std::f32::consts::TAU * 0.55)).exp();
        let (color, size) = match b.kind {
            BlipKind::Focus => (FOCUS, 11.0),
            BlipKind::Other => (OTHER, 9.0),
            BlipKind::Ground => (GROUND, 7.0),
        };
        let a = (0.3 + life).min(1.0);
        c.fill_circle(b.x, b.y, size * 2.2 * k, rgb(color, (a * 70.0) as u8));
        c.fill_circle(b.x, b.y, size * k, rgb(color, (a * 255.0) as u8));
    }
}

fn details(
    c: &mut Canvas,
    t: &mut TextEngine,
    view: &AircraftView,
    rect: (f32, f32, f32, f32),
    k: f32,
) {
    let (x, _, width, h) = rect;
    let acc = [FOCUS[0], FOCUS[1], FOCUS[2], 255];
    let mut y = 40.0 * k;
    let count = format!("半径内 {} 機 / 受信 {} 機", view.planes.len(), view.total);
    let Some(p) = view.planes.get(view.focus) else {
        let msg = view.notice.as_deref().unwrap_or("半径内に機体はありません");
        t.draw(c, msg, x, y + 40.0 * k, 48.0 * k, DIM);
        if !view.time.is_empty() {
            let last = format!("最終受信 {}   {count}", view.time);
            t.draw(c, &last, x, y + 130.0 * k, 32.0 * k, DIM);
        }
        return;
    };
    let fit = |t: &mut TextEngine, s: &str, max: f32| {
        let m = t.measure(s, max).max(1.0);
        (max * (width / m).min(1.0)).max(max * 0.5)
    };
    let name = if p.callsign.is_empty() {
        dash(&p.reg).to_string()
    } else {
        p.callsign.clone()
    };
    let heading = if view.focus == 0 {
        "いちばん近い機体".to_string()
    } else {
        format!("{} 番目に近い機体(5 でいちばん近い機体へ)", view.focus + 1)
    };
    t.draw(c, &heading, x, y, 30.0 * k, DIM);
    y += 46.0 * k;
    t.draw(c, &name, x, y, 84.0 * k, acc);
    y += 112.0 * k;
    let model = match (p.model.is_empty(), p.type_code.is_empty()) {
        (false, false) => format!("{} ({})", p.model, p.type_code),
        (false, true) => p.model.clone(),
        (true, false) => p.type_code.clone(),
        (true, true) => "機種不明".into(),
    };
    let s = fit(t, &model, 50.0 * k);
    t.draw(c, &model, x, y, s, FG);
    y += s + 22.0 * k;
    let who = [p.reg.as_str(), p.owner.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ・  ");
    let s = fit(t, dash(&who), 38.0 * k);
    t.draw(c, dash(&who), x, y, s, DIM);
    y += s + 30.0 * k;
    if !p.from.is_empty() || !p.to.is_empty() {
        let route = format!("{}  →  {}", dash(&p.from), dash(&p.to));
        let s = fit(t, &route, 46.0 * k);
        t.draw(c, &route, x, y, s, FG);
        y += s + 34.0 * k;
    }
    let climb = match p.vrate {
        Some(v) if v > 300 => "  ↑上昇中",
        Some(v) if v < -300 => "  ↓降下中",
        _ => "",
    };
    let alt = match (p.ground, p.alt_ft) {
        (false, Some(a)) => format!(
            "{} ({} m){climb}",
            altitude_label(p),
            thousands((a as f64 * 0.3048).round() as i64)
        ),
        _ => altitude_label(p),
    };
    let speed = p.gs_kt.map_or("—".to_string(), |g| {
        format!("{:.0} kt ({:.0} km/h)", g, g * 1.852)
    });
    let pos = format!(
        "{:.1} km   方位 {}{}",
        p.dist_km,
        compass(p.bearing),
        p.track.map_or(String::new(), |tr| format!(
            "   進行 {:03.0}°",
            tr.rem_euclid(360.0)
        ))
    );
    for (label, value) in [("高度", alt), ("速度", speed), ("距離", pos)] {
        t.draw(c, label, x, y, 36.0 * k, DIM);
        let s = fit(t, &value, 40.0 * k);
        t.draw(c, &value, x + 120.0 * k, y, s, FG);
        y += 58.0 * k;
    }
    y += 10.0 * k;
    c.fill_rect(x, y, width, 2.0 * k, rgb(LINE, 255));
    y += 24.0 * k;
    t.draw(c, "ほかの機体(近い順)", x, y, 30.0 * k, DIM);
    y += 50.0 * k;
    let cols = [0.0, 0.22, 0.38, 0.62, 0.8];
    let others = view
        .planes
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != view.focus)
        .map(|(_, p)| p);
    for p in others.take(6) {
        if y > h - 110.0 * k {
            break;
        }
        let name = if p.callsign.is_empty() {
            dash(&p.reg).to_string()
        } else {
            p.callsign.clone()
        };
        let cells = [
            name,
            dash(&p.type_code).to_string(),
            altitude_label(p),
            format!("{:.1} km", p.dist_km),
            p.to.split(' ').nth(1).unwrap_or(dash(&p.to)).to_string(),
        ];
        for (i, cell) in cells.iter().enumerate() {
            let ink = if i == 0 {
                [OTHER[0], OTHER[1], OTHER[2], 255]
            } else {
                FG
            };
            t.draw(c, cell, x + width * cols[i], y, 32.0 * k, ink);
        }
        y += 48.0 * k;
    }
    t.draw_right(c, &count, x + width, h - 70.0 * k, 28.0 * k, DIM);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(thousands(32000), "32,000");
        assert_eq!(thousands(950), "950");
        assert_eq!(thousands(-1200), "-1,200");
        assert_eq!(compass(0.0), "北");
        assert_eq!(compass(44.0), "北東");
        assert_eq!(compass(359.0), "北");
        assert_eq!(compass(-90.0), "西");
        let mut p = Plane {
            alt_ft: Some(32000),
            ..Default::default()
        };
        assert_eq!(altitude_label(&p), "32,000 ft");
        p.ground = true;
        assert_eq!(altitude_label(&p), "地上");
        p.ground = false;
        p.alt_ft = None;
        assert_eq!(altitude_label(&p), "—");
    }

    #[test]
    fn radar_geometry_centers_on_receiver() {
        let plane = |lat: f64, lon: f64, ground: bool| Plane {
            lat,
            lon,
            ground,
            alt_ft: (!ground).then_some(10000),
            ..Default::default()
        };
        let view = AircraftView {
            receiver: (35.0, 139.0),
            radius_km: 50.0,
            // 真北 25km、受信機の上(地上)
            planes: vec![
                plane(35.0 + 25.0 / 111.0, 139.0, false),
                plane(35.0, 139.0, true),
            ],
            ..Default::default()
        };
        let g = radar_geometry(&view, (2560, 1080));
        // 左半分(1280×1080)の中央
        assert!((g.center.0 - 640.0).abs() < 1.0, "{:?}", g.center);
        assert!((g.center.1 - 540.0).abs() < 1.0, "{:?}", g.center);
        // 円の外に方位の文字を置く余白をとる
        assert!(g.radius > 400.0 && g.radius < 540.0, "{}", g.radius);
        let (n, gnd) = (g.blips[0], g.blips[1]);
        assert_eq!(n.kind, BlipKind::Focus);
        assert!((n.x - g.center.0).abs() < 1.0);
        assert!((g.center.1 - n.y - g.radius / 2.0).abs() < 2.0, "{n:?}");
        assert_eq!(gnd.kind, BlipKind::Ground);
        // 真北の点は線が通った直後に最も明るい
        assert!(behind(0.01, bearing_px(g.center, n.x, n.y)) < 0.02);
        assert!(behind(-0.01, bearing_px(g.center, n.x, n.y)) > 6.2);
    }
}
