//! 災害アラート。気象庁の警報・注意報(2026 年からの r8 形式)を別スレッドで定期的に読み、
//! 自宅から半径 N km(既定 50km)の市町村に次のどれかが新しく出たら画面ロジックへ送る。
//! - 特別警報(レベル 5: 大雨・土砂災害・高潮・暴風・暴風雪・大雪・波浪)
//! - 土砂災害危険警報(レベル 4。以前の土砂災害警戒情報にあたる)
//!
//! 大雨・高潮のレベル 4(危険警報)は対象にしない(`is_alert` を変えれば足せる)。
//! 氾濫(指定河川洪水予報)は別の配信(flood_xml)なのでここでは扱わない。
//!
//! 対象の市町村は、Home Assistant の自宅の位置と気象庁の市町村の外接矩形(class20relm.json)から、
//! 矩形の最も近い点までの距離で選ぶ。警報は市町村を受け持つ気象台ごとのファイルを
//! ETag 付きで読み、変わっていなければ 304 で済ませる。
//!
//! 同じ取得で、自宅の市町村に出ている警報・注意報(全種類)を `HomeWarnings` に写す
//! (室内環境と天気の画面の下の帯。docs/DESIGN-screens.md §2.1)。
//! ネットワークの応答はすべて欠けうるものとして扱い、読めない項目は飛ばす(パニックさせない)。
use crate::config::{Alerts, HomeAssistant};
use crate::env::FeedStatus;
use crate::quake::{AREA_URL, get_json, home_location, home_muni_code};
use miharashi_render::alert::{AlertView, DisasterArea, DisasterKind, DisasterView};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RELM_URL: &str = "https://www.jma.go.jp/bosai/common/const/class20relm.json";
const OFFICE_URL: &str = "https://www.jma.go.jp/bosai/warning/data/r8";
/// 起動時に、これより前の発表はアラートにしない(再起動のたびに出さない)。
/// そのため、発表から 10 分以上たってから再起動すると、続いている特別警報は出ない。
const STARTUP_MAX_AGE_MIN: i64 = 10;
/// 対象の市町村を決め直す間隔(成功時)と、失敗時の再試行間隔
const AREA_REFRESH: Duration = Duration::from_secs(24 * 3600);
const AREA_RETRY: Duration = Duration::from_secs(600);

/// 警報・注意報の種類。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kind {
    pub name: &'static str,
    /// 警戒レベル(注意報 2、警報 3、危険警報 4、特別警報 5)
    pub level: u8,
}

/// 気象庁のコード(2 桁の文字列)から種類。気象庁の警報ページの表から写したもの(氾濫は別配信なので除く)。
pub fn kind(code: &str) -> Option<Kind> {
    let (name, level) = match code {
        "10" => ("大雨注意報", 2),
        "03" => ("大雨警報", 3),
        "43" => ("大雨危険警報", 4),
        "33" => ("大雨特別警報", 5),
        "29" => ("土砂災害注意報", 2),
        "09" => ("土砂災害警報", 3),
        "49" => ("土砂災害危険警報", 4),
        "39" => ("土砂災害特別警報", 5),
        "19" => ("高潮注意報", 2),
        "08" => ("高潮警報", 3),
        "48" => ("高潮危険警報", 4),
        "38" => ("高潮特別警報", 5),
        "15" => ("強風注意報", 2),
        "05" => ("暴風警報", 3),
        "35" => ("暴風特別警報", 5),
        "13" => ("風雪注意報", 2),
        "02" => ("暴風雪警報", 3),
        "32" => ("暴風雪特別警報", 5),
        "12" => ("大雪注意報", 2),
        "06" => ("大雪警報", 3),
        "36" => ("大雪特別警報", 5),
        "16" => ("波浪注意報", 2),
        "07" => ("波浪警報", 3),
        "37" => ("波浪特別警報", 5),
        "14" => ("雷注意報", 2),
        "17" => ("融雪注意報", 2),
        "20" => ("濃霧注意報", 2),
        "21" => ("乾燥注意報", 2),
        "22" => ("なだれ注意報", 2),
        "23" => ("低温注意報", 2),
        "24" => ("霜注意報", 2),
        "25" => ("着氷注意報", 2),
        "26" => ("着雪注意報", 2),
        _ => return None,
    };
    Some(Kind { name, level })
}

/// アラートにするコードか(特別警報すべてと土砂災害危険警報)。
pub fn is_alert(code: &str) -> bool {
    code == "49" || kind(code).is_some_and(|k| k.level == 5)
}

/// 発表中を表す状態か。"解除" と "発表警報・注意報はなし" 以外(発表・継続など)。
fn is_active_status(status: &str) -> bool {
    !status.is_empty() && !status.contains("解除") && !status.contains("なし")
}

/// 市町村(気象庁の class20)。
#[derive(Debug, Clone, PartialEq)]
pub struct Muni {
    pub name: String,
    /// 南西 (緯度, 経度)
    pub sw: (f64, f64),
    /// 北東 (緯度, 経度)
    pub ne: (f64, f64),
}

/// 自宅のまわりで見張る範囲。
#[derive(Debug, Clone, PartialEq)]
pub struct Watch {
    pub home: (f64, f64),
    /// 市町村コード(7 桁)→ 市町村
    pub munis: HashMap<String, Muni>,
    /// 警報を読む気象台のコード
    pub offices: Vec<String>,
    /// 自宅の市町村(コード, 名前, 受け持つ気象台)。決められなければ None
    pub home_muni: Option<(String, String, String)>,
}

/// 自宅の市町村に出ている警報・注意報(室内環境と天気の画面の下の帯)。
#[derive(Debug, Clone, Default)]
pub struct HomeWarnings {
    /// 自宅の市町村の名前。まだ分からなければ空
    pub area: String,
    /// 発表中の (名前, 警戒レベル)。レベルの高い順。まだ読めていなければ None
    pub kinds: Option<Vec<(String, u8)>>,
    /// 気象台の最新の発表時刻("2026-10-15T10:39:00+09:00")
    pub report: String,
    pub status: FeedStatus,
}

pub type HomeShared = Arc<Mutex<HomeWarnings>>;

/// 気象台のファイルのうち、いちばん新しい発表の時刻。
fn latest_report(doc: &Value) -> String {
    doc.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("reportDatetime").and_then(Value::as_str))
        .max()
        .unwrap_or("")
        .to_string()
}

/// 市町村 `code` に発表中の警報・注意報 (名前, レベル)。レベルの高い順。表に無いコードは出さない。
fn kinds_for(doc: &Value, code: &str) -> Vec<(String, u8)> {
    let only: HashMap<String, Muni> = [(
        code.to_string(),
        Muni {
            name: String::new(),
            sw: (0.0, 0.0),
            ne: (0.0, 0.0),
        },
    )]
    .into();
    let (mut hits, mut unknown) = (BTreeMap::new(), BTreeSet::new());
    active_hits(doc, &only, &mut hits, &mut unknown);
    let mut out: Vec<(String, u8)> = hits
        .keys()
        .filter_map(|h| kind(&h.code).map(|k| (k.name.to_string(), k.level)))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.dedup();
    out
}

fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
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

fn latlon(v: Option<&Value>) -> Option<(f64, f64)> {
    let a = v?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// 外接矩形のいちばん近い点が自宅から `km` 以内の市町村を選ぶ。
fn munis_within(relm: &Value, home: (f64, f64), km: f64) -> HashMap<String, Muni> {
    let Some(all) = relm.as_object() else {
        return HashMap::new();
    };
    all.iter()
        .filter_map(|(code, v)| {
            let (ne, sw) = (latlon(v.get("ne"))?, latlon(v.get("sw"))?);
            let nearest = (home.0.clamp(sw.0, ne.0), home.1.clamp(sw.1, ne.1));
            (haversine_km(home, nearest) <= km).then(|| {
                let name = v.get("name").and_then(Value::as_str).unwrap_or(code);
                (
                    code.clone(),
                    Muni {
                        name: name.to_string(),
                        sw,
                        ne,
                    },
                )
            })
        })
        .collect()
}

/// 市町村を受け持つ気象台(area.json の class20 → class15 → class10 → office)。
fn offices_for<'a>(area: &Value, codes: impl Iterator<Item = &'a String>) -> Vec<String> {
    let parent = |kind: &str, code: &str| -> Option<String> {
        Some(
            area.get(kind)?
                .get(code)?
                .get("parent")?
                .as_str()?
                .to_string(),
        )
    };
    let set: BTreeSet<String> = codes
        .filter_map(|c| {
            let c15 = parent("class20s", c)?;
            let c10 = parent("class15s", &c15)?;
            parent("class10s", &c10)
        })
        .collect();
    set.into_iter().collect()
}

/// 発表中の 1 件(市町村とコード)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Hit {
    area: String,
    code: String,
}

/// 気象台のファイル(発表の配列)から、見張っている市町村で発表中のものを集める。
/// 値は、その発表の時刻(reportDatetime)。表に無いコードは `unknown` へ入れる。
fn active_hits(
    doc: &Value,
    munis: &HashMap<String, Muni>,
    out: &mut BTreeMap<Hit, String>,
    unknown: &mut BTreeSet<String>,
) {
    let Some(entries) = doc.as_array() else {
        return;
    };
    for e in entries {
        let report = e
            .get("reportDatetime")
            .and_then(Value::as_str)
            .unwrap_or("");
        let Some(items) = e
            .get("warning")
            .and_then(|w| w.get("class20Items"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in items {
            let Some(area) = item.get("areaCode").and_then(Value::as_str) else {
                continue;
            };
            if !munis.contains_key(area) {
                continue;
            }
            let Some(kinds) = item.get("kinds").and_then(Value::as_array) else {
                continue;
            };
            for k in kinds {
                let (Some(code), Some(status)) = (
                    k.get("code").and_then(Value::as_str),
                    k.get("status").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if !is_active_status(status) {
                    continue;
                }
                if kind(code).is_none() {
                    unknown.insert(code.to_string());
                }
                let hit = Hit {
                    area: area.to_string(),
                    code: code.to_string(),
                };
                let t = out.entry(hit).or_default();
                if report > t.as_str() {
                    *t = report.to_string();
                }
            }
        }
    }
}

/// "2026-10-13T21:29:00+09:00" → "10/13 21:29 発表"
fn report_label(t: &str) -> String {
    match (t.get(5..7), t.get(8..10), t.get(11..16)) {
        (Some(m), Some(d), Some(hm)) => format!("{m}/{d} {hm} 発表"),
        _ => String::new(),
    }
}

/// アラートの対象(特別警報・土砂災害危険警報)の発表から、画面に出す内容を作る。
fn build_view(hits: &BTreeMap<Hit, String>, watch: &Watch, radius_km: f64) -> DisasterView {
    let mut by_code: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut level_of: BTreeMap<&str, u8> = BTreeMap::new();
    let mut latest = "";
    for (h, t) in hits.iter().filter(|(h, _)| is_alert(&h.code)) {
        let Some(m) = watch.munis.get(&h.area) else {
            continue;
        };
        let Some(k) = kind(&h.code) else { continue };
        by_code.entry(&h.code).or_default().push(&m.name);
        let l = level_of.entry(&h.area).or_default();
        *l = (*l).max(k.level);
        if t.as_str() > latest {
            latest = t;
        }
    }
    let mut kinds: Vec<DisasterKind> = by_code
        .into_iter()
        .filter_map(|(code, names)| {
            let k = kind(code)?;
            Some(DisasterKind {
                name: k.name.to_string(),
                level: k.level,
                areas: names.into_iter().map(str::to_string).collect(),
            })
        })
        .collect();
    kinds.sort_by_key(|k| std::cmp::Reverse(k.level));
    let areas = level_of
        .into_iter()
        .filter_map(|(code, level)| {
            let m = watch.munis.get(code)?;
            Some(DisasterArea {
                level,
                sw: m.sw,
                ne: m.ne,
            })
        })
        .collect();
    DisasterView {
        kinds,
        areas,
        home: watch.home,
        radius_km,
        time: report_label(latest),
    }
}

/// 新しく出たアラート対象。起動直後(`min_report` あり)は、それより前の発表を見たことにする。
fn new_alerts(
    hits: &BTreeMap<Hit, String>,
    prev: &HashSet<Hit>,
    min_report: Option<&str>,
) -> Vec<Hit> {
    hits.iter()
        .filter(|(h, _)| is_alert(&h.code) && !prev.contains(*h))
        .filter(|(_, t)| min_report.is_none_or(|m| t.as_str() >= m))
        .map(|(h, _)| h.clone())
        .collect()
}

/// アラートの実演用(設定の demo_after_secs と `png` の画像)。
pub fn demo_view() -> DisasterView {
    let area = |level, sw, ne| DisasterArea { level, sw, ne };
    DisasterView {
        kinds: vec![
            DisasterKind {
                name: "大雨特別警報(テスト表示)".into(),
                level: 5,
                areas: ["千代田区", "中央区", "港区", "新宿区"]
                    .map(String::from)
                    .to_vec(),
            },
            DisasterKind {
                name: "土砂災害危険警報(テスト表示)".into(),
                level: 4,
                areas: [
                    "文京区",
                    "台東区",
                    "墨田区",
                    "江東区",
                    "品川区",
                    "目黒区",
                    "大田区",
                    "世田谷区",
                    "渋谷区",
                    "中野区",
                    "杉並区",
                    "豊島区",
                    "北区",
                    "荒川区",
                    "板橋区",
                    "練馬区",
                    "足立区",
                    "葛飾区",
                    "江戸川区",
                    "川口市",
                    "戸田市",
                    "和光市",
                    "浦安市",
                    "市川市",
                ]
                .map(String::from)
                .to_vec(),
            },
        ],
        areas: vec![
            area(5, (35.6686, 139.7312), (35.7054, 139.7825)),
            area(5, (35.646, 139.7586), (35.6966, 139.7927)),
            area(5, (35.6231, 139.7086), (35.6815, 139.7824)),
            area(5, (35.6734, 139.6735), (35.7294, 139.7459)),
            area(4, (35.6997, 139.7175), (35.7359, 139.7724)),
            area(4, (35.6947, 139.7628), (35.7312, 139.8097)),
            area(4, (35.5897, 139.7713), (35.708, 139.8477)),
        ],
        home: miharashi_render::sample::HOME,
        radius_km: 50.0,
        time: "10/13 21:30 発表".into(),
    }
}

/// 災害アラートの問い合わせを始める。アラートは `tx` へ送る。
/// 戻り値: 自宅の市町村の警報・注意報(無効なら None)。
pub fn start(alerts: &Alerts, ha: &HomeAssistant, tx: Sender<AlertView>) -> Option<HomeShared> {
    if !alerts.disaster {
        return None;
    }
    if alerts.demo_after_secs > 0 {
        // 地震のテスト表示の 30 秒後に出す
        let (tx, delay) = (tx.clone(), Duration::from_secs(alerts.demo_after_secs + 30));
        let _ = std::thread::Builder::new()
            .name("disaster-demo".into())
            .spawn(move || {
                std::thread::sleep(delay);
                eprintln!("disaster: テスト表示を送る");
                let _ = tx.send(AlertView::Disaster(demo_view()));
            });
    }
    let (ha, poll, km) = (
        ha.clone(),
        Duration::from_secs(alerts.disaster_poll_secs),
        alerts.disaster_radius_km,
    );
    let home: HomeShared = Arc::default();
    let out = home.clone();
    let spawned = std::thread::Builder::new()
        .name("disaster".into())
        .spawn(move || run(tx, ha, poll, km, out));
    if let Err(e) = spawned {
        eprintln!("disaster: スレッドを起動できない: {e}");
        return None;
    }
    Some(home)
}

fn find_watch(agent: &ureq::Agent, ha: &HomeAssistant, km: f64) -> Result<Watch, String> {
    let home = home_location(agent, ha)?;
    let munis = munis_within(&get_json(agent, RELM_URL, None)?, home, km);
    if munis.is_empty() {
        return Err("半径内に市町村が見つからない".into());
    }
    let area = get_json(agent, AREA_URL, None)?;
    // 自宅の市町村(帯に出す)。決められなくてもアラートは続ける
    let home_muni = match home_muni_code(agent, ha) {
        Ok(muni) => crate::weather::class20_for(&area, &muni).and_then(|(code, name)| {
            let office = offices_for(&area, std::iter::once(&code)).pop()?;
            Some((code, name, office))
        }),
        Err(e) => {
            eprintln!("disaster: 自宅の市町村を決められない: {e}");
            None
        }
    };
    let mut offices = offices_for(&area, munis.keys());
    if offices.is_empty() {
        return Err("市町村を受け持つ気象台が area.json にない".into());
    }
    if let Some((_, _, o)) = &home_muni
        && !offices.contains(o)
    {
        offices.push(o.clone());
    }
    Ok(Watch {
        home,
        munis,
        offices,
        home_muni,
    })
}

/// 自宅の市町村の警報・注意報を写す。`ok` は今回すべての気象台を読めたか。
fn publish_home(
    out: &HomeShared,
    w: &Watch,
    docs: &HashMap<String, (Value, Option<String>)>,
    ok: bool,
) {
    let Ok(mut g) = out.lock() else { return };
    let Some((code, name, office)) = &w.home_muni else {
        g.status.fail("自宅の市町村を決められない".into());
        return;
    };
    g.area = name.clone();
    if let Some((doc, _)) = docs.get(office) {
        g.kinds = Some(kinds_for(doc, code));
        g.report = latest_report(doc);
    }
    if ok && g.kinds.is_some() {
        g.status.ok();
    } else {
        g.status.fail("気象庁から警報を読めない".into());
    }
}

/// 気象台のファイルを読む。前回と同じ(304)なら None。
fn fetch_office(
    agent: &ureq::Agent,
    office: &str,
    etag: Option<&String>,
) -> Result<Option<(Value, Option<String>)>, String> {
    let mut req = agent.get(format!("{OFFICE_URL}/{office}.json"));
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let mut resp = req.call().map_err(|e| e.to_string())?;
    if resp.status() == 304 {
        return Ok(None);
    }
    let tag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = resp
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Some((v, tag)))
}

fn run(tx: Sender<AlertView>, ha: HomeAssistant, poll: Duration, km: f64, home: HomeShared) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let mut watch: Option<Watch> = None;
    let mut next_watch = Instant::now();
    // 気象台ごとの最新のファイルと ETag
    let mut docs: HashMap<String, (Value, Option<String>)> = HashMap::new();
    let mut prev: HashSet<Hit> = HashSet::new();
    let mut first = true;
    let mut warned = false;
    let mut unknown_seen: BTreeSet<String> = BTreeSet::new();
    loop {
        if Instant::now() >= next_watch {
            match find_watch(&agent, &ha, km) {
                Ok(w) => {
                    if watch.as_ref() != Some(&w) {
                        eprintln!(
                            "disaster: 自宅から{km}km以内の {} 市町村を見張る(気象台 {:?})",
                            w.munis.len(),
                            w.offices
                        );
                        docs.retain(|o, _| w.offices.contains(o));
                    }
                    watch = Some(w);
                    next_watch = Instant::now() + AREA_REFRESH;
                }
                Err(e) => {
                    eprintln!("disaster: 見張る範囲を決められない(再試行する): {e}");
                    next_watch = Instant::now() + AREA_RETRY;
                    if watch.is_none()
                        && let Ok(mut g) = home.lock()
                    {
                        g.status.fail(format!("見張る範囲を決められない: {e}"));
                    }
                }
            }
        }
        let Some(w) = watch.as_ref() else {
            std::thread::sleep(poll);
            continue;
        };
        let mut ok = true;
        for office in &w.offices {
            match fetch_office(&agent, office, docs.get(office).and_then(|d| d.1.as_ref())) {
                Ok(Some(d)) => {
                    docs.insert(office.clone(), d);
                }
                Ok(None) => {}
                Err(e) => {
                    ok = false;
                    if !warned {
                        eprintln!("disaster: 警報を取得できない(再試行を続ける): {office}: {e}");
                    }
                }
            }
        }
        if ok && warned {
            eprintln!("disaster: 警報の取得が回復");
        }
        warned = !ok;
        publish_home(&home, w, &docs, ok);
        // すべての気象台を一度は読めてから判定する(欠けたまま比べると解除と見分けられない)
        if w.offices.iter().all(|o| docs.contains_key(o)) {
            let mut hits = BTreeMap::new();
            let mut unknown = BTreeSet::new();
            for o in &w.offices {
                if let Some((doc, _)) = docs.get(o) {
                    active_hits(doc, &w.munis, &mut hits, &mut unknown);
                }
            }
            for code in unknown.difference(&unknown_seen) {
                eprintln!("disaster: 表に無いコード {code:?}(気象庁の区分が変わったかもしれない)");
            }
            unknown_seen.extend(unknown);
            // 気象庁の時刻は日本時間の RFC 3339 なので、同じ形にして文字列で比べる
            let min_report = first.then(|| {
                let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("valid offset");
                (chrono::Utc::now() - chrono::Duration::minutes(STARTUP_MAX_AGE_MIN))
                    .with_timezone(&jst)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
            });
            let fresh = new_alerts(&hits, &prev, min_report.as_deref());
            if !fresh.is_empty() {
                let view = build_view(&hits, w, km);
                let names: Vec<&str> = view.kinds.iter().map(|k| k.name.as_str()).collect();
                eprintln!("disaster: アラート {names:?}(新しく {} 件)", fresh.len());
                if tx.send(AlertView::Disaster(view)).is_err() {
                    return; // 画面ロジックが終わった
                }
            }
            prev = hits.into_keys().filter(|h| is_alert(&h.code)).collect();
            first = false;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOME: (f64, f64) = miharashi_render::sample::HOME;

    #[test]
    fn alert_codes_are_special_warnings_and_landslide_level4() {
        for c in ["33", "39", "38", "35", "32", "36", "37", "49"] {
            assert!(is_alert(c), "{c}");
        }
        for c in ["43", "48", "03", "09", "10", "29", "20", "51", "x", ""] {
            assert!(!is_alert(c), "{c}");
        }
        assert_eq!(kind("49").unwrap().name, "土砂災害危険警報");
        assert_eq!(kind("33").unwrap().level, 5);
    }

    #[test]
    fn statuses() {
        assert!(is_active_status("発表"));
        assert!(is_active_status("継続"));
        assert!(!is_active_status("解除"));
        assert!(!is_active_status("発表警報・注意報はなし"));
        assert!(!is_active_status(""));
    }

    fn relm() -> Value {
        json!({
            "1310100": {"name": "千代田区", "ne": [35.7054, 139.7825], "sw": [35.6686, 139.7312]},
            "1220600": {"name": "木更津市", "ne": [35.4443, 140.1297], "sw": [35.3135, 139.8875]},
            "0820100": {"name": "水戸市", "ne": [36.4643, 140.5868], "sw": [36.3009, 140.3217]},
            "bad": {"name": "欠け", "ne": [1]},
        })
    }

    #[test]
    fn radius_selects_by_nearest_bbox_point() {
        let m = munis_within(&relm(), HOME, 50.0);
        assert!(m.contains_key("1310100"), "自宅の市");
        assert!(m.contains_key("1220600"), "木更津市は 50km 以内");
        assert!(!m.contains_key("0820100"), "水戸市は外");
        assert!(!m.contains_key("bad"));
        assert!(munis_within(&json!([]), HOME, 50.0).is_empty());
    }

    #[test]
    fn offices_follow_area_parents() {
        let area = json!({
            "class20s": {"1310100": {"parent": "130011"}, "9999999": {"parent": "x"}},
            "class15s": {"130011": {"parent": "130010"}},
            "class10s": {"130010": {"parent": "130000"}},
        });
        let codes = [
            "1310100".to_string(),
            "9999999".to_string(),
            "0".to_string(),
        ];
        assert_eq!(offices_for(&area, codes.iter()), ["130000"]);
    }

    fn watch() -> Watch {
        Watch {
            home: HOME,
            munis: munis_within(&relm(), HOME, 50.0),
            offices: vec!["130000".into()],
            home_muni: Some(("1310100".into(), "千代田区".into(), "130000".into())),
        }
    }

    /// 自宅の市町村の分だけを、レベルの高い順に。解除と表に無いコードは出さない。
    #[test]
    fn home_kinds_and_latest_report() {
        let d = json!([
            {"reportDatetime": "2026-10-13T21:00:00+09:00", "warning": {"class20Items": [
                {"areaCode": "1310100", "kinds": [
                    {"code": "14", "status": "発表"},
                    {"code": "03", "status": "継続"},
                    {"code": "10", "status": "解除"},
                    {"code": "77", "status": "発表"},
                ]},
                {"areaCode": "1220600", "kinds": [{"code": "33", "status": "発表"}]},
            ]}},
            {"reportDatetime": "2026-10-13T21:30:00+09:00", "warning": {"class20Items": [
                {"areaCode": "1310100", "kinds": [{"code": "14", "status": "継続"}]},
            ]}},
        ]);
        assert_eq!(
            kinds_for(&d, "1310100"),
            [("大雨警報".to_string(), 3), ("雷注意報".to_string(), 2)]
        );
        assert!(kinds_for(&d, "0000000").is_empty());
        assert_eq!(latest_report(&d), "2026-10-13T21:30:00+09:00");
        assert_eq!(latest_report(&json!({})), "");

        let home: HomeShared = Arc::default();
        let docs: HashMap<String, (Value, Option<String>)> =
            [("130000".to_string(), (d, None))].into();
        publish_home(&home, &watch(), &docs, true);
        let g = home.lock().unwrap().clone();
        assert_eq!(
            (g.area.as_str(), g.kinds.map(|k| k.len())),
            ("千代田区", Some(2))
        );
        assert!(!g.status.failing);
        // 読めなくなったら、最後の内容を残して失敗にする
        publish_home(&home, &watch(), &HashMap::new(), false);
        let g = home.lock().unwrap().clone();
        assert!(g.status.failing && g.kinds.is_some());
        let no_home = Watch {
            home_muni: None,
            ..watch()
        };
        let fresh: HomeShared = Arc::default();
        publish_home(&fresh, &no_home, &docs, true);
        assert!(fresh.lock().unwrap().status.failing);
    }

    fn doc(report: &str, items: Value) -> Value {
        json!([
            {"reportDatetime": report, "warning": {"class10Items": [], "class20Items": items}},
            {"reportDatetime": report, "warning": "壊れた形"},
            {"reportDatetime": report},
        ])
    }

    #[test]
    fn hits_skip_released_unwatched_and_odd_entries() {
        let d = doc(
            "2026-10-13T21:00:00+09:00",
            json!([
                {"areaCode": "1310100", "kinds": [
                    {"code": "33", "status": "発表"},
                    {"code": "03", "status": "継続"},
                    {"code": "49", "status": "解除"},
                    {"status": "発表警報・注意報はなし"},
                    {"code": "77", "status": "発表"},
                ]},
                {"areaCode": "0820100", "kinds": [{"code": "33", "status": "発表"}]},
                {"areaCode": "1220600"},
                {"kinds": []},
            ]),
        );
        let (mut hits, mut unknown) = (BTreeMap::new(), BTreeSet::new());
        active_hits(&d, &watch().munis, &mut hits, &mut unknown);
        let codes: Vec<&str> = hits.keys().map(|h| h.code.as_str()).collect();
        assert_eq!(codes, ["03", "33", "77"]);
        assert!(hits.keys().all(|h| h.area == "1310100"), "水戸市は見ない");
        assert_eq!(unknown.iter().collect::<Vec<_>>(), ["77"]);
        active_hits(&json!({"x": 1}), &watch().munis, &mut hits, &mut unknown);
    }

    #[test]
    fn new_alerts_and_startup_gate() {
        let hit = |area: &str, code: &str| Hit {
            area: area.into(),
            code: code.into(),
        };
        let hits: BTreeMap<Hit, String> = [
            (
                hit("1310100", "33"),
                "2026-10-13T21:00:00+09:00".to_string(),
            ),
            (
                hit("1220600", "49"),
                "2026-10-13T20:00:00+09:00".to_string(),
            ),
            (
                hit("1310100", "03"),
                "2026-10-13T21:00:00+09:00".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        // 起動直後は 20:50 より前の発表を出さない(警報 03 はそもそも対象外)
        let fresh = new_alerts(&hits, &HashSet::new(), Some("2026-10-13T20:50:00+09:00"));
        assert_eq!(fresh, [hit("1310100", "33")]);
        // 起動後は、前回に無かった対象だけ
        let prev: HashSet<Hit> = [hit("1310100", "33")].into_iter().collect();
        assert_eq!(new_alerts(&hits, &prev, None), [hit("1220600", "49")]);
        // 49 → 39 への引き上げは新しい対象として出る
        let up: BTreeMap<Hit, String> = [(hit("1220600", "39"), "t".to_string())]
            .into_iter()
            .collect();
        let prev: HashSet<Hit> = [hit("1220600", "49")].into_iter().collect();
        assert_eq!(new_alerts(&up, &prev, None).len(), 1);
    }

    #[test]
    fn view_groups_by_kind_and_orders_by_level() {
        let hit = |area: &str, code: &str| Hit {
            area: area.into(),
            code: code.into(),
        };
        let hits: BTreeMap<Hit, String> = [
            (
                hit("1310100", "49"),
                "2026-10-13T21:00:00+09:00".to_string(),
            ),
            (
                hit("1220600", "49"),
                "2026-10-13T21:10:00+09:00".to_string(),
            ),
            (
                hit("1310100", "33"),
                "2026-10-13T21:05:00+09:00".to_string(),
            ),
            (
                hit("1310100", "03"),
                "2026-10-13T21:20:00+09:00".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let v = build_view(&hits, &watch(), 50.0);
        let kinds: Vec<(&str, u8, usize)> = v
            .kinds
            .iter()
            .map(|k| (k.name.as_str(), k.level, k.areas.len()))
            .collect();
        assert_eq!(kinds, [("大雨特別警報", 5, 1), ("土砂災害危険警報", 4, 2)]);
        assert_eq!(v.time, "10/13 21:10 発表", "警報(対象外)の時刻は使わない");
        let mut levels: Vec<u8> = v.areas.iter().map(|a| a.level).collect();
        levels.sort();
        assert_eq!(levels, [4, 5], "市町村ごとに高いほうのレベル");
    }
}
