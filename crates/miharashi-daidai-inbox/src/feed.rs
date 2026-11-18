//! フィードファイルへの書き込み。既存のフィードを壊さないことを最優先にする。
//!
//! 手順(直列化した中で): ロック → 既存のフィードを読む → 期限切れを除く → 先頭に新しいカードを入れる →
//! 同じディレクトリの一時ファイルに書いて `fsync` → `rename` → ディレクトリを `fsync`。
//!
//! - ロックは、プロセス内の `Mutex` と、フィードと同じディレクトリの `<フィードのファイル名>.lock` への
//!   `flock(LOCK_EX)`(ほかの書き手が同じ作法に従えば協調できる。ロックファイルは 0600 で作り、消さない)。
//! - 既存のフィードが読めない・不正なら書き換えない(`FeedError::Unreadable`)。
//! - 既存のカードと、知らない最上位のキーは `serde_json::Value` のまま残す。
//! - 読み手(`miharashi-app` の `daidai::parse_feed`)は先頭 16 枚しか取り込まないので、新しいカードは先頭に入れ、
//!   有効なカードがすでに 16 枚あるときは追い出さずに断る。

use crate::notice::{NewNotice, rfc3339, truncate_secs};
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 読み手と同じ値(`miharashi-app` の `daidai::MAX_FEED_BYTES` / `MAX_CARDS_KEPT`)。
pub const MAX_FEED_BYTES: usize = 256 * 1024;
pub const MAX_CARDS: usize = 16;
/// ロックを待つ長さの上限。
const LOCK_WAIT: Duration = Duration::from_secs(5);
const DEFAULT_MODE: u32 = 0o644;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedError {
    /// 既存のフィードが読めない・不正(書き換えない)
    Unreadable,
    /// 有効なカードがすでに 16 枚ある
    Full,
    /// 書いた結果が 256 KiB を超える
    TooLarge,
    /// ロックを取れなかった
    Busy,
    /// 書き込みの途中の失敗(一時ファイルは消す)
    Io,
}

/// 登録できたカードの、応答に返す部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub id: String,
    pub posted_at: String,
    pub expires_at: String,
}

fn random_bytes<const N: usize>() -> Result<[u8; N], FeedError> {
    let mut buf = [0u8; N];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|_| FeedError::Io)?;
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `muse-<UTC の YYYYMMDDTHHMMSSZ>-<乱数 16 進 8 桁>`。64 文字以内で、通知の id の形に合う。
pub fn new_id(now: &DateTime<FixedOffset>) -> Result<String, FeedError> {
    Ok(format!(
        "muse-{}-{}",
        now.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ"),
        hex(&random_bytes::<4>()?)
    ))
}

fn empty_feed() -> Value {
    json!({"version": 1, "avatar_pose": "idle", "cards": []})
}

fn dir_of(feed: &Path) -> PathBuf {
    match feed.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn file_name_of(feed: &Path) -> Result<String, FeedError> {
    feed.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or(FeedError::Io)
}

/// `<フィードのファイル名>.lock` を `flock(LOCK_EX)` で取る。返した `File` を閉じる(drop)と放す。
fn lock_feed(feed: &Path) -> Result<File, FeedError> {
    let lock_path = dir_of(feed).join(format!("{}.lock", file_name_of(feed)?));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|_| FeedError::Io)?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        // SAFETY: file は開いている FD。flock は FD だけを使う
        let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if r == 0 {
            return Ok(file);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) | Some(libc::EINTR) => {
                if Instant::now() >= deadline {
                    return Err(FeedError::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return Err(FeedError::Io),
        }
    }
}

/// 既存のフィードを読む。無ければ空のフィードと `None`。戻りの `u32` は既存ファイルの権限。
fn read_feed(feed: &Path) -> Result<(Value, Option<u32>), FeedError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY)
        .open(feed)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((empty_feed(), None)),
        Err(_) => return Err(FeedError::Unreadable),
    };
    let meta = file.metadata().map_err(|_| FeedError::Unreadable)?;
    if !meta.file_type().is_file() {
        return Err(FeedError::Unreadable);
    }
    let mut buf = Vec::new();
    file.take(MAX_FEED_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| FeedError::Unreadable)?;
    if buf.len() > MAX_FEED_BYTES {
        return Err(FeedError::Unreadable);
    }
    let root: Value = serde_json::from_slice(&buf).map_err(|_| FeedError::Unreadable)?;
    let Value::Object(o) = &root else {
        return Err(FeedError::Unreadable);
    };
    // 読み手は version 無しを 1 として読む。ほかの値(文字列・2 など)は読まない
    if o.get("version").is_some_and(|v| v.as_u64() != Some(1)) {
        return Err(FeedError::Unreadable);
    }
    if o.get("cards").is_some_and(|c| !c.is_array()) {
        return Err(FeedError::Unreadable);
    }
    Ok((root, Some(meta.mode() & 0o777)))
}

fn expires_of(card: &Value) -> Option<DateTime<FixedOffset>> {
    card.get("expires_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
}

/// 画面にも通知にも出ない(期限が読めて、今以前)。読めない期限は触らない(読み手が捨てる)。
fn is_expired(card: &Value, now: &DateTime<FixedOffset>) -> bool {
    expires_of(card).is_some_and(|e| e <= *now)
}

/// 読み手(`daidai::parse_card`)が取り込むカードか: オブジェクトで、title が空でなく、期限が無い・null・読める文字列。
fn reader_accepts(card: &Value) -> bool {
    let Some(o) = card.as_object() else {
        return false;
    };
    let title_ok = o
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(|t| t.chars().any(|c| !c.is_control() && !c.is_whitespace()));
    let expires_ok = match o.get("expires_at") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => DateTime::parse_from_rfc3339(s).is_ok(),
        Some(_) => false,
    };
    title_ok && expires_ok
}

fn write_atomically(feed: &Path, text: &str, mode: u32) -> Result<(), FeedError> {
    write_atomically_with(feed, text, mode, |_| Ok(()))
}

/// `write_atomically` の本体。`before_rename` は、一時ファイルを書き終え(`fsync` と権限の設定まで)、
/// `rename` する直前に呼ぶ。試験が、ここで失敗を差し込む(本番は何もしない)。
fn write_atomically_with(
    feed: &Path,
    text: &str,
    mode: u32,
    before_rename: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), FeedError> {
    let dir = dir_of(feed);
    let name = file_name_of(feed)?;
    let tmp = dir.join(format!("{name}.tmp.{}", hex(&random_bytes::<8>()?)));
    let result = (|| -> Result<(), FeedError> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
            .map_err(|_| FeedError::Io)?;
        f.write_all(text.as_bytes()).map_err(|_| FeedError::Io)?;
        f.sync_all().map_err(|_| FeedError::Io)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|_| FeedError::Io)?;
        before_rename(&tmp).map_err(|_| FeedError::Io)?;
        std::fs::rename(&tmp, feed).map_err(|_| FeedError::Io)?;
        // rename のあとは、フィードはもう新しい内容。ディレクトリの fsync に失敗しても失敗にはしない
        // (失敗で返すと、書けているのに相手が再送して同じカードが 2 枚になる)
        if let Err(e) = File::open(&dir).and_then(|d| d.sync_all()) {
            eprintln!("フィードのディレクトリの fsync に失敗(書き込みは済んでいる): {e}");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// カードを 1 枚、フィードの先頭に追加する(プロセス内の排他 + `flock`)。
pub fn append_notice(
    feed: &Path,
    notice: &NewNotice,
    now: DateTime<FixedOffset>,
) -> Result<Posted, FeedError> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    append_notice_flock(feed, notice, now)
}

/// `append_notice` のうち、`flock` だけで直列化する部分(プロセス内の `Mutex` は取らない)。
/// 別プロセス(や `Mutex` を通らない書き手)との協調の経路で、試験で直接呼ぶ。
pub fn append_notice_flock(
    feed: &Path,
    notice: &NewNotice,
    now: DateTime<FixedOffset>,
) -> Result<Posted, FeedError> {
    let now = truncate_secs(now);
    let _lock = lock_feed(feed)?;
    let (mut root, existing_mode) = read_feed(feed)?;
    let Some(root_obj) = root.as_object_mut() else {
        return Err(FeedError::Unreadable);
    };

    let mut cards = match root_obj.remove("cards") {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    cards.retain(|c| !is_expired(c, &now));
    if cards.iter().filter(|c| reader_accepts(c)).count() >= MAX_CARDS {
        return Err(FeedError::Full);
    }

    let used: HashSet<&str> = cards
        .iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str))
        .collect();
    let mut id = new_id(&now)?;
    while used.contains(id.as_str()) {
        id = new_id(&now)?;
    }

    let posted_at = rfc3339(&now);
    let expires_at = rfc3339(&notice.expires_at);
    let mut card = Map::new();
    card.insert("id".into(), json!(id));
    card.insert("kind".into(), json!(notice.kind));
    card.insert("title".into(), json!(notice.title));
    card.insert("body".into(), json!(notice.body));
    card.insert("posted_at".into(), json!(posted_at));
    card.insert("expires_at".into(), json!(expires_at));
    cards.insert(0, Value::Object(card));

    root_obj.insert("cards".into(), Value::Array(cards));
    root_obj.insert("updated_at".into(), json!(posted_at));
    if let Some(pose) = notice.avatar_pose {
        root_obj.insert("avatar_pose".into(), json!(pose));
    }

    let mut text = serde_json::to_string_pretty(&root).map_err(|_| FeedError::Io)?;
    text.push('\n');
    if text.len() > MAX_FEED_BYTES {
        return Err(FeedError::TooLarge);
    }
    write_atomically(feed, &text, existing_mode.unwrap_or(DEFAULT_MODE))?;
    Ok(Posted {
        id,
        posted_at,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::{DEFAULT_TTL_SECS, parse_request};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-11-18T12:00:00+09:00").unwrap()
    }

    fn dir(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "inbox-feed-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn notice(title: &str) -> NewNotice {
        parse_request(
            format!(r#"{{"kind":"info","title":"{title}","body":"b"}}"#).as_bytes(),
            now(),
        )
        .unwrap()
    }

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn cards(p: &Path) -> Vec<Value> {
        read(p)["cards"].as_array().unwrap().clone()
    }

    fn leftovers(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn id_has_the_documented_shape_and_differs_each_time() {
        let a = new_id(&now()).unwrap();
        let b = new_id(&now()).unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("muse-20261118T030000Z-"), "{a}");
        assert_eq!(a.len(), "muse-20261118T030000Z-".len() + 8);
        assert!(a.len() <= 64);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        );
    }

    #[test]
    fn creates_a_new_feed_when_there_is_none() {
        let d = dir("new");
        let p = d.join("feed.json");
        let posted = append_notice(&p, &notice("hello"), now()).unwrap();
        let v = read(&p);
        assert_eq!(v["version"], 1);
        assert_eq!(v["avatar_pose"], "idle");
        assert_eq!(v["updated_at"], "2026-11-18T12:00:00+09:00");
        let c = &v["cards"][0];
        assert_eq!(c["id"], posted.id.as_str());
        assert_eq!(c["kind"], "info");
        assert_eq!(c["title"], "hello");
        assert_eq!(c["body"], "b");
        assert_eq!(c["posted_at"], "2026-11-18T12:00:00+09:00");
        assert_eq!(c["expires_at"], "2026-11-19T12:00:00+09:00");
        assert_eq!(posted.expires_at, "2026-11-19T12:00:00+09:00");
        assert_eq!(c.as_object().unwrap().len(), 6);
        // 権限(既定 0644)、ロックファイル(0600)が残り、一時ファイルは残らない
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o644);
        let lock = std::fs::metadata(d.join("feed.json.lock")).unwrap();
        assert_eq!(lock.mode() & 0o777, 0o600);
        assert_eq!(leftovers(&d), ["feed.json", "feed.json.lock"]);
        assert_eq!(DEFAULT_TTL_SECS, 86400);
    }

    #[test]
    fn keeps_existing_cards_and_unknown_keys_and_puts_the_new_card_first() {
        let d = dir("keep");
        let p = d.join("feed.json");
        std::fs::write(
            &p,
            r#"{"version":1,"avatar_pose":"working","extra_top":{"a":1},"cards":[
              {"id":"old-1","kind":"morning","title":"old","priority":"low","unknown":[1,2],"expires_at":null},
              {"id":"old-2","title":"keeps","expires_at":"2099-01-01T00:00:00+09:00"}]}"#,
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        append_notice(&p, &notice("new"), now()).unwrap();
        let v = read(&p);
        assert_eq!(v["extra_top"]["a"], 1);
        assert_eq!(v["avatar_pose"], "working", "省略なら既存のポーズを残す");
        let c = cards(&p);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0]["title"], "new");
        assert_eq!(c[1]["id"], "old-1");
        assert_eq!(c[1]["priority"], "low");
        assert_eq!(c[1]["unknown"], json!([1, 2]));
        assert_eq!(c[2]["id"], "old-2");
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn avatar_pose_is_replaced_only_when_given() {
        let d = dir("pose");
        let p = d.join("feed.json");
        let mut n = notice("a");
        n.avatar_pose = Some("making");
        append_notice(&p, &n, now()).unwrap();
        assert_eq!(read(&p)["avatar_pose"], "making");
        append_notice(&p, &notice("b"), now()).unwrap();
        assert_eq!(read(&p)["avatar_pose"], "making");
        n.avatar_pose = Some("milestone");
        append_notice(&p, &n, now()).unwrap();
        assert_eq!(read(&p)["avatar_pose"], "milestone");
    }

    #[test]
    fn expired_cards_are_removed_but_unreadable_expiry_is_left_alone() {
        let d = dir("expired");
        let p = d.join("feed.json");
        std::fs::write(
            &p,
            r#"{"version":1,"cards":[
              {"id":"gone","title":"x","expires_at":"2026-11-18T11:00:00+09:00"},
              {"id":"edge","title":"x","expires_at":"2026-11-18T12:00:00+09:00"},
              {"id":"live","title":"x","expires_at":"2026-11-18T12:00:01+09:00"},
              {"id":"never","title":"x"},
              {"id":"weird","title":"x","expires_at":"soon"},
              "not-a-card"]}"#,
        )
        .unwrap();
        append_notice(&p, &notice("n"), now()).unwrap();
        let ids: Vec<Value> = cards(&p).iter().map(|c| c["id"].clone()).collect();
        assert_eq!(ids.len(), 5);
        assert!(!ids.contains(&json!("gone")) && !ids.contains(&json!("edge")));
        assert!(ids.contains(&json!("live")) && ids.contains(&json!("never")));
        assert!(ids.contains(&json!("weird")));
        assert!(cards(&p).contains(&json!("not-a-card")));
    }

    #[test]
    fn refuses_when_16_valid_cards_exist_and_does_not_touch_the_file() {
        let d = dir("full");
        let p = d.join("feed.json");
        let mut cs = Vec::new();
        for i in 0..16 {
            cs.push(json!({"id": format!("c{i}"), "title": "t", "expires_at": null}));
        }
        let text = serde_json::to_string(&json!({"version":1,"cards":cs})).unwrap();
        std::fs::write(&p, &text).unwrap();
        assert_eq!(append_notice(&p, &notice("n"), now()), Err(FeedError::Full));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        // 期限切れが 1 枚あれば、それを除いて 16 枚に収まる
        let mut cs2 = cs.clone();
        cs2[3]["expires_at"] = json!("2026-11-18T11:00:00+09:00");
        std::fs::write(
            &p,
            serde_json::to_string(&json!({"version":1,"cards":cs2})).unwrap(),
        )
        .unwrap();
        append_notice(&p, &notice("n"), now()).unwrap();
        assert_eq!(cards(&p).len(), 16);
        assert_eq!(cards(&p)[0]["title"], "n");
    }

    #[test]
    fn unreadable_feeds_are_never_rewritten() {
        let d = dir("bad");
        let p = d.join("feed.json");
        let bad: Vec<Vec<u8>> = vec![
            b"{not json".to_vec(),
            b"".to_vec(),
            br#"{"version":2,"cards":[]}"#.to_vec(),
            br#"{"version":"1","cards":[]}"#.to_vec(),
            br#"{"version":1,"cards":{}}"#.to_vec(),
            br#"[1,2]"#.to_vec(),
            b"\xff\xfe".to_vec(),
            vec![b' '; MAX_FEED_BYTES + 1],
        ];
        for content in bad {
            std::fs::write(&p, &content).unwrap();
            assert_eq!(
                append_notice(&p, &notice("n"), now()),
                Err(FeedError::Unreadable)
            );
            assert_eq!(
                std::fs::read(&p).unwrap(),
                content,
                "元のファイルがそのまま"
            );
        }
        assert_eq!(leftovers(&d), ["feed.json", "feed.json.lock"]);
        // フィードがシンボリックリンクでも書かない
        let real = d.join("real.json");
        std::fs::write(&real, r#"{"version":1,"cards":[]}"#).unwrap();
        let link = d.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            append_notice(&link, &notice("n"), now()),
            Err(FeedError::Unreadable)
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            r#"{"version":1,"cards":[]}"#
        );
    }

    #[test]
    fn a_feed_without_version_or_cards_is_accepted_like_the_reader_does() {
        let d = dir("lenient");
        let p = d.join("feed.json");
        std::fs::write(&p, r#"{}"#).unwrap();
        append_notice(&p, &notice("n"), now()).unwrap();
        assert_eq!(cards(&p).len(), 1);
    }

    #[test]
    fn refuses_when_the_result_would_exceed_the_size_limit() {
        let d = dir("big");
        let p = d.join("feed.json");
        // 16 枚に満たないが大きい(不正なカード = 読み手が捨てるもの)で上限に近づける
        let filler = "x".repeat(MAX_FEED_BYTES - 200);
        let text = serde_json::to_string(&json!({"version":1,"cards":[{"junk":filler}]})).unwrap();
        assert!(text.len() < MAX_FEED_BYTES);
        std::fs::write(&p, &text).unwrap();
        assert_eq!(
            append_notice(&p, &notice("n"), now()),
            Err(FeedError::TooLarge)
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
    }

    #[test]
    fn a_missing_directory_fails_before_any_file_is_made() {
        let d = dir("nodir");
        let p = d.join("missing-dir").join("feed.json");
        assert!(append_notice(&p, &notice("n"), now()).is_err());
        assert!(leftovers(&d).is_empty());
    }

    #[test]
    fn a_failure_after_the_temp_file_is_written_keeps_the_feed_and_removes_the_temp_file() {
        let d = dir("tmpfail");
        let p = d.join("feed.json");
        append_notice(&p, &notice("first"), now()).unwrap();
        let before = std::fs::read(&p).unwrap();

        // 一時ファイルを書き終えたあと、rename の前に失敗する。一時ファイルは実在し、そのあとで消える
        let mut seen_tmp = None;
        let r = write_atomically_with(&p, "replacement\n", DEFAULT_MODE, |tmp| {
            assert_eq!(std::fs::read_to_string(tmp).unwrap(), "replacement\n");
            seen_tmp = Some(tmp.to_path_buf());
            Err(std::io::Error::other("injected"))
        });
        assert_eq!(r, Err(FeedError::Io));
        let tmp = seen_tmp.expect("before_rename が呼ばれていない(一時ファイルまで届いていない)");
        assert!(!tmp.exists(), "一時ファイルが残った");
        assert_eq!(std::fs::read(&p).unwrap(), before, "フィードが変わった");
        assert_eq!(leftovers(&d), ["feed.json", "feed.json.lock"]);

        // 失敗のあとも、普通に書き足せる
        append_notice(&p, &notice("second"), now()).unwrap();
        assert_eq!(cards(&p).len(), 2);
    }

    #[test]
    fn a_failing_rename_removes_the_temp_file_and_leaves_the_target_alone() {
        // rename 先がディレクトリだと、rename だけが失敗する(一時ファイルの作成・書き込み・fsync は成功する)
        let d = dir("renamefail");
        let target = d.join("feed.json");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        assert_eq!(
            write_atomically(&target, "text\n", DEFAULT_MODE),
            Err(FeedError::Io)
        );
        assert_eq!(leftovers(&d), ["feed.json"]);
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"x");
    }

    #[test]
    fn the_writer_output_is_read_back_by_flock_writers_concurrently() {
        // Mutex を通らない 2 つの書き手(別プロセス相当)が flock だけで直列化される
        let d = dir("flock");
        let p = d.join("feed.json");
        let mut handles = Vec::new();
        for t in 0..2 {
            let p = p.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..6 {
                    append_notice_flock(&p, &notice(&format!("t{t}-{i}")), now()).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let c = cards(&p);
        assert_eq!(c.len(), 12);
        let ids: HashSet<&str> = c.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 12);
        assert_eq!(leftovers(&d), ["feed.json", "feed.json.lock"]);
    }

    #[test]
    fn mutex_writers_in_threads_do_not_corrupt_the_feed() {
        let d = dir("mutex");
        let p = d.join("feed.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || append_notice(&p, &notice(&format!("m{i}")), now()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert_eq!(cards(&p).len(), 8);
    }
}
