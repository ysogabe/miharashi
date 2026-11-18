//! 最小の HTTP/1.1(1 接続 1 要求、応答は常に `Connection: close`)。
//!
//! `tiny_http` は本文を読み残した接続のスレッドが残る(`miharashi-app` の `panel.rs` の説明)ので使わず、
//! `std::net` だけで書く。読み取りは接続ごとに合計の締め切り(既定 5 秒)を持ち、要求行 + ヘッダーは 8 KiB まで、
//! 本文は `Content-Length` 必須で 8 KiB まで。`Transfer-Encoding` は受けない(501)。

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub const MAX_HEAD_BYTES: usize = 8 * 1024;
pub const MAX_BODY_BYTES: usize = 8 * 1024;
pub const IO_DEADLINE: Duration = Duration::from_secs(5);
const MAX_HEADERS: usize = 64;

/// 読み取りの待ち時間を変えられる入力(`TcpStream`。試験では手元のバイト列)。
pub trait TimedRead: Read {
    fn set_read_timeout_to(&mut self, d: Duration) -> io::Result<()>;
}

impl TimedRead for TcpStream {
    fn set_read_timeout_to(&mut self, d: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(d.max(Duration::from_millis(1))))
    }
}

/// 要求を読めなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// 何も読まないうちに閉じられた(応答しない)
    Closed,
    /// 締め切りを過ぎた(408)
    Timeout,
    /// 要求行 + ヘッダーが 8 KiB を超えた(431)
    HeadTooLarge,
    /// 本文が 8 KiB を超える(413)
    BodyTooLarge,
    /// 形が不正(400)
    Bad(&'static str),
    /// `Transfer-Encoding` など、実装していない(501)
    NotImplemented(&'static str),
    /// 読み取りの I/O エラー
    Io,
}

impl HttpError {
    /// 返す HTTP ステータスと短い理由。`Closed` / `Io` は応答しない。
    pub fn response(self) -> Option<(u16, &'static str)> {
        match self {
            Self::Closed | Self::Io => None,
            Self::Timeout => Some((408, "request timeout")),
            Self::HeadTooLarge => Some((431, "request header too large")),
            Self::BodyTooLarge => Some((413, "body too large")),
            Self::Bad(m) => Some((400, m)),
            Self::NotImplemented(m) => Some((501, m)),
        }
    }
}

/// 読んだ要求行 + ヘッダー。`path` はクエリを含まない。ヘッダー名は小文字。
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// 同じ名前が 2 回以上あれば不正(認証・長さの曖昧さを作らない)。
    pub fn single(&self, name: &str) -> Result<Option<&str>, HttpError> {
        let mut it = self.headers.iter().filter(|(n, _)| n == name);
        let first = it.next().map(|(_, v)| v.as_str());
        if it.next().is_some() {
            return Err(HttpError::Bad("duplicate header"));
        }
        Ok(first)
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn remaining(deadline: Instant) -> Result<Duration, HttpError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(HttpError::Timeout)
    } else {
        Ok(left)
    }
}

/// 1 回読む。タイムアウトの `WouldBlock` / `TimedOut` は `Ok(None)`(呼び出し側が締め切りを見直す)。
fn read_some<R: TimedRead>(
    r: &mut R,
    chunk: &mut [u8],
    deadline: Instant,
) -> Result<Option<usize>, HttpError> {
    r.set_read_timeout_to(remaining(deadline)?)
        .map_err(|_| HttpError::Io)?;
    match r.read(chunk) {
        Ok(n) => Ok(Some(n)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(_) => Err(HttpError::Io),
    }
}

/// 要求行 + ヘッダーを読む。戻りの `Vec` は、ヘッダーの後ろまで読み込んでしまった本文の先頭。
pub fn read_head<R: TimedRead>(r: &mut R, deadline: Instant) -> Result<(Head, Vec<u8>), HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(end) = find_head_end(&buf) {
            break end;
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(HttpError::HeadTooLarge);
        }
        match read_some(r, &mut chunk, deadline)? {
            None => {}
            Some(0) if buf.is_empty() => return Err(HttpError::Closed),
            Some(0) => return Err(HttpError::Bad("truncated request")),
            Some(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    if end > MAX_HEAD_BYTES {
        return Err(HttpError::HeadTooLarge);
    }
    let head = parse_head(&buf[..end - 4])?;
    if head.headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Err(HttpError::NotImplemented(
            "transfer-encoding is not supported",
        ));
    }
    Ok((head, buf[end..].to_vec()))
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// 要求行 + ヘッダー(末尾の空行は含まない)を読む。ASCII の印字文字と空白・タブだけ、行区切りは CRLF だけを受ける。
pub fn parse_head(raw: &[u8]) -> Result<Head, HttpError> {
    if !raw
        .iter()
        .all(|&b| b == b'\r' || b == b'\n' || b == b'\t' || (0x20..=0x7e).contains(&b))
    {
        return Err(HttpError::Bad("invalid character in header"));
    }
    // 上の検査で ASCII だけ
    let text = std::str::from_utf8(raw).map_err(|_| HttpError::Bad("invalid header"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    if request_line.contains(['\r', '\n']) {
        return Err(HttpError::Bad("invalid line ending"));
    }
    let parts: Vec<&str> = request_line.split(' ').collect();
    let [method, target, version] = parts[..] else {
        return Err(HttpError::Bad("invalid request line"));
    };
    if method.is_empty() || method.len() > 16 || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(HttpError::Bad("invalid method"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HttpError::Bad("unsupported http version"));
    }
    let path = parse_target(target)?;

    let mut headers = Vec::new();
    for line in lines {
        if line.contains(['\r', '\n']) {
            return Err(HttpError::Bad("invalid line ending"));
        }
        if line.starts_with([' ', '\t']) {
            return Err(HttpError::Bad("folded header not allowed"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpError::Bad("invalid header line"));
        };
        if name.is_empty() || !name.bytes().all(is_token_char) {
            return Err(HttpError::Bad("invalid header name"));
        }
        if headers.len() >= MAX_HEADERS {
            return Err(HttpError::HeadTooLarge);
        }
        headers.push((
            name.to_ascii_lowercase(),
            value.trim_matches([' ', '\t']).to_string(),
        ));
    }
    Ok(Head {
        method: method.to_string(),
        path,
        headers,
    })
}

/// 宛先からパスを取り出す。origin-form(`/path`)と、HTTP の forward proxy 経由で来る absolute-form
/// (`http://host:port/path`)を受ける。クエリ・フラグメントは 400。
pub fn parse_target(target: &str) -> Result<String, HttpError> {
    let path = if target.starts_with('/') {
        target
    } else if target.len() > 7 && target[..7].eq_ignore_ascii_case("http://") {
        let rest = &target[7..];
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) => rest.split_at(i),
            None => (rest, "/"),
        };
        if authority.is_empty()
            || !authority
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._:[]".contains(&b))
        {
            return Err(HttpError::Bad("invalid request target"));
        }
        // 権限部の直後が `?` / `#` なら、パスは無くクエリだけ(下で 400)
        if path.starts_with(['?', '#']) {
            return Err(HttpError::Bad("query string not allowed"));
        }
        path
    } else {
        return Err(HttpError::Bad("invalid request target"));
    };
    if path.contains(['?', '#']) {
        return Err(HttpError::Bad("query string not allowed"));
    }
    Ok(path.to_string())
}

/// `Content-Length` の値(数字だけ。長さの上限は呼び出し側が見る)。
pub fn parse_content_length(v: &str) -> Result<usize, HttpError> {
    if v.is_empty() || v.len() > 9 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(HttpError::Bad("invalid content-length"));
    }
    v.parse()
        .map_err(|_| HttpError::Bad("invalid content-length"))
}

/// 本文をちょうど `len` バイト読む(`prefix` は既に読んだ先頭)。
pub fn read_body<R: TimedRead>(
    r: &mut R,
    prefix: Vec<u8>,
    len: usize,
    deadline: Instant,
) -> Result<Vec<u8>, HttpError> {
    if len > MAX_BODY_BYTES {
        return Err(HttpError::BodyTooLarge);
    }
    let mut body = prefix;
    body.truncate(len);
    let mut chunk = [0u8; 1024];
    while body.len() < len {
        let want = (len - body.len()).min(chunk.len());
        match read_some(r, &mut chunk[..want], deadline)? {
            None => {}
            Some(0) => return Err(HttpError::Bad("truncated body")),
            Some(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Ok(body)
}

/// 応答。本文は JSON。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
        }
    }

    pub fn error(status: u16, msg: &str) -> Self {
        Self::json(status, serde_json::json!({ "error": msg }).to_string())
    }

    pub fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

pub fn write_response<W: Write>(w: &mut W, r: &Response) -> io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        r.status,
        reason(r.status),
        r.body.len()
    );
    for (n, v) in &r.headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&r.body);
    w.write_all(out.as_bytes())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Mem(Cursor<Vec<u8>>);

    impl Read for Mem {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl TimedRead for Mem {
        fn set_read_timeout_to(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    fn mem(s: impl Into<Vec<u8>>) -> Mem {
        Mem(Cursor::new(s.into()))
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn parses_origin_form_and_absolute_form_targets() {
        assert_eq!(parse_target("/api/v1/notices").unwrap(), "/api/v1/notices");
        assert_eq!(
            parse_target("http://100.64.0.1:8097/api/v1/notices").unwrap(),
            "/api/v1/notices"
        );
        assert_eq!(
            parse_target("HTTP://example.test/api/v1/health").unwrap(),
            "/api/v1/health"
        );
        assert_eq!(parse_target("http://[::1]:80/x").unwrap(), "/x");
        assert_eq!(parse_target("http://host").unwrap(), "/");
        for bad in [
            "https://host/x",
            "*",
            "host:80",
            "http:///x",
            "http://user@host/x",
            "http://ho st/x",
            "x",
            "",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_query_string_or_fragment_is_a_bad_request() {
        for t in [
            "/api/v1/notices?x=1",
            "/api/v1/notices?",
            "/api/v1/notices#f",
            "http://h:1/api/v1/notices?x=1",
            "http://h:1?x=1",
        ] {
            assert_eq!(
                parse_target(t),
                Err(HttpError::Bad("query string not allowed")),
                "{t}"
            );
        }
    }

    #[test]
    fn reads_a_request_and_keeps_the_body_prefix() {
        let raw = "POST /api/v1/notices HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nAuthorization:  Bearer t \r\n\r\nhello";
        let (head, prefix) = read_head(&mut mem(raw), deadline()).unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/api/v1/notices");
        assert_eq!(head.single("content-length").unwrap(), Some("5"));
        assert_eq!(head.single("authorization").unwrap(), Some("Bearer t"));
        assert_eq!(prefix, b"hello");
        let body = read_body(&mut mem(""), prefix, 5, deadline()).unwrap();
        assert_eq!(body, b"hello");
    }

    #[test]
    fn rejects_bad_heads() {
        let bad = [
            "GET / HTTP/2\r\n\r\n",
            "GET /\r\n\r\n",
            "get / HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nNoColon\r\n\r\n",
            "GET / HTTP/1.1\r\n folded: x\r\n\r\n",
            "GET / HTTP/1.1\r\nBad Name: x\r\n\r\n",
            "GET / HTTP/1.1\r\nX: a\nb\r\n\r\n",
            "GET / HTTP/1.1\r\nX: \u{3042}\r\n\r\n",
            "GET / HTTP/1.1\r\nX: a\u{0}b\r\n\r\n",
        ];
        for raw in bad {
            assert!(
                matches!(read_head(&mut mem(raw), deadline()), Err(HttpError::Bad(_))),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn transfer_encoding_is_not_implemented() {
        let raw = "POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            read_head(&mut mem(raw), deadline())
                .unwrap_err()
                .response()
                .unwrap()
                .0,
            501
        );
    }

    #[test]
    fn head_size_limit_is_8_kib() {
        let ok_pad = "a".repeat(MAX_HEAD_BYTES - "GET / HTTP/1.1\r\nX: \r\n\r\n".len());
        let ok = format!("GET / HTTP/1.1\r\nX: {ok_pad}\r\n\r\n");
        assert_eq!(ok.len(), MAX_HEAD_BYTES);
        assert!(read_head(&mut mem(ok), deadline()).is_ok());
        let big = format!("GET / HTTP/1.1\r\nX: {ok_pad}a\r\n\r\n");
        assert_eq!(
            read_head(&mut mem(big), deadline()).unwrap_err(),
            HttpError::HeadTooLarge
        );
        // 終端が来ないまま 8 KiB を超えて読み続けない
        let endless = format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(20_000));
        assert_eq!(
            read_head(&mut mem(endless), deadline()).unwrap_err(),
            HttpError::HeadTooLarge
        );
    }

    #[test]
    fn empty_and_truncated_input() {
        assert_eq!(
            read_head(&mut mem(""), deadline()).unwrap_err(),
            HttpError::Closed
        );
        assert!(matches!(
            read_head(&mut mem("GET / HT"), deadline()),
            Err(HttpError::Bad(_))
        ));
        assert!(matches!(
            read_body(&mut mem("ab"), Vec::new(), 5, deadline()),
            Err(HttpError::Bad(_))
        ));
    }

    #[test]
    fn duplicate_headers_are_rejected_by_single() {
        let raw = "POST / HTTP/1.1\r\nContent-Length: 1\r\ncontent-length: 1\r\n\r\n";
        let (head, _) = read_head(&mut mem(raw), deadline()).unwrap();
        assert!(head.single("content-length").is_err());
        assert_eq!(head.single("authorization").unwrap(), None);
    }

    #[test]
    fn content_length_parsing_and_body_limit() {
        assert_eq!(parse_content_length("0").unwrap(), 0);
        assert_eq!(parse_content_length("8192").unwrap(), 8192);
        for bad in ["", "-1", "+1", "1.5", "0x10", "1 2", "99999999999"] {
            assert!(parse_content_length(bad).is_err(), "{bad}");
        }
        assert_eq!(
            read_body(&mut mem(""), Vec::new(), MAX_BODY_BYTES + 1, deadline()).unwrap_err(),
            HttpError::BodyTooLarge
        );
        let full = vec![b'a'; MAX_BODY_BYTES];
        assert_eq!(
            read_body(
                &mut mem(full.clone()),
                Vec::new(),
                MAX_BODY_BYTES,
                deadline()
            )
            .unwrap(),
            full
        );
    }

    #[test]
    fn writes_a_close_response_with_json_and_extra_headers() {
        let mut out = Vec::new();
        let r =
            Response::error(401, "unauthorized").with_header("WWW-Authenticate", "Bearer".into());
        write_response(&mut out, &r).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.contains("WWW-Authenticate: Bearer\r\n"));
        assert!(s.contains(&format!("Content-Length: {}\r\n", r.body.len())));
        assert!(s.ends_with("\r\n\r\n{\"error\":\"unauthorized\"}"));
    }
}
