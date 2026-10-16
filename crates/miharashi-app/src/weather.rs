//! 天気予報(docs/DESIGN-screens.md §2.1)。気象庁の府県予報と週間予報(`bosai/forecast/data/forecast/{office}.json`。
//! 1 つのファイルに両方入っている)を別スレッドで定期的に読んで共有する。予報の地域(class10)は、Home Assistant の
//! 自宅の位置 → 国土地理院の逆ジオコーダーの市区町村(届かなければ `home` が覚えている前回の値)→ 気象庁の
//! area.json とたどって決める(設定で固定もできる)。
//! 気象庁に届かないときは最後の予報を残し、取得の状態(`FeedStatus`)だけを変える。
//! ネットワークの応答はすべて欠けうるものとして扱い、読めない項目は飛ばす(パニックさせない)。
use crate::config::{HomeAssistant, Room};
use crate::env::FeedStatus;
use crate::home::home_muni_code;
use crate::quake::{AREA_URL, get_json};
use chrono::NaiveDate;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FORECAST_URL: &str = "https://www.jma.go.jp/bosai/forecast/data/forecast";
const FORECAST_AREA_URL: &str = "https://www.jma.go.jp/bosai/forecast/const/forecast_area.json";
/// 予報の地域を決め直す間隔(成功時)と、失敗時の再試行間隔
const AREA_REFRESH: Duration = Duration::from_secs(24 * 3600);
const AREA_RETRY: Duration = Duration::from_secs(600);
/// 予報を読めなかったときの再試行間隔
const FORECAST_RETRY: Duration = Duration::from_secs(300);

/// 予報の地域。
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastArea {
    /// class10 のコード("130010")
    pub class10: String,
    /// 府県予報区(気象台)のコード("130000")
    pub office: String,
    /// "東京都東京地方"
    pub name: String,
    /// 気温を読むアメダスの地点(forecast_area.json。"44132" など)
    pub amedas: Vec<String>,
}

/// 府県予報の 1 日。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Day {
    pub date: Option<NaiveDate>,
    /// "くもり時々晴れ"(気象庁の文。区切りの空白は詰める)
    pub weather: String,
    pub max: Option<i32>,
    pub min: Option<i32>,
    /// 6 時間ごとの降水確率 [%](0〜6 時 … 18〜24 時)
    pub pops: [Option<u8>; 4],
}

/// 週間予報の 1 日。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WeekEntry {
    pub date: Option<NaiveDate>,
    /// "晴時々曇"(天気のコードから)
    pub weather: String,
    pub max: Option<i32>,
    pub min: Option<i32>,
    pub pop: Option<u8>,
}

/// 予報。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Forecast {
    pub area: String,
    /// 発表の時刻("2026-10-15T11:00:00+09:00")
    pub report: String,
    pub days: Vec<Day>,
    pub week: Vec<WeekEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct WeatherData {
    pub forecast: Option<Forecast>,
    pub status: FeedStatus,
}

pub type Shared = Arc<Mutex<WeatherData>>;

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// "2026-10-15T11:00:00+09:00" → (日付, 時)。時刻は気象庁の日本時間のまま使う。
fn date_hour(t: &str) -> Option<(NaiveDate, u32)> {
    let d = NaiveDate::parse_from_str(t.get(0..10)?, "%Y-%m-%d").ok()?;
    let h = t.get(11..13)?.parse().ok()?;
    Some((d, h))
}

fn int(v: &Value) -> Option<i32> {
    let f: f64 = v.as_str()?.trim().parse().ok()?;
    f.is_finite().then(|| f.round() as i32)
}

fn percent(v: &Value) -> Option<u8> {
    int(v).filter(|p| (0..=100).contains(p)).map(|p| p as u8)
}

/// 地域の配列から、`codes` のどれかに当たる地域。無ければ `fallback` 番目、それも無ければ先頭。
fn pick_area<'a>(series: &'a Value, codes: &[&str], fallback: usize) -> Option<(usize, &'a Value)> {
    let areas = series.get("areas")?.as_array()?;
    areas
        .iter()
        .position(|a| {
            let code = a.get("area").map_or("", |x| str_at(x, "code"));
            codes.contains(&code)
        })
        .or((fallback < areas.len()).then_some(fallback))
        .or((!areas.is_empty()).then_some(0))
        .map(|i| (i, &areas[i]))
}

fn times(series: &Value) -> Vec<&str> {
    series
        .get("timeDefines")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|t| t.as_str().unwrap_or("")).collect())
        .unwrap_or_default()
}

fn list<'a>(area: &'a Value, key: &str) -> &'a [Value] {
    area.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// 予報のファイル(配列の 0 番目が府県予報、1 番目が週間予報)から、地域の予報を読む。
pub fn parse_forecast(doc: &Value, area: &ForecastArea) -> Option<Forecast> {
    let short = doc.get(0)?;
    let report = str_at(short, "reportDatetime").to_string();
    let ts = short.get("timeSeries")?.as_array()?;
    // 天気(今日・明日・明後日)
    let ts0 = ts.first()?;
    let (idx, a0) = pick_area(ts0, &[&area.class10], 0)?;
    let weathers = list(a0, "weathers");
    let codes = list(a0, "weatherCodes");
    let mut days: Vec<Day> = times(ts0)
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let text = weathers
                .get(i)
                .and_then(Value::as_str)
                .map(|s| s.replace(['\u{3000}', ' '], ""))
                .filter(|s| !s.is_empty());
            Day {
                date: date_hour(t).map(|d| d.0),
                weather: text.unwrap_or_else(|| {
                    codes
                        .get(i)
                        .and_then(Value::as_str)
                        .map_or("", telop)
                        .into()
                }),
                ..Day::default()
            }
        })
        .collect();
    // 6 時間ごとの降水確率
    if let Some((_, a1)) = ts.get(1).and_then(|s| pick_area(s, &[&area.class10], idx)) {
        for (t, p) in times(&ts[1]).iter().zip(list(a1, "pops")) {
            let Some((d, h)) = date_hour(t) else { continue };
            if let Some(day) = days.iter_mut().find(|x| x.date == Some(d)) {
                day.pops[(h / 6).min(3) as usize] = percent(p);
            }
        }
    }
    // 気温: 0 時は朝の最低、9 時は日中の最高。11 時・17 時の発表の「今日の 0 時」は最高気温の
    // 写し(朝はもう過ぎた)なので使わない
    let amedas: Vec<&str> = area.amedas.iter().map(String::as_str).collect();
    if let Some((_, a2)) = ts.get(2).and_then(|s| pick_area(s, &amedas, idx)) {
        let issued = date_hour(&report);
        for (t, v) in times(&ts[2]).iter().zip(list(a2, "temps")) {
            let Some((d, h)) = date_hour(t) else { continue };
            let Some(day) = days.iter_mut().find(|x| x.date == Some(d)) else {
                continue;
            };
            match h {
                0 if issued.is_some_and(|(rd, rh)| rd == d && rh >= 9) => {}
                0 => day.min = int(v),
                9 => day.max = int(v),
                _ => {}
            }
        }
    }
    // 週間予報(明日から 7 日)。地域は class10 か府県のもの、気温はアメダスの地点で選ぶ
    let mut week = Vec::new();
    if let Some(wts) = doc
        .get(1)
        .and_then(|w| w.get("timeSeries"))
        .and_then(Value::as_array)
        && let Some((_, w0)) = wts
            .first()
            .and_then(|s| pick_area(s, &[&area.class10, &area.office], 0))
    {
        let wcodes = list(w0, "weatherCodes");
        let wpops = list(w0, "pops");
        let temps = wts.get(1).and_then(|s| pick_area(s, &amedas, 0));
        let empty: &[Value] = &[];
        let (tmax, tmin) = temps.map_or((empty, empty), |(_, a)| {
            (list(a, "tempsMax"), list(a, "tempsMin"))
        });
        for (i, t) in times(&wts[0]).iter().enumerate() {
            week.push(WeekEntry {
                date: date_hour(t).map(|d| d.0),
                weather: wcodes
                    .get(i)
                    .and_then(Value::as_str)
                    .map_or("", telop)
                    .into(),
                max: tmax.get(i).and_then(int),
                min: tmin.get(i).and_then(int),
                pop: wpops.get(i).and_then(percent),
            });
        }
    }
    Some(Forecast {
        area: area.name.clone(),
        report,
        days,
        week,
    })
}

/// 市区町村コード(5 桁)→ class20 の (コード, 名前)。area.json に "13101" + "00" があればそれ、
/// 無ければ同じ 5 桁で始まる最初のもの(区に分かれた市など)。
pub fn class20_for(area: &Value, muni: &str) -> Option<(String, String)> {
    let all = area.get("class20s")?.as_object()?;
    let code = Some(format!("{muni}00"))
        .filter(|c| all.contains_key(c))
        .or_else(|| {
            let mut keys: Vec<&String> = all.keys().filter(|k| k.starts_with(muni)).collect();
            keys.sort();
            keys.first().map(|k| k.to_string())
        })?;
    let name = str_at(&all[&code], "name").to_string();
    Some((code, name))
}

fn parent(area: &Value, kind: &str, code: &str) -> Option<String> {
    Some(
        area.get(kind)?
            .get(code)?
            .get("parent")?
            .as_str()?
            .to_string(),
    )
}

/// class10 から予報の地域を作る。
fn forecast_area(area: &Value, fa: &Value, class10: &str) -> Option<ForecastArea> {
    let office = parent(area, "class10s", class10)?;
    let name = format!(
        "{}{}",
        str_at(area.get("offices")?.get(&office)?, "name"),
        str_at(area.get("class10s")?.get(class10)?, "name")
    );
    let amedas = fa
        .get(&office)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| str_at(e, "class10") == class10)
        .flat_map(|e| list(e, "amedas").iter().filter_map(Value::as_str))
        .map(str::to_string)
        .collect();
    Some(ForecastArea {
        class10: class10.to_string(),
        office,
        name,
        amedas,
    })
}

/// 市町村(class20)から予報の地域(class10)をたどる。
fn class10_of(area: &Value, class20: &str) -> Option<String> {
    parent(area, "class15s", &parent(area, "class20s", class20)?)
}

fn find_area(agent: &ureq::Agent, ha: &HomeAssistant, fixed: &str) -> Result<ForecastArea, String> {
    let class10 = if fixed.is_empty() {
        let muni = home_muni_code(agent, ha)?;
        let area = get_json(agent, AREA_URL, None)?;
        let (c20, _) = class20_for(&area, &muni)
            .ok_or_else(|| format!("市区町村 {muni} が area.json にない"))?;
        class10_of(&area, &c20).ok_or_else(|| format!("{c20} の予報の地域が area.json にない"))?
    } else {
        fixed.to_string()
    };
    let area = get_json(agent, AREA_URL, None)?;
    let fa = get_json(agent, FORECAST_AREA_URL, None)?;
    forecast_area(&area, &fa, &class10)
        .ok_or_else(|| format!("予報の地域 {class10} が area.json にない"))
}

/// 天気予報の取得を始める。予報の地域を決められない設定(HA も forecast_area も無い)なら None。
pub fn start(ha: &HomeAssistant, room: &Room) -> Option<Shared> {
    if ha.url.is_empty() && room.forecast_area.is_empty() {
        return None;
    }
    let shared: Shared = Arc::default();
    let (out, ha, room) = (shared.clone(), ha.clone(), room.clone());
    let spawned = std::thread::Builder::new()
        .name("weather".into())
        .spawn(move || run(out, ha, room));
    if let Err(e) = spawned {
        eprintln!("weather: スレッドを起動できない: {e}");
        return None;
    }
    Some(shared)
}

fn run(out: Shared, ha: HomeAssistant, room: Room) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let every = Duration::from_secs(room.forecast_minutes * 60);
    let mut area: Option<ForecastArea> = None;
    let mut next_area = Instant::now();
    let fail = |e: String| {
        if let Ok(mut g) = out.lock()
            && g.status.fail(e.clone())
        {
            eprintln!("weather: {e}(最後の予報を出し、再試行を続ける)");
        }
    };
    loop {
        if Instant::now() >= next_area {
            match find_area(&agent, &ha, &room.forecast_area) {
                Ok(a) => {
                    if area.as_ref() != Some(&a) {
                        eprintln!(
                            "weather: 予報の地域 {}({} / 気象台 {})",
                            a.name, a.class10, a.office
                        );
                    }
                    area = Some(a);
                    next_area = Instant::now() + AREA_REFRESH;
                }
                Err(e) => {
                    next_area = Instant::now() + AREA_RETRY;
                    if area.is_none() {
                        fail(format!("予報の地域を決められない: {e}"));
                    }
                }
            }
        }
        let Some(a) = area.as_ref() else {
            std::thread::sleep(AREA_RETRY);
            continue;
        };
        let url = format!("{FORECAST_URL}/{}.json", a.office);
        let wait = match get_json(&agent, &url, None).map(|doc| parse_forecast(&doc, a)) {
            Ok(Some(f)) => {
                if let Ok(mut g) = out.lock() {
                    if g.status.failing {
                        eprintln!("weather: 予報を読めるようになった");
                    }
                    g.forecast = Some(f);
                    g.status.ok();
                }
                every
            }
            Ok(None) => {
                fail("予報の形が想定と違う".into());
                FORECAST_RETRY.min(every)
            }
            Err(e) => {
                fail(format!("予報を取得できない: {e}"));
                FORECAST_RETRY.min(every)
            }
        };
        std::thread::sleep(wait);
    }
}

/// 天気のコード → 文("晴時々曇")。気象庁の予報ページの表(Forecast.Const.TELOPS)から写したもの。
pub fn telop(code: &str) -> &'static str {
    match code.parse::<u16>().unwrap_or(0) {
        100 => "晴",
        101 => "晴時々曇",
        102 => "晴一時雨",
        103 => "晴時々雨",
        104 => "晴一時雪",
        105 => "晴時々雪",
        106 => "晴一時雨か雪",
        107 => "晴時々雨か雪",
        108 => "晴一時雨か雷雨",
        110 => "晴後時々曇",
        111 => "晴後曇",
        112 => "晴後一時雨",
        113 => "晴後時々雨",
        114 => "晴後雨",
        115 => "晴後一時雪",
        116 => "晴後時々雪",
        117 => "晴後雪",
        118 => "晴後雨か雪",
        119 => "晴後雨か雷雨",
        120 => "晴朝夕一時雨",
        121 => "晴朝の内一時雨",
        122 => "晴夕方一時雨",
        123 => "晴山沿い雷雨",
        124 => "晴山沿い雪",
        125 => "晴午後は雷雨",
        126 => "晴昼頃から雨",
        127 => "晴夕方から雨",
        128 => "晴夜は雨",
        130 => "朝の内霧後晴",
        131 => "晴明け方霧",
        132 => "晴朝夕曇",
        140 => "晴時々雨で雷を伴う",
        160 => "晴一時雪か雨",
        170 => "晴時々雪か雨",
        181 => "晴後雪か雨",
        200 => "曇",
        201 => "曇時々晴",
        202 => "曇一時雨",
        203 => "曇時々雨",
        204 => "曇一時雪",
        205 => "曇時々雪",
        206 => "曇一時雨か雪",
        207 => "曇時々雨か雪",
        208 => "曇一時雨か雷雨",
        209 => "霧",
        210 => "曇後時々晴",
        211 => "曇後晴",
        212 => "曇後一時雨",
        213 => "曇後時々雨",
        214 => "曇後雨",
        215 => "曇後一時雪",
        216 => "曇後時々雪",
        217 => "曇後雪",
        218 => "曇後雨か雪",
        219 => "曇後雨か雷雨",
        220 => "曇朝夕一時雨",
        221 => "曇朝の内一時雨",
        222 => "曇夕方一時雨",
        223 => "曇日中時々晴",
        224 => "曇昼頃から雨",
        225 => "曇夕方から雨",
        226 => "曇夜は雨",
        228 => "曇昼頃から雪",
        229 => "曇夕方から雪",
        230 => "曇夜は雪",
        231 => "曇海上海岸は霧か霧雨",
        240 => "曇時々雨で雷を伴う",
        250 => "曇時々雪で雷を伴う",
        260 => "曇一時雪か雨",
        270 => "曇時々雪か雨",
        281 => "曇後雪か雨",
        300 => "雨",
        301 => "雨時々晴",
        302 => "雨時々止む",
        303 => "雨時々雪",
        304 => "雨か雪",
        306 => "大雨",
        308 => "雨で暴風を伴う",
        309 => "雨一時雪",
        311 => "雨後晴",
        313 => "雨後曇",
        314 => "雨後時々雪",
        315 => "雨後雪",
        316 => "雨か雪後晴",
        317 => "雨か雪後曇",
        320 => "朝の内雨後晴",
        321 => "朝の内雨後曇",
        322 => "雨朝晩一時雪",
        323 => "雨昼頃から晴",
        324 => "雨夕方から晴",
        325 => "雨夜は晴",
        326 => "雨夕方から雪",
        327 => "雨夜は雪",
        328 => "雨一時強く降る",
        329 => "雨一時みぞれ",
        340 => "雪か雨",
        350 => "雨で雷を伴う",
        361 => "雪か雨後晴",
        371 => "雪か雨後曇",
        400 => "雪",
        401 => "雪時々晴",
        402 => "雪時々止む",
        403 => "雪時々雨",
        405 => "大雪",
        406 => "風雪強い",
        407 => "暴風雪",
        409 => "雪一時雨",
        411 => "雪後晴",
        413 => "雪後曇",
        414 => "雪後雨",
        420 => "朝の内雪後晴",
        421 => "朝の内雪後曇",
        422 => "雪昼頃から雨",
        423 => "雪夕方から雨",
        425 => "雪一時強く降る",
        426 => "雪後みぞれ",
        427 => "雪一時みぞれ",
        450 => "雪で雷を伴う",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tokyo() -> ForecastArea {
        ForecastArea {
            class10: "130010".into(),
            office: "130000".into(),
            name: "東京都東京地方".into(),
            amedas: vec!["44132".into()],
        }
    }

    fn area(name: &str, code: &str) -> Value {
        json!({"name": name, "code": code})
    }

    /// 11 時発表の形(気象庁の 130000.json を縮めたもの)。
    fn doc(report: &str, temp_times: Value, temps: Value) -> Value {
        json!([
            {
                "reportDatetime": report,
                "timeSeries": [
                    {
                        "timeDefines": ["2026-10-15T11:00:00+09:00", "2026-10-16T00:00:00+09:00", "2026-10-17T00:00:00+09:00"],
                        "areas": [
                            {"area": area("東京地方", "130010"), "weatherCodes": ["201", "201", "101"],
                             "weathers": ["くもり\u{3000}時々\u{3000}晴れ", "くもり\u{3000}昼前\u{3000}から\u{3000}夕方\u{3000}晴れ"]},
                            {"area": area("伊豆諸島北部", "130020"), "weatherCodes": ["300", "300", "300"], "weathers": ["雨", "雨", "雨"]},
                        ]
                    },
                    {
                        "timeDefines": ["2026-10-15T12:00:00+09:00", "2026-10-15T18:00:00+09:00", "2026-10-16T00:00:00+09:00",
                                        "2026-10-16T06:00:00+09:00", "2026-10-16T12:00:00+09:00", "2026-10-16T18:00:00+09:00"],
                        "areas": [
                            {"area": area("東京地方", "130010"), "pops": ["10", "20", "30", "40", "0", ""]},
                            {"area": area("伊豆諸島北部", "130020"), "pops": ["90", "90", "90", "90", "90", "90"]},
                        ]
                    },
                    {
                        "timeDefines": temp_times,
                        "areas": [
                            {"area": area("東京", "44132"), "temps": temps},
                            {"area": area("大島", "44172"), "temps": ["1", "1", "1", "1"]},
                        ]
                    }
                ]
            },
            {
                "reportDatetime": report,
                "timeSeries": [
                    {
                        "timeDefines": ["2026-10-16T00:00:00+09:00", "2026-10-17T00:00:00+09:00", "2026-10-18T00:00:00+09:00"],
                        "areas": [{"area": area("東京都", "130000"), "weatherCodes": ["201", "101", "999"], "pops": ["", "10", "20"]}]
                    },
                    {
                        "timeDefines": ["2026-10-16T00:00:00+09:00", "2026-10-17T00:00:00+09:00", "2026-10-18T00:00:00+09:00"],
                        "areas": [{"area": area("大島", "44172"), "tempsMin": ["", "18", "18"], "tempsMax": ["", "23", "24.4"]}]
                    }
                ]
            }
        ])
    }

    fn d(day: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2026, 10, day)
    }

    #[test]
    fn parses_the_11am_issue() {
        let doc = doc(
            "2026-10-15T11:00:00+09:00",
            json!([
                "2026-10-15T09:00:00+09:00",
                "2026-10-15T00:00:00+09:00",
                "2026-10-16T00:00:00+09:00",
                "2026-10-16T09:00:00+09:00"
            ]),
            json!(["23", "23", "18", "25"]),
        );
        let f = parse_forecast(&doc, &tokyo()).unwrap();
        assert_eq!(f.area, "東京都東京地方");
        assert_eq!(f.days.len(), 3);
        let today = &f.days[0];
        assert_eq!(today.date, d(15));
        assert_eq!(today.weather, "くもり時々晴れ", "区切りの全角空白を詰める");
        assert_eq!(
            (today.max, today.min),
            (Some(23), None),
            "11 時発表の今日の 0 時は使わない"
        );
        assert_eq!(
            today.pops,
            [None, None, Some(10), Some(20)],
            "過ぎた時間帯は無い"
        );
        let tomorrow = &f.days[1];
        assert_eq!((tomorrow.max, tomorrow.min), (Some(25), Some(18)));
        assert_eq!(
            tomorrow.pops,
            [Some(30), Some(40), Some(0), None],
            "空の値は無し"
        );
        assert_eq!(f.days[2].weather, "晴時々曇", "文が無ければコードから");
        assert_eq!(f.days[2].max, None);
        // 週間予報: 地域は府県のもの、気温はアメダスの地点が無ければ先頭
        let w: Vec<_> = f
            .week
            .iter()
            .map(|w| (w.date, w.weather.as_str(), w.max, w.min, w.pop))
            .collect();
        assert_eq!(
            w,
            [
                (d(16), "曇時々晴", None, None, None),
                (d(17), "晴時々曇", Some(23), Some(18), Some(10)),
                (d(18), "", Some(24), Some(18), Some(20)),
            ]
        );
    }

    #[test]
    fn the_5am_issue_has_this_mornings_minimum() {
        let doc = doc(
            "2026-10-15T05:00:00+09:00",
            json!([
                "2026-10-15T00:00:00+09:00",
                "2026-10-15T09:00:00+09:00",
                "2026-10-16T00:00:00+09:00",
                "2026-10-16T09:00:00+09:00"
            ]),
            json!(["15", "24", "16", "21"]),
        );
        let f = parse_forecast(&doc, &tokyo()).unwrap();
        assert_eq!((f.days[0].min, f.days[0].max), (Some(15), Some(24)));
        assert_eq!((f.days[1].min, f.days[1].max), (Some(16), Some(21)));
    }

    /// 地域・地点が見つからなければ、天気の地域と同じ番目、それも無ければ先頭を使う。
    #[test]
    fn falls_back_when_the_area_is_missing() {
        let doc = doc(
            "2026-10-15T05:00:00+09:00",
            json!(["2026-10-15T09:00:00+09:00"]),
            json!(["30"]),
        );
        let other = ForecastArea {
            class10: "130020".into(),
            amedas: vec![],
            ..tokyo()
        };
        let f = parse_forecast(&doc, &other).unwrap();
        assert_eq!(f.days[0].weather, "雨");
        assert_eq!(f.days[0].pops[2], Some(90));
        assert_eq!(
            f.days[0].max,
            Some(1),
            "2 番目の地域なので 2 番目の地点(大島)"
        );
        let unknown = ForecastArea {
            class10: "999999".into(),
            amedas: vec![],
            ..tokyo()
        };
        assert_eq!(
            parse_forecast(&doc, &unknown).unwrap().days[0].max,
            Some(30)
        );
        assert!(parse_forecast(&json!([]), &tokyo()).is_none());
        assert!(parse_forecast(&json!([{"timeSeries": "壊れた形"}]), &tokyo()).is_none());
        let only_short = json!([doc[0].clone()]);
        assert!(
            parse_forecast(&only_short, &tokyo())
                .unwrap()
                .week
                .is_empty()
        );
    }

    /// 東京のように、週間予報に class10 と同じ地域があればそれを使う。
    #[test]
    fn weekly_area_prefers_the_class10() {
        let mut doc = doc("2026-10-15T05:00:00+09:00", json!([]), json!([]));
        doc[1]["timeSeries"][0]["areas"] = json!([
            {"area": area("伊豆諸島", "130100"), "weatherCodes": ["300"], "pops": ["90"]},
            {"area": area("東京地方", "130010"), "weatherCodes": ["100"], "pops": ["0"]},
        ]);
        let tokyo = ForecastArea {
            class10: "130010".into(),
            office: "130000".into(),
            name: "東京都東京地方".into(),
            amedas: vec!["44132".into()],
        };
        let f = parse_forecast(&doc, &tokyo).unwrap();
        assert_eq!((f.week[0].weather.as_str(), f.week[0].pop), ("晴", Some(0)));
    }

    #[test]
    fn home_municipality_and_forecast_area() {
        let a = json!({
            "offices": {"130000": {"name": "東京都"}},
            "class10s": {"130010": {"name": "東京地方", "parent": "130000"}},
            "class15s": {"130011": {"name": "２３区西部", "parent": "130010"}},
            "class20s": {
                "1310300": {"name": "港区", "parent": "130011"},
                "1310101": {"name": "千代田区北部", "parent": "x"},
                "1310102": {"name": "千代田区南部", "parent": "x"},
            },
        });
        assert_eq!(
            class20_for(&a, "13103"),
            Some(("1310300".into(), "港区".into()))
        );
        assert_eq!(
            class20_for(&a, "13101").unwrap().0,
            "1310101",
            "分かれた市区は最初のもの"
        );
        assert_eq!(class20_for(&a, "99999"), None);
        assert_eq!(class10_of(&a, "1310300").as_deref(), Some("130010"));
        let fa = json!({"130000": [
            {"class10": "130010", "amedas": ["44132"], "class20": "1310100"},
            {"class10": "130020", "amedas": ["44172"], "class20": "1336100"},
        ]});
        assert_eq!(forecast_area(&a, &fa, "130010"), Some(tokyo()));
        assert_eq!(forecast_area(&a, &fa, "999999"), None);
    }

    #[test]
    fn telops() {
        assert_eq!(telop("100"), "晴");
        assert_eq!(telop("101"), "晴時々曇");
        assert_eq!(telop("313"), "雨後曇");
        assert_eq!(telop(""), "");
        assert_eq!(telop("999"), "");
    }
}
