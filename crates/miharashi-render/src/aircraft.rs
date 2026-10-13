//! 航空機の画面。左半分に受信機を中心にしたレーダー、右半分にいちばん近い機体の諸元とほかの機体。
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
}

const FG: [u8; 4] = [235, 240, 248, 255];
const DIM: [u8; 4] = [150, 165, 185, 255];
const FOCUS: [u8; 3] = [255, 190, 60];
const OTHER: [u8; 3] = [90, 200, 255];

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

pub fn aircraft_screen(c: &mut Canvas, t: &mut TextEngine, view: &AircraftView) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let k = scale(c.height());
    c.fill_rect(0.0, 0.0, w, h, rgba(6, 10, 18, 255));
    let half = w / 2.0;
    radar(c, t, view, (0.0, 0.0, half, h), k);
    c.fill_rect(half, 0.0, 2.0 * k, h, rgba(40, 55, 75, 255));
    details(
        c,
        t,
        view,
        (half + 56.0 * k, 0.0, w - half - 112.0 * k, h),
        k,
    );
}

fn radar(
    c: &mut Canvas,
    t: &mut TextEngine,
    view: &AircraftView,
    rect: (f32, f32, f32, f32),
    k: f32,
) {
    let (rx, ry, rw, rh) = rect;
    let (lat, lon) = view.receiver;
    let r_km = view.radius_km.max(1.0);
    // 半径の 1.12 倍を南北の半分にとる(円の外に方位の文字を置く余白)
    let half = r_km * 1.12 / 111.0;
    let half_lon = half / lat.to_radians().cos().max(0.2);
    let b = (lon - half_lon, lon + half_lon, lat - half, lat + half);
    let mut m = Canvas::new(rw.max(1.0) as u32, rh.max(1.0) as u32);
    m.clear(rgba(6, 12, 22, 255));
    let proj = projector(b, (0.0, 0.0, rw, rh));
    for r in prefs().iter().filter(|r| overlaps(r.bbox, b)) {
        let rings: Vec<Vec<(f32, f32)>> = r
            .rings
            .iter()
            .map(|ring| ring.iter().map(|&(x, y)| proj(x, y)).collect())
            .collect();
        m.fill_rings(&rings, rgba(20, 32, 46, 255));
        m.stroke_rings(&rings, 1.2 * k, rgba(50, 70, 92, 255));
    }
    let (cx, cy) = proj(lon, lat);
    let px_per_km = cy - proj(lon, lat + 1.0 / 111.0).1;
    // 距離の輪と十字
    for ring in [10.0, 25.0, 50.0, 100.0] {
        if ring > r_km + 0.1 {
            continue;
        }
        let rr = ring as f32 * px_per_km;
        m.stroke_circle(cx, cy, rr, 1.5 * k, rgba(60, 120, 90, 200));
        // 東の軸の上、輪の内側に距離を書く
        let label = format!("{ring:.0}km");
        let lw = t.measure(&label, 22.0 * k);
        t.draw(
            &mut m,
            &label,
            cx + rr - lw - 6.0 * k,
            cy - 28.0 * k,
            22.0 * k,
            [90, 160, 120, 255],
        );
    }
    let outer = r_km as f32 * px_per_km;
    m.polyline(
        &[(cx - outer, cy), (cx + outer, cy)],
        1.0 * k,
        rgba(60, 120, 90, 120),
    );
    m.polyline(
        &[(cx, cy - outer), (cx, cy + outer)],
        1.0 * k,
        rgba(60, 120, 90, 120),
    );
    for (label, dx, dy) in [
        ("N", 0.0, -1.0),
        ("E", 1.0, 0.0),
        ("S", 0.0, 1.0),
        ("W", -1.0, 0.0),
    ] {
        let s = 30.0 * k;
        let x = cx + dx * (outer + 26.0 * k) - t.measure(label, s) / 2.0;
        let y = cy + dy * (outer + 26.0 * k) - s * 0.62;
        t.draw(&mut m, label, x, y, s, [120, 180, 140, 255]);
    }
    m.fill_circle(cx, cy, 8.0 * k, rgba(255, 255, 255, 255));
    m.fill_circle(cx, cy, 5.0 * k, rgba(30, 140, 255, 255));
    // 機体(遠いものから描き、いちばん近い機体を最後に重ねる)
    for (i, p) in view.planes.iter().enumerate().rev() {
        let (x, y) = proj(p.lon, p.lat);
        let color = if p.ground {
            [130, 140, 150]
        } else if i == 0 {
            FOCUS
        } else {
            OTHER
        };
        let size = if i == 0 { 30.0 } else { 20.0 } * k;
        plane_marker(&mut m, x, y, p.track.unwrap_or(0.0), size, color);
        let name = if p.callsign.is_empty() {
            dash(&p.reg).to_string()
        } else {
            p.callsign.clone()
        };
        let ink = [color[0], color[1], color[2], 255];
        t.draw(
            &mut m,
            &name,
            x + size + 8.0 * k,
            y - 32.0 * k,
            28.0 * k,
            ink,
        );
        t.draw(
            &mut m,
            &altitude_label(p),
            x + size + 8.0 * k,
            y + 2.0 * k,
            24.0 * k,
            DIM,
        );
    }
    t.draw(
        &mut m,
        &format!("航空機  受信機から半径{:.0}km", r_km),
        24.0 * k,
        20.0 * k,
        34.0 * k,
        FG,
    );
    t.draw_right(&mut m, &view.time, rw - 24.0 * k, 20.0 * k, 34.0 * k, DIM);
    c.draw_pixmap(&m.pixmap, rx as i32, ry as i32);
}

/// 進行方向を向いた三角形。
fn plane_marker(c: &mut Canvas, x: f32, y: f32, track: f64, size: f32, color: [u8; 3]) {
    let a = (track as f32).to_radians();
    let (s, co) = (a.sin(), a.cos());
    // 北向きの三角形を回す(画面の y は下向き)
    let rot = |dx: f32, dy: f32| (x + dx * co - dy * s, y + dx * s + dy * co);
    let tri = vec![
        rot(0.0, -size),
        rot(size * 0.6, size * 0.7),
        rot(0.0, size * 0.35),
        rot(-size * 0.6, size * 0.7),
    ];
    let [r, g, b] = color;
    c.fill_rings(std::slice::from_ref(&tri), rgba(r, g, b, 255));
    c.stroke_rings(&[tri], 1.5 * size / 14.0, rgba(0, 0, 0, 180));
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
    let Some(p) = view.planes.first() else {
        t.draw(
            c,
            "半径内に機体はありません",
            x,
            y + 40.0 * k,
            48.0 * k,
            DIM,
        );
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
    t.draw(c, "いちばん近い機体", x, y, 30.0 * k, DIM);
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
    c.fill_rect(x, y, width, 2.0 * k, rgba(40, 55, 75, 255));
    y += 24.0 * k;
    t.draw(c, "ほかの機体(近い順)", x, y, 30.0 * k, DIM);
    y += 50.0 * k;
    let cols = [0.0, 0.22, 0.38, 0.62, 0.8];
    for p in view.planes.iter().skip(1).take(6) {
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
    t.draw_right(
        c,
        &format!("半径内 {} 機 / 受信 {} 機", view.planes.len(), view.total),
        x + width,
        h - 70.0 * k,
        28.0 * k,
        DIM,
    );
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
}
