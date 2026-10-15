//! 室内環境(docs/DESIGN-screens.md §2.1)。Home Assistant のセンサーの値を別スレッドで定期的に読み、
//! 過去 N 時間の履歴(`/api/history/period`)を数分ごとに読み直して、10 分きざみの点にならして共有する。
//! HA に届かないときは最後の値を残し、取得の状態(`FeedStatus`)だけを変える。
//! ネットワークの応答はすべて欠けうるものとして扱い、読めない項目は飛ばす(パニックさせない)。
use crate::config::{HomeAssistant, Room};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 履歴をならす区間の長さ(秒)。24 時間で 144 点
pub const BUCKET_SECS: i64 = 600;

/// 取得の状態。画面に「HA 16:20 取得」「最終 16:20 取得 · HA に接続できません」と出す。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedStatus {
    /// 最後に取得できた時刻
    pub last_ok: Option<chrono::DateTime<chrono::Local>>,
    /// 直近の取得に失敗している
    pub failing: bool,
    /// 失敗の理由、または取得できない設定の説明
    pub error: Option<String>,
    /// 書き換えるたびに増やす(画面の描き直しの判定)
    pub version: u64,
}

impl FeedStatus {
    /// 取得できた。
    pub fn ok(&mut self) {
        self.last_ok = Some(chrono::Local::now());
        self.failing = false;
        self.error = None;
        self.version += 1;
    }

    /// 取得できなかった。最初の失敗なら true(ログに出す)。
    pub fn fail(&mut self, error: String) -> bool {
        let first = !self.failing;
        self.failing = true;
        self.error = Some(error);
        self.version += 1;
        first
    }
}

/// センサーの値と履歴。
#[derive(Debug, Clone, Default)]
pub struct EnvData {
    /// entity → (値, 単位)。数として読めない state(unavailable など)は None
    pub states: HashMap<String, (Option<f64>, String)>,
    /// entity → 履歴(古い順・`BUCKET_SECS` きざみ)。欠けは NaN
    pub history: HashMap<String, Vec<f32>>,
    pub status: FeedStatus,
}

pub type Shared = Arc<Mutex<EnvData>>;

fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// HA の時刻("2026-10-15T07:12:24.894785+00:00")→ UNIX 秒。
fn unix(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp())
}

/// `GET /api/history/period` の応答(entity ごとの変化の配列。entity_id は各配列の先頭にだけある)を
/// entity → (UNIX 秒, 値) の列にする。
pub fn parse_history(doc: &Value) -> HashMap<String, Vec<(i64, Option<f64>)>> {
    let mut out = HashMap::new();
    let Some(series) = doc.as_array() else {
        return out;
    };
    for s in series.iter().filter_map(Value::as_array) {
        let Some(id) = s
            .first()
            .and_then(|e| e.get("entity_id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let points = s
            .iter()
            .filter_map(|e| {
                let t = unix(e.get("last_changed")?.as_str()?)?;
                Some((t, e.get("state").and_then(Value::as_str).and_then(number)))
            })
            .collect();
        out.insert(id.to_string(), points);
    }
    out
}

/// 変化の列(時刻順)を、`start` から `n` 個の `BUCKET_SECS` 区間の値にならす。
/// HA は変化したときだけ記録するので、区間の値は「区間の始めの値と区間内の変化」の平均にし、
/// 変化の無い区間は直前の値を引き継ぐ。最初の記録より前と、数でない state(unavailable)の間は NaN。
pub fn bucket(changes: &[(i64, Option<f64>)], start: i64, n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    let mut cur: Option<f64> = None;
    let mut i = 0;
    for b in 0..n as i64 {
        let end = start + (b + 1) * BUCKET_SECS;
        let (mut sum, mut cnt) = (cur.unwrap_or(0.0), cur.is_some() as u32);
        while i < changes.len() && changes[i].0 < end {
            cur = changes[i].1;
            if let Some(v) = cur {
                sum += v;
                cnt += 1;
            }
            i += 1;
        }
        out.push(if cnt > 0 {
            (sum / cnt as f64) as f32
        } else {
            f32::NAN
        });
    }
    out
}

/// HA の読み取り専用クライアント。
struct Client {
    agent: ureq::Agent,
    base: String,
    token_file: String,
}

/// 1 つの entity の値。entity が無い(404)なら Ok(None)。
type StateResult = Result<Option<(Option<f64>, String)>, String>;

impl Client {
    fn get(&self, path: &str) -> Result<Option<Value>, String> {
        let token = crate::screen_power::read_token(&self.token_file)?;
        let resp = self
            .agent
            .get(&format!("{}{path}", self.base))
            .header("Authorization", &format!("Bearer {token}"))
            .call();
        let mut resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let text = resp
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    fn state(&self, entity: &str) -> StateResult {
        let Some(v) = self.get(&format!("/api/states/{entity}"))? else {
            return Ok(None);
        };
        let value = v.get("state").and_then(Value::as_str).and_then(number);
        let unit = v
            .get("attributes")
            .and_then(|a| a.get("unit_of_measurement"))
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(Some((value, unit.to_string())))
    }

    /// 過去 `hours` 時間の履歴を `BUCKET_SECS` きざみにならす。
    fn history(
        &self,
        entities: &[String],
        hours: u64,
    ) -> Result<HashMap<String, Vec<f32>>, String> {
        let now = chrono::Utc::now().timestamp();
        let n = (hours as i64 * 3600 / BUCKET_SECS) as usize;
        // 区間の境目を BUCKET_SECS にそろえ、最後の区間が今を含むようにする
        let start = (now / BUCKET_SECS + 1) * BUCKET_SECS - n as i64 * BUCKET_SECS;
        let from = chrono::DateTime::from_timestamp(start, 0)
            .ok_or("時刻を作れない")?
            .format("%Y-%m-%dT%H:%M:%SZ");
        let path = format!(
            "/api/history/period/{from}?filter_entity_id={}&minimal_response&no_attributes",
            entities.join(",")
        );
        let doc = self.get(&path)?.ok_or("history の API が無い(404)")?;
        Ok(parse_history(&doc)
            .into_iter()
            .map(|(id, ch)| (id, bucket(&ch, start, n)))
            .collect())
    }
}

/// 室内センサーの取得を始める。HA の url かタイルが無ければ None。
pub fn start(ha: &HomeAssistant, room: &Room) -> Option<Shared> {
    if ha.url.is_empty() || room.tiles.is_empty() {
        return None;
    }
    let mut entities: Vec<String> = Vec::new();
    for t in &room.tiles {
        for e in [&t.entity, &t.second] {
            if !e.is_empty() && !entities.contains(e) {
                entities.push(e.clone());
            }
        }
    }
    let client = Client {
        agent: ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into(),
        base: ha.url.trim_end_matches('/').to_string(),
        token_file: ha.token_file.clone(),
    };
    let shared: Shared = Arc::default();
    let (out, room) = (shared.clone(), room.clone());
    let spawned = std::thread::Builder::new()
        .name("env".into())
        .spawn(move || run(out, client, entities, room));
    if let Err(e) = spawned {
        eprintln!("env: スレッドを起動できない: {e}");
        return None;
    }
    Some(shared)
}

fn run(out: Shared, client: Client, entities: Vec<String>, room: Room) {
    let poll = Duration::from_secs(room.poll_secs);
    let history_every = Duration::from_secs(room.history_minutes * 60);
    let mut next_history = Instant::now();
    let mut missing: Vec<String> = Vec::new();
    loop {
        let mut states = HashMap::new();
        let mut error = None;
        for e in &entities {
            match client.state(e) {
                Ok(Some(v)) => {
                    states.insert(e.clone(), v);
                }
                Ok(None) => {
                    if !missing.contains(e) {
                        eprintln!("env: HA に entity {e} が無い(設定の room.tiles を確かめる)");
                        missing.push(e.clone());
                    }
                }
                // 届かないときは残りも届かないので、待たずにやめる
                Err(err) => {
                    error = Some(err);
                    break;
                }
            }
        }
        let history = if error.is_none() && Instant::now() >= next_history {
            match client.history(&entities, room.history_hours) {
                Ok(h) => {
                    next_history = Instant::now() + history_every;
                    Some(h)
                }
                Err(e) => {
                    error = Some(format!("履歴: {e}"));
                    None
                }
            }
        } else {
            None
        };
        if let Ok(mut g) = out.lock() {
            match error {
                None => {
                    if g.status.failing {
                        eprintln!("env: HA から読めるようになった");
                    }
                    g.states = states;
                    if let Some(h) = history {
                        g.history = h;
                    }
                    g.status.ok();
                }
                Some(e) => {
                    if g.status.fail(e.clone()) {
                        eprintln!("env: HA から読めない(最後の値を出し、再試行を続ける): {e}");
                    }
                }
            }
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn history_maps_series_by_their_first_entity_id() {
        let doc = json!([
            [
                {"entity_id": "sensor.t", "state": "23.84", "last_changed": "2026-10-14T07:12:28+00:00"},
                {"state": "unavailable", "last_changed": "2026-10-14T07:22:38.688978+00:00"},
                {"state": "24.1", "last_changed": "2026-10-14T07:32:00+00:00"},
                {"state": "25", "last_changed": "壊れた時刻"},
            ],
            [{"state": "1", "last_changed": "2026-10-14T07:12:28+00:00"}],
            [],
            "壊れた行",
        ]);
        let h = parse_history(&doc);
        assert_eq!(h.len(), 1, "entity_id の無い列は飛ばす");
        let t = &h["sensor.t"];
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], (1_791_961_948, Some(23.84)));
        assert_eq!(t[1].1, None, "unavailable は値なし");
        assert!(parse_history(&json!({"x": 1})).is_empty());
    }

    #[test]
    fn buckets_average_carry_forward_and_mark_gaps() {
        let s = 1_000_000 * BUCKET_SECS;
        let ch = [
            (s + 100, Some(10.0)),
            (s + 200, Some(20.0)),
            // 区間 1・2 は変化なし → 20 を引き継ぐ
            (s + 3 * BUCKET_SECS + 10, None),
            // 区間 4 は unavailable のまま → NaN
            (s + 5 * BUCKET_SECS + 10, Some(30.0)),
        ];
        let b = bucket(&ch, s - BUCKET_SECS, 7);
        assert!(b[0].is_nan(), "最初の記録より前");
        assert_eq!(b[1], 15.0);
        assert_eq!((b[2], b[3]), (20.0, 20.0));
        assert_eq!(b[4], 20.0, "区間の始めの値(20)だけ。unavailable は数えない");
        assert!(b[5].is_nan());
        assert_eq!(b[6], 30.0);
        assert!(bucket(&[], s, 3).iter().all(|v| v.is_nan()));
    }

    #[test]
    fn feed_status_logs_only_the_first_failure() {
        let mut f = FeedStatus::default();
        assert!(f.fail("x".into()));
        assert!(!f.fail("y".into()));
        assert!(f.failing && f.last_ok.is_none());
        f.ok();
        assert!(!f.failing && f.last_ok.is_some() && f.error.is_none());
        assert_eq!(f.version, 3);
    }
}
