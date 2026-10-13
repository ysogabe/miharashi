//! 全画面の上に重ねるアラート(地震・災害)。左に地図、右に要点を大きく出す。
//! 画面の種類に依存しないので、状況画面でも写真画面(GPU の重ね絵)でも同じ関数で描く。
use crate::canvas::{Canvas, rgba};
use crate::geo::{prefs, projector};
use crate::photo_layout::scale;
use crate::text::TextEngine;

/// 震度の段階。1〜4 はそのまま、5 弱 = 5、5 強 = 6、6 弱 = 7、6 強 = 8、7 = 9。
pub type Intensity = u8;

pub const SHINDO_4: Intensity = 4;
pub const SHINDO_5_LOWER: Intensity = 5;

/// 気象庁の表記("1"〜"4"、"5-"、"5+"、"6-"、"6+"、"7")を段階にする。それ以外は None。
pub fn parse_intensity(s: &str) -> Option<Intensity> {
    Some(match s {
        "1" => 1,
        "2" => 2,
        "3" => 3,
        "4" => 4,
        "5-" => 5,
        "5+" => 6,
        "6-" => 7,
        "6+" => 8,
        "7" => 9,
        _ => return None,
    })
}

pub fn intensity_label(i: Intensity) -> &'static str {
    match i {
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5弱",
        6 => "5強",
        7 => "6弱",
        8 => "6強",
        9 => "7",
        _ => "-",
    }
}

/// 気象庁の震度の配色に合わせる。
fn intensity_color(i: Intensity) -> [u8; 3] {
    match i {
        1 => [242, 242, 255],
        2 => [0, 170, 255],
        3 => [0, 65, 255],
        4 => [250, 230, 150],
        5 => [255, 230, 0],
        6 => [255, 153, 0],
        7 => [255, 40, 0],
        8 => [165, 0, 33],
        _ => [180, 0, 104],
    }
}

/// 地震アラートに出す内容。取得できなかった項目は空文字か None。
#[derive(Debug, Clone, PartialEq)]
pub struct QuakeView {
    /// 情報の種類(震度速報 / 震源・震度情報 など)
    pub title: String,
    pub max_intensity: Option<Intensity>,
    /// 震源地の名前
    pub place: String,
    /// 発生時刻("21:03" など)
    pub time: String,
    pub magnitude: String,
    /// 深さの表記("深さ 10km" / "ごく浅い" など)
    pub depth: String,
    /// 震源 (緯度, 経度)
    pub epicenter: Option<(f64, f64)>,
    /// 都道府県コード(2 桁)と、その都道府県の最大震度
    pub prefs: Vec<(String, Intensity)>,
}

/// 地図に出す範囲(経度・緯度の最小と最大)。震源と震度のあった都道府県を含め、
/// 狭すぎないよう最低 6 度四方にし、日本の範囲に収める。
fn map_bounds(view: &QuakeView) -> (f64, f64, f64, f64) {
    const JAPAN: (f64, f64, f64, f64) = (122.5, 146.5, 24.0, 46.0);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for p in prefs() {
        if view.prefs.iter().any(|(c, _)| *c == p.code) {
            pts.extend(p.rings.iter().flatten().copied());
        }
    }
    if let Some((lat, lon)) = view.epicenter {
        pts.push((lon, lat));
    }
    if pts.is_empty() {
        return JAPAN;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (x, y) in pts {
        (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
    }
    let pad = 1.0;
    (x0, x1, y0, y1) = (x0 - pad, x1 + pad, y0 - pad, y1 + pad);
    let min_span = 6.0;
    for (lo, hi) in [(&mut x0, &mut x1), (&mut y0, &mut y1)] {
        let span = *hi - *lo;
        if span < min_span {
            *lo -= (min_span - span) / 2.0;
            *hi += (min_span - span) / 2.0;
        }
    }
    (x0, x1, y0, y1)
}

/// 画面全体に地震アラートを重ねる。下の画面は暗くして透かす。
pub fn quake_overlay(c: &mut Canvas, t: &mut TextEngine, view: &QuakeView) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let k = scale(c.height());
    c.fill_rect(0.0, 0.0, w, h, rgba(4, 8, 14, 250));

    let max = view.max_intensity.unwrap_or(0);
    let accent = if max >= SHINDO_5_LOWER {
        [255, 80, 60]
    } else {
        [255, 190, 60]
    };
    // 上端の帯
    c.fill_rect(
        0.0,
        0.0,
        w,
        12.0 * k,
        rgba(accent[0], accent[1], accent[2], 255),
    );

    // 左: 地図(高さいっぱいの正方形に近い領域)
    let margin = 48.0 * k;
    let map_rect = (
        margin,
        margin,
        (h - 2.0 * margin).min(w * 0.5),
        h - 2.0 * margin,
    );
    draw_map(c, view, map_rect, k);

    // 右: 要点
    let x = map_rect.0 + map_rect.2 + 64.0 * k;
    let mut y = margin + 8.0 * k;
    let fg = [235, 240, 248, 255];
    let dim = [150, 165, 185, 255];
    let acc = [accent[0], accent[1], accent[2], 255];
    t.draw(c, &format!("地震  {}", view.title), x, y, 44.0 * k, acc);
    y += 84.0 * k;
    t.draw(c, "最大震度", x, y + 40.0 * k, 44.0 * k, dim);
    let label = view.max_intensity.map_or("-", intensity_label);
    let lx = x + t.measure("最大震度", 44.0 * k) + 32.0 * k;
    if let Some(i) = view.max_intensity {
        let [r, g, b] = intensity_color(i);
        let lw = t.measure(label, 120.0 * k) + 48.0 * k;
        c.fill_rect(lx, y, lw, 150.0 * k, rgba(r, g, b, 255));
        let ink = if matches!(i, 1 | 4 | 5) {
            [10, 10, 10, 255]
        } else {
            [255, 255, 255, 255]
        };
        t.draw(c, label, lx + 24.0 * k, y + 4.0 * k, 120.0 * k, ink);
    } else {
        t.draw(c, label, lx, y, 120.0 * k, fg);
    }
    y += 190.0 * k;
    let place = if view.place.is_empty() {
        "震源は調査中"
    } else {
        &view.place
    };
    t.draw(c, place, x, y, 64.0 * k, fg);
    y += 100.0 * k;
    let detail: Vec<&str> = [view.time.as_str(), &view.magnitude, &view.depth]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    t.draw(c, &detail.join("    "), x, y, 44.0 * k, fg);
    y += 90.0 * k;
    // 震度のあった都道府県(大きい順に最大 6 件)
    let mut by = view.prefs.clone();
    by.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (code, i) in by.iter().take(6) {
        let name = pref_name(code);
        let [r, g, b] = intensity_color(*i);
        c.fill_rect(x, y + 8.0 * k, 28.0 * k, 28.0 * k, rgba(r, g, b, 255));
        t.draw(
            c,
            &format!("{name}  震度{}", intensity_label(*i)),
            x + 44.0 * k,
            y,
            36.0 * k,
            fg,
        );
        y += 52.0 * k;
    }
    t.draw(
        c,
        "気象庁の情報    キーを押すと閉じます",
        x,
        h - margin - 36.0 * k,
        28.0 * k,
        dim,
    );
}

fn draw_map(c: &mut Canvas, view: &QuakeView, rect: (f32, f32, f32, f32), k: f32) {
    // 枠の外へはみ出さないよう、枠と同じ大きさのキャンバスに描いてから貼る
    let (mw, mh) = (rect.2.max(1.0) as u32, rect.3.max(1.0) as u32);
    let mut m = Canvas::new(mw, mh);
    m.clear(rgba(10, 22, 38, 255));
    draw_map_body(&mut m, view, k);
    c.draw_pixmap(&m.pixmap, rect.0 as i32, rect.1 as i32);
    c.stroke_rect(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        2.0 * k,
        rgba(70, 90, 115, 255),
    );
}

fn draw_map_body(c: &mut Canvas, view: &QuakeView, k: f32) {
    let rect = (0.0, 0.0, c.width() as f32, c.height() as f32);
    let proj = projector(map_bounds(view), rect);
    for p in prefs() {
        let rings: Vec<Vec<(f32, f32)>> = p
            .rings
            .iter()
            .map(|r| r.iter().map(|&(lon, lat)| proj(lon, lat)).collect())
            .collect();
        let fill = match view.prefs.iter().find(|(code, _)| *code == p.code) {
            Some((_, i)) => {
                let [r, g, b] = intensity_color(*i);
                rgba(r, g, b, 255)
            }
            None => rgba(44, 58, 76, 255),
        };
        c.fill_rings(&rings, fill);
        c.stroke_rings(&rings, 1.5 * k, rgba(110, 130, 150, 255));
    }
    if let Some((lat, lon)) = view.epicenter {
        let (x, y) = proj(lon, lat);
        // 震源は赤い ✕ と輪
        c.stroke_circle(x, y, 26.0 * k, 4.0 * k, rgba(255, 255, 255, 230));
        let d = 18.0 * k;
        c.polyline(
            &[(x - d, y - d), (x + d, y + d)],
            9.0 * k,
            rgba(255, 255, 255, 255),
        );
        c.polyline(
            &[(x - d, y + d), (x + d, y - d)],
            9.0 * k,
            rgba(255, 255, 255, 255),
        );
        c.polyline(
            &[(x - d, y - d), (x + d, y + d)],
            5.0 * k,
            rgba(230, 20, 20, 255),
        );
        c.polyline(
            &[(x - d, y + d), (x + d, y - d)],
            5.0 * k,
            rgba(230, 20, 20, 255),
        );
    }
}

/// 画面に重ねるアラート。新しいものが前のものを置き換える。
#[derive(Debug, Clone, PartialEq)]
pub enum AlertView {
    Quake(QuakeView),
    Disaster(DisasterView),
}

/// アラートを画面全体に重ねる。
pub fn overlay(c: &mut Canvas, t: &mut TextEngine, view: &AlertView) {
    match view {
        AlertView::Quake(v) => quake_overlay(c, t, v),
        AlertView::Disaster(v) => disaster_overlay(c, t, v),
    }
}

/// 災害アラートの 1 種類(例: 大雨特別警報)と、それが出ている市町村。
#[derive(Debug, Clone, PartialEq)]
pub struct DisasterKind {
    /// "大雨特別警報" など
    pub name: String,
    /// 警戒レベル(4 か 5)
    pub level: u8,
    pub areas: Vec<String>,
}

/// 地図に塗る市町村(気象庁の市町村の外接矩形)。
#[derive(Debug, Clone, PartialEq)]
pub struct DisasterArea {
    pub level: u8,
    /// 南西 (緯度, 経度)
    pub sw: (f64, f64),
    /// 北東 (緯度, 経度)
    pub ne: (f64, f64),
}

/// 災害アラートに出す内容。
#[derive(Debug, Clone, PartialEq)]
pub struct DisasterView {
    /// レベルの高い順
    pub kinds: Vec<DisasterKind>,
    pub areas: Vec<DisasterArea>,
    /// 自宅 (緯度, 経度)
    pub home: (f64, f64),
    /// 対象にしている半径(km)
    pub radius_km: f64,
    /// 発表時刻("10/13 21:30 発表" など)
    pub time: String,
}

/// 警戒レベルの色(気象庁: レベル 5 は黒、レベル 4 は紫)。暗い背景で見えるよう縁取りで補う。
fn level_color(level: u8) -> [u8; 3] {
    if level >= 5 {
        [12, 0, 12]
    } else {
        [170, 0, 170]
    }
}

/// 災害アラートを重ねる。左に自宅まわりの地図、右に種類と市町村。
pub fn disaster_overlay(c: &mut Canvas, t: &mut TextEngine, view: &DisasterView) {
    let (w, h) = (c.width() as f32, c.height() as f32);
    let k = scale(c.height());
    c.fill_rect(0.0, 0.0, w, h, rgba(4, 8, 14, 250));
    let top = view.kinds.iter().map(|d| d.level).max().unwrap_or(4);
    let accent = if top >= 5 {
        [235, 70, 235]
    } else {
        [200, 90, 220]
    };
    c.fill_rect(
        0.0,
        0.0,
        w,
        12.0 * k,
        rgba(accent[0], accent[1], accent[2], 255),
    );

    let margin = 48.0 * k;
    let map_rect = (
        margin,
        margin,
        (h - 2.0 * margin).min(w * 0.5),
        h - 2.0 * margin,
    );
    {
        let (mw, mh) = (map_rect.2.max(1.0) as u32, map_rect.3.max(1.0) as u32);
        let mut m = Canvas::new(mw, mh);
        m.clear(rgba(10, 22, 38, 255));
        draw_disaster_map(&mut m, view, k);
        c.draw_pixmap(&m.pixmap, map_rect.0 as i32, map_rect.1 as i32);
        c.stroke_rect(
            map_rect.0,
            map_rect.1,
            map_rect.2,
            map_rect.3,
            2.0 * k,
            rgba(70, 90, 115, 255),
        );
    }

    let x = map_rect.0 + map_rect.2 + 64.0 * k;
    let right = w - margin;
    let mut y = margin + 8.0 * k;
    let fg = [235, 240, 248, 255];
    let dim = [150, 165, 185, 255];
    let acc = [accent[0], accent[1], accent[2], 255];
    t.draw(
        c,
        &format!("災害  自宅から半径{}km", view.radius_km.round()),
        x,
        y,
        44.0 * k,
        acc,
    );
    y += 90.0 * k;
    let bottom = h - margin - 100.0 * k;
    for kind in &view.kinds {
        if y + 140.0 * k > bottom {
            break;
        }
        // レベルの札と種類の名前
        let badge = format!("レベル{}", kind.level);
        let bw = t.measure(&badge, 40.0 * k) + 32.0 * k;
        let [r, g, b] = level_color(kind.level);
        c.fill_rect(x, y + 6.0 * k, bw, 64.0 * k, rgba(r, g, b, 255));
        c.stroke_rect(
            x,
            y + 6.0 * k,
            bw,
            64.0 * k,
            2.0 * k,
            rgba(235, 240, 248, 255),
        );
        t.draw(
            c,
            &badge,
            x + 16.0 * k,
            y + 10.0 * k,
            40.0 * k,
            [255, 255, 255, 255],
        );
        // 長い名前は右端に収まるよう字を小さくする
        let nx = x + bw + 24.0 * k;
        let full = t.measure(&kind.name, 60.0 * k).max(1.0);
        let size = (60.0 * k * ((right - nx) / full).min(1.0)).max(28.0 * k);
        t.draw(c, &kind.name, nx, y + (60.0 * k - size) / 2.0, size, fg);
        y += 96.0 * k;
        // 市町村を折り返して並べる(入りきらなければ「ほか N」)
        let size = 34.0 * k;
        let line_h = 50.0 * k;
        let max_lines = (((bottom - y) / line_h).floor() as usize).clamp(1, 4);
        for line in wrap_names(t, &kind.areas, right - x, size, max_lines) {
            t.draw(c, &line, x, y, size, fg);
            y += line_h;
        }
        y += 28.0 * k;
    }
    if !view.time.is_empty() {
        t.draw(c, &view.time, x, bottom + 20.0 * k, 36.0 * k, fg);
    }
    t.draw(
        c,
        "気象庁の情報    キーを押すと閉じます",
        x,
        h - margin - 36.0 * k,
        28.0 * k,
        dim,
    );
}

/// 名前を「、」でつなぎ、幅に収まるよう最大 `max_lines` 行に折り返す。あふれた分は「ほか N」。
fn wrap_names(
    t: &mut TextEngine,
    names: &[String],
    width: f32,
    size: f32,
    max_lines: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new()];
    for (i, name) in names.iter().enumerate() {
        let rest = names.len() - i;
        let last_line = lines.len() >= max_lines;
        let Some(cur) = lines.last_mut() else {
            break;
        };
        let piece = if cur.is_empty() {
            name.clone()
        } else {
            format!("、{name}")
        };
        // 最後の行では「ほか N」が入る余地を残す
        let reserve = if last_line && rest > 1 {
            t.measure(&format!("  ほか{}", rest - 1), size)
        } else {
            0.0
        };
        if cur.is_empty() || t.measure(&format!("{cur}{piece}"), size) + reserve <= width {
            cur.push_str(&piece);
            continue;
        }
        if last_line {
            cur.push_str(&format!("  ほか{rest}"));
            return lines;
        }
        lines.push(name.clone());
    }
    lines
}

fn draw_disaster_map(c: &mut Canvas, view: &DisasterView, k: f32) {
    let rect = (0.0, 0.0, c.width() as f32, c.height() as f32);
    let (lat, lon) = view.home;
    // 半径の 1.6 倍を南北の半分にとる(円と周りの市町村が収まる)
    let half = (view.radius_km * 1.6 / 111.0).max(0.3);
    let half_lon = half / lat.to_radians().cos().max(0.2);
    let proj = projector(
        (lon - half_lon, lon + half_lon, lat - half, lat + half),
        rect,
    );
    let pref_rings: Vec<Vec<Vec<(f32, f32)>>> = prefs()
        .iter()
        .map(|p| {
            p.rings
                .iter()
                .map(|r| r.iter().map(|&(lon, lat)| proj(lon, lat)).collect())
                .collect()
        })
        .collect();
    for rings in &pref_rings {
        c.fill_rings(rings, rgba(44, 58, 76, 255));
    }
    // 市町村は外接矩形で示す(低いレベルを先に塗り、高いレベルを上に重ねる)
    let mut areas: Vec<&DisasterArea> = view.areas.iter().collect();
    areas.sort_by_key(|a| a.level);
    for a in areas {
        let (x0, y0) = proj(a.sw.1, a.ne.0);
        let (x1, y1) = proj(a.ne.1, a.sw.0);
        let [r, g, b] = level_color(a.level);
        c.fill_rect(x0, y0, x1 - x0, y1 - y0, rgba(r, g, b, 200));
        // レベル 5 の黒は背景に沈むので白で縁取る
        let (edge, ew) = if a.level >= 5 {
            (rgba(255, 255, 255, 255), 3.0 * k)
        } else {
            (rgba(235, 120, 235, 255), 1.5 * k)
        };
        c.stroke_rect(x0, y0, x1 - x0, y1 - y0, ew, edge);
    }
    for rings in &pref_rings {
        c.stroke_rings(rings, 1.5 * k, rgba(110, 130, 150, 255));
    }
    let (hx, hy) = proj(lon, lat);
    let (_, ry) = proj(lon, lat + view.radius_km / 111.0);
    c.stroke_circle(hx, hy, hy - ry, 2.0 * k, rgba(200, 215, 235, 200));
    c.fill_circle(hx, hy, 12.0 * k, rgba(255, 255, 255, 255));
    c.fill_circle(hx, hy, 7.0 * k, rgba(30, 140, 255, 255));
}

/// 都道府県コード(2 桁)から名前。
pub fn pref_name(code: &str) -> &'static str {
    const NAMES: [&str; 47] = [
        "北海道",
        "青森県",
        "岩手県",
        "宮城県",
        "秋田県",
        "山形県",
        "福島県",
        "茨城県",
        "栃木県",
        "群馬県",
        "埼玉県",
        "千葉県",
        "東京都",
        "神奈川県",
        "新潟県",
        "富山県",
        "石川県",
        "福井県",
        "山梨県",
        "長野県",
        "岐阜県",
        "静岡県",
        "愛知県",
        "三重県",
        "滋賀県",
        "京都府",
        "大阪府",
        "兵庫県",
        "奈良県",
        "和歌山県",
        "鳥取県",
        "島根県",
        "岡山県",
        "広島県",
        "山口県",
        "徳島県",
        "香川県",
        "愛媛県",
        "高知県",
        "福岡県",
        "佐賀県",
        "長崎県",
        "熊本県",
        "大分県",
        "宮崎県",
        "鹿児島県",
        "沖縄県",
    ];
    code.parse::<usize>()
        .ok()
        .and_then(|n| NAMES.get(n.wrapping_sub(1)))
        .copied()
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_scale_round_trips() {
        for (s, label) in [
            ("1", "1"),
            ("4", "4"),
            ("5-", "5弱"),
            ("5+", "5強"),
            ("6-", "6弱"),
            ("6+", "6強"),
            ("7", "7"),
        ] {
            assert_eq!(intensity_label(parse_intensity(s).unwrap()), label);
        }
        assert_eq!(parse_intensity(""), None);
        assert_eq!(parse_intensity("5"), None);
        assert!(parse_intensity("5-").unwrap() > parse_intensity("4").unwrap());
    }

    #[test]
    fn bundled_prefectures_cover_all_47() {
        let p = prefs();
        let mut codes: Vec<&str> = p.iter().map(|p| p.code.as_str()).collect();
        codes.dedup();
        assert_eq!(codes.len(), 47);
        assert_eq!(pref_name("01"), "北海道");
        assert_eq!(pref_name("12"), "千葉県");
        assert_eq!(pref_name("47"), "沖縄県");
        assert_eq!(pref_name("48"), "");
        assert_eq!(pref_name("x"), "");
    }

    #[test]
    fn map_bounds_include_epicenter_and_have_a_minimum_span() {
        let view = QuakeView {
            title: String::new(),
            max_intensity: Some(4),
            place: String::new(),
            time: String::new(),
            magnitude: String::new(),
            depth: String::new(),
            epicenter: Some((35.6, 140.1)),
            prefs: vec![],
        };
        let (x0, x1, y0, y1) = map_bounds(&view);
        assert!(x0 < 140.1 && 140.1 < x1 && y0 < 35.6 && 35.6 < y1);
        assert!(x1 - x0 >= 6.0 && y1 - y0 >= 6.0);
        let none = QuakeView {
            epicenter: None,
            ..view
        };
        assert_eq!(map_bounds(&none).0, 122.5, "何も無ければ日本全体");
    }
}
