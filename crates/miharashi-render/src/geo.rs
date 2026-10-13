//! 地図の共通部品。同梱の都道府県・国境・都市のデータ、経度緯度から画面への投影、
//! 撮影地点から国名・都市名を引く処理。データは最初に使うときに 1 回だけ読む。
use std::collections::HashMap;
use std::sync::OnceLock;

/// 都道府県か国(コードと、経度・緯度のリング)。
pub struct Region {
    pub code: String,
    pub rings: Vec<Vec<(f64, f64)>>,
    /// 外接矩形 (経度最小, 経度最大, 緯度最小, 緯度最大)
    pub bbox: (f64, f64, f64, f64),
}

/// 都市(日本以外)。
pub struct City {
    pub lon: f64,
    pub lat: f64,
    pub pop: u64,
    pub name: String,
}

/// 同梱の都道府県ポリゴン。
pub fn prefs() -> &'static [Region] {
    static PREFS: OnceLock<Vec<Region>> = OnceLock::new();
    PREFS.get_or_init(|| parse_regions(include_str!("../assets/japan_prefs.txt")))
}

/// 同梱の国境ポリゴン。
pub fn countries() -> &'static [Region] {
    static C: OnceLock<Vec<Region>> = OnceLock::new();
    C.get_or_init(|| parse_regions(include_str!("../assets/world_countries.txt")))
}

/// 国コード → 日本語名。
fn country_names() -> &'static HashMap<String, String> {
    static N: OnceLock<HashMap<String, String>> = OnceLock::new();
    N.get_or_init(|| {
        include_str!("../assets/world_countries.txt")
            .lines()
            .filter_map(|l| l.strip_prefix('@')?.split_once(' '))
            .map(|(c, n)| (c.to_string(), n.to_string()))
            .collect()
    })
}

pub fn cities() -> &'static [City] {
    static C: OnceLock<Vec<City>> = OnceLock::new();
    C.get_or_init(|| {
        include_str!("../assets/world_cities.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut it = l.splitn(3, ' ');
                let (lon, lat) = it.next()?.split_once(',')?;
                Some(City {
                    lon: lon.parse().ok()?,
                    lat: lat.parse().ok()?,
                    pop: it.next()?.parse().ok()?,
                    name: it.next()?.to_string(),
                })
            })
            .collect()
    })
}

/// 「<コード> <経度>,<緯度> ...」の行(1 行 = 1 リング)を読む。同じコードの行はまとめる。
fn parse_regions(text: &str) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('@'))
    {
        let mut it = line.split(' ');
        let Some(code) = it.next() else { continue };
        let ring: Vec<(f64, f64)> = it
            .filter_map(|p| {
                let (lon, lat) = p.split_once(',')?;
                Some((lon.parse().ok()?, lat.parse().ok()?))
            })
            .collect();
        if ring.len() < 3 {
            continue;
        }
        let b = bbox_of(&ring);
        match out.last_mut() {
            Some(p) if p.code == code => {
                p.bbox = union(p.bbox, b);
                p.rings.push(ring);
            }
            _ => out.push(Region {
                code: code.to_string(),
                rings: vec![ring],
                bbox: b,
            }),
        }
    }
    out
}

fn bbox_of(ring: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    ring.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(x0, x1, y0, y1), &(x, y)| (x0.min(x), x1.max(x), y0.min(y), y1.max(y)),
    )
}

fn union(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    (a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3))
}

/// 外接矩形が `view`(経度最小, 経度最大, 緯度最小, 緯度最大)と重なるか。
pub fn overlaps(b: (f64, f64, f64, f64), view: (f64, f64, f64, f64)) -> bool {
    b.0 <= view.1 && b.1 >= view.0 && b.2 <= view.3 && b.3 >= view.2
}

/// 点がリング群の内側か(偶奇規則。穴も扱える)。
fn inside(rings: &[Vec<(f64, f64)>], lon: f64, lat: f64) -> bool {
    let mut odd = false;
    for ring in rings {
        let n = ring.len();
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = ring[i];
            let (xj, yj) = ring[j];
            if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
                odd = !odd;
            }
            j = i;
        }
    }
    odd
}

/// 撮影地点の国(日本語名)。海の上など、どの国にも入らなければ最も近い国境の国。
pub fn country_at(lat: f64, lon: f64) -> Option<&'static str> {
    let names = country_names();
    let hit = countries()
        .iter()
        .find(|c| overlaps(c.bbox, (lon, lon, lat, lat)) && inside(&c.rings, lon, lat))
        .or_else(|| {
            // 簡略化で海岸線がずれた地点(港・島)は、国境の頂点が 0.5 度以内で最も近い国にする
            countries()
                .iter()
                .filter(|c| overlaps(c.bbox, (lon - 1.0, lon + 1.0, lat - 1.0, lat + 1.0)))
                .map(|c| (nearest_vertex(c, lon, lat), c))
                .filter(|(d, _)| *d <= 0.25)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, c)| c)
        })?;
    names.get(&hit.code).map(String::as_str)
}

fn nearest_vertex(r: &Region, lon: f64, lat: f64) -> f64 {
    r.rings
        .iter()
        .flatten()
        .map(|&(x, y)| (x - lon).powi(2) + (y - lat).powi(2))
        .fold(f64::MAX, f64::min)
}

/// 2 点間の距離(km)。
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1, la2, lo2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().min(1.0).asin()
}

/// 撮影地点に近い都市(日本以外)。`max_km` 以内で、人口が多いほど遠くても選ばれやすくする。
pub fn city_near(lat: f64, lon: f64, max_km: f64) -> Option<&'static str> {
    let deg = max_km / 111.0;
    cities()
        .iter()
        .filter(|c| (c.lat - lat).abs() <= deg && (c.lon - lon).abs() <= deg * 3.0)
        .filter_map(|c| {
            let d = distance_km((lat, lon), (c.lat, c.lon));
            // 人口 10 倍ごとに 5km 近いものとみなす
            (d <= max_km).then(|| (d - 5.0 * (c.pop.max(1) as f64).log10(), c))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c.name.as_str())
}

/// 経度・緯度を地図の矩形 (x, y, w, h) へ写す関数を返す(正距円筒図法に緯度の cos で横を縮める)。
/// `b` は (経度最小, 経度最大, 緯度最小, 緯度最大)。
pub fn projector(
    b: (f64, f64, f64, f64),
    rect: (f32, f32, f32, f32),
) -> impl Fn(f64, f64) -> (f32, f32) {
    let (x0, x1, y0, y1) = b;
    let kx = ((y0 + y1) / 2.0).to_radians().cos();
    let (gw, gh) = ((x1 - x0) * kx, y1 - y0);
    let (rx, ry, rw, rh) = rect;
    let s = (rw as f64 / gw).min(rh as f64 / gh);
    let ox = rx as f64 + (rw as f64 - gw * s) / 2.0;
    let oy = ry as f64 + (rh as f64 - gh * s) / 2.0;
    move |lon, lat| {
        (
            (ox + (lon - x0) * kx * s) as f32,
            (oy + (y1 - lat) * s) as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries_by_point() {
        assert_eq!(
            country_at(39.65, 66.96),
            Some("ウズベキスタン"),
            "サマルカンド"
        );
        assert_eq!(country_at(25.03, 121.56), Some("台湾"), "台北");
        assert_eq!(country_at(47.6, -122.33), Some("アメリカ"), "シアトル");
        assert_eq!(country_at(11.56, 104.92), Some("カンボジア"), "プノンペン");
        assert_eq!(
            country_at(-45.03, 168.66),
            Some("ニュージーランド"),
            "クイーンズタウン"
        );
        assert_eq!(country_at(35.68, 139.76), Some("日本"));
        assert_eq!(country_at(0.0, -150.0), None, "太平洋の真ん中");
    }

    #[test]
    fn cities_by_point() {
        assert_eq!(city_near(39.65, 66.96, 40.0), Some("サマルカンド"));
        assert_eq!(city_near(47.61, -122.33, 40.0), Some("シアトル"));
        assert_eq!(city_near(0.0, -150.0, 40.0), None);
    }

    #[test]
    fn bundled_data_loads() {
        assert_eq!(prefs().len(), 47);
        assert!(countries().len() > 200);
        assert!(cities().len() > 5000);
        assert!(prefs().iter().all(|p| p.bbox.0 > 120.0 && p.bbox.1 < 155.0));
    }
}
