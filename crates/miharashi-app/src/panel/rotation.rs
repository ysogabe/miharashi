//! 追加先の token 配送役(`systemd/miharashi-t3-remote-token`)が残す配送結果の取り込み(管理ページ用)。
//!
//! 配送役は接続ごとに `<状態ディレクトリ>/<接続 id>.state.json`(秘密を含まない)へ、結果・終了コード・段階・
//! 開始/終了時刻・短い理由・残った古いセッションの数を書く。管理ページはそれを**読み取り専用で**取り込み、
//! 接続別の `tokens[].rotation` として出す。token ファイルが正常で取得に成功していても、更新が失敗していることに
//! 気づけるようにするため。状態ディレクトリは設定の `agents.rotation_state_dir`(省略すると取り込まない)。
//!
//! 信頼境界(別のプロセスが書いたファイルを読むので、中身は信用しない):
//! - 状態ディレクトリ: リンクを含まない正規の絶対パスで、実行ユーザー所有、所有者以外に書き込み権限がない。
//! - ファイル: リンクを追跡せず(`O_NOFOLLOW`)、開いた FD の `fstat` で通常ファイル・実行ユーザー所有・0600 系
//!   (グループ/その他に権限なし)を確かめる。16 KiB を超えるものは読まない。
//! - 中身: JSON のオブジェクトで、`server` が接続 id と一致すること。知っているキーだけを型を確かめて取り出し、
//!   文字列は制御文字を落として上限で切る。残りのキー・不正な型は捨てる。ページ側は `textContent` で出す。
//! - 欠落(ディレクトリ・ファイルが無い、設定なし)は「未設定」(`None`)。読めない・不正は `unreadable`。

use super::feeds::clip_error;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Value, json};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::SystemTime;

/// 状態ファイルの上限。
const MAX_STATE_BYTES: usize = 16 * 1024;
/// 実行中のまま止まっている(配送役が強制終了された)とみなす経過時間。
const RUNNING_STALE_SECS: i64 = 2 * 3600;
/// 成功から、この日数更新がなければ「止まっている」とみなす(週 1 回の更新が 2 週間以上ない)。
const OK_STALE_SECS: i64 = 14 * 24 * 3600;

/// 1 接続ぶんの配送結果。設定なし・ファイルなしは `None`(未設定)。
pub(super) fn report(
    state_dir: &str,
    server_id: &str,
    euid: u32,
    now: SystemTime,
) -> Option<Value> {
    if state_dir.is_empty() {
        return None;
    }
    match read(state_dir, server_id, euid) {
        Ok(None) => None,
        Ok(Some(raw)) => Some(summarize(&raw, server_id, DateTime::<Utc>::from(now))),
        Err(why) => Some(json!({"status": "unreadable", "detail": why})),
    }
}

/// 状態ファイルを安全に読む。`Ok(None)` = 無い(未設定扱い)。
fn read(state_dir: &str, server_id: &str, euid: u32) -> Result<Option<Value>, String> {
    use crate::token_file::{TokenFileError, open_regular_nofollow};
    let dir = Path::new(state_dir);
    let dir_meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("状態ディレクトリを調べられません".into()),
    };
    if !dir_meta.is_dir() {
        return Err("状態ディレクトリがディレクトリではありません(リンクは追跡しません)".into());
    }
    if std::fs::canonicalize(dir).ok().as_deref() != Some(dir) {
        return Err("状態ディレクトリの途中にリンクか不正な書き方があります".into());
    }
    if dir_meta.uid() != euid || dir_meta.mode() & 0o022 != 0 {
        return Err(
            "状態ディレクトリの所有者・権限が規則と違います(実行ユーザー所有で、他者が書けない)"
                .into(),
        );
    }
    let path = dir.join(format!("{server_id}.state.json"));
    let (file, meta) = match open_regular_nofollow(&path.to_string_lossy()) {
        Ok(v) => v,
        Err(TokenFileError::Missing) => return Ok(None),
        Err(TokenFileError::Symlink) => return Err("状態ファイルがリンクです(追跡しません)".into()),
        Err(TokenFileError::NotRegular) => {
            return Err("状態ファイルが通常のファイルではありません".into());
        }
        Err(_) => return Err("状態ファイルを開けません".into()),
    };
    if meta.uid() != euid || meta.mode() & 0o177 != 0 {
        return Err("状態ファイルの所有者・権限が規則と違います(実行ユーザーの 0600)".into());
    }
    let mut buf = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(&file, MAX_STATE_BYTES as u64 + 1),
        &mut buf,
    )
    .map_err(|_| "状態ファイルを読めません".to_string())?;
    if buf.len() > MAX_STATE_BYTES {
        return Err("状態ファイルが大きすぎます(16 KiB まで)".into());
    }
    match serde_json::from_slice::<Value>(&buf) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        _ => Err("状態ファイルが JSON のオブジェクトではありません".into()),
    }
}

/// `[A-Za-z0-9._-]` の短い名前だけを通す(結果の種類・段階)。
fn short_name(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    .then(|| s.to_string())
}

/// 配送役が書く `%Y-%m-%dT%H:%M:%SZ`(UTC)だけを通す。
fn timestamp(v: Option<&Value>) -> Option<DateTime<Utc>> {
    let s = v?.as_str()?;
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .map(|n| n.and_utc())
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 取り出した値だけで作る JSON。`status` は ok / running / attention / failed / stale / unreadable。
fn summarize(raw: &Value, server_id: &str, now: DateTime<Utc>) -> Value {
    if raw.get("server").and_then(Value::as_str) != Some(server_id) {
        return json!({"status": "unreadable", "detail": "状態ファイルの接続 id が一致しません"});
    }
    let result = short_name(raw.get("result")).unwrap_or_else(|| "unknown".into());
    let stage = short_name(raw.get("stage"));
    let exit_code = raw
        .get("exit_code")
        .and_then(Value::as_i64)
        .filter(|c| (0..=255).contains(c));
    let started = timestamp(raw.get("started_at"));
    let finished = timestamp(raw.get("finished_at"));
    let remaining_old = raw
        .get("remaining_old")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
        .min(1000);
    let cleanup_pending = raw.get("cleanup_pending").and_then(Value::as_bool) == Some(true);
    let detail = raw
        .get("detail")
        .and_then(Value::as_str)
        .map(|d| {
            clip_error(
                &d.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>(),
            )
        })
        .unwrap_or_default();
    let age = |t: DateTime<Utc>| (now - t).num_seconds();
    let status = match (result.as_str(), exit_code) {
        ("running", _) => match started {
            Some(t) if age(t) <= RUNNING_STALE_SECS => "running",
            // 開始から長く経つのに終わっていない: 配送役が強制終了された、または時刻が読めない
            _ => "stale",
        },
        ("ok", Some(0)) if !cleanup_pending && remaining_old == 0 => match finished {
            Some(t) if age(t) <= OK_STALE_SECS => "ok",
            _ => "stale",
        },
        (_, Some(3 | 4)) => "attention",
        // 成功・失敗にかかわらず、古い/発行した覚えのあるセッションが残っているなら要調査
        _ if cleanup_pending || remaining_old > 0 => "attention",
        _ => "failed",
    };
    json!({
        "status": status,
        "result": result,
        "exit_code": exit_code,
        "stage": stage,
        "started_at": started.map(rfc3339),
        "finished_at": finished.map(rfc3339),
        "remaining_old": remaining_old,
        "cleanup_pending": cleanup_pending,
        "detail": detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    struct Fx(PathBuf);
    impl Fx {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("miharashi-rotation-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            // 正規の絶対パス(/tmp がリンクの環境でも比較が通るように)
            Self(std::fs::canonicalize(p).unwrap())
        }
        fn dir(&self) -> &str {
            self.0.to_str().unwrap()
        }
        fn write(&self, id: &str, body: &str, mode: u32) {
            let p = self.0.join(format!("{id}.state.json"));
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }
    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn euid() -> u32 {
        crate::agents::effective_uid()
    }
    fn now() -> SystemTime {
        SystemTime::now()
    }
    fn stamp(secs_ago: i64) -> String {
        rfc3339(Utc::now() - chrono::Duration::seconds(secs_ago)).replace("+00:00", "Z")
    }
    fn state(result: &str, code: i64, finished_ago: i64) -> String {
        json!({
            "server": "container", "label": "x", "started_at": stamp(finished_ago + 5), "finished_at": stamp(finished_ago),
            "result": result, "exit_code": code, "stage": "done", "new_session_id": "sess-001",
            "revoked": [], "remaining_old": [], "detail": "",
        })
        .to_string()
    }
    fn report_of(fx: &Fx) -> Option<Value> {
        report(fx.dir(), "container", euid(), now())
    }

    #[test]
    fn missing_or_unconfigured_is_none_and_primary_style_empty_dir_is_ignored() {
        let fx = Fx::new("none");
        assert_eq!(report("", "container", euid(), now()), None, "設定なし");
        assert_eq!(report_of(&fx), None, "ファイルなし");
        assert_eq!(
            report("/nonexistent-miharashi-dir/x", "container", euid(), now()),
            None,
            "ディレクトリなし"
        );
    }

    #[test]
    fn classifies_success_failure_and_attention() {
        let fx = Fx::new("classify");
        fx.write("container", &state("ok", 0, 60), 0o600);
        let v = report_of(&fx).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["exit_code"].as_i64()),
            (Some("ok"), Some(0))
        );
        for (result, code, want) in [
            ("receive-failed", 1, "failed"),
            ("bad-scope", 1, "failed"),
            ("verify-failed", 3, "attention"),
            ("worker-unconfirmed", 3, "attention"),
            ("revoke-incomplete", 4, "attention"),
            ("interrupted", 1, "failed"),
        ] {
            fx.write("container", &state(result, code, 60), 0o600);
            let v = report_of(&fx).unwrap();
            assert_eq!(v["status"], want, "{result}");
            assert_eq!(v["result"], result);
        }
        // 発行の応答を失ったまま後始末できなかった(残存の可能性)は、終了コードが 1 でも要調査
        let mut s: Value = serde_json::from_str(&state("interrupted", 1, 60)).unwrap();
        s["cleanup_pending"] = json!(true);
        fx.write("container", &s.to_string(), 0o600);
        assert_eq!(report_of(&fx).unwrap()["status"], "attention");
        assert_eq!(report_of(&fx).unwrap()["cleanup_pending"], true);
        // 成功でも古いセッションが残っていれば要調査
        let mut s: Value = serde_json::from_str(&state("ok", 0, 60)).unwrap();
        s["remaining_old"] = json!(["a", "b"]);
        fx.write("container", &s.to_string(), 0o600);
        let v = report_of(&fx).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["remaining_old"].as_u64()),
            (Some("attention"), Some(2))
        );
    }

    #[test]
    fn old_success_and_stuck_running_are_stale() {
        let fx = Fx::new("stale");
        fx.write("container", &state("ok", 0, 15 * 24 * 3600), 0o600);
        assert_eq!(
            report_of(&fx).unwrap()["status"],
            "stale",
            "2 週間以上更新がない"
        );
        let running = |started_ago: i64| {
            json!({"server": "container", "started_at": stamp(started_ago), "finished_at": null, "result": "running", "exit_code": null, "stage": "issue"}).to_string()
        };
        fx.write("container", &running(30), 0o600);
        assert_eq!(report_of(&fx).unwrap()["status"], "running");
        fx.write("container", &running(3 * 3600), 0o600);
        assert_eq!(
            report_of(&fx).unwrap()["status"],
            "stale",
            "実行中のまま止まっている"
        );
    }

    #[test]
    fn untrusted_fields_are_whitelisted_typed_and_clipped() {
        let fx = Fx::new("untrusted");
        let body = json!({
            "server": "container", "result": "<img src=x onerror=alert(1)>", "stage": "a b", "exit_code": 9999,
            "started_at": "javascript:alert(1)", "finished_at": 12, "remaining_old": "many",
            "detail": format!("ok\u{7}\u{1b}[31m{}", "あ".repeat(500)),
            "extra": {"deep": [1, 2, 3]}, "token": "must-not-appear",
        })
        .to_string();
        fx.write("container", &body, 0o600);
        let v = report_of(&fx).unwrap();
        assert_eq!(v["result"], "unknown", "記号を含む結果の種類は通さない");
        assert!(v["stage"].is_null() && v["exit_code"].is_null());
        assert!(v["started_at"].is_null() && v["finished_at"].is_null());
        assert_eq!(v["remaining_old"], 0);
        let detail = v["detail"].as_str().unwrap();
        assert!(
            detail.chars().count() <= 201 && !detail.chars().any(char::is_control),
            "{detail}"
        );
        let text = v.to_string();
        for banned in ["must-not-appear", "alert(1)", "extra"] {
            assert!(!text.contains(banned), "{banned}");
        }
        assert_eq!(v["status"], "failed");
    }

    #[test]
    fn unsafe_or_invalid_files_are_unreadable_never_trusted() {
        let fx = Fx::new("unsafe");
        let unreadable = |fx: &Fx| {
            let v = report_of(fx).unwrap();
            assert_eq!(v["status"], "unreadable", "{v}");
            assert!(v.get("result").is_none(), "信用しない中身は出さない");
        };
        // 権限が緩い
        fx.write("container", &state("ok", 0, 60), 0o644);
        unreadable(&fx);
        // 別の接続の状態
        fx.write(
            "container",
            &state("ok", 0, 60).replace("\"container\"", "\"other\""),
            0o600,
        );
        unreadable(&fx);
        // JSON ではない・配列・大きすぎる
        for body in [
            "not json",
            "[1,2]",
            &format!(
                "{{\"server\":\"container\",\"detail\":\"{}\"}}",
                "x".repeat(20 * 1024)
            ),
        ] {
            fx.write("container", body, 0o600);
            unreadable(&fx);
        }
        // リンク(中身が正しくても追跡しない)
        fx.write("real", &state("ok", 0, 60), 0o600);
        let link = fx.0.join("container.state.json");
        std::fs::remove_file(&link).unwrap();
        symlink(fx.0.join("real.state.json"), &link).unwrap();
        unreadable(&fx);
        // FIFO で固まらない
        std::fs::remove_file(&link).unwrap();
        let c = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        unreadable(&fx);
        // ディレクトリが他者に書ける・リンク
        std::fs::remove_file(&link).unwrap();
        fx.write("container", &state("ok", 0, 60), 0o600);
        std::fs::set_permissions(&fx.0, std::fs::Permissions::from_mode(0o777)).unwrap();
        unreadable(&fx);
        std::fs::set_permissions(&fx.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias =
            std::env::temp_dir().join(format!("miharashi-rotation-alias-{}", std::process::id()));
        let _ = std::fs::remove_file(&alias);
        symlink(&fx.0, &alias).unwrap();
        let v = report(alias.to_str().unwrap(), "container", euid(), now()).unwrap();
        let _ = std::fs::remove_file(&alias);
        assert_eq!(v["status"], "unreadable", "状態ディレクトリがリンク");
    }
}
