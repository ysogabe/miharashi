//! 航空機。readsb/tar1090 の aircraft.json を別スレッドで定期的に読み、受信機から半径内の機体を
//! 近い順に並べて共有する。機種・登録記号・運航会社・経路は adsbdb で引き、メモリに覚えておく
//! (1 回の取得で引くのは最大 `LOOKUPS_PER_POLL` 件。見つからなかったものも覚える)。
//! ネットワークの応答はすべて欠けうるものとして扱い、読めない項目は飛ばす(パニックさせない)。
use crate::config::Aircraft as Config;
use crate::quake::get_json;
use miharashi_render::aircraft::{AircraftView, Plane};
use miharashi_render::geo::distance_km;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ADSBDB: &str = "https://api.adsbdb.com/v0";
const LOOKUPS_PER_POLL: usize = 3;
/// 位置がこれより古い機体は出さない(秒)
const MAX_SEEN_POS: f64 = 60.0;

/// 最新の取得結果と、取得した時刻。
pub type Shared = Arc<Mutex<Option<(Instant, AircraftView)>>>;

/// 主な空港の日本語名(IATA)。表に無ければ adsbdb の都市名(英語)を使う。
fn airport_ja(iata: &str) -> Option<&'static str> {
    Some(match iata {
        "HND" => "東京",
        "NRT" => "成田",
        "KIX" => "関西",
        "ITM" => "伊丹",
        "UKB" => "神戸",
        "NGO" => "中部",
        "CTS" => "札幌",
        "OKD" => "札幌丘珠",
        "HKD" => "函館",
        "AKJ" => "旭川",
        "KUH" => "釧路",
        "OBO" => "帯広",
        "MMB" => "女満別",
        "WKJ" => "稚内",
        "AOJ" => "青森",
        "MSJ" => "三沢",
        "HNA" => "花巻",
        "AXT" => "秋田",
        "ONJ" => "大館能代",
        "GAJ" => "山形",
        "SYO" => "庄内",
        "SDJ" => "仙台",
        "FKS" => "福島",
        "IBR" => "茨城",
        "KIJ" => "新潟",
        "TOY" => "富山",
        "KMQ" => "小松",
        "NTQ" => "能登",
        "FSZ" => "静岡",
        "MMJ" => "松本",
        "HSG" => "佐賀",
        "OKJ" => "岡山",
        "HIJ" => "広島",
        "IZO" => "出雲",
        "YGJ" => "米子",
        "TTJ" => "鳥取",
        "UBJ" => "山口宇部",
        "IWK" => "岩国",
        "TAK" => "高松",
        "TKS" => "徳島",
        "MYJ" => "松山",
        "KCZ" => "高知",
        "FUK" => "福岡",
        "KKJ" => "北九州",
        "NGS" => "長崎",
        "KMJ" => "熊本",
        "OIT" => "大分",
        "KMI" => "宮崎",
        "KOJ" => "鹿児島",
        "ASJ" => "奄美",
        "OKA" => "那覇",
        "ISG" => "石垣",
        "MMY" => "宮古",
        "SHI" => "下地島",
        "HAC" => "八丈島",
        "OIM" => "大島",
        "ICN" => "ソウル仁川",
        "GMP" => "ソウル金浦",
        "PUS" => "釜山",
        "TPE" => "台北桃園",
        "TSA" => "台北松山",
        "KHH" => "高雄",
        "HKG" => "香港",
        "MFM" => "マカオ",
        "PEK" => "北京",
        "PKX" => "北京大興",
        "PVG" => "上海浦東",
        "SHA" => "上海虹橋",
        "CAN" => "広州",
        "SZX" => "深圳",
        "DLC" => "大連",
        "MNL" => "マニラ",
        "CEB" => "セブ",
        "BKK" => "バンコク",
        "DMK" => "バンコク",
        "SIN" => "シンガポール",
        "KUL" => "クアラルンプール",
        "CGK" => "ジャカルタ",
        "DPS" => "バリ",
        "SGN" => "ホーチミン",
        "HAN" => "ハノイ",
        "DEL" => "デリー",
        "DXB" => "ドバイ",
        "DOH" => "ドーハ",
        "IST" => "イスタンブール",
        "LHR" => "ロンドン",
        "CDG" => "パリ",
        "FRA" => "フランクフルト",
        "MUC" => "ミュンヘン",
        "AMS" => "アムステルダム",
        "HEL" => "ヘルシンキ",
        "ZRH" => "チューリッヒ",
        "FCO" => "ローマ",
        "TAS" => "タシュケント",
        "SYD" => "シドニー",
        "MEL" => "メルボルン",
        "BNE" => "ブリスベン",
        "AKL" => "オークランド",
        "GUM" => "グアム",
        "SPN" => "サイパン",
        "HNL" => "ホノルル",
        "SEA" => "シアトル",
        "SFO" => "サンフランシスコ",
        "LAX" => "ロサンゼルス",
        "JFK" => "ニューヨーク",
        "EWR" => "ニューアーク",
        "ORD" => "シカゴ",
        "DFW" => "ダラス",
        "IAH" => "ヒューストン",
        "ATL" => "アトランタ",
        "YVR" => "バンクーバー",
        "YYZ" => "トロント",
        "ANC" => "アンカレッジ",
        _ => return None,
    })
}

/// 経路の片方("HND 東京")。
fn airport_label(a: &Value) -> String {
    let iata = a.get("iata_code").and_then(Value::as_str).unwrap_or("");
    let city = airport_ja(iata)
        .map(str::to_string)
        .or_else(|| {
            a.get("municipality")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    format!("{iata} {city}").trim().to_string()
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|x| x.is_finite())
}

fn bearing(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (la1, la2) = (from.0.to_radians(), to.0.to_radians());
    let dlon = (to.1 - from.1).to_radians();
    let y = dlon.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dlon.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// aircraft.json の機体のうち、位置が新しく、受信機から `radius_km` 以内のものを近い順に。
/// 戻り値の 2 つ目は、位置の分かる機体の総数。
pub fn parse_aircraft(doc: &Value, receiver: (f64, f64), radius_km: f64) -> (Vec<Plane>, usize) {
    let Some(rows) = doc.get("aircraft").and_then(Value::as_array) else {
        return (Vec::new(), 0);
    };
    let mut total = 0;
    let mut out: Vec<Plane> = rows
        .iter()
        .filter_map(|a| {
            let (lat, lon) = (num(a, "lat")?, num(a, "lon")?);
            let seen = num(a, "seen_pos")?;
            if !(0.0..=MAX_SEEN_POS).contains(&seen) {
                return None;
            }
            total += 1;
            let dist = distance_km(receiver, (lat, lon));
            if dist > radius_km {
                return None;
            }
            let text = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let ground = a.get("alt_baro").and_then(Value::as_str) == Some("ground");
            Some(Plane {
                hex: text("hex").to_uppercase(),
                callsign: text("flight"),
                lat,
                lon,
                alt_ft: num(a, "alt_baro").map(|x| x.round() as i32),
                ground,
                gs_kt: num(a, "gs"),
                track: num(a, "track").or_else(|| num(a, "true_heading")),
                vrate: num(a, "baro_rate")
                    .or_else(|| num(a, "geom_rate"))
                    .map(|x| x.round() as i32),
                dist_km: dist,
                bearing: bearing(receiver, (lat, lon)),
                type_code: text("t"),
                reg: text("r"),
                ..Default::default()
            })
        })
        .collect();
    // 飛んでいる機体を近い順に、地上の機体はその後ろ
    out.sort_by(|a, b| {
        a.ground
            .cmp(&b.ground)
            .then(a.dist_km.total_cmp(&b.dist_km))
    });
    (out, total)
}

/// adsbdb の結果(見つからなかったものは None)。
#[derive(Default)]
struct Lookups {
    aircraft: HashMap<String, Option<Value>>,
    routes: HashMap<String, Option<Value>>,
}

impl Lookups {
    /// 覚えている結果で機体を埋める。未照会のものは予算の範囲で照会する(近い機体から)。
    fn fill(&mut self, agent: &ureq::Agent, planes: &mut [Plane]) {
        let mut budget = LOOKUPS_PER_POLL;
        for p in planes.iter_mut() {
            if !p.hex.is_empty() {
                if !self.aircraft.contains_key(&p.hex) && budget > 0 {
                    budget -= 1;
                    let v =
                        lookup(agent, "aircraft", &p.hex).and_then(|r| r.get("aircraft").cloned());
                    self.aircraft.insert(p.hex.clone(), v);
                }
                if let Some(Some(ac)) = self.aircraft.get(&p.hex) {
                    let s = |k: &str| ac.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    let (maker, model) = (s("manufacturer"), s("type"));
                    p.model = format!("{maker} {model}").trim().to_string();
                    if p.type_code.is_empty() {
                        p.type_code = s("icao_type");
                    }
                    if p.reg.is_empty() {
                        p.reg = s("registration");
                    }
                    p.owner = s("registered_owner");
                }
            }
            if !p.callsign.is_empty() {
                if !self.routes.contains_key(&p.callsign) && budget > 0 {
                    budget -= 1;
                    let v = lookup(agent, "callsign", &p.callsign)
                        .and_then(|r| r.get("flightroute").cloned());
                    self.routes.insert(p.callsign.clone(), v);
                }
                if let Some(Some(fr)) = self.routes.get(&p.callsign) {
                    if let Some(o) = fr.get("origin") {
                        p.from = airport_label(o);
                    }
                    if let Some(d) = fr.get("destination") {
                        p.to = airport_label(d);
                    }
                    if let Some(name) = fr
                        .get("airline")
                        .and_then(|a| a.get("name"))
                        .and_then(Value::as_str)
                    {
                        p.owner = name.to_string();
                    }
                }
            }
        }
        // 覚えすぎないように(1 日で数千機程度)
        if self.aircraft.len() > 20_000 {
            self.aircraft.clear();
        }
        if self.routes.len() > 20_000 {
            self.routes.clear();
        }
    }
}

/// adsbdb の `response`。見つからない("unknown ...")・失敗は None。
fn lookup(agent: &ureq::Agent, kind: &str, key: &str) -> Option<Value> {
    let key: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    if key.is_empty() {
        return None;
    }
    let v = get_json(agent, &format!("{ADSBDB}/{kind}/{key}"), None).ok()?;
    v.get("response").filter(|r| r.is_object()).cloned()
}

/// 受信機の位置(receiver.json)。
fn receiver(agent: &ureq::Agent, url: &str) -> Option<(f64, f64)> {
    let base = url.rsplit_once('/')?.0;
    let v = get_json(agent, &format!("{base}/receiver.json"), None).ok()?;
    Some((num(&v, "lat")?, num(&v, "lon")?))
}

/// 試しの表示用(`png`)。
pub fn demo_view() -> AircraftView {
    let r = (35.681236, 139.767125);
    let plane = |cs: &str, lat: f64, lon: f64, alt: Option<i32>, track: f64| {
        let mut p = Plane {
            callsign: cs.into(),
            lat,
            lon,
            alt_ft: alt,
            ground: alt.is_none(),
            gs_kt: alt.map(|_| 450.0),
            track: Some(track),
            vrate: Some(1500),
            dist_km: distance_km(r, (lat, lon)),
            bearing: bearing(r, (lat, lon)),
            ..Default::default()
        };
        p.type_code = "B789".into();
        p
    };
    let mut planes = vec![
        plane("JAL516", 35.95, 140.05, Some(32000), 35.0),
        plane("ANA21", 35.70, 139.80, Some(8500), 250.0),
        plane("APJ123", 35.55, 140.25, Some(4500), 120.0),
        plane("SKY601", 36.15, 139.70, Some(28000), 300.0),
        plane("", 35.80, 139.90, None, 0.0),
    ];
    planes[0].model = "Boeing 787 9".into();
    planes[0].reg = "JA873J".into();
    planes[0].owner = "Japan Airlines".into();
    planes[0].from = "CTS 札幌".into();
    planes[0].to = "HND 東京".into();
    planes[1].to = "ITM 伊丹".into();
    planes[2].to = "NRT 成田".into();
    planes[3].to = "FUK 福岡".into();
    planes[4].reg = "JA05RK".into();
    planes.sort_by(|a, b| {
        a.ground
            .cmp(&b.ground)
            .then(a.dist_km.total_cmp(&b.dist_km))
    });
    AircraftView {
        receiver: r,
        radius_km: 50.0,
        total: 9,
        planes,
        time: "22:41:05".into(),
        ..Default::default()
    }
}

/// 取得してから `secs` 秒たった位置に、速度と進行方向で機体を進める(位置は 5 秒ごとにしか届かないので、
/// その間も滑らかに動かす)。地上の機体と、速度か進行方向が分からない機体は動かさない。
pub fn advanced(view: &AircraftView, secs: f64) -> AircraftView {
    let mut v = view.clone();
    for p in &mut v.planes {
        let (Some(gs), Some(track), false) = (p.gs_kt, p.track, p.ground) else {
            continue;
        };
        let km = gs * 1.852 / 3600.0 * secs;
        let t = track.to_radians();
        p.lat += km * t.cos() / 111.0;
        p.lon += km * t.sin() / (111.0 * p.lat.to_radians().cos().max(0.2));
        p.dist_km = distance_km(v.receiver, (p.lat, p.lon));
        p.bearing = bearing(v.receiver, (p.lat, p.lon));
    }
    v
}

/// 取得を始める。無効なら None。
pub fn start(cfg: &Config) -> Option<Shared> {
    if !cfg.enabled || cfg.url.is_empty() {
        return None;
    }
    let shared: Shared = Arc::new(Mutex::new(None));
    let (out, cfg) = (shared.clone(), cfg.clone());
    let spawned = std::thread::Builder::new()
        .name("aircraft".into())
        .spawn(move || run(out, cfg));
    if let Err(e) = spawned {
        eprintln!("aircraft: スレッドを起動できない: {e}");
        return None;
    }
    Some(shared)
}

fn run(out: Shared, cfg: Config) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .into();
    let mut rx: Option<(f64, f64)> = None;
    let mut lookups = Lookups::default();
    let mut warned = false;
    let poll = Duration::from_secs(cfg.poll_secs);
    loop {
        if rx.is_none() {
            rx = receiver(&agent, &cfg.url);
            match rx {
                Some(r) => eprintln!("aircraft: 受信機 {:.4},{:.4}", r.0, r.1),
                None if !warned => eprintln!("aircraft: 受信機の位置を取れない(再試行を続ける)"),
                None => {}
            }
        }
        if let Some(r) = rx {
            match get_json(&agent, &cfg.url, None) {
                Ok(doc) => {
                    if warned {
                        eprintln!("aircraft: 取得が回復");
                        warned = false;
                    }
                    let (mut planes, total) = parse_aircraft(&doc, r, cfg.radius_km);
                    lookups.fill(&agent, &mut planes);
                    let view = AircraftView {
                        receiver: r,
                        radius_km: cfg.radius_km,
                        planes,
                        total,
                        time: chrono::Local::now().format("%H:%M:%S").to_string(),
                        ..Default::default()
                    };
                    if let Ok(mut g) = out.lock() {
                        *g = Some((Instant::now(), view));
                    }
                }
                Err(e) if !warned => {
                    warned = true;
                    eprintln!("aircraft: 取得できない(再試行を続ける): {e}");
                }
                Err(_) => {}
            }
        } else {
            warned = true;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RX: (f64, f64) = (35.681236, 139.767125);

    #[test]
    fn parses_fresh_planes_within_radius_nearest_first() {
        let doc = json!({"now": 1.0, "aircraft": [
            {"hex": "86e7c4", "flight": "JAL516  ", "lat": 36.0, "lon": 140.0, "seen_pos": 1.2,
             "alt_baro": 32000, "gs": 450.5, "track": 35.0, "baro_rate": 1600, "t": "B789", "r": "JA873J"},
            {"hex": "861afe", "lat": 35.69, "lon": 139.77, "seen_pos": 0.5, "alt_baro": "ground"},
            {"hex": "old", "lat": 35.69, "lon": 139.77, "seen_pos": 120.0},
            {"hex": "far", "lat": 34.0, "lon": 135.0, "seen_pos": 1.0},
            {"hex": "nopos", "seen_pos": 1.0},
            "壊れた行",
        ]});
        let (p, total) = parse_aircraft(&doc, RX, 50.0);
        assert_eq!(total, 3, "位置が新しい機体の数(半径外も数える)");
        let hexes: Vec<&str> = p.iter().map(|p| p.hex.as_str()).collect();
        assert_eq!(
            hexes,
            ["86E7C4", "861AFE"],
            "飛んでいる機体が先・古い位置と半径外は除く"
        );
        assert!(p[1].ground && p[1].alt_ft.is_none());
        let j = &p[0];
        assert_eq!(
            (j.callsign.as_str(), j.alt_ft, j.vrate),
            ("JAL516", Some(32000), Some(1600))
        );
        assert!(j.dist_km > 10.0 && j.dist_km < 30.0, "{}", j.dist_km);
        assert!(
            j.bearing > 270.0 || j.bearing < 90.0,
            "北寄り {}",
            j.bearing
        );
        assert_eq!(parse_aircraft(&json!({}), RX, 50.0).1, 0);
    }

    #[test]
    fn airport_labels() {
        assert_eq!(
            airport_label(&json!({"iata_code": "HND", "municipality": "Tokyo"})),
            "HND 東京"
        );
        assert_eq!(
            airport_label(&json!({"iata_code": "XYZ", "municipality": "Somewhere"})),
            "XYZ Somewhere"
        );
        assert_eq!(airport_label(&json!({})), "");
    }

    #[test]
    fn advanced_moves_planes_along_their_track() {
        let v = demo_view();
        let a = advanced(&v, 10.0);
        for (p, q) in v.planes.iter().zip(&a.planes) {
            if p.ground {
                assert_eq!((p.lat, p.lon), (q.lat, q.lon), "地上の機体は動かさない");
                continue;
            }
            // 450 kt で 10 秒 = 約 2.3 km
            let moved = distance_km((p.lat, p.lon), (q.lat, q.lon));
            assert!((moved - 2.315).abs() < 0.05, "{moved}");
            assert!(
                (bearing((p.lat, p.lon), (q.lat, q.lon)) - p.track.unwrap()).abs() < 1.0,
                "進行方向へ進む"
            );
            assert!((q.dist_km - distance_km(v.receiver, (q.lat, q.lon))).abs() < 1e-9);
        }
        assert_eq!(advanced(&v, 0.0), v);
    }

    #[test]
    fn bearings() {
        assert!((bearing((35.0, 139.0), (36.0, 139.0)) - 0.0).abs() < 0.5);
        assert!((bearing((35.0, 139.0), (35.0, 140.0)) - 90.0).abs() < 1.0);
        assert!((bearing((35.0, 139.0), (34.0, 139.0)) - 180.0).abs() < 0.5);
    }
}
