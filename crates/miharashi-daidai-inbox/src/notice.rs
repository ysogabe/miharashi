//! 受け付ける本文(`POST /api/v1/notices`)の検証。厳格: 知らないキーは拒否し、切り詰めはしない。
//!
//! 操作パネルは通知の題を 40 文字・本文を 200 文字で切る(画面は 120 / 400)。ここでは切らずに 120 / 400 まで受ける。

use chrono::{DateTime, Duration, FixedOffset, SecondsFormat, Timelike};
use serde_json::{Map, Value};

pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_BODY_CHARS: usize = 400;
pub const MAX_TTL_SECS: i64 = 7 * 24 * 3600;
pub const DEFAULT_TTL_SECS: i64 = 24 * 3600;

const KINDS: [&str; 3] = ["morning", "request", "info"];
const POSES: [&str; 4] = ["idle", "working", "milestone", "making"];
const ALLOWED_KEYS: [&str; 6] = [
    "kind",
    "title",
    "body",
    "avatar_pose",
    "expires_at",
    "ttl_secs",
];

/// 検証済みの登録内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewNotice {
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    pub avatar_pose: Option<&'static str>,
    pub expires_at: DateTime<FixedOffset>,
}

/// 現在時刻を秒に丸める(RFC 3339 の出力が秒までのため)。
pub fn truncate_secs(t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    t.with_nanosecond(0).unwrap_or(t)
}

pub fn rfc3339(t: &DateTime<FixedOffset>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// 表示の偽装に使われうる文字(双方向制御文字・ゼロ幅文字)。
fn is_spoofing_char(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'
            | '\u{2060}'
            | '\u{FEFF}'
    )
}

fn check_text(s: &str, allow_newline: bool) -> Result<(), &'static str> {
    for c in s.chars() {
        if c.is_control() && !(allow_newline && c == '\n') {
            return Err("control character not allowed");
        }
        if is_spoofing_char(c) {
            return Err("bidi or zero-width character not allowed");
        }
    }
    Ok(())
}

fn string_field<'a>(o: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, &'static str> {
    match o.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err("field must be a string"),
    }
}

/// 本文(JSON のバイト列)を検証する。`now` は秒に丸めた現在時刻。エラーは短い固定の理由(入力は含めない)。
pub fn parse_request(body: &[u8], now: DateTime<FixedOffset>) -> Result<NewNotice, &'static str> {
    let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(body) else {
        return Err("body must be a JSON object");
    };
    if o.keys().any(|k| !ALLOWED_KEYS.contains(&k.as_str())) {
        return Err("unknown key");
    }

    let kind = match string_field(&o, "kind")? {
        None => return Err("kind is required"),
        Some(k) => *KINDS.iter().find(|x| **x == k).ok_or("invalid kind")?,
    };

    let title_raw = string_field(&o, "title")?.ok_or("title is required")?;
    check_text(title_raw, false)?;
    let title = title_raw.trim();
    let title_len = title.chars().count();
    if title_len == 0 || title_len > MAX_TITLE_CHARS {
        return Err("title must be 1 to 120 characters");
    }

    let body_text = string_field(&o, "body")?.unwrap_or("");
    check_text(body_text, true)?;
    if body_text.chars().count() > MAX_BODY_CHARS {
        return Err("body must be at most 400 characters");
    }

    let avatar_pose = match string_field(&o, "avatar_pose")? {
        None => None,
        Some(p) => Some(
            *POSES
                .iter()
                .find(|x| **x == p)
                .ok_or("invalid avatar_pose")?,
        ),
    };

    let expires_at = match (o.get("expires_at"), o.get("ttl_secs")) {
        (Some(_), Some(_)) => return Err("expires_at and ttl_secs are mutually exclusive"),
        (Some(Value::String(s)), None) => {
            let t = DateTime::parse_from_rfc3339(s).map_err(|_| "expires_at must be RFC 3339")?;
            truncate_secs(t)
        }
        (Some(_), None) => return Err("expires_at must be a string"),
        (None, Some(v)) => {
            let secs = v.as_i64().ok_or("ttl_secs must be an integer")?;
            if !(1..=MAX_TTL_SECS).contains(&secs) {
                return Err("ttl_secs must be 1 to 604800");
            }
            now + Duration::seconds(secs)
        }
        (None, None) => now + Duration::seconds(DEFAULT_TTL_SECS),
    };
    if expires_at <= now || expires_at > now + Duration::seconds(MAX_TTL_SECS) {
        return Err("expiry must be in the future and within 7 days");
    }

    Ok(NewNotice {
        kind,
        title: title.to_string(),
        body: body_text.to_string(),
        avatar_pose,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-11-18T12:00:00+09:00").unwrap()
    }

    fn parse(s: &str) -> Result<NewNotice, &'static str> {
        parse_request(s.as_bytes(), now())
    }

    #[test]
    fn accepts_a_minimal_body_and_defaults_to_24_hours() {
        let n = parse(r#"{"kind":"info","title":"  こんにちは  "}"#).unwrap();
        assert_eq!(n.kind, "info");
        assert_eq!(n.title, "こんにちは");
        assert_eq!(n.body, "");
        assert_eq!(n.avatar_pose, None);
        assert_eq!(rfc3339(&n.expires_at), "2026-11-19T12:00:00+09:00");
    }

    #[test]
    fn accepts_every_kind_and_pose() {
        for k in KINDS {
            for p in POSES {
                let s = format!(r#"{{"kind":"{k}","title":"t","avatar_pose":"{p}"}}"#);
                let n = parse(&s).unwrap();
                assert_eq!((n.kind, n.avatar_pose), (k, Some(p)));
            }
        }
        assert!(parse(r#"{"kind":"alert","title":"t"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","avatar_pose":"dance"}"#).is_err());
    }

    #[test]
    fn kind_and_title_are_required_and_typed() {
        assert!(parse(r#"{"title":"t"}"#).is_err());
        assert!(parse(r#"{"kind":"info"}"#).is_err());
        assert!(parse(r#"{"kind":1,"title":"t"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":null}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","body":null}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"   "}"#).is_err());
        assert!(parse(r#"[1]"#).is_err());
        assert!(parse("not json").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn counts_unicode_characters_not_bytes() {
        let t120 = "あ".repeat(120);
        assert!(parse(&format!(r#"{{"kind":"info","title":"{t120}"}}"#)).is_ok());
        let t121 = "あ".repeat(121);
        assert!(parse(&format!(r#"{{"kind":"info","title":"{t121}"}}"#)).is_err());
        let b400 = "い".repeat(400);
        assert!(parse(&format!(r#"{{"kind":"info","title":"t","body":"{b400}"}}"#)).is_ok());
        let b401 = "い".repeat(401);
        assert!(parse(&format!(r#"{{"kind":"info","title":"t","body":"{b401}"}}"#)).is_err());
    }

    #[test]
    fn rejects_unknown_keys_including_id_and_posted_at() {
        for k in ["id", "posted_at", "priority", "updated_at", "version", "x"] {
            let s = format!(r#"{{"kind":"info","title":"t","{k}":"v"}}"#);
            assert_eq!(parse(&s), Err("unknown key"), "{k}");
        }
    }

    #[test]
    fn control_characters_are_rejected_but_body_may_have_newlines() {
        assert!(parse(r#"{"kind":"info","title":"a\nb"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"a\tb"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"a\u0000b"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"a\u007fb"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"a\u0085b"}"#).is_err());
        assert_eq!(
            parse(r#"{"kind":"info","title":"t","body":"a\nb"}"#)
                .unwrap()
                .body,
            "a\nb"
        );
        assert!(parse(r#"{"kind":"info","title":"t","body":"a\rb"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","body":"a\tb"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","body":"a\u001bb"}"#).is_err());
    }

    #[test]
    fn bidi_and_zero_width_characters_are_rejected_in_title_and_body() {
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200B}', '\u{2060}',
            '\u{FEFF}',
        ] {
            let t = format!(r#"{{"kind":"info","title":"a{c}b"}}"#);
            assert!(parse(&t).is_err(), "title {:04x}", c as u32);
            let b = format!(r#"{{"kind":"info","title":"t","body":"a{c}b"}}"#);
            assert!(parse(&b).is_err(), "body {:04x}", c as u32);
        }
    }

    #[test]
    fn ttl_and_expires_at_are_exclusive_and_range_checked() {
        let n = parse(r#"{"kind":"info","title":"t","ttl_secs":3600}"#).unwrap();
        assert_eq!(rfc3339(&n.expires_at), "2026-11-18T13:00:00+09:00");
        let n = parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-18T18:00:00+09:00"}"#)
            .unwrap();
        assert_eq!(rfc3339(&n.expires_at), "2026-11-18T18:00:00+09:00");
        assert!(
            parse(r#"{"kind":"info","title":"t","ttl_secs":10,"expires_at":"2026-11-18T18:00:00+09:00"}"#)
                .is_err()
        );
        // 範囲
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":0}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":-5}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":604800}"#).is_ok());
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":604801}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":1.5}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","ttl_secs":"60"}"#).is_err());
        // 過去・ちょうど今・7 日より先
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-18T11:59:59+09:00"}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-18T12:00:00+09:00"}"#)
                .is_err()
        );
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-18T12:00:01+09:00"}"#)
                .is_ok()
        );
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-25T12:00:00+09:00"}"#)
                .is_ok()
        );
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-25T12:00:01+09:00"}"#)
                .is_err()
        );
        // 形
        assert!(parse(r#"{"kind":"info","title":"t","expires_at":"tomorrow"}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","expires_at":null}"#).is_err());
        assert!(parse(r#"{"kind":"info","title":"t","expires_at":12345}"#).is_err());
        // UTC 表記でも同じ瞬間として比べる
        assert!(
            parse(r#"{"kind":"info","title":"t","expires_at":"2026-11-18T05:00:00Z"}"#).is_ok()
        );
    }

    #[test]
    fn errors_do_not_echo_the_input() {
        let e = parse(r#"{"kind":"info","title":"t","SECRET-KEY":"v"}"#).unwrap_err();
        assert!(!e.contains("SECRET"));
    }
}
