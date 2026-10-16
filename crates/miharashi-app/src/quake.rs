//! 地震アラート。気象庁の地震情報の一覧(bosai/quake/data/list.json)を別スレッドで定期的に読み、
//! 条件を満たす発表を画面ロジックへ送る。条件は次のどちらか。
//! - 住んでいる地方(気象庁の地方区分。例: 関東甲信地方)のどこかで震度 4 以上
//! - 全国のどこかで震度 5 弱以上
//!
//! 住んでいる地方は、Home Assistant の自宅の位置 → 国土地理院の逆ジオコーダ(市区町村コード)→
//! 気象庁の地域定義(area.json)の順にたどって決める(逆ジオコーダーに届かなければ、`home` が覚えている前回の
//! 市区町村コード)。どこかで失敗したら全国の条件だけで動く。
//! ネットワークの応答はすべて欠けうるものとして扱い、読めない項目は飛ばす(パニックさせない)。
use crate::config::{Alerts, HomeAssistant};
use miharashi_render::alert::{
    AlertView, Intensity, QuakeView, SHINDO_4, SHINDO_5_LOWER, parse_intensity,
};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const LIST_URL: &str = "https://www.jma.go.jp/bosai/quake/data/list.json";
pub const AREA_URL: &str = "https://www.jma.go.jp/bosai/common/const/area.json";
/// 起動時に、これより古い発表はアラートにしない(再起動のたびに出さない)
const STARTUP_MAX_AGE_MIN: i64 = 10;
/// 地方の判定をやり直す間隔(成功時)と、失敗時の再試行間隔
const REGION_REFRESH: Duration = Duration::from_secs(24 * 3600);
const REGION_RETRY: Duration = Duration::from_secs(3600);
/// 一覧の先頭から見る件数(新しい順に並んでいる)
const LIST_SCAN: usize = 40;

/// 住んでいる地方。
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub name: String,
    /// 都道府県コード(2 桁)
    pub prefs: HashSet<String>,
}

/// 一覧の 1 件(1 回の発表)。
#[derive(Debug, Clone, PartialEq)]
struct Report {
    /// 発表を一意にする鍵(発表時刻 + 地震の ID)
    key: String,
    /// 発表時刻(YYYYMMDDhhmmss)
    ctt: String,
    max: Option<Intensity>,
    view: QuakeView,
}

/// "+36.5+140.6-60000/" → (緯度, 経度, 深さ[m])。深さが無い形("+07.5-080.8/")もある。
fn parse_cod(cod: &str) -> Option<(f64, f64, Option<f64>)> {
    let body = cod.trim_end_matches('/');
    let mut nums = Vec::new();
    let mut start = None;
    for (i, ch) in body.char_indices() {
        if ch == '+' || ch == '-' {
            if let Some(s) = start {
                nums.push(body[s..i].parse::<f64>().ok()?);
            }
            start = Some(i);
        }
    }
    nums.push(body[start?..].parse::<f64>().ok()?);
    match nums[..] {
        [lat, lon] => Some((lat, lon, None)),
        [lat, lon, depth] => Some((lat, lon, Some(depth))),
        _ => None,
    }
}

fn depth_label(depth_m: Option<f64>) -> String {
    match depth_m {
        Some(d) if d.abs() < 1.0 => "ごく浅い".into(),
        Some(d) => format!("深さ {}km", (d.abs() / 1000.0).round()),
        None => String::new(),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 一覧の 1 件を読む。震度の情報を含まない発表(震源だけ・遠地地震など)は None。
fn parse_report(v: &Value) -> Option<Report> {
    let ttl = str_of(v, "ttl");
    if !matches!(ttl, "震度速報" | "震源・震度情報") {
        return None;
    }
    let eid = str_of(v, "eid");
    let ctt = str_of(v, "ctt");
    if eid.is_empty() || ctt.is_empty() {
        return None;
    }
    let prefs: Vec<(String, Intensity)> = v
        .get("int")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let code = str_of(p, "code");
                    let i = parse_intensity(str_of(p, "maxi"))?;
                    (code.len() == 2).then(|| (code.to_string(), i))
                })
                .collect()
        })
        .unwrap_or_default();
    let max = parse_intensity(str_of(v, "maxi")).or_else(|| prefs.iter().map(|p| p.1).max());
    let cod = parse_cod(str_of(v, "cod"));
    let at = str_of(v, "at");
    // "2026-10-13T20:53:00+09:00" → "10/13 20:53"
    let time = match (at.get(5..7), at.get(8..10), at.get(11..16)) {
        (Some(m), Some(d), Some(hm)) => format!("{m}/{d} {hm} ごろ"),
        _ => String::new(),
    };
    let mag = str_of(v, "mag");
    Some(Report {
        key: format!("{ctt}/{eid}"),
        ctt: ctt.to_string(),
        max,
        view: QuakeView {
            title: ttl.to_string(),
            max_intensity: max,
            place: str_of(v, "anm").to_string(),
            time,
            magnitude: if mag.is_empty() || mag == "Ｍ不明" {
                String::new()
            } else {
                format!("M{mag}")
            },
            depth: depth_label(cod.and_then(|c| c.2)),
            epicenter: cod.map(|c| (c.0, c.1)),
            prefs,
        },
    })
}

/// アラートを出す発表か。
fn qualifies(r: &Report, region: Option<&Region>) -> bool {
    if r.max.is_some_and(|m| m >= SHINDO_5_LOWER) {
        return true;
    }
    region.is_some_and(|reg| {
        r.view
            .prefs
            .iter()
            .any(|(code, i)| *i >= SHINDO_4 && reg.prefs.contains(code))
    })
}

/// 一覧(新しい順)のうち、まだ見ていない発表を古い順に返し、`seen` を今回の一覧に合わせて更新する。
/// `min_ctt` より古い発表は見たことにする(起動直後の古い発表を出さないため)。
fn fresh_reports(list: &Value, seen: &mut HashSet<String>, min_ctt: Option<&str>) -> Vec<Report> {
    let Some(items) = list.as_array() else {
        return Vec::new();
    };
    let reports: Vec<Report> = items
        .iter()
        .take(LIST_SCAN)
        .filter_map(parse_report)
        .collect();
    let mut fresh: Vec<Report> = reports
        .iter()
        .filter(|r| !seen.contains(&r.key))
        .filter(|r| min_ctt.is_none_or(|m| r.ctt.as_str() >= m))
        .cloned()
        .collect();
    fresh.reverse();
    // 一覧から消えた古い鍵は捨てる(集合が増え続けないように)
    *seen = reports.into_iter().map(|r| r.key).collect();
    fresh
}

/// 気象庁の地域定義から、都道府県コードを含む地方を探す。
fn region_from_area(area: &Value, pref: &str) -> Option<Region> {
    let offices = area.get("offices")?.as_object()?;
    let center_code = offices
        .iter()
        .find(|(code, _)| code.starts_with(pref))
        .and_then(|(_, o)| o.get("parent")?.as_str())?;
    let center = area.get("centers")?.get(center_code)?;
    let prefs = center
        .get("children")?
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str()?.get(0..2).map(str::to_string))
        .collect();
    Some(Region {
        name: str_of(center, "name").to_string(),
        prefs,
    })
}

pub fn get_json(agent: &ureq::Agent, url: &str, auth: Option<&str>) -> Result<Value, String> {
    let mut req = agent.get(url);
    if let Some(a) = auth {
        req = req.header("Authorization", a);
    }
    let text = req
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// 自宅の位置から住んでいる地方を決める。
fn detect_region(agent: &ureq::Agent, ha: &HomeAssistant) -> Result<Region, String> {
    let pref = crate::home::home_muni_code(agent, ha)?[0..2].to_string();
    let area = get_json(agent, AREA_URL, None)?;
    region_from_area(&area, &pref)
        .ok_or_else(|| format!("都道府県 {pref} の地方が area.json にない"))
}

/// アラートの実演用(設定の demo_after_secs と `png` の画像)。
pub fn demo_view() -> QuakeView {
    QuakeView {
        title: "震源・震度情報(テスト表示)".into(),
        max_intensity: Some(6),
        place: "東京湾".into(),
        time: "10/13 21:30 ごろ".into(),
        magnitude: "M6.1".into(),
        depth: "深さ 70km".into(),
        epicenter: Some((35.5, 139.9)),
        prefs: vec![
            ("13".into(), 6),
            ("12".into(), 5),
            ("11".into(), 5),
            ("14".into(), 4),
            ("08".into(), 4),
            ("09".into(), 3),
            ("10".into(), 3),
            ("19".into(), 2),
        ],
    }
}

/// 地震アラートの問い合わせを始める。アラートは `tx` へ送る。
pub fn start(alerts: &Alerts, ha: &HomeAssistant, tx: Sender<AlertView>) {
    if !alerts.quake {
        return;
    }
    if alerts.demo_after_secs > 0 {
        let (tx, delay) = (tx.clone(), Duration::from_secs(alerts.demo_after_secs));
        let _ = std::thread::Builder::new()
            .name("quake-demo".into())
            .spawn(move || {
                std::thread::sleep(delay);
                eprintln!("quake: テスト表示を送る");
                let _ = tx.send(AlertView::Quake(demo_view()));
            });
    }
    let (ha, poll) = (ha.clone(), Duration::from_secs(alerts.quake_poll_secs));
    let spawned = std::thread::Builder::new()
        .name("quake".into())
        .spawn(move || run(tx, ha, poll));
    if let Err(e) = spawned {
        eprintln!("quake: スレッドを起動できない: {e}");
    }
}

fn run(tx: Sender<AlertView>, ha: HomeAssistant, poll: Duration) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let mut region: Option<Region> = None;
    let mut next_region = Instant::now();
    let mut seen = HashSet::new();
    let mut first = true;
    let mut warned = false;
    loop {
        if Instant::now() >= next_region {
            match detect_region(&agent, &ha) {
                Ok(r) => {
                    if region.as_ref() != Some(&r) {
                        let mut p: Vec<&String> = r.prefs.iter().collect();
                        p.sort();
                        eprintln!("quake: 住んでいる地方 = {} {p:?}", r.name);
                    }
                    region = Some(r);
                    next_region = Instant::now() + REGION_REFRESH;
                }
                Err(e) => {
                    eprintln!(
                        "quake: 住んでいる地方を決められない(全国で震度5弱以上だけを出す): {e}"
                    );
                    next_region = Instant::now() + REGION_RETRY;
                }
            }
        }
        match get_json(&agent, LIST_URL, None) {
            Ok(list) => {
                if warned {
                    eprintln!("quake: 地震情報の取得が回復");
                    warned = false;
                }
                let min_ctt = first.then(|| {
                    (chrono::Local::now() - chrono::Duration::minutes(STARTUP_MAX_AGE_MIN))
                        .format("%Y%m%d%H%M%S")
                        .to_string()
                });
                for r in fresh_reports(&list, &mut seen, min_ctt.as_deref()) {
                    if qualifies(&r, region.as_ref()) {
                        eprintln!(
                            "quake: アラート {} 最大震度{} {}",
                            r.view.title,
                            r.max.map_or("-", miharashi_render::alert::intensity_label),
                            r.view.place
                        );
                        if tx.send(AlertView::Quake(r.view)).is_err() {
                            return; // 画面ロジックが終わった
                        }
                    }
                }
                first = false;
            }
            Err(e) if !warned => {
                warned = true;
                eprintln!("quake: 地震情報を取得できない(再試行を続ける): {e}");
            }
            Err(_) => {}
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cod_formats() {
        assert_eq!(
            parse_cod("+36.5+140.6-60000/"),
            Some((36.5, 140.6, Some(-60000.0)))
        );
        assert_eq!(parse_cod("+32.3+130.5+0/"), Some((32.3, 130.5, Some(0.0))));
        assert_eq!(parse_cod("+07.5-080.8/"), Some((7.5, -80.8, None)));
        assert_eq!(parse_cod(""), None);
        assert_eq!(parse_cod("abc"), None);
        assert_eq!(depth_label(Some(-60000.0)), "深さ 60km");
        assert_eq!(depth_label(Some(0.0)), "ごく浅い");
    }

    fn entry(ctt: &str, eid: &str, ttl: &str, maxi: &str, ints: &[(&str, &str)]) -> Value {
        json!({
            "ctt": ctt, "eid": eid, "ttl": ttl, "ser": "1",
            "at": "2026-10-13T20:53:00+09:00", "anm": "東京湾",
            "cod": "+35.5+139.9-70000/", "mag": "5.0", "maxi": maxi,
            "int": ints.iter().map(|(c, m)| json!({"code": c, "maxi": m, "city": []})).collect::<Vec<_>>(),
        })
    }

    fn kanto() -> Region {
        Region {
            name: "関東甲信地方".into(),
            prefs: ["08", "09", "10", "11", "12", "13", "14", "19", "20"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    #[test]
    fn parses_a_real_shaped_report() {
        let r = parse_report(&entry(
            "20261013205553",
            "E1",
            "震源・震度情報",
            "4",
            &[("12", "4"), ("13", "3")],
        ))
        .unwrap();
        assert_eq!(r.max, Some(4));
        assert_eq!(r.view.time, "10/13 20:53 ごろ");
        assert_eq!(r.view.magnitude, "M5.0");
        assert_eq!(r.view.depth, "深さ 70km");
        assert_eq!(r.view.epicenter, Some((35.5, 139.9)));
        assert_eq!(
            r.view.prefs,
            vec![("12".to_string(), 4), ("13".to_string(), 3)]
        );
        // 震度を含まない発表と、欠けた項目
        assert!(
            parse_report(&json!({"ttl": "震源に関する情報", "eid": "E", "ctt": "1"})).is_none()
        );
        assert!(parse_report(&json!({"ttl": "震度速報"})).is_none());
        let sparse =
            parse_report(&json!({"ttl": "震度速報", "eid": "E", "ctt": "1", "ser": 0, "int": "x"}))
                .unwrap();
        assert_eq!((sparse.max, sparse.view.epicenter), (None, None));
    }

    #[test]
    fn alert_rules() {
        let region = kanto();
        let r = |maxi, ints: &[(&str, &str)]| {
            parse_report(&entry("1", "E", "震度速報", maxi, ints)).unwrap()
        };
        assert!(
            qualifies(&r("4", &[("12", "4")]), Some(&region)),
            "地方内で震度4"
        );
        assert!(
            qualifies(&r("4", &[("20", "4")]), Some(&region)),
            "長野も関東甲信"
        );
        assert!(
            !qualifies(&r("3", &[("12", "3")]), Some(&region)),
            "地方内でも震度3は出さない"
        );
        assert!(
            !qualifies(&r("4", &[("27", "4")]), Some(&region)),
            "地方外の震度4は出さない"
        );
        assert!(
            qualifies(&r("5-", &[("27", "5-")]), Some(&region)),
            "全国で5弱"
        );
        assert!(
            qualifies(&r("5-", &[("27", "5-")]), None),
            "地方が決まらなくても全国の条件は効く"
        );
        assert!(!qualifies(&r("4", &[("12", "4")]), None));
    }

    #[test]
    fn only_new_reports_are_returned_and_seen_stays_bounded() {
        let list = json!([
            entry(
                "20261013210500",
                "E2",
                "震源・震度情報",
                "4",
                &[("12", "4")]
            ),
            entry("20261013210000", "E2", "震度速報", "4", &[("12", "4")]),
            entry("20261013200000", "E1", "震度速報", "1", &[("43", "1")]),
        ]);
        let mut seen = HashSet::new();
        // 起動直後: 20:58 より古い発表は見たことにする
        let fresh = fresh_reports(&list, &mut seen, Some("20261013205800"));
        let keys: Vec<&str> = fresh.iter().map(|r| r.ctt.as_str()).collect();
        assert_eq!(keys, ["20261013210000", "20261013210500"], "古い順");
        assert_eq!(seen.len(), 3);
        assert!(
            fresh_reports(&list, &mut seen, None).is_empty(),
            "同じ一覧では何も出ない"
        );
        let newer = json!([entry(
            "20261013211000",
            "E3",
            "震度速報",
            "2",
            &[("12", "2")]
        )]);
        assert_eq!(fresh_reports(&newer, &mut seen, None).len(), 1);
        assert_eq!(seen.len(), 1, "一覧から消えた鍵は捨てる");
        assert!(fresh_reports(&json!({"x": 1}), &mut seen, None).is_empty());
    }

    #[test]
    fn region_lookup_from_area_json() {
        let area = json!({
            "centers": {"010300": {"name": "関東甲信地方", "children": ["080000", "130000", "190000", "200000"]}},
            "offices": {"130000": {"name": "東京都", "parent": "010300"}, "011000": {"name": "宗谷地方", "parent": "010100"}},
        });
        let r = region_from_area(&area, "13").unwrap();
        assert_eq!(r.name, "関東甲信地方");
        assert!(r.prefs.contains("19") && r.prefs.contains("13"));
        assert!(region_from_area(&area, "01").is_none(), "center が無い");
        assert!(region_from_area(&json!([]), "13").is_none());
    }
}
