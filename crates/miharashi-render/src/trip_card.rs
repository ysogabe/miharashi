//! 旅と「いつもの暮らし」の切り替わりに挟むカード。左に撮影地点の地図、右に名前・期間・枚数。
use crate::canvas::{Canvas, rgba};
use crate::geo::{countries, distance_km, overlaps, prefs, projector};
use crate::photo_layout::scale;
use crate::text::TextEngine;

/// カードに出す内容。
#[derive(Debug, Clone, PartialEq)]
pub struct TripCard {
    /// 「旅」か「いつもの暮らし」
    pub everyday: bool,
    /// "高崎市の旅" / "2026年 秋" など
    pub title: String,
    /// "2026.09.22 〜 09.25" など
    pub period: String,
    /// まとまりの写真の枚数
    pub count: usize,
    /// 主な地名(多い順)
    pub places: Vec<String>,
    /// 撮影地点 (緯度, 経度)。撮影順
    pub points: Vec<(f64, f64)>,
    /// 自宅 (緯度, 経度)
    pub home: Option<(f64, f64)>,
    /// 「いつもの暮らし」の範囲(km)。地図の広さに使う
    pub radius_km: f64,
}

/// 地図に出す範囲 (経度最小, 経度最大, 緯度最小, 緯度最大)。
fn bounds(card: &TripCard) -> (f64, f64, f64, f64) {
    if card.everyday
        && let Some((lat, lon)) = card.home
    {
        let half = (card.radius_km * 1.3 / 111.0).max(0.2);
        let half_lon = half / lat.to_radians().cos().max(0.2);
        return (lon - half_lon, lon + half_lon, lat - half, lat + half);
    }
    if card.points.is_empty() {
        return (122.5, 146.5, 24.0, 46.0);
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(lat, lon) in &card.points {
        (x0, x1, y0, y1) = (x0.min(lon), x1.max(lon), y0.min(lat), y1.max(lat));
    }
    // 2 割の余白と、狭すぎないよう最低 0.4 度
    let pad_y = ((y1 - y0) * 0.2).max(0.0);
    let pad_x = ((x1 - x0) * 0.2).max(0.0);
    (x0, x1, y0, y1) = (x0 - pad_x, x1 + pad_x, y0 - pad_y, y1 + pad_y);
    let min_span = 0.4;
    for (lo, hi) in [(&mut x0, &mut x1), (&mut y0, &mut y1)] {
        let span = *hi - *lo;
        if span < min_span {
            *lo -= (min_span - span) / 2.0;
            *hi += (min_span - span) / 2.0;
        }
    }
    (x0, x1, y0.max(-85.0), y1.min(85.0))
}

/// 文字が `width` に収まる大きさ(`max` から `min` まで縮める)。
fn fit(t: &mut TextEngine, s: &str, width: f32, max: f32, min: f32) -> f32 {
    let w = t.measure(s, max).max(1.0);
    (max * (width / w).min(1.0)).max(min)
}

pub fn trip_card(c: &mut Canvas, t: &mut TextEngine, card: &TripCard) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let k = scale(c.height());
    c.fill_rect(0.0, 0.0, w, h, rgba(8, 12, 20, 255));
    let accent = if card.everyday {
        [120, 200, 140]
    } else {
        [255, 190, 60]
    };
    let acc = [accent[0], accent[1], accent[2], 255];

    let margin = 48.0 * k;
    let rect = (
        margin,
        margin,
        (h - 2.0 * margin).min(w * 0.5),
        h - 2.0 * margin,
    );
    let mut m = Canvas::new(rect.2.max(1.0) as u32, rect.3.max(1.0) as u32);
    m.clear(rgba(10, 22, 38, 255));
    draw_map(&mut m, card, accent, k);
    c.draw_pixmap(&m.pixmap, rect.0 as i32, rect.1 as i32);
    c.stroke_rect(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        2.0 * k,
        rgba(70, 90, 115, 255),
    );

    let x = rect.0 + rect.2 + 72.0 * k;
    let width = w - margin - x;
    let fg = [235, 240, 248, 255];
    let dim = [150, 165, 185, 255];
    let mut y = margin + 40.0 * k;
    let kind = if card.everyday {
        "いつもの暮らし"
    } else {
        "旅の記録"
    };
    t.draw(c, kind, x, y, 40.0 * k, acc);
    y += 90.0 * k;
    let size = fit(t, &card.title, width, 96.0 * k, 48.0 * k);
    t.draw(c, &card.title, x, y, size, fg);
    y += size + 50.0 * k;
    if !card.period.is_empty() {
        let size = fit(t, &card.period, width, 52.0 * k, 28.0 * k);
        t.draw(c, &card.period, x, y, size, fg);
        y += size + 40.0 * k;
    }
    t.draw(c, &format!("{} 枚", card.count), x, y, 44.0 * k, dim);
    y += 100.0 * k;
    let places = card.places.join("・");
    if !places.is_empty() {
        let size = fit(t, &places, width, 40.0 * k, 26.0 * k);
        t.draw(c, &places, x, y, size, fg);
    }
}

fn draw_map(c: &mut Canvas, card: &TripCard, accent: [u8; 3], k: f32) {
    let view = bounds(card);
    let rect = (0.0, 0.0, c.width() as f32, c.height() as f32);
    let proj = projector(view, rect);
    let project = |rings: &[Vec<(f64, f64)>]| -> Vec<Vec<(f32, f32)>> {
        rings
            .iter()
            .map(|r| r.iter().map(|&(lon, lat)| proj(lon, lat)).collect())
            .collect()
    };
    for r in countries().iter().filter(|r| overlaps(r.bbox, view)) {
        let rings = project(&r.rings);
        c.fill_rings(&rings, rgba(44, 58, 76, 255));
        c.stroke_rings(&rings, 1.5 * k, rgba(110, 130, 150, 255));
    }
    // 日本は都道府県の境も描く(国の簡略化よりも細かい)
    for r in prefs().iter().filter(|r| overlaps(r.bbox, view)) {
        let rings = project(&r.rings);
        c.fill_rings(&rings, rgba(44, 58, 76, 255));
        c.stroke_rings(&rings, 1.0 * k, rgba(90, 110, 130, 255));
    }
    let [r, g, b] = accent;
    let pts: Vec<(f32, f32)> = card
        .points
        .iter()
        .map(|&(lat, lon)| proj(lon, lat))
        .collect();
    if !card.everyday && pts.len() > 1 {
        c.polyline(&pts, 2.0 * k, rgba(r, g, b, 120));
    }
    for &(x, y) in &pts {
        c.fill_circle(x, y, 7.0 * k, rgba(r, g, b, 230));
    }
    if let Some((lat, lon)) = card.home {
        let (hx, hy) = proj(lon, lat);
        if (0.0..rect.2).contains(&hx) && (0.0..rect.3).contains(&hy) {
            c.fill_circle(hx, hy, 11.0 * k, rgba(255, 255, 255, 255));
            c.fill_circle(hx, hy, 7.0 * k, rgba(30, 140, 255, 255));
        }
    }
    if let Some(b) = locator_bounds(card) {
        draw_locator(c, card, b, accent, k);
    }
}

/// 遠い旅(自宅から 1000km 以上)の広域図の範囲。自宅と旅先が両方入り、最低 30×20 度。
fn locator_bounds(card: &TripCard) -> Option<(f64, f64, f64, f64)> {
    let home = card.home?;
    if card.everyday || !card.points.iter().any(|&p| distance_km(home, p) >= 1000.0) {
        return None;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (home.1, home.1, home.0, home.0);
    for &(lat, lon) in &card.points {
        (x0, x1, y0, y1) = (x0.min(lon), x1.max(lon), y0.min(lat), y1.max(lat));
    }
    let (pad_x, pad_y) = ((x1 - x0) * 0.15 + 4.0, (y1 - y0) * 0.15 + 4.0);
    (x0, x1, y0, y1) = (x0 - pad_x, x1 + pad_x, y0 - pad_y, y1 + pad_y);
    for (lo, hi, min) in [(&mut x0, &mut x1, 30.0), (&mut y0, &mut y1, 20.0)] {
        let span = *hi - *lo;
        if span < min {
            *lo -= (min - span) / 2.0;
            *hi += (min - span) / 2.0;
        }
    }
    Some((x0.max(-180.0), x1.min(180.0), y0.max(-80.0), y1.min(84.0)))
}

/// 地図の左下に、自宅(★の代わりに青い点)と旅先を結ぶ広域図を重ねる。
fn draw_locator(
    c: &mut Canvas,
    card: &TripCard,
    view: (f64, f64, f64, f64),
    accent: [u8; 3],
    k: f32,
) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let iw = (w * 0.42).round();
    let ih = (iw * 0.62).round();
    let (ix, iy) = (16.0 * k, h - ih - 16.0 * k);
    let mut m = Canvas::new(iw.max(1.0) as u32, ih.max(1.0) as u32);
    m.clear(rgba(6, 14, 26, 255));
    let proj = projector(view, (0.0, 0.0, iw, ih));
    for r in countries().iter().filter(|r| overlaps(r.bbox, view)) {
        let rings: Vec<Vec<(f32, f32)>> = r
            .rings
            .iter()
            .map(|ring| ring.iter().map(|&(lon, lat)| proj(lon, lat)).collect())
            .collect();
        m.fill_rings(&rings, rgba(52, 66, 86, 255));
    }
    let [r, g, b] = accent;
    // 旅先は、ほかの撮影地点との距離の合計が最も小さい地点(乗り継ぎ地に引っ張られない)
    let pts = &card.points;
    let step = (pts.len() / 200).max(1); // 多いときは間引いて比べる
    let (clat, clon) = pts
        .iter()
        .step_by(step)
        .min_by(|a, b| {
            let cost = |p: &(f64, f64)| -> f64 {
                pts.iter()
                    .step_by(step)
                    .map(|q| (p.0 - q.0).abs() + (p.1 - q.1).abs())
                    .sum()
            };
            cost(a).total_cmp(&cost(b))
        })
        .copied()
        .unwrap_or_default();
    let dest = proj(clon, clat);
    if let Some((lat, lon)) = card.home {
        let home = proj(lon, lat);
        m.polyline(&[home, dest], 2.5 * k, rgba(r, g, b, 200));
        m.fill_circle(home.0, home.1, 8.0 * k, rgba(255, 255, 255, 255));
        m.fill_circle(home.0, home.1, 5.0 * k, rgba(30, 140, 255, 255));
    }
    m.fill_circle(dest.0, dest.1, 8.0 * k, rgba(r, g, b, 255));
    c.draw_pixmap(&m.pixmap, ix as i32, iy as i32);
    c.stroke_rect(ix, iy, iw, ih, 2.0 * k, rgba(150, 165, 185, 255));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(everyday: bool, points: Vec<(f64, f64)>) -> TripCard {
        TripCard {
            everyday,
            title: String::new(),
            period: String::new(),
            count: points.len(),
            places: vec![],
            points,
            home: Some(crate::sample::HOME),
            radius_km: 50.0,
        }
    }

    #[test]
    fn bounds_cover_points_with_minimum_span() {
        let b = bounds(&card(false, vec![(39.65, 66.96), (39.77, 64.42)]));
        assert!(b.0 < 64.42 && b.1 > 66.96 && b.2 < 39.65 && b.3 > 39.77);
        let one = bounds(&card(false, vec![(36.3, 139.0)]));
        assert!((one.1 - one.0) > 0.399 && (one.3 - one.2) > 0.399);
        // いつもの暮らしは自宅まわり
        let e = bounds(&card(true, vec![(36.3, 139.0)]));
        let h = crate::sample::HOME;
        assert!(e.0 < h.1 && h.1 < e.1 && e.2 < h.0 && h.0 < e.3);
        assert!(e.3 - e.2 < 2.0);
        assert_eq!(bounds(&card(false, vec![])).0, 122.5);
    }

    #[test]
    fn locator_only_for_far_trips_and_covers_home() {
        assert!(
            locator_bounds(&card(false, vec![(36.3, 139.0)])).is_none(),
            "国内"
        );
        assert!(
            locator_bounds(&card(true, vec![(39.65, 66.96)])).is_none(),
            "暮らし"
        );
        let b = locator_bounds(&card(false, vec![(39.65, 66.96), (41.31, 69.28)])).unwrap();
        let h = crate::sample::HOME;
        assert!(b.0 < 66.96 && b.1 > h.1 && b.2 < h.0 && b.3 > 41.31);
        let near = locator_bounds(&card(false, vec![(25.03, 121.56)])).unwrap();
        assert!(
            near.1 - near.0 >= 30.0 && near.3 - near.2 >= 20.0,
            "台湾でも最低の広さ"
        );
    }
}
