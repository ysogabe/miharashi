//! 写真を「旅」と「いつもの暮らし」に分け、1 周ごとの再生順を決める(旧 GEOFRAME の表示と同じ考え方)。
//!
//! - 自宅から `radius_km`(既定 50km)以上離れた写真だけを撮影順に並べ、48 時間以上空いたら別の旅にする。
//! - 自宅の近くの写真は、季節ごと(春 3〜5 月・夏 6〜8 月・秋 9〜11 月・冬 12〜2 月)にまとめる。
//!   12 月はその年の冬(「2025年 冬」= 2025 年 12 月〜2026 年 2 月)。
//! - 自宅の位置が分からないときは、GEOFRAME の `trip`(48 時間区切り)をそのまま旅として使う。
//! - 1 周ごとにまとまりの順番を混ぜ、各まとまりから最大 `PER_GROUP` 枚を優先度の高い順に選んで、
//!   まとまりの中は撮影順に流す。5 枚以上のまとまりの前にはカードを挟む。
//! - 優先度: まだ出ていない写真を最優先にし、あとは出ていない周の数(上限なし)と、
//!   新しく取り込んだ写真・撮影日が今日に近い写真・お気に入りへの有界な加点。上限なしの項があるので、
//!   どの写真もいつか必ず選ばれる。
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use miharashi_render::geo;
use miharashi_render::trip_card::TripCard;
use std::collections::{BTreeMap, HashMap};

/// 1 周で 1 つのまとまりから出す最大枚数
pub const PER_GROUP: usize = 6;
/// これ以上の枚数のまとまりにカードを挟む
pub const CARD_MIN: usize = 5;
const TRIP_GAP_SECS: i64 = 48 * 3600;
const PRI_NEVER: f64 = 45.0;
const PRI_NEW_MAX: f64 = 12.0;
const NEW_HALFLIFE_D: f64 = 4.0;
const PRI_ANNIV_MAX: f64 = 3.0;
const ANNIV_WINDOW_D: f64 = 14.0;
const PRI_FAV: f64 = 2.0;
const PRI_JITTER: f64 = 4.0;

/// 写真 1 枚の、並べ方に使う情報。
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// 撮影日時("2026-09-22 10:31" など。空もある)
    pub time: String,
    pub pref: String,
    pub city: String,
    pub fav: bool,
    /// 取り込んだ日時(新着の加点)
    pub fetched_at: String,
    /// GEOFRAME の旅番号(自宅が分からないときに使う)
    pub trip: Option<i64>,
}

impl Meta {
    fn latlon(&self) -> Option<(f64, f64)> {
        Some((self.lat?, self.lon?))
    }
    /// 国内か(GEOFRAME の住所検索は国内だけなので、都道府県があれば国内)。
    fn domestic(&self) -> bool {
        !self.pref.is_empty()
    }
}

/// 表示対象か: 似た写真のまとまりでは推奨されたものだけ。まとまりに入らない写真とお気に入りは対象。
/// (完全に同じ写真の重複は GEOFRAME が一覧から除いている)
pub fn eligible(quality_kind: &str, quality_recommended: bool, fav: bool) -> bool {
    quality_kind != "similar" || quality_recommended || fav
}

fn parse_time(s: &str) -> Option<NaiveDateTime> {
    let s = s.get(..16).unwrap_or(s);
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(12, 0, 0)
        })
}

/// 季節 (年, 0=春 1=夏 2=秋 3=冬)。1〜2 月は前の年の冬。
pub fn season(d: NaiveDate) -> (i32, u8) {
    match d.month() {
        3..=5 => (d.year(), 0),
        6..=8 => (d.year(), 1),
        9..=11 => (d.year(), 2),
        12 => (d.year(), 3),
        _ => (d.year() - 1, 3),
    }
}

fn season_label((y, s): (i32, u8)) -> String {
    let name = ["春", "夏", "秋", "冬"][s as usize % 4];
    let months = ["3〜5月", "6〜8月", "9〜11月", "12〜2月"][s as usize % 4];
    format!("{y}年 {name}({months})")
}

/// まとまり(旅か、ある季節のいつもの暮らし)。
#[derive(Debug, Clone)]
pub struct Group {
    pub everyday: bool,
    /// 撮影順の写真(Meta の添字)
    pub members: Vec<usize>,
    pub card: TripCard,
}

/// 撮影地点の地名(国内は市区町村、海外は都市か国)。
fn place_of(m: &Meta, overseas: &HashMap<usize, (String, String)>, i: usize) -> String {
    if m.domestic() {
        return if m.city.is_empty() {
            m.pref.clone()
        } else {
            m.city.clone()
        };
    }
    overseas
        .get(&i)
        .map(|(country, city)| {
            if city.is_empty() {
                country.clone()
            } else {
                city.clone()
            }
        })
        .unwrap_or_default()
}

/// 多い順の上位 n 件(同数は先に出たもの)。
fn top(names: impl Iterator<Item = String>, n: usize) -> Vec<String> {
    let mut count: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (i, s) in names.filter(|s| !s.is_empty()).enumerate() {
        let e = count.entry(s).or_insert((0, i));
        e.0 += 1;
    }
    let mut v: Vec<(String, (usize, usize))> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    v.into_iter().take(n).map(|(s, _)| s).collect()
}

fn period(first: Option<NaiveDate>, last: Option<NaiveDate>) -> String {
    match (first, last) {
        (Some(a), Some(b)) if a == b => a.format("%Y.%m.%d").to_string(),
        (Some(a), Some(b)) if a.year() == b.year() => {
            format!("{} 〜 {}", a.format("%Y.%m.%d"), b.format("%m.%d"))
        }
        (Some(a), Some(b)) => format!("{} 〜 {}", a.format("%Y.%m.%d"), b.format("%Y.%m.%d")),
        _ => String::new(),
    }
}

/// 海外の写真の (国, 都市) を引く(国内の写真は引かない)。
pub fn overseas_places(metas: &[Meta]) -> HashMap<usize, (String, String)> {
    metas
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.domestic())
        .filter_map(|(i, m)| {
            let (lat, lon) = m.latlon()?;
            let country = geo::country_at(lat, lon).unwrap_or("");
            // 住所の分からなかった国内の写真(都市の表は日本を含まない)
            if country == "日本" {
                return None;
            }
            let city = geo::city_near(lat, lon, 40.0).unwrap_or("").to_string();
            Some((i, (country.to_string(), city)))
        })
        .collect()
}

/// 写真をまとまりに分ける。`metas` の順番は問わない(中で撮影順に並べる)。
pub fn build_groups(
    metas: &[Meta],
    overseas: &HashMap<usize, (String, String)>,
    home: Option<(f64, f64)>,
    radius_km: f64,
) -> Vec<Group> {
    let times: Vec<Option<NaiveDateTime>> = metas.iter().map(|m| parse_time(&m.time)).collect();
    let mut order: Vec<usize> = (0..metas.len()).collect();
    order.sort_by_key(|&i| (times[i].is_none(), times[i]));
    let mut trips: Vec<Vec<usize>> = Vec::new();
    let mut seasons: BTreeMap<(i32, u8), Vec<usize>> = BTreeMap::new();
    let mut undated: Vec<usize> = Vec::new();
    match home {
        Some(h) => {
            let mut last: Option<NaiveDateTime> = None;
            for &i in &order {
                let near = metas[i]
                    .latlon()
                    .is_none_or(|p| geo::distance_km(h, p) < radius_km);
                match (near, times[i]) {
                    (_, None) => undated.push(i),
                    (true, Some(t)) => seasons.entry(season(t.date())).or_default().push(i),
                    (false, Some(t)) => {
                        // 間の自宅近くの写真は数えない(遠い写真どうしの間隔で区切る)
                        if last.is_none_or(|l| (t - l).num_seconds() > TRIP_GAP_SECS) {
                            trips.push(Vec::new());
                        }
                        if let Some(g) = trips.last_mut() {
                            g.push(i);
                        }
                        last = Some(t);
                    }
                }
            }
        }
        None => {
            // 自宅が分からない: GEOFRAME の旅番号でまとめる
            let mut by_trip: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
            for &i in &order {
                match (metas[i].trip, times[i]) {
                    (Some(t), Some(_)) => by_trip.entry(t).or_default().push(i),
                    _ => undated.push(i),
                }
            }
            trips = by_trip.into_values().collect();
        }
    }
    let mut groups = Vec::new();
    let card = |members: &[usize], everyday: bool, title: String| -> TripCard {
        let dates: Vec<NaiveDate> = members
            .iter()
            .filter_map(|&i| times[i].map(|t| t.date()))
            .collect();
        let places = top(members.iter().map(|&i| place_of(&metas[i], overseas, i)), 4);
        TripCard {
            everyday,
            title,
            period: period(dates.first().copied(), dates.last().copied()),
            count: members.len(),
            places,
            points: members.iter().filter_map(|&i| metas[i].latlon()).collect(),
            home,
            radius_km,
        }
    };
    for members in trips {
        let overseas_n = members
            .iter()
            .filter(|&&i| overseas.contains_key(&i))
            .count();
        let name = if overseas_n * 2 > members.len() {
            // 海外が多い旅は国名。2 番目の国は写真の 4 分の 1 以上あるときだけ(乗り継ぎは除く)
            let countries: Vec<String> = members
                .iter()
                .filter_map(|&i| overseas.get(&i).map(|p| p.0.clone()))
                .collect();
            let mut names = top(countries.iter().cloned(), 2);
            if let Some(second) = names.get(1)
                && countries.iter().filter(|c| *c == second).count() * 4 < members.len()
            {
                names.truncate(1);
            }
            names.join("・")
        } else {
            top(
                members
                    .iter()
                    .filter(|&&i| metas[i].domestic())
                    .map(|&i| place_of(&metas[i], overseas, i)),
                1,
            )
            .join("")
        };
        let title = if name.is_empty() {
            "旅".to_string()
        } else {
            format!("{name}の旅")
        };
        let mut c = card(&members, false, title);
        // 旅の地名は、題名の地名を除いた上位 3 件
        c.places.retain(|p| !c.title.starts_with(p.as_str()));
        c.places.truncate(3);
        groups.push(Group {
            everyday: false,
            card: c,
            members,
        });
    }
    for (key, members) in seasons {
        let c = card(&members, true, season_label(key));
        groups.push(Group {
            everyday: true,
            card: c,
            members,
        });
    }
    if !undated.is_empty() {
        let c = card(&undated, true, "撮影日の分からない写真".into());
        groups.push(Group {
            everyday: true,
            card: c,
            members: undated,
        });
    }
    groups
}

/// カードの見本(`png` で一覧に旅が無いとき)。
pub fn demo_card() -> TripCard {
    TripCard {
        everyday: false,
        title: "ウズベキスタンの旅".into(),
        period: "2025.06.01 〜 06.09".into(),
        count: 453,
        places: vec![
            "サマルカンド".into(),
            "ブハラ".into(),
            "タシュケント".into(),
        ],
        points: vec![
            (41.31, 69.28),
            (39.65, 66.96),
            (39.77, 64.42),
            (41.38, 60.36),
        ],
        home: Some((35.856, 139.903)),
        radius_km: 50.0,
    }
}

/// 再生の 1 コマ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// まとまりのカード
    Card(usize),
    /// 写真(Meta の添字)と、この周での番号 (k, n)
    Photo { entry: usize, k: usize, n: usize },
}

/// 1 周ずつ再生順を作る。表示した写真の周を覚え、長く出ていない写真を優先する。
pub struct Scheduler {
    shown: HashMap<usize, u64>,
    round: u64,
    rng: u64,
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// 撮影月日と今日の近さ(0〜1。年をまたいで環状に数える)。
fn anniv_weight(taken: Option<NaiveDate>, today: NaiveDate) -> f64 {
    let Some(t) = taken else { return 0.0 };
    let doy = |d: NaiveDate| d.with_year(2001).map_or(d.ordinal0(), |x| x.ordinal0()) as f64;
    let mut diff = (doy(t) - doy(today)).abs();
    if diff > 182.0 {
        diff = 365.0 - diff;
    }
    if diff >= ANNIV_WINDOW_D {
        0.0
    } else {
        1.0 - diff / ANNIV_WINDOW_D
    }
}

impl Scheduler {
    pub fn new(seed: u64) -> Self {
        Self {
            shown: HashMap::new(),
            round: 0,
            rng: seed | 1,
        }
    }

    fn rand(&mut self) -> f64 {
        (xorshift(&mut self.rng) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 表示した。
    pub fn mark(&mut self, entry: usize) {
        self.shown.insert(entry, self.round);
    }

    fn priority(&mut self, m: &Meta, entry: usize, now: NaiveDateTime) -> f64 {
        let mut s = match self.shown.get(&entry) {
            None => PRI_NEVER,
            Some(r) => (self.round - r) as f64,
        };
        if let Some(f) = parse_time(&m.fetched_at) {
            let days = (now - f).num_seconds() as f64 / 86400.0;
            if days >= 0.0 {
                s += PRI_NEW_MAX * 0.5f64.powf(days / NEW_HALFLIFE_D);
            }
        }
        s += PRI_ANNIV_MAX * anniv_weight(parse_time(&m.time).map(|t| t.date()), now.date());
        if m.fav {
            s += PRI_FAV;
        }
        s + self.rand() * PRI_JITTER
    }

    /// 次の 1 周。まとまりの順番を混ぜ、各まとまりから最大 PER_GROUP 枚を撮影順に並べる。
    pub fn next_round(
        &mut self,
        groups: &[Group],
        metas: &[Meta],
        now: NaiveDateTime,
    ) -> Vec<Slot> {
        self.round += 1;
        let mut gi: Vec<usize> = (0..groups.len()).collect();
        for i in (1..gi.len()).rev() {
            let j = (xorshift(&mut self.rng) % (i as u64 + 1)) as usize;
            gi.swap(i, j);
        }
        let mut picks: Vec<(usize, Vec<usize>)> = Vec::new();
        for g in gi {
            let members = &groups[g].members;
            let mut scored: Vec<(f64, usize)> = members
                .iter()
                .enumerate()
                .map(|(pos, &e)| (self.priority(&metas[e], e, now), pos))
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut chosen: Vec<usize> = scored.iter().take(PER_GROUP).map(|s| s.1).collect();
            chosen.sort_unstable();
            picks.push((g, chosen.into_iter().map(|pos| members[pos]).collect()));
        }
        let n = picks.iter().map(|p| p.1.len()).sum();
        let mut out = Vec::new();
        let mut k = 0;
        for (g, entries) in picks {
            if groups[g].members.len() >= CARD_MIN {
                out.push(Slot::Card(g));
            }
            for entry in entries {
                k += 1;
                out.push(Slot::Photo { entry, k, n });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: (f64, f64) = (35.856, 139.903);

    fn meta(lat: f64, lon: f64, time: &str, pref: &str, city: &str) -> Meta {
        Meta {
            lat: Some(lat),
            lon: Some(lon),
            time: time.into(),
            pref: pref.into(),
            city: city.into(),
            ..Default::default()
        }
    }

    /// 自宅から真北に km 離れた地点
    fn north(km: f64) -> (f64, f64) {
        (HOME.0 + km / 111.195, HOME.1)
    }

    #[test]
    fn eligibility_table() {
        assert!(eligible("", false, false), "まとまりに入らない写真");
        assert!(eligible("exact", true, false));
        assert!(eligible("similar", true, false), "似た写真の推奨");
        assert!(!eligible("similar", false, false), "似た写真の残り");
        assert!(eligible("similar", false, true), "お気に入りは常に");
    }

    #[test]
    fn seasons_put_december_with_the_next_winter() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(season(d(2025, 12, 31)), (2025, 3));
        assert_eq!(season(d(2026, 1, 1)), (2025, 3));
        assert_eq!(season(d(2026, 2, 28)), (2025, 3));
        assert_eq!(season(d(2026, 3, 1)), (2026, 0));
        assert_eq!(season(d(2026, 8, 31)), (2026, 1));
        assert_eq!(season(d(2026, 11, 30)), (2026, 2));
        assert_eq!(season_label((2025, 3)), "2025年 冬(12〜2月)");
    }

    #[test]
    fn radius_and_gap_split_trips_and_everyday() {
        let (a, b) = (north(49.9), north(50.1));
        let far = north(120.0);
        let metas = vec![
            meta(a.0, a.1, "2026-09-20 10:00", "千葉県", "近所"),
            meta(b.0, b.1, "2026-09-21 10:00", "茨城県", "水戸市"),
            // 間に自宅近くの写真があっても旅は切れない
            meta(HOME.0, HOME.1, "2026-09-22 09:00", "千葉県", "流山市"),
            meta(far.0, far.1, "2026-09-22 20:00", "茨城県", "水戸市"),
            // 48 時間を超えて空いたら別の旅
            meta(far.0, far.1, "2026-09-25 21:00", "栃木県", "宇都宮市"),
            meta(HOME.0, HOME.1, "2026-12-05 09:00", "千葉県", "流山市"),
            meta(HOME.0, HOME.1, "2027-01-05 09:00", "千葉県", "流山市"),
            meta(HOME.0, HOME.1, "", "千葉県", "流山市"),
        ];
        let g = build_groups(&metas, &HashMap::new(), Some(HOME), 50.0);
        let trips: Vec<&Vec<usize>> = g
            .iter()
            .filter(|g| !g.everyday)
            .map(|g| &g.members)
            .collect();
        assert_eq!(trips, [&vec![1, 3], &vec![4]]);
        let everyday: Vec<(&str, &Vec<usize>)> = g
            .iter()
            .filter(|g| g.everyday)
            .map(|g| (g.card.title.as_str(), &g.members))
            .collect();
        assert_eq!(
            everyday,
            [
                ("2026年 秋(9〜11月)", &vec![0, 2]),
                ("2026年 冬(12〜2月)", &vec![5, 6]),
                ("撮影日の分からない写真", &vec![7]),
            ]
        );
        assert_eq!(g[0].card.title, "水戸市の旅");
        assert_eq!(g[0].card.period, "2026.09.21 〜 09.22");
        assert_eq!(g[0].card.count, 2);
    }

    #[test]
    fn overseas_trip_is_named_by_country_and_cities() {
        let metas = vec![
            meta(39.65, 66.96, "2025-06-01 10:00", "", ""),
            meta(39.77, 64.42, "2025-06-03 10:00", "", ""),
            meta(39.66, 66.95, "2025-06-04 10:00", "", ""),
        ];
        let ov = overseas_places(&metas);
        let g = build_groups(&metas, &ov, Some(HOME), 50.0);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].card.title, "ウズベキスタンの旅");
        assert_eq!(g[0].card.places, ["サマルカンド", "ブハラ"]);
    }

    #[test]
    fn domestic_photos_without_address_are_not_overseas() {
        let metas = vec![
            meta(26.21, 127.68, "2007-06-16 10:00", "", ""),
            meta(26.21, 127.68, "2007-06-16 11:00", "", ""),
            meta(26.21, 127.68, "2007-06-16 12:00", "沖縄県", "那覇市"),
        ];
        let ov = overseas_places(&metas);
        assert!(ov.is_empty());
        let g = build_groups(&metas, &ov, Some(HOME), 50.0);
        assert_eq!(g[0].card.title, "那覇市の旅");
        assert!(g[0].card.places.is_empty());
    }

    #[test]
    fn without_home_the_server_trip_numbers_are_used() {
        let mut metas = vec![
            meta(HOME.0, HOME.1, "2026-01-01 10:00", "千葉県", "流山市"),
            meta(HOME.0, HOME.1, "2026-03-01 10:00", "千葉県", "流山市"),
        ];
        metas[0].trip = Some(7);
        metas[1].trip = Some(8);
        let g = build_groups(&metas, &HashMap::new(), None, 50.0);
        assert_eq!(g.len(), 2);
        assert!(g.iter().all(|g| !g.everyday));
    }

    fn groups_of(sizes: &[usize]) -> (Vec<Group>, Vec<Meta>) {
        let mut metas = Vec::new();
        let mut groups = Vec::new();
        for (gi, &n) in sizes.iter().enumerate() {
            let members: Vec<usize> = (metas.len()..metas.len() + n).collect();
            for j in 0..n {
                metas.push(meta(
                    HOME.0,
                    HOME.1,
                    &format!("20{:02}-05-01 {:02}:00", 10 + gi, j % 24),
                    "千葉県",
                    "流山市",
                ));
            }
            groups.push(Group {
                everyday: true,
                members,
                card: TripCard {
                    everyday: true,
                    title: String::new(),
                    period: String::new(),
                    count: n,
                    places: vec![],
                    points: vec![],
                    home: None,
                    radius_km: 50.0,
                },
            });
        }
        (groups, metas)
    }

    #[test]
    fn rounds_take_up_to_six_per_group_in_shooting_order_and_cover_everything() {
        let (groups, metas) = groups_of(&[20, 3, 1, 6]);
        let now = parse_time("2026-10-13 22:00").unwrap();
        let mut s = Scheduler::new(42);
        let mut seen = std::collections::HashSet::new();
        // 20 枚のまとまりは ceil(20/6) = 4 周で必ず一巡する
        for _ in 0..4 {
            let round = s.next_round(&groups, &metas, now);
            let cards: Vec<usize> = round
                .iter()
                .filter_map(|x| match x {
                    Slot::Card(g) => Some(*g),
                    _ => None,
                })
                .collect();
            let mut sorted = cards.clone();
            sorted.sort();
            assert_eq!(sorted, [0, 3], "カードは 5 枚以上のまとまりだけ");
            let photos: Vec<usize> = round
                .iter()
                .filter_map(|x| match x {
                    Slot::Photo { entry, .. } => Some(*entry),
                    _ => None,
                })
                .collect();
            assert_eq!(photos.len(), 6 + 3 + 1 + 6);
            for g in &groups {
                let mine: Vec<usize> = photos
                    .iter()
                    .copied()
                    .filter(|e| g.members.contains(e))
                    .collect();
                assert!(mine.len() <= PER_GROUP);
                assert!(mine.windows(2).all(|w| w[0] < w[1]), "まとまりの中は撮影順");
            }
            if let Some(Slot::Photo { k, n, .. }) = round.last() {
                assert_eq!(k, n);
            }
            for e in photos {
                s.mark(e);
                seen.insert(e);
            }
        }
        assert_eq!(seen.len(), metas.len(), "4 周ですべて出る");
    }

    #[test]
    fn anniversary_weight_wraps_the_year() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day);
        let w = anniv_weight(d(2020, 12, 30), d(2026, 1, 2).unwrap());
        assert!(w > 0.7 && w < 0.85, "{w}");
        assert_eq!(anniv_weight(d(2020, 6, 1), d(2026, 1, 2).unwrap()), 0.0);
        assert_eq!(anniv_weight(None, d(2026, 1, 2).unwrap()), 0.0);
    }
}
