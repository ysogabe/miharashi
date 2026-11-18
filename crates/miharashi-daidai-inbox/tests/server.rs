//! 実際に TCP で待ち受けるサーバーに対する試験(127.0.0.1)。

use miharashi_daidai_inbox::config::Config;
use miharashi_daidai_inbox::server::Server;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const TOKEN: &str = "test-token-0123456789abcdefghijklmnopqrstuv";

struct Fixture {
    addr: SocketAddr,
    feed: PathBuf,
    token_file: PathBuf,
}

fn dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "inbox-srv-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_token(path: &PathBuf, token: &str) {
    std::fs::write(path, format!("{token}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn start_with(name: &str, allow: &str, burst: u32, per_minute: u32) -> Fixture {
    let dir = dir(name);
    let token_file = dir.join("token");
    write_token(&token_file, TOKEN);
    let feed = dir.join("feed.json");
    let mut cfg = Config::new(
        "127.0.0.1:0".parse().unwrap(),
        token_file.clone(),
        feed.clone(),
        vec![allow.parse::<IpAddr>().unwrap()],
    );
    cfg.burst = burst;
    cfg.per_minute = per_minute;
    let server = Arc::new(Server::bind(cfg).unwrap());
    let addr = server.local_addr().unwrap();
    std::thread::spawn(move || server.run());
    Fixture {
        addr,
        feed,
        token_file,
    }
}

fn start(name: &str) -> Fixture {
    start_with(name, "127.0.0.1", 1000, 60000)
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn send_raw(addr: SocketAddr, raw: &[u8]) -> Reply {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(raw);
    let _ = s.shutdown(Shutdown::Write);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

fn post_with(addr: SocketAddr, target: &str, auth: Option<&str>, ctype: &str, body: &str) -> Reply {
    let auth = auth.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let raw = format!(
        "POST {target} HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: {ctype}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    send_raw(addr, raw.as_bytes())
}

fn post(addr: SocketAddr, body: &str) -> Reply {
    post_with(
        addr,
        "/api/v1/notices",
        Some(TOKEN),
        "application/json",
        body,
    )
}

fn get(addr: SocketAddr, target: &str, auth: Option<&str>) -> Reply {
    let auth = auth.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    send_raw(
        addr,
        format!("GET {target} HTTP/1.1\r\nHost: x\r\n{auth}\r\n").as_bytes(),
    )
}

fn json(r: &Reply) -> Value {
    serde_json::from_str(&r.body).unwrap_or_else(|_| panic!("not json: {:?}", r.body))
}

fn feed_cards(f: &Fixture) -> Vec<Value> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&f.feed).unwrap()).unwrap();
    v["cards"].as_array().unwrap().clone()
}

const OK_BODY: &str = r#"{"kind":"info","title":"hello","body":"world","avatar_pose":"working"}"#;

#[test]
fn health_requires_a_bearer_token() {
    let f = start("health");
    let r = get(f.addr, "/api/v1/health", None);
    assert_eq!(r.status, 401);
    assert!(r.head.contains("WWW-Authenticate: Bearer"));
    assert_eq!(json(&r)["error"], "unauthorized");
    assert_eq!(get(f.addr, "/api/v1/health", Some("wrong")).status, 401);
    assert_eq!(
        get(f.addr, "/api/v1/health", Some(&format!("{TOKEN}x"))).status,
        401
    );
    let r = get(f.addr, "/api/v1/health", Some(TOKEN));
    assert_eq!(r.status, 200);
    assert_eq!(r.body, r#"{"ok":true}"#);
    assert!(r.head.contains("Connection: close"));
    // 疎通確認はフィードを作らない
    assert!(!f.feed.exists());
}

#[test]
fn post_creates_a_card_over_origin_form_and_absolute_form() {
    let f = start("post");
    let r = post(f.addr, OK_BODY);
    assert_eq!(r.status, 201, "{}", r.body);
    let v = json(&r);
    let id = v["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("muse-"));
    assert!(v["posted_at"].is_string() && v["expires_at"].is_string());
    let cards = feed_cards(&f);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["id"], id.as_str());
    assert_eq!(cards[0]["title"], "hello");

    // forward proxy 経由の absolute-form(content-type の charset 付き、Bearer は大文字小文字を問わない)
    let target = format!("http://{}/api/v1/notices", f.addr);
    let body = r#"{"kind":"request","title":"second"}"#;
    let raw = format!(
        "POST {target} HTTP/1.1\r\nHost: {}\r\nauthorization: bearer {TOKEN}\r\nContent-Type: Application/JSON; charset=utf-8\r\nContent-Length: {}\r\nProxy-Connection: close\r\n\r\n{body}",
        f.addr,
        body.len()
    );
    let r = send_raw(f.addr, raw.as_bytes());
    assert_eq!(r.status, 201, "{}", r.body);
    let cards = feed_cards(&f);
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["title"], "second", "新しいカードが先頭");
    assert_eq!(cards[1]["id"], id.as_str());
}

#[test]
fn routing_and_request_errors() {
    let f = start("errors");
    assert_eq!(get(f.addr, "/nope", Some(TOKEN)).status, 404);
    assert_eq!(get(f.addr, "/api/v1/notices", Some(TOKEN)).status, 405);
    let r = send_raw(
        f.addr,
        format!("POST /api/v1/health HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 0\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(r.status, 405);
    assert!(r.head.contains("Allow: GET"));
    assert_eq!(get(f.addr, "/api/v1/health?x=1", Some(TOKEN)).status, 400);
    assert_eq!(
        post_with(
            f.addr,
            "/api/v1/notices?x=1",
            Some(TOKEN),
            "application/json",
            OK_BODY
        )
        .status,
        400
    );
    assert_eq!(
        post_with(
            f.addr,
            "/api/v1/notices",
            Some(TOKEN),
            "text/plain",
            OK_BODY
        )
        .status,
        415
    );
    // 認証が先: トークン無しで content-type が違っても 401
    assert_eq!(
        post_with(f.addr, "/api/v1/notices", None, "text/plain", OK_BODY).status,
        401
    );
    // Content-Length 無し
    let r = send_raw(
        f.addr,
        format!("POST /api/v1/notices HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\r\n{OK_BODY}")
            .as_bytes(),
    );
    assert_eq!(r.status, 411);
    // 本文が 8 KiB を超える(宣言だけで断る)
    let r = send_raw(
        f.addr,
        format!("POST /api/v1/notices HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 8193\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(r.status, 413);
    // Transfer-Encoding
    let r = send_raw(
        f.addr,
        format!("POST /api/v1/notices HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(r.status, 501);
    // ヘッダーが 8 KiB を超える
    let big = format!(
        "GET /api/v1/health HTTP/1.1\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(9000)
    );
    assert_eq!(send_raw(f.addr, big.as_bytes()).status, 431);
    // 本文の検証
    for bad in [
        "not json",
        r#"{"kind":"info"}"#,
        r#"{"kind":"info","title":"t","id":"x"}"#,
        r#"{"kind":"info","title":"t","ttl_secs":5,"expires_at":"2099-01-01T00:00:00Z"}"#,
    ] {
        let r = post(f.addr, bad);
        assert_eq!(r.status, 400, "{bad}");
        assert!(json(&r)["error"].is_string());
    }
    assert!(!f.feed.exists(), "失敗した要求ではフィードを作らない");
    // どの応答にもトークンは出ない
    assert!(
        !send_raw(f.addr, b"GET / HTTP/1.1\r\n\r\n")
            .body
            .contains(TOKEN)
    );
}

#[test]
fn senders_outside_the_allow_list_get_403_without_a_read() {
    let f = start_with("allow", "127.0.0.2", 1000, 60000);
    // 何も送らずに 403 が返る(読む前に照合する)
    let mut s = TcpStream::connect(f.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 403 Forbidden"), "{out:?}");
    // 正しいトークンでも通らない
    let r = post(f.addr, OK_BODY);
    assert_eq!(r.status, 403);
    assert!(!f.feed.exists());
}

#[test]
fn rate_limit_counts_failures_and_returns_retry_after() {
    let f = start_with("rate", "127.0.0.1", 3, 6);
    for _ in 0..3 {
        assert_eq!(get(f.addr, "/api/v1/health", Some("wrong")).status, 401);
    }
    let r = get(f.addr, "/api/v1/health", Some("wrong"));
    assert_eq!(r.status, 429);
    let retry: u64 = r
        .head
        .lines()
        .find_map(|l| l.strip_prefix("Retry-After: "))
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=12).contains(&retry), "{retry}");
    // 認証に成功した疎通確認は数えない
    assert_eq!(get(f.addr, "/api/v1/health", Some(TOKEN)).status, 200);
    // 枠が空なら、正しい POST も 429
    assert_eq!(post(f.addr, OK_BODY).status, 429);
    assert!(!f.feed.exists());
}

#[test]
fn successful_and_failed_posts_both_count() {
    let f = start_with("rate-post", "127.0.0.1", 2, 6);
    assert_eq!(post(f.addr, OK_BODY).status, 201);
    assert_eq!(post(f.addr, "not json").status, 400);
    assert_eq!(post(f.addr, OK_BODY).status, 429);
    assert_eq!(feed_cards(&f).len(), 1);
}

#[test]
fn eight_concurrent_posts_all_land_in_a_valid_feed() {
    let f = start("concurrent");
    let addr = f.addr;
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                post(
                    addr,
                    &format!(r#"{{"kind":"info","title":"n{i}","body":"b"}}"#),
                )
            })
        })
        .collect();
    let mut ids = Vec::new();
    for h in handles {
        let r = h.join().unwrap();
        assert_eq!(r.status, 201, "{}", r.body);
        ids.push(json(&r)["id"].as_str().unwrap().to_string());
    }
    let cards = feed_cards(&f);
    assert_eq!(cards.len(), 8);
    let mut titles: Vec<String> = cards
        .iter()
        .map(|c| c["title"].as_str().unwrap().to_string())
        .collect();
    titles.sort();
    assert_eq!(titles, (0..8).map(|i| format!("n{i}")).collect::<Vec<_>>());
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8);
}

#[test]
fn a_broken_feed_gives_503_and_stays_byte_identical() {
    let f = start("broken");
    for content in [&b"{broken"[..], br#"{"version":2,"cards":[]}"#] {
        std::fs::write(&f.feed, content).unwrap();
        let r = post(f.addr, OK_BODY);
        assert_eq!(r.status, 503);
        assert_eq!(json(&r)["error"], "feed unreadable");
        assert_eq!(std::fs::read(&f.feed).unwrap(), content);
    }
}

#[test]
fn a_full_feed_gives_409() {
    let f = start("full");
    let cards: Vec<Value> = (0..16)
        .map(|i| serde_json::json!({"id": format!("c{i}"), "title": "t"}))
        .collect();
    std::fs::write(
        &f.feed,
        serde_json::to_string(&serde_json::json!({"version":1,"cards":cards})).unwrap(),
    )
    .unwrap();
    let r = post(f.addr, OK_BODY);
    assert_eq!(r.status, 409);
    assert_eq!(json(&r)["error"], "feed full");
    assert_eq!(feed_cards(&f).len(), 16);
}

#[test]
fn the_token_file_is_reread_for_each_request() {
    let f = start("rotate");
    assert_eq!(get(f.addr, "/api/v1/health", Some(TOKEN)).status, 200);
    let new = "another-token-0123456789abcdefghijklmnopqr";
    write_token(&f.token_file, new);
    assert_eq!(get(f.addr, "/api/v1/health", Some(TOKEN)).status, 401);
    assert_eq!(get(f.addr, "/api/v1/health", Some(new)).status, 200);
    // 規則に合わなくなったら通さない(値は出ない)
    std::fs::set_permissions(&f.token_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let r = get(f.addr, "/api/v1/health", Some(new));
    assert_eq!(r.status, 503);
    assert!(!r.body.contains(new));
}

#[test]
fn a_slow_client_is_cut_off_with_408() {
    let f = start("slow");
    let mut s = TcpStream::connect(f.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"POST /api/v1/notices HTTP/1.1\r\nHost: x\r\n")
        .unwrap();
    let started = std::time::Instant::now();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 408"), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(8));
    // そのあとも普通に動く
    assert_eq!(get(f.addr, "/api/v1/health", Some(TOKEN)).status, 200);
}

#[test]
fn bind_refuses_bad_configs() {
    let d = dir("bind");
    let tf = d.join("token");
    write_token(&tf, TOKEN);
    let allow = vec!["127.0.0.1".parse().unwrap()];
    let mk = |listen: &str, tf: &PathBuf| {
        Config::new(
            listen.parse().unwrap(),
            tf.clone(),
            d.join("feed.json"),
            allow.clone(),
        )
    };
    assert!(Server::bind(mk("0.0.0.0:0", &tf)).is_err());
    assert!(Server::bind(mk("[::]:0", &tf)).is_err());
    assert!(Server::bind(mk("127.0.0.1:0", &tf)).is_ok());
    // トークンファイルの規則違反は起動しない
    std::fs::set_permissions(&tf, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Server::bind(mk("127.0.0.1:0", &tf)).is_err());
    assert!(Server::bind(mk("127.0.0.1:0", &d.join("none"))).is_err());
    let link = d.join("link");
    std::fs::set_permissions(&tf, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&tf, &link).unwrap();
    assert!(Server::bind(mk("127.0.0.1:0", &link)).is_err());
    let short = d.join("short");
    write_token(&short, "short");
    assert!(Server::bind(mk("127.0.0.1:0", &short)).is_err());
    let mut no_allow = mk("127.0.0.1:0", &tf);
    no_allow.allow.clear();
    assert!(Server::bind(no_allow).is_err());
    let _ = &d;
}
