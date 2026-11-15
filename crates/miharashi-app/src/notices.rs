//! 通知の帳簿(Issue #101)。だいだいフィードのカードを操作パネルへ出す「通知」にして、rev・既読・
//! 最初に見た時刻を持つ。操作パネルの `/api/v1/status` と SSE の `daidai` ブロック、`POST /api/v1/daidai/ack` の元。
//!
//! 取得のスレッド(`daidai::start`)が取得のたびに `observe` する。画面が出ていなくても動く。
//! 内容が変わったとき(通知の追加・消滅・期限切れ・既読)だけ `rev` を 1 増やし、状態を書き、パネルへ
//! 「今すぐ送る」を知らせる。`age_s` は状態ではなく、`block` で組み立てるときに計算する。
//!
//! ロック: `inner`(帳簿の中身)を持ったまま `persist` と `waker` のロックを取らない。書き込みは
//! `persist` を取ってから `inner` を短く取る(順序は persist → inner だけ)。知らせるのは `inner` を
//! 外してから(パネルの state のロックは別のスレッドが inner より先に取る: state → inner)。
use crate::daidai::{DaidaiSnapshot, is_notice_id};
use chrono::{DateTime, Utc};
use miharashi_render::daidai::Pose;
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 操作パネルへ出す通知の数の上限(未読の数 `unseen` は窓の外も数える)。
pub const MAX_NOTICES: usize = 5;
/// 覚えておく id の数の上限(今フィードにある id は捨てない)。
const MAX_KNOWN: usize = 256;
/// 状態ファイルを読む大きさの上限。
const MAX_STATE_BYTES: u64 = 256 * 1024;

/// 知らせる先(パネルの SSE を今すぐ進める)。
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// 今の通知 1 件(`age_s` を除く)。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: String,
    title: String,
    body: String,
    low: bool,
    /// 通知の時刻(UNIX 秒)。`posted_at`、無ければホストが最初に見た時刻
    at: i64,
}

/// 既知の id 1 件。フィードから消えても、上限までは残す。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Known {
    first_seen: i64,
    read: bool,
}

/// パネルに出す内容(`age_s` を除く)。これが変わったときだけ rev を進める。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    rows: Vec<Row>,
    unseen: usize,
    pose: Pose,
}

struct Inner {
    rev: u64,
    known: HashMap<String, Known>,
    /// 今の通知(条件を満たす全部。新しい順、同じ時刻ならフィードの順)
    rows: Vec<Row>,
    pose: Pose,
    /// 最後に rev へ反映した内容。None = まだ一度も `observe` していない
    last: Option<Signature>,
}

impl Inner {
    fn unseen(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| !self.known.get(&r.id).is_some_and(|k| k.read))
            .count()
    }

    fn signature(&self) -> Signature {
        Signature {
            // パネルに出す窓(先頭 5 件)だけ。6 件目以降の中身が変わっても、出る内容は同じ
            rows: self.rows.iter().take(MAX_NOTICES).cloned().collect(),
            unseen: self.unseen(),
            pose: self.pose,
        }
    }

    /// 内容が前回と違えば rev を進める。進めたか。
    fn commit(&mut self) -> bool {
        let sig = self.signature();
        if self.last.as_ref() == Some(&sig) {
            return false;
        }
        self.last = Some(sig);
        self.rev += 1;
        true
    }

    /// 覚えている id を上限まで減らす。今フィードにある id は残し、残りは古い(最初に見た)順に捨てる。
    fn trim_known(&mut self) {
        if self.known.len() <= MAX_KNOWN {
            return;
        }
        let mut gone: Vec<(i64, String)> = self
            .known
            .iter()
            .filter(|(id, _)| !self.rows.iter().any(|r| &r.id == *id))
            .map(|(id, k)| (k.first_seen, id.clone()))
            .collect();
        gone.sort();
        let excess = self.known.len() - MAX_KNOWN;
        for (_, id) in gone.into_iter().take(excess) {
            self.known.remove(&id);
        }
    }
}

/// 状態ファイルへの書き込み。`path` が None ならメモリだけ。
struct Persist {
    path: Option<PathBuf>,
    /// 直近の失敗の理由(同じ理由を毎回は出さない)
    last_error: Option<String>,
}

/// 通知の帳簿。`Arc` で取得のスレッドとパネルが共有する。
pub struct NoticeBoard {
    inner: Mutex<Inner>,
    persist: Mutex<Persist>,
    waker: Mutex<Option<Waker>>,
}

impl NoticeBoard {
    /// `path` の状態を読んで始める。rev は `max(保存してあった rev, now_unix)` から。
    /// ファイルが無い・壊れている・大きすぎるときは空から始める(rev は UNIX 秒から)。
    pub fn new(path: Option<PathBuf>, now_unix: i64) -> Self {
        let (saved_rev, known) = match &path {
            Some(p) => match load(p) {
                Ok(v) => v,
                Err(why) => {
                    if why != LoadError::Missing {
                        eprintln!(
                            "daidai: 通知の状態を読めない(空から始める): {}: {why}",
                            p.display()
                        );
                    }
                    (0, HashMap::new())
                }
            },
            None => (0, HashMap::new()),
        };
        let rev = saved_rev.max(u64::try_from(now_unix).unwrap_or(0));
        Self {
            inner: Mutex::new(Inner {
                rev,
                known,
                rows: Vec::new(),
                pose: Pose::Idle,
                last: None,
            }),
            persist: Mutex::new(Persist {
                path,
                last_error: None,
            }),
            waker: Mutex::new(None),
        }
    }

    /// 変化を知らせる先を登録する(パネルが作り直されたら置き換える)。
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    /// 取得に成功したフィードを帳簿に反映する。`now` で期限と最初に見た時刻を決める。
    /// 取得に失敗した周期では呼ばない(一時的な失敗で通知を消さない)。変わったか。
    pub fn observe(&self, snap: &DaidaiSnapshot, now: DateTime<Utc>) -> bool {
        let now_unix = now.timestamp();
        let changed = {
            let mut g = self.inner.lock().unwrap();
            // 同じ id は最初の 1 枚だけ。そのあと期限の切れたカードを除く
            let mut seen_ids: Vec<&str> = Vec::new();
            let mut cards = Vec::new();
            for c in &snap.cards {
                let Some(n) = &c.notice else { continue };
                if seen_ids.contains(&n.id.as_str()) {
                    continue;
                }
                seen_ids.push(&n.id);
                cards.push((c, n));
            }
            let mut rows: Vec<Row> = Vec::new();
            for (c, n) in cards {
                if c.expires_at.is_some_and(|e| e <= now) {
                    continue;
                }
                let first_seen = g
                    .known
                    .entry(n.id.clone())
                    .or_insert(Known {
                        first_seen: now_unix,
                        read: false,
                    })
                    .first_seen;
                rows.push(Row {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    body: n.body.clone(),
                    low: n.low,
                    at: n.posted_at.unwrap_or(first_seen),
                });
            }
            // 安定ソート: 同じ時刻はフィードの順のまま
            rows.sort_by_key(|r| Reverse(r.at));
            g.rows = rows;
            g.pose = snap.pose;
            g.trim_known();
            g.commit()
        };
        if changed {
            self.persist();
            self.notify();
        }
        changed
    }

    /// `id` とそれより古い今の通知を全部既読にする。今の通知に無い id・既に既読のものだけなら何もしない。
    /// 変わったか。
    pub fn ack(&self, id: &str) -> bool {
        let changed = {
            let mut g = self.inner.lock().unwrap();
            let Some(pos) = g.rows.iter().position(|r| r.id == id) else {
                return false;
            };
            let ids: Vec<String> = g.rows[pos..].iter().map(|r| r.id.clone()).collect();
            let mut flipped = false;
            for i in ids {
                if let Some(k) = g.known.get_mut(&i)
                    && !k.read
                {
                    k.read = true;
                    flipped = true;
                }
            }
            flipped && g.commit()
        };
        if changed {
            self.persist();
            self.notify();
        }
        changed
    }

    /// `/status` の `daidai` ブロック。最初の `observe` が終わるまでは None。`now_unix` で `age_s` を決める。
    pub fn block(&self, now_unix: i64) -> Option<Value> {
        let g = self.inner.lock().unwrap();
        g.last.as_ref()?;
        let notices: Vec<Value> = g
            .rows
            .iter()
            .take(MAX_NOTICES)
            .map(|r| {
                json!({
                    "id": r.id,
                    "title": r.title,
                    "body": r.body,
                    "age_s": (now_unix - r.at).max(0),
                    "priority": if r.low { "low" } else { "normal" },
                })
            })
            .collect();
        Some(json!({
            "rev": g.rev,
            "notices": notices,
            "unseen": g.unseen(),
            "ack": true,
            "avatar_pose": g.pose.name(),
        }))
    }

    /// 今の rev(テスト用)。
    #[cfg(test)]
    pub fn rev(&self) -> u64 {
        self.inner.lock().unwrap().rev
    }

    fn notify(&self) {
        let waker = self.waker.lock().unwrap().clone();
        if let Some(w) = waker {
            w();
        }
    }

    /// 状態を書く。`persist` を持ったまま帳簿の中身を写すので、書き込みの順番が前後しない。
    fn persist(&self) {
        let mut p = self.persist.lock().unwrap();
        let Some(path) = p.path.clone() else { return };
        let text = {
            let g = self.inner.lock().unwrap();
            state_json(g.rev, &g.known)
        };
        let result = save(&path, &text);
        match (&result, &p.last_error) {
            (Err(e), last) if last.as_ref() != Some(e) => {
                eprintln!(
                    "daidai: 通知の状態を書けない(メモリの状態で続ける): {}: {e}",
                    path.display()
                );
            }
            (Ok(()), Some(_)) => eprintln!("daidai: 通知の状態を書けた"),
            _ => {}
        }
        p.last_error = result.err();
    }
}

fn state_json(rev: u64, known: &HashMap<String, Known>) -> String {
    let mut ids: Vec<(&String, &Known)> = known.iter().collect();
    ids.sort_by(|a, b| a.1.first_seen.cmp(&b.1.first_seen).then(a.0.cmp(b.0)));
    json!({
        "version": 1,
        "rev": rev,
        "known": ids.iter().map(|(id, k)| json!({
            "id": id,
            "first_seen": k.first_seen,
            "read": k.read,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

#[derive(Debug, PartialEq, Eq)]
enum LoadError {
    Missing,
    Other(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Missing => write!(f, "ファイルが無い"),
            LoadError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// 状態ファイルを読む。大きさに上限を設け、形の不正な項目は捨てる(全体が読めなければ Err)。
fn load(path: &Path) -> Result<(u64, HashMap<String, Known>), LoadError> {
    let f = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            LoadError::Missing
        } else {
            LoadError::Other(e.to_string())
        }
    })?;
    let mut text = String::new();
    f.take(MAX_STATE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| LoadError::Other(e.to_string()))?;
    if text.len() as u64 > MAX_STATE_BYTES {
        return Err(LoadError::Other(format!(
            "{MAX_STATE_BYTES} バイトを超えている"
        )));
    }
    parse_state(&text).map_err(LoadError::Other)
}

fn parse_state(text: &str) -> Result<(u64, HashMap<String, Known>), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON が不正: {e}"))?;
    if v.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("version が 1 でない".into());
    }
    let rev = v
        .get("rev")
        .and_then(Value::as_u64)
        .ok_or("rev が無い・整数でない")?;
    let mut known = HashMap::new();
    for e in v
        .get("known")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_KNOWN)
    {
        let (Some(id), Some(first_seen)) = (
            e.get("id").and_then(Value::as_str),
            e.get("first_seen").and_then(Value::as_i64),
        ) else {
            continue;
        };
        if !is_notice_id(id) {
            continue;
        }
        known.insert(
            id.to_string(),
            Known {
                first_seen,
                read: e.get("read").and_then(Value::as_bool).unwrap_or(false),
            },
        );
    }
    Ok((rev, known))
}

/// 一時ファイル(0600)に書いてから置き換える(`home.rs` と同じ書き方)。
fn save(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // 前に残った一時ファイルを開いたときも 0600 にする
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daidai::parse_feed;

    const T0: i64 = 1_794_700_000;

    fn at(unix: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(unix, 0).unwrap()
    }

    fn board() -> NoticeBoard {
        NoticeBoard::new(None, T0)
    }

    /// カード 1 枚の JSON
    fn card(id: &str, title: &str, extra: &str) -> String {
        format!(r#"{{"id":"{id}","title":"{title}","body":"本文{id}"{extra}}}"#)
    }

    fn feed(cards: &[String]) -> DaidaiSnapshot {
        parse_feed(&format!(
            r#"{{"version":1,"avatar_pose":"working","cards":[{}]}}"#,
            cards.join(",")
        ))
    }

    /// JSON の文字列リテラル(Rust の Debug 表記は `\u{202e}` のように JSON で読めない形になる)
    fn js(s: &str) -> String {
        serde_json::to_string(s).unwrap()
    }

    fn ids(b: &NoticeBoard, now: i64) -> Vec<String> {
        b.block(now).unwrap()["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("miharashi-notices-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn no_block_until_the_first_observe_and_that_observe_always_counts() {
        let b = board();
        assert!(b.block(T0).is_none());
        assert_eq!(b.rev(), T0 as u64);
        assert!(b.observe(&feed(&[]), at(T0)), "空のフィードでも最初は変化");
        assert_eq!(b.rev(), T0 as u64 + 1);
        let v = b.block(T0).unwrap();
        assert_eq!(v["notices"], json!([]));
        assert_eq!(v["unseen"], 0);
        assert_eq!(v["ack"], true);
    }

    #[test]
    fn the_same_feed_again_changes_nothing() {
        let dir = tmp_dir("same");
        let path = dir.join("n.json");
        let b = NoticeBoard::new(Some(path.clone()), T0);
        let f = feed(&[card("a", "A", "")]);
        assert!(b.observe(&f, at(T0)));
        let rev = b.rev();
        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        for dt in [1, 5, 600] {
            assert!(!b.observe(&f, at(T0 + dt)));
        }
        assert_eq!(b.rev(), rev);
        assert!(!path.exists(), "変化が無ければ書かない");
        // age_s だけは進む
        assert_eq!(b.block(T0 + 600).unwrap()["notices"][0]["age_s"], 600);
        assert!(written.contains(r#""rev""#));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_remove_expire_and_edit_bump_the_rev() {
        let b = board();
        b.observe(&feed(&[card("a", "A", "")]), at(T0));
        let r0 = b.rev();
        // 追加
        assert!(b.observe(&feed(&[card("a", "A", ""), card("b", "B", "")]), at(T0 + 1)));
        assert_eq!(b.rev(), r0 + 1);
        // 本文の変更
        assert!(b.observe(
            &feed(&[card("a", "A2", ""), card("b", "B", "")]),
            at(T0 + 2)
        ));
        assert_eq!(b.rev(), r0 + 2);
        // priority の変更
        assert!(b.observe(
            &feed(&[card("a", "A2", r#","priority":"low""#), card("b", "B", "")]),
            at(T0 + 3)
        ));
        assert_eq!(b.rev(), r0 + 3);
        // ポーズの変更
        let mut f = feed(&[card("a", "A2", r#","priority":"low""#), card("b", "B", "")]);
        f.pose = Pose::Making;
        assert!(b.observe(&f, at(T0 + 4)));
        assert_eq!(b.block(T0 + 4).unwrap()["avatar_pose"], "making");
        // 消滅
        assert!(b.observe(&feed(&[card("b", "B", "")]), at(T0 + 5)));
        assert_eq!(ids(&b, T0 + 5), ["b"]);
        // 期限切れ(ファイルは同じ。時刻だけ進む)
        let exp = feed(&[card(
            "b",
            "B",
            &format!(r#","expires_at":"{}""#, at(T0 + 100).to_rfc3339()),
        )]);
        assert!(
            !b.observe(&exp, at(T0 + 6)),
            "期限を書き足しただけでは変わらない"
        );
        assert_eq!(ids(&b, T0 + 6), ["b"]);
        assert!(!b.observe(&exp, at(T0 + 99)));
        assert!(b.observe(&exp, at(T0 + 100)), "期限ちょうどは切れた");
        assert!(ids(&b, T0 + 100).is_empty());
    }

    #[test]
    fn only_explicit_valid_unique_ids_are_notices() {
        let b = board();
        let long = "x".repeat(65);
        let f = parse_feed(&format!(
            r#"{{"version":1,"cards":[
              {{"title":"no id"}},
              {{"id":"ok-1.a:b_c","title":"T1"}},
              {{"id":"ok-1.a:b_c","title":"duplicate"}},
              {{"id":"has space","title":"T2"}},
              {{"id":" pad ","title":"T3"}},
              {{"id":"{long}","title":"T4"}},
              {{"id":"日本語","title":"T5"}},
              {{"id":"","title":"T6"}},
              {{"id":"card-9","title":"T7"}},
              {{"id":{max:?},"title":"T8"}}
            ]}}"#,
            max = "y".repeat(64)
        ));
        b.observe(&f, at(T0));
        let v = b.block(T0).unwrap();
        let titles: Vec<&str> = v["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["title"].as_str().unwrap())
            .collect();
        // card-9 は明示の id(正規表現に合う)なので通知。`card-{index}` の補完だけが除外される
        assert_eq!(titles.len(), 3, "{titles:?}");
        assert!(titles.contains(&"T1") && titles.contains(&"T7") && titles.contains(&"T8"));
        assert!(!titles.contains(&"duplicate"), "id の重複は最初の 1 枚だけ");
        // 補完される id(card-{index})は通知にならない
        let b = board();
        b.observe(&feed(&[r#"{"title":"no id"}"#.into()]), at(T0));
        assert!(ids(&b, T0).is_empty());
        // 補完の id と同じ形の明示 id は、書かれていれば通知
        let b = board();
        b.observe(&feed(&[r#"{"id":"card-0","title":"x"}"#.into()]), at(T0));
        assert_eq!(ids(&b, T0), ["card-0"]);
    }

    #[test]
    fn titles_and_bodies_are_cut_by_scalar_values_and_cleaned() {
        let b = board();
        let title = format!("{}{}", "あ".repeat(39), "🙂🙂🙂");
        let body = "ん".repeat(300);
        let f = feed(&[format!(
            r#"{{"id":"a","title":{},"body":{}}}"#,
            js(&title),
            js(&body)
        )]);
        b.observe(&f, at(T0));
        let n = &b.block(T0).unwrap()["notices"][0];
        assert_eq!(n["title"].as_str().unwrap().chars().count(), 40);
        assert!(n["title"].as_str().unwrap().ends_with('🙂'));
        assert_eq!(n["body"].as_str().unwrap().chars().count(), 200);

        // 双方向制御・ゼロ幅は除き、制御文字(改行)は空白にする。画面用の title はそのまま残る
        let dirty =
            "\u{202E}お\u{200B}は\u{2066}よ\u{FEFF}う\u{061C}\u{200E}\u{200F}\u{2060}\nね\u{2069}";
        let f = feed(&[format!(
            r#"{{"id":"a","title":{},"body":"b\u0007c"}}"#,
            js(dirty)
        )]);
        b.observe(&f, at(T0 + 1));
        let n = &b.block(T0 + 1).unwrap()["notices"][0];
        assert_eq!(n["title"], "おはよう ね");
        assert_eq!(n["body"], "b c");

        // 先頭に除去する文字が並んでも、40 文字ぶんの中身が残る
        let lead = format!("{}{}", "\u{200B}".repeat(100), "a".repeat(50));
        let f = feed(&[format!(r#"{{"id":"a","title":{}}}"#, js(&lead))]);
        b.observe(&f, at(T0 + 2));
        let n = &b.block(T0 + 2).unwrap()["notices"][0];
        assert_eq!(n["title"].as_str().unwrap(), "a".repeat(40));

        // title も body も空になるカードは通知にしない
        let f = feed(&[
            r#"{"id":"a","title":"\u200b\u202e","body":"\u200b"}"#.into(),
            card("z", "Z", ""),
        ]);
        b.observe(&f, at(T0 + 3));
        assert_eq!(ids(&b, T0 + 3), ["z"]);
        // title だけ空で body がある通知は残る
        let f = feed(&[r#"{"id":"a","title":"\u200b","body":"本文"}"#.into()]);
        b.observe(&f, at(T0 + 4));
        let n = &b.block(T0 + 4).unwrap()["notices"][0];
        assert_eq!(
            (n["title"].as_str(), n["body"].as_str()),
            (Some(""), Some("本文"))
        );
    }

    #[test]
    fn priority_low_only_when_written_exactly() {
        let b = board();
        let f = feed(&[
            card("a", "A", r#","priority":"low""#),
            card("b", "B", r#","priority":"high""#),
            card("c", "C", r#","priority":"LOW""#),
            card("d", "D", r#","priority":3"#),
            card("e", "E", ""),
        ]);
        b.observe(&f, at(T0));
        let v = b.block(T0).unwrap();
        let p: Vec<&str> = v["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["priority"].as_str().unwrap())
            .collect();
        assert_eq!(p, ["low", "normal", "normal", "normal", "normal"]);
    }

    #[test]
    fn age_and_order_follow_posted_at_then_first_seen() {
        let b = board();
        let p = |unix: i64| format!(r#","posted_at":"{}""#, at(unix).to_rfc3339());
        let f = feed(&[
            card("old", "old", &p(T0 - 3000)),
            card("new", "new", &p(T0 - 10)),
            card("noposted", "np", ""),
            card("future", "fu", &p(T0 + 500)),
            card("bad", "bad", r#","posted_at":"yesterday""#),
        ]);
        b.observe(&f, at(T0));
        // 未来の posted_at は一番新しいが age_s は 0。posted_at が無い・読めないものは first_seen(= T0)
        assert_eq!(ids(&b, T0), ["future", "noposted", "bad", "new", "old"]);
        let v = b.block(T0 + 20).unwrap();
        let age: Vec<i64> = v["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["age_s"].as_i64().unwrap())
            .collect();
        assert_eq!(age, [0, 20, 20, 30, 3020]);
        // 同じ時刻はフィードの順
        let f = feed(&[card("b", "b", &p(T0)), card("a", "a", &p(T0))]);
        b.observe(&f, at(T0 + 1));
        assert_eq!(ids(&b, T0 + 1), ["b", "a"]);
        // 後から来たものは first_seen が新しい
        let f = feed(&[
            card("b", "b", &p(T0)),
            card("a", "a", &p(T0)),
            card("n", "n", ""),
        ]);
        b.observe(&f, at(T0 + 50));
        assert_eq!(ids(&b, T0 + 50)[0], "n");
        assert_eq!(b.block(T0 + 60).unwrap()["notices"][0]["age_s"], 10);
    }

    #[test]
    fn the_window_is_five_but_unseen_counts_all() {
        let b = board();
        let cards: Vec<String> = (0..8)
            .map(|i| {
                card(
                    &format!("n{i}"),
                    "t",
                    &format!(r#","posted_at":"{}""#, at(T0 - i).to_rfc3339()),
                )
            })
            .collect();
        b.observe(&feed(&cards), at(T0));
        let v = b.block(T0).unwrap();
        assert_eq!(v["notices"].as_array().unwrap().len(), 5);
        assert_eq!(v["unseen"], 8);
        assert_eq!(ids(&b, T0), ["n0", "n1", "n2", "n3", "n4"]);
        // 窓の外の通知を ack すると、それより古い(窓の外)ものだけが既読になる
        assert!(b.ack("n6"));
        assert_eq!(b.block(T0).unwrap()["unseen"], 6);
    }

    #[test]
    fn a_change_outside_the_window_alone_does_not_bump_the_rev() {
        let b = board();
        let p = |unix: i64| format!(r#","posted_at":"{}""#, at(unix).to_rfc3339());
        let cards = |tail: &str| -> Vec<String> {
            (0..7)
                .map(|i| {
                    card(
                        &format!("n{i}"),
                        if i == 6 { tail } else { "t" },
                        &p(T0 - i),
                    )
                })
                .collect()
        };
        b.observe(&feed(&cards("old")), at(T0));
        let rev = b.rev();
        // 7 件目(窓の外)の title だけ変わる: パネルに出る内容は同じ
        assert!(!b.observe(&feed(&cards("new")), at(T0 + 1)));
        assert_eq!(b.rev(), rev);
        // 窓の中が変われば増える
        let mut c = cards("new");
        c[0] = card("n0", "changed", &p(T0));
        assert!(b.observe(&feed(&c), at(T0 + 2)));
    }

    #[test]
    fn ack_reads_that_id_and_everything_older_and_is_idempotent() {
        let b = board();
        let p = |unix: i64| format!(r#","posted_at":"{}""#, at(unix).to_rfc3339());
        b.observe(
            &feed(&[
                card("c", "c", &p(T0 - 10)),
                card("b", "b", &p(T0 - 20)),
                card("a", "a", &p(T0 - 30)),
            ]),
            at(T0),
        );
        let rev = b.rev();
        assert!(!b.ack("nope"), "今の通知に無い id");
        assert!(!b.ack("card-0"));
        assert_eq!(b.rev(), rev);
        assert!(b.ack("b"), "b と、それより古い a");
        assert_eq!(b.rev(), rev + 1);
        assert_eq!(b.block(T0).unwrap()["unseen"], 1, "新しい c だけ未読");
        assert!(!b.ack("b"), "既読");
        assert!(!b.ack("a"), "既読");
        assert_eq!(b.rev(), rev + 1);
        assert!(b.ack("c"));
        assert_eq!(b.block(T0).unwrap()["unseen"], 0);
        assert_eq!(b.rev(), rev + 2);
        // 既読は覚えている: 同じ通知が内容を変えても未読に戻らない
        b.observe(&feed(&[card("c", "c2", &p(T0 - 10))]), at(T0 + 1));
        assert_eq!(b.block(T0 + 1).unwrap()["unseen"], 0);
        // 消えて戻っても既読のまま
        b.observe(&feed(&[]), at(T0 + 2));
        b.observe(&feed(&[card("c", "c2", &p(T0 - 10))]), at(T0 + 3));
        assert_eq!(b.block(T0 + 3).unwrap()["unseen"], 0);
        // 新しい通知は未読
        b.observe(
            &feed(&[card("d", "d", ""), card("c", "c2", &p(T0 - 10))]),
            at(T0 + 4),
        );
        assert_eq!(b.block(T0 + 4).unwrap()["unseen"], 1);
    }

    #[test]
    fn expired_and_gone_notices_do_not_count_as_unseen() {
        let b = board();
        b.observe(&feed(&[card("a", "a", ""), card("b", "b", "")]), at(T0));
        assert_eq!(b.block(T0).unwrap()["unseen"], 2);
        b.observe(&feed(&[card("a", "a", "")]), at(T0 + 1));
        assert_eq!(b.block(T0).unwrap()["unseen"], 1);
    }

    #[test]
    fn known_ids_are_capped_but_the_ones_in_the_feed_stay() {
        let b = board();
        for i in 0..300 {
            b.observe(&feed(&[card(&format!("old{i}"), "t", "")]), at(T0 + i));
        }
        {
            let g = b.inner.lock().unwrap();
            assert_eq!(g.known.len(), MAX_KNOWN);
            assert!(g.known.contains_key("old299"), "新しい方を残す");
            assert!(!g.known.contains_key("old0"));
        }
        // 古い id が捨てられても、今フィードにあるものは捨てない
        let cards: Vec<String> = (0..16).map(|i| card(&format!("cur{i}"), "t", "")).collect();
        b.observe(&feed(&cards), at(T0 + 1000));
        let g = b.inner.lock().unwrap();
        assert!(g.known.len() <= MAX_KNOWN);
        assert!((0..16).all(|i| g.known.contains_key(&format!("cur{i}"))));
    }

    #[test]
    fn state_round_trips_and_rev_never_goes_back() {
        let dir = tmp_dir("rt");
        let path = dir.join("n.json");
        let p = |unix: i64| format!(r#","posted_at":"{}""#, at(unix).to_rfc3339());
        let f = feed(&[card("a", "a", &p(T0 - 5)), card("b", "b", &p(T0 - 9))]);
        let b = NoticeBoard::new(Some(path.clone()), T0);
        b.observe(&f, at(T0));
        assert!(b.ack("a"));
        let rev = b.rev();
        drop(b);
        // 0600 で書いてある
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // 再起動(時計が少し戻っていても rev は戻らない)
        let b = NoticeBoard::new(Some(path.clone()), T0 - 100);
        assert!(b.rev() >= rev, "{} < {rev}", b.rev());
        assert!(b.block(T0).is_none(), "最初の observe まで出さない");
        assert!(b.observe(&f, at(T0 + 10)), "最初の observe は必ず変化");
        assert!(b.rev() > rev);
        assert_eq!(b.block(T0 + 10).unwrap()["unseen"], 0, "既読を覚えている");
        // first_seen も覚えている(posted_at の無い通知)
        let g = feed(&[card("p", "p", "")]);
        b.observe(&g, at(T0 + 20));
        drop(b);
        let b = NoticeBoard::new(Some(path.clone()), T0);
        b.observe(&g, at(T0 + 999));
        assert_eq!(b.block(T0 + 999).unwrap()["notices"][0]["age_s"], 979);
        drop(b);

        // 保存してあった rev が時計より先なら、そこから
        std::fs::write(&path, r#"{"version":1,"rev":9999999999,"known":[]}"#).unwrap();
        let b = NoticeBoard::new(Some(path.clone()), T0);
        assert_eq!(b.rev(), 9_999_999_999);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_broken_oversized_or_unwritable_state_still_keeps_rev_monotonic() {
        let dir = tmp_dir("bad");
        let path = dir.join("n.json");
        // ファイルなし
        let b = NoticeBoard::new(Some(path.clone()), T0);
        assert_eq!(b.rev(), T0 as u64);
        // 壊れている・形が違う・大きすぎる
        for text in [
            "".to_string(),
            "{".to_string(),
            "[]".to_string(),
            r#"{"version":2,"rev":5,"known":[]}"#.into(),
            r#"{"version":1,"rev":"5","known":[]}"#.into(),
            r#"{"version":1,"rev":-5,"known":[]}"#.into(),
            r#"{"version":1,"rev":5.5,"known":[]}"#.into(),
            " ".repeat(MAX_STATE_BYTES as usize + 1),
        ] {
            std::fs::write(&path, &text).unwrap();
            let b = NoticeBoard::new(Some(path.clone()), T0);
            assert_eq!(b.rev(), T0 as u64, "{:?}", &text[..text.len().min(30)]);
        }
        // 項目の一部が不正でも、読める項目は使う
        std::fs::write(
            &path,
            r#"{"version":1,"rev":7,"known":[{"id":"ok","first_seen":5,"read":true},{"id":"bad id","first_seen":1},{"id":"x"},3]}"#,
        )
        .unwrap();
        let b = NoticeBoard::new(Some(path.clone()), 3);
        assert_eq!(b.rev(), 7);
        assert_eq!(b.inner.lock().unwrap().known.len(), 1);

        // 書けない置き場でも続く(rev は進む。再起動後は時計から始まる)
        let bad = dir.join("no-such-dir").join("n.json");
        let b = NoticeBoard::new(Some(bad.clone()), T0);
        assert!(b.observe(&feed(&[card("a", "a", "")]), at(T0)));
        assert!(b.ack("a"));
        assert_eq!(b.rev(), T0 as u64 + 2);
        assert!(!bad.exists());
        assert_eq!(b.block(T0).unwrap()["unseen"], 0);
        let b2 = NoticeBoard::new(Some(bad), T0 + 1);
        assert!(b2.rev() > T0 as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_path_resolution() {
        use crate::daidai::notice_state_path;
        use std::ffi::OsStr;
        assert_eq!(
            notice_state_path("/x/n.json", Some(OsStr::new("/var/lib/m"))),
            Some(PathBuf::from("/x/n.json"))
        );
        assert_eq!(
            notice_state_path("", Some(OsStr::new("/var/lib/m"))),
            Some(PathBuf::from("/var/lib/m/daidai-notices.json"))
        );
        assert_eq!(notice_state_path("", None), None);
        assert_eq!(notice_state_path("", Some(OsStr::new(""))), None);
    }

    #[test]
    fn a_failed_poll_cycle_keeps_the_notices() {
        use crate::daidai::{FeedSource, poll_once};
        struct Flaky(Vec<Result<String, String>>);
        impl FeedSource for Flaky {
            fn fetch(&mut self) -> Result<String, String> {
                self.0.remove(0)
            }
        }
        let good = r#"{"version":1,"cards":[{"id":"a","title":"A"}]}"#.to_string();
        let mut src = Flaky(vec![Ok(good.clone()), Err("io".into()), Ok(good)]);
        let b = board();
        // daidai::start のスレッドと同じ手順: 失敗した周期は observe しない
        let mut cycle = |n: i64| {
            let (snap, err) = poll_once(&mut src);
            if err.is_none() {
                b.observe(&snap, at(T0 + n));
            }
        };
        cycle(0);
        let rev = b.rev();
        cycle(1);
        assert_eq!(ids(&b, T0 + 1), ["a"], "取得に失敗した周期で通知が消えない");
        assert_eq!(b.rev(), rev);
        cycle(2);
        assert_eq!(b.rev(), rev, "戻っても同じ内容なら rev は変わらない");
    }

    #[test]
    fn the_waker_runs_after_a_change_without_holding_the_board_lock() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let b = Arc::new(board());
        let n = Arc::new(AtomicUsize::new(0));
        let (b2, n2) = (b.clone(), n.clone());
        b.set_waker(Arc::new(move || {
            // 知らせの中で帳簿を読めること(ロックを持ったまま呼んでいない)
            let _ = b2.block(T0);
            n2.fetch_add(1, Ordering::SeqCst);
        }));
        let f = feed(&[card("a", "a", "")]);
        b.observe(&f, at(T0));
        b.observe(&f, at(T0 + 1));
        assert_eq!(n.load(Ordering::SeqCst), 1, "変化が無ければ知らせない");
        b.ack("a");
        b.ack("a");
        assert_eq!(n.load(Ordering::SeqCst), 2);
        // 置き換え
        let m = Arc::new(AtomicUsize::new(0));
        let m2 = m.clone();
        b.set_waker(Arc::new(move || {
            m2.fetch_add(1, Ordering::SeqCst);
        }));
        b.observe(&feed(&[]), at(T0 + 2));
        assert_eq!((n.load(Ordering::SeqCst), m.load(Ordering::SeqCst)), (2, 1));
    }
}
