//! 室内環境と天気の画面(docs/DESIGN-screens.md §2.1)。左に HA の室内センサー、右に気象庁の天気予報、
//! 下の帯に自宅の市町村の警報・注意報。取得はそれぞれのスレッド(env・weather・disaster)が行い、
//! この画面は最新の値を読んで文字にするだけ。届かないときは最後の値と取得時刻を出す。
//! 描き直すのは、時計の分が変わった時と、取得した内容が変わった時だけ。
use crate::config::RoomTile;
use crate::disaster::HomeShared;
use crate::env::{self, FeedStatus};
use crate::screen::{RenderContext, Screen, ScreenId, World};
use crate::weather::{self, Forecast};
use chrono::{DateTime, Datelike, Local, NaiveDate};
use miharashi_render::room::{
    self, Feed, ForecastDay, RoomView, SensorTile, Warning, Weather, WeekDay,
};
use std::time::Instant;

/// 取得のスレッドとの受け口。無効なもの(設定が無い)は None。
#[derive(Default, Clone)]
pub struct RoomFeeds {
    pub env: Option<env::Shared>,
    pub weather: Option<weather::Shared>,
    pub warnings: Option<HomeShared>,
}

/// 描き直しの判定に使う (時計の分, 室内・天気・警報の版)。
type Key = (String, u64, u64, u64);

pub struct RoomScreen {
    tiles: Vec<RoomTile>,
    feeds: RoomFeeds,
    visible: bool,
    drawn: Option<Key>,
}

const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

fn hm(t: &DateTime<Local>) -> String {
    t.format("%H:%M").to_string()
}

/// 取得の状態の文。`what` は内容の時刻("11:00 発表")。無ければ取得した時刻を出す。
fn feed_of(s: &FeedStatus, source: &str, what: Option<String>) -> Feed {
    match (&s.last_ok, s.failing) {
        (Some(t), false) => Feed {
            text: match what {
                Some(w) => format!("{source} {w}"),
                None => format!("{source} {} 取得", hm(t)),
            },
            stale: false,
        },
        (Some(t), true) => Feed {
            text: format!(
                "{}最終取得 {} · {source}に接続できません",
                what.map(|w| w + " · ").unwrap_or_default(),
                hm(t)
            ),
            stale: true,
        },
        (None, false) => Feed {
            text: format!("{source}に接続しています…"),
            stale: false,
        },
        (None, true) => Feed {
            text: match &s.error {
                Some(e) => format!("{source}に接続できません({e})"),
                None => format!("{source}に接続できません"),
            },
            stale: true,
        },
    }
}

/// 設定が無くて取得していないとき。
fn off(text: &str) -> Feed {
    Feed {
        text: text.into(),
        stale: false,
    }
}

/// 発表の時刻 "2026-10-15T11:00:00+09:00" → "11:00 発表"(今日でなければ "10/14 17:00 発表")。
fn issued(report: &str, today: NaiveDate) -> Option<String> {
    let hm = report.get(11..16)?;
    let date = NaiveDate::parse_from_str(report.get(0..10)?, "%Y-%m-%d").ok()?;
    Some(if date == today {
        format!("{hm} 発表")
    } else {
        format!("{} {hm} 発表", date.format("%-m/%-d"))
    })
}

/// 単位の表記をそろえる(HA は気圧を mbar、温度を °C で返す)。
fn unit_label(u: &str) -> &str {
    match u {
        "mbar" => "hPa",
        "°C" => "℃",
        other => other,
    }
}

/// 値の表記。温度と気圧は小数 1 桁、ほかは 10 未満だけ小数 1 桁。
fn value_text(v: Option<f64>, unit: &str) -> String {
    match v {
        None => "--".into(),
        Some(v) if matches!(unit, "℃" | "°F" | "hPa") || v.abs() < 10.0 => format!("{v:.1}"),
        Some(v) => format!("{v:.0}"),
    }
}

/// `hours` 時間前からの変化("↗ 上昇 +1.2")。履歴が足りなければ None。
fn trend(history: &[f32], hours: u64) -> Option<String> {
    let back = (hours as i64 * 3600 / env::BUCKET_SECS) as usize;
    let last = history.iter().rposition(|v| v.is_finite())?;
    let then = history.get(last.checked_sub(back)?)?;
    let d = history[last] - then;
    if !d.is_finite() {
        return None;
    }
    Some(if d >= 0.5 {
        format!("↗ 上昇 {d:+.1}")
    } else if d <= -0.5 {
        format!("↘ 下降 {d:+.1}")
    } else {
        "→ 横ばい".into()
    })
}

fn sensor_tile(cfg: &RoomTile, data: Option<&env::EnvData>) -> SensorTile {
    let state = |e: &str| data.and_then(|d| d.states.get(e));
    let (value, unit) = state(&cfg.entity).map_or((None, ""), |(v, u)| (*v, u.as_str()));
    let unit = unit_label(unit);
    let history = data
        .and_then(|d| d.history.get(&cfg.entity))
        .cloned()
        .unwrap_or_default();
    let level = match value {
        Some(v) if cfg.alert.is_some_and(|a| v >= a) => 2,
        Some(v) if cfg.warn.is_some_and(|w| v >= w) => 1,
        _ => 0,
    };
    let note = if !cfg.second.is_empty() {
        let (v2, u2) = state(&cfg.second).map_or((None, ""), |(v, u)| (*v, u.as_str()));
        let u2 = unit_label(u2);
        format!("{} {u2}", value_text(v2, u2))
            .trim_end()
            .to_string()
    } else if let Some(h) = cfg.trend_hours {
        trend(&history, h).unwrap_or_default()
    } else {
        match level {
            2 => "高い".into(),
            1 => "やや高い".into(),
            _ => String::new(),
        }
    };
    SensorTile {
        label: cfg.label.clone(),
        value: value_text(value, unit),
        unit: unit.to_string(),
        note,
        level,
        history,
    }
}

fn day_label(date: NaiveDate, today: NaiveDate) -> String {
    let wd = WEEKDAYS[date.weekday().num_days_from_monday() as usize];
    let rel = match (date - today).num_days() {
        0 => "今日",
        1 => "明日",
        2 => "明後日",
        _ => "",
    };
    format!("{rel} {}日({wd})", date.day())
        .trim_start()
        .to_string()
}

/// 予報を画面の形にする。過ぎた日は落とし、週間予報で空の日(明日)は府県予報で埋める。
fn weather_view(f: &Forecast, today: NaiveDate) -> Weather {
    let days: Vec<&weather::Day> = f
        .days
        .iter()
        .filter(|d| d.date.is_some_and(|x| x >= today))
        .collect();
    let week = f
        .week
        .iter()
        .filter_map(|w| Some((w.date.filter(|d| *d > today)?, w)))
        .take(7)
        .map(|(date, w)| {
            let short = days.iter().find(|d| d.date == Some(date));
            WeekDay {
                day: date.day().to_string(),
                weekday: date.weekday().num_days_from_monday() as usize,
                weather: if w.weather.is_empty() {
                    short.map(|d| d.weather.clone()).unwrap_or_default()
                } else {
                    w.weather.clone()
                },
                max: w.max.or(short.and_then(|d| d.max)),
                min: w.min.or(short.and_then(|d| d.min)),
                pop: w
                    .pop
                    .or(short.and_then(|d| d.pops.iter().flatten().max().copied())),
            }
        })
        .collect();
    Weather {
        area: f.area.clone(),
        days: days
            .iter()
            .take(2)
            .map(|d| ForecastDay {
                label: d.date.map(|x| day_label(x, today)).unwrap_or_default(),
                weather: d.weather.clone(),
                max: d.max,
                min: d.min,
                pops: d.pops,
            })
            .collect(),
        week,
    }
}

impl RoomScreen {
    pub fn new(tiles: Vec<RoomTile>, feeds: RoomFeeds) -> Self {
        Self {
            tiles,
            feeds,
            visible: false,
            drawn: None,
        }
    }

    fn key(&self, now: &DateTime<Local>) -> Key {
        fn ver<T>(s: &Option<std::sync::Arc<std::sync::Mutex<T>>>, f: fn(&T) -> u64) -> u64 {
            s.as_ref()
                .and_then(|s| s.lock().ok().map(|g| f(&g)))
                .unwrap_or(0)
        }
        (
            now.format("%Y%m%d%H%M").to_string(),
            ver(&self.feeds.env, |d| d.status.version),
            ver(&self.feeds.weather, |d| d.status.version),
            ver(&self.feeds.warnings, |d| d.status.version),
        )
    }

    /// 今出す内容。
    pub fn view(&self, now: &DateTime<Local>) -> RoomView {
        let today = now.date_naive();
        let wd = WEEKDAYS[now.weekday().num_days_from_monday() as usize];
        let mut v = RoomView {
            clock: format!("{} ({wd})  {}", now.format("%Y/%m/%d"), now.format("%H:%M")),
            ..RoomView::default()
        };
        // 室内
        let env = self
            .feeds
            .env
            .as_ref()
            .and_then(|s| s.lock().ok().map(|g| g.clone()));
        v.tiles = self
            .tiles
            .iter()
            .take(room::MAX_TILES)
            .map(|t| sensor_tile(t, env.as_ref()))
            .collect();
        v.env = match &env {
            Some(d) => feed_of(&d.status, "HA", None),
            None if self.tiles.is_empty() => off(""),
            None => off("HA が設定されていません"),
        };
        // 天気
        v.weather_feed = match &self.feeds.weather {
            Some(s) => match s.lock() {
                Ok(g) => {
                    v.weather = g.forecast.as_ref().map(|f| weather_view(f, today));
                    let what = g.forecast.as_ref().and_then(|f| issued(&f.report, today));
                    feed_of(&g.status, "気象庁", what)
                }
                Err(_) => off(""),
            },
            None => off("予報の地域が決まっていません(HA か room.forecast_area)"),
        };
        // 警報・注意報
        v.warn_feed = match &self.feeds.warnings {
            Some(s) => match s.lock() {
                Ok(g) => {
                    v.warn_area = g.area.clone();
                    v.warnings = g.kinds.as_ref().map(|k| {
                        k.iter()
                            .map(|(name, level)| Warning {
                                name: name.clone(),
                                level: *level,
                            })
                            .collect()
                    });
                    feed_of(&g.status, "気象庁", issued(&g.report, today))
                }
                Err(_) => off(""),
            },
            None => off("警報の取得は無効です([alerts] disaster)"),
        };
        v
    }
}

impl Screen for RoomScreen {
    fn id(&self) -> ScreenId {
        "room"
    }

    fn title(&self) -> &'static str {
        "ROOM & WEATHER"
    }

    fn on_enter(&mut self, _now: Instant) {
        self.visible = true;
        self.drawn = None;
    }

    fn on_leave(&mut self, _now: Instant) {
        self.visible = false;
    }

    fn tick(&mut self, _world: &World, _now: Instant) -> bool {
        self.visible && self.drawn.as_ref() != Some(&self.key(&Local::now()))
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        let now = Local::now();
        // 先に版を取る(描いている間に届いた更新は、次の周期で描き直す)
        let key = self.key(&now);
        room::room_screen(ctx.canvas, ctx.text, &self.view(&now));
        self.drawn = Some(key);
    }

    fn invalidate(&mut self) {
        self.drawn = None;
    }
}

/// どの取得も、1 回目の結果(取れた・取れなかった)が出たか(`png --live` の待ち合わせ)。
pub fn settled(f: &RoomFeeds) -> bool {
    fn done<T>(s: &Option<std::sync::Arc<std::sync::Mutex<T>>>, f: fn(&T) -> bool) -> bool {
        s.as_ref()
            .is_none_or(|s| s.lock().map(|g| f(&g)).unwrap_or(true))
    }
    done(&f.env, |d| d.status.failing || !d.history.is_empty())
        && done(&f.weather, |d| d.status.failing || d.forecast.is_some())
        && done(&f.warnings, |d| d.status.failing || d.kinds.is_some())
}

/// HA と気象庁に届かなくなったところ(`png` の画像)。最後の値と取得時刻を出す。
pub fn offline_demo(v: &RoomView) -> RoomView {
    let t = Local::now()
        .date_naive()
        .and_hms_opt(8, 12, 0)
        .and_then(|t| t.and_local_timezone(Local).single());
    let s = FeedStatus {
        last_ok: t,
        failing: true,
        ..FeedStatus::default()
    };
    RoomView {
        env: feed_of(&s, "HA", None),
        weather_feed: feed_of(&s, "気象庁", Some("05:00 発表".into())),
        warn_feed: feed_of(&s, "気象庁", Some("08:00 発表".into())),
        ..v.clone()
    }
}

/// 起動してまだ何も取れていないところ(`png` の画像)。
pub fn first_demo() -> RoomView {
    let tiles: Vec<RoomTile> = room::demo_view()
        .tiles
        .iter()
        .map(|t| RoomTile {
            label: t.label.clone(),
            entity: format!("sensor.{}", t.label),
            ..RoomTile::default()
        })
        .collect();
    let feeds = RoomFeeds {
        env: Some(env::Shared::default()),
        weather: Some(weather::Shared::default()),
        warnings: Some(HomeShared::default()),
    };
    RoomScreen::new(tiles, feeds).view(&Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weather::{Day, WeekEntry};

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    fn tile(label: &str, entity: &str) -> RoomTile {
        RoomTile {
            label: label.into(),
            entity: entity.into(),
            ..RoomTile::default()
        }
    }

    #[test]
    fn feed_texts() {
        let t = Local::now();
        let ok = FeedStatus {
            last_ok: Some(t),
            ..FeedStatus::default()
        };
        assert_eq!(feed_of(&ok, "HA", None).text, format!("HA {} 取得", hm(&t)));
        assert_eq!(
            feed_of(&ok, "気象庁", Some("11:00 発表".into())).text,
            "気象庁 11:00 発表"
        );
        let stale = FeedStatus {
            failing: true,
            ..ok.clone()
        };
        let f = feed_of(&stale, "気象庁", Some("11:00 発表".into()));
        assert!(f.stale);
        assert_eq!(
            f.text,
            format!("11:00 発表 · 最終取得 {} · 気象庁に接続できません", hm(&t))
        );
        let never = FeedStatus::default();
        assert!(!feed_of(&never, "HA", None).stale);
        assert!(feed_of(&never, "HA", None).text.contains("接続しています"));
        let broken = FeedStatus {
            failing: true,
            error: Some("401".into()),
            ..FeedStatus::default()
        };
        assert_eq!(feed_of(&broken, "HA", None).text, "HAに接続できません(401)");
        assert_eq!(
            issued("2026-10-15T11:00:00+09:00", d(15)).as_deref(),
            Some("11:00 発表")
        );
        assert_eq!(
            issued("2026-10-14T17:00:00+09:00", d(15)).as_deref(),
            Some("10/14 17:00 発表")
        );
        assert_eq!(issued("", d(15)), None);
    }

    #[test]
    fn tiles_format_values_levels_and_notes() {
        let mut data = env::EnvData::default();
        let mut put = |e: &str, v: Option<f64>, u: &str| {
            data.states.insert(e.into(), (v, u.into()));
        };
        put("t", Some(24.8), "°C");
        put("co2", Some(1200.0), "ppm");
        put("p", Some(1016.79), "mbar");
        put("h2", Some(20.3999996185303), "%");
        put("bad", None, "%");
        let mut hist = vec![1010.0f32; 30];
        hist[29] = 1012.5;
        data.history.insert("p".into(), hist);

        let t = sensor_tile(&tile("温度", "t"), Some(&data));
        assert_eq!(
            (t.value.as_str(), t.unit.as_str(), t.level),
            ("24.8", "℃", 0)
        );
        let co2 = RoomTile {
            warn: Some(1000.0),
            alert: Some(1500.0),
            ..tile("CO₂", "co2")
        };
        let t = sensor_tile(&co2, Some(&data));
        assert_eq!(
            (t.value.as_str(), t.level, t.note.as_str()),
            ("1200", 1, "やや高い")
        );
        let p = RoomTile {
            trend_hours: Some(3),
            ..tile("気圧", "p")
        };
        let t = sensor_tile(&p, Some(&data));
        assert_eq!((t.value.as_str(), t.unit.as_str()), ("1016.8", "hPa"));
        assert_eq!(t.note, "↗ 上昇 +2.5", "3 時間前(18 点前)との差");
        assert_eq!(t.history.len(), 30);
        let door = RoomTile {
            second: "h2".into(),
            ..tile("玄関", "t")
        };
        assert_eq!(sensor_tile(&door, Some(&data)).note, "20 %");
        let t = sensor_tile(&tile("欠け", "bad"), Some(&data));
        assert_eq!((t.value.as_str(), t.unit.as_str()), ("--", "%"));
        let t = sensor_tile(&tile("無い", "nothing"), None);
        assert_eq!(
            (t.value.as_str(), t.unit.as_str(), t.history.len()),
            ("--", "", 0)
        );
    }

    #[test]
    fn trends() {
        let mut h = vec![f32::NAN; 40];
        assert_eq!(trend(&h, 3), None, "履歴なし");
        h[39] = 1000.0;
        assert_eq!(trend(&h, 3), None, "3 時間前が欠け");
        h[21] = 1001.2;
        assert_eq!(trend(&h, 3).as_deref(), Some("↘ 下降 -1.2"));
        h[21] = 1000.3;
        assert_eq!(trend(&h, 3).as_deref(), Some("→ 横ばい"));
        assert_eq!(trend(&h[30..], 3), None, "履歴が 3 時間に足りない");
    }

    #[test]
    fn weather_view_drops_past_days_and_fills_tomorrow() {
        let f = Forecast {
            area: "千葉県北西部".into(),
            report: "2026-10-14T17:00:00+09:00".into(),
            days: vec![
                Day {
                    date: Some(d(14)),
                    weather: "晴れ".into(),
                    ..Day::default()
                },
                Day {
                    date: Some(d(15)),
                    weather: "くもり".into(),
                    max: Some(21),
                    min: Some(16),
                    pops: [Some(10), Some(60), Some(30), None],
                },
                Day {
                    date: Some(d(16)),
                    weather: "雨".into(),
                    ..Day::default()
                },
            ],
            week: (15..=22)
                .map(|day| WeekEntry {
                    date: Some(d(day)),
                    weather: if day == 15 {
                        String::new()
                    } else {
                        "晴".into()
                    },
                    max: (day > 15).then_some(20),
                    min: (day > 15).then_some(10),
                    pop: (day > 15).then_some(0),
                })
                .collect(),
        };
        // 日付が変わって 15 日になった(14 日の 17 時発表を出している)
        let w = weather_view(&f, d(15));
        let labels: Vec<&str> = w.days.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["今日 15日(木)", "明日 16日(金)"]);
        let days: Vec<&str> = w.week.iter().map(|x| x.day.as_str()).collect();
        assert_eq!(days, ["16", "17", "18", "19", "20", "21", "22"], "明日から");
        // 14 日のうちに見れば、週間予報の 15 日の空欄は府県予報で埋める
        let w = weather_view(&f, d(14));
        let first = &w.week[0];
        assert_eq!(
            (
                first.day.as_str(),
                first.weather.as_str(),
                first.max,
                first.min,
                first.pop
            ),
            ("15", "くもり", Some(21), Some(16), Some(60))
        );
        assert_eq!(w.week.len(), 7);
        assert_eq!(w.days[0].label, "今日 14日(水)");
    }

    #[test]
    fn view_reports_what_is_missing() {
        let s = RoomScreen::new(Vec::new(), RoomFeeds::default());
        let v = s.view(&Local::now());
        assert!(v.tiles.is_empty() && v.weather.is_none() && v.warnings.is_none());
        assert!(v.weather_feed.text.contains("地域"));
        assert!(v.warn_feed.text.contains("無効"));
        let v = first_demo();
        assert_eq!(v.tiles.len(), 6);
        assert!(v.tiles.iter().all(|t| t.value == "--"));
        assert!(v.env.text.contains("接続しています"));
        assert!(v.warn_feed.text.contains("接続しています"));
    }

    /// 分が変わるか、取得の版が上がったときだけ描き直す。非表示なら描かない。
    #[test]
    fn redraws_on_new_minute_or_new_data() {
        let feeds = RoomFeeds {
            env: Some(env::Shared::default()),
            ..RoomFeeds::default()
        };
        let mut s = RoomScreen::new(vec![tile("温度", "t")], feeds.clone());
        let t0 = Local::now()
            .date_naive()
            .and_hms_opt(8, 42, 5)
            .and_then(|t| t.and_local_timezone(Local).single())
            .unwrap();
        let k0 = s.key(&t0);
        assert_eq!(s.key(&(t0 + chrono::Duration::seconds(30))), k0, "同じ分");
        assert_ne!(
            s.key(&(t0 + chrono::Duration::seconds(60))),
            k0,
            "分が変わった"
        );
        feeds.env.as_ref().unwrap().lock().unwrap().status.ok();
        assert_ne!(s.key(&t0), k0, "取得したら版が上がる");

        let w = World::default();
        let now = Instant::now();
        assert!(!s.tick(&w, now), "非表示");
        s.on_enter(now);
        assert!(s.tick(&w, now), "まだ描いていない");
        let mut text = crate::app::text_engine("");
        let mut c = miharashi_render::Canvas::new(640, 270);
        s.render(&mut RenderContext {
            canvas: &mut c,
            text: &mut text,
            world: &w,
            covered: false,
            force_overlay: false,
        });
        assert!(s.drawn.is_some());
        s.invalidate();
        assert!(s.tick(&w, now));
        s.on_leave(now);
        assert!(!s.tick(&w, now));
    }
}
