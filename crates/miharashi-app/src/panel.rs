//! 手元の操作パネル(Echo Show 5 など)向けの HTTP API(docs/panel/DESIGN.md §5.2)。
//!
//! パネルはキーボードと同じ `KeyPress` を送るだけで、画面の論理はキオスク側にある。
//! サーバーは別スレッド 1 本(写真の `Feed` と同じ形)。キーは mpsc で実行ループへ渡し、
//! 実行ループは画面が変わったときに `publish` で状態を置く。SSE の接続はそれぞれ別スレッドで待つ。
//!
//! P1 の範囲: `prev` / `next` / `select` の 3 つ(今の `KeyPress` の Prev / Next / Switch)。
//! `up` / `down` / `menu` は #5(PoC-003)で `KeyPress` が増えてから受け付ける。`/api/v1/screen` も #5 の後。
use crate::config;
use crate::screen::{KeyPress, PanelView};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryIter, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};

/// `/api/v1/status` の形を変えたら上げる。パネルはこれを見て古い形を読み分ける。
const SCHEMA_VERSION: u32 = 1;
/// 同時に開いておける SSE の数。パネルは 1 台の想定なので少なくてよい。
const MAX_SSE: usize = 4;
const KEEPALIVE: Duration = Duration::from_secs(15);
const MAX_BODY: u64 = 1024;
/// 最後の要求からこの時間はパネルが「つながっている」とみなす(パネルは 5 秒ごとに /status を読む)。
const CONNECTED_FOR: Duration = Duration::from_secs(30);

/// `listen` が、待ち受けるインターフェースを 1 つに決めた IP:ポートか。
pub fn check_listen(listen: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = listen
        .parse()
        .map_err(|_| format!("IP:ポートで書く(例: 192.168.0.132:8790): {listen:?}"))?;
    if addr.ip().is_unspecified() {
        return Err("0.0.0.0 / :: は不可。LAN のインターフェースの IP を指定する".into());
    }
    Ok(addr)
}

/// パネルが送るキーの名前を `KeyPress` にする。Err は 400 の本文。
fn parse_key(name: &str) -> Result<KeyPress, String> {
    match name {
        "prev" => Ok(KeyPress::Prev),
        "next" => Ok(KeyPress::Next),
        "select" => Ok(KeyPress::Switch),
        "up" | "down" | "menu" => Err(format!(
            "{name} は PoC-003(#5)まで未対応。使えるのは prev / next / select"
        )),
        _ => Err(format!(
            "未知のキー {name:?}。使えるのは prev / next / select"
        )),
    }
}

/// 長さに依らない比較(トークンを 1 文字ずつ当てられないように)。
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// トークンのファイルを読む。他人が読めるファイルは使わない。
fn read_token(path: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("{path}: {e}"))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!("{path}: 権限が {:o}。0600 にする", mode & 0o777));
        }
    }
    let token = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let token = token.trim().to_string();
    if token.len() < 16 {
        return Err(format!("{path}: トークンが短い(16 文字以上)"));
    }
    Ok(token)
}

/// 1 秒間の窓で数えるレート制限。
struct RateLimit {
    max: usize,
    hits: VecDeque<Instant>,
}

impl RateLimit {
    fn new(max_per_sec: u32) -> Self {
        Self {
            max: max_per_sec as usize,
            hits: VecDeque::new(),
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        while self
            .hits
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
        {
            self.hits.pop_front();
        }
        if self.hits.len() >= self.max {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}

struct State {
    view: Option<PanelView>,
    /// `view` が変わるたびに 1 増える。SSE はこれで変化を知る
    generation: u64,
}

/// サーバーのスレッド群と実行ループで共有するもの。
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    host: String,
    started: Instant,
    sse: AtomicUsize,
    last_request: Mutex<Option<Instant>>,
}

impl Shared {
    fn status_json(&self, view: &PanelView) -> Value {
        let screen = view.screens.get(view.current).map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "index": view.current,
                "count": view.screens.len(),
                "page": view.page,
            })
        });
        json!({
            "schema_version": SCHEMA_VERSION,
            "host": self.host,
            "screen": screen,
            "screens": view.screens.iter().map(|s| json!({"id": s.id, "title": s.title})).collect::<Vec<_>>(),
            "photo": view.photo.map(|(position, total)| json!({"position": position, "total": total})),
            "feed": if view.feed_online { "online" } else { "retrying" },
            "uptime_s": self.started.elapsed().as_secs(),
            "generated_at": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        })
    }

    /// 今の状態の JSON と世代。まだ一度も置かれていなければ None。
    fn snapshot(&self) -> (Option<Value>, u64) {
        let st = self.state.lock().unwrap();
        (st.view.as_ref().map(|v| self.status_json(v)), st.generation)
    }
}

/// 実行ループ側の持ち手。
pub struct Panel {
    keys: Receiver<KeyPress>,
    shared: Arc<Shared>,
    addr: SocketAddr,
}

impl Panel {
    /// 設定どおりに待ち受けを始める。失敗したら Err(キオスクはパネルなしで動き続ける)。
    pub fn start(cfg: &config::Panel) -> Result<Self, String> {
        let addr = check_listen(&cfg.listen)?;
        let token = read_token(&cfg.token_file)?;
        let server = Server::http(addr).map_err(|e| format!("{addr} で待ち受けられない: {e}"))?;
        Ok(Self::spawn(server, token, cfg.max_rate_per_sec))
    }

    fn spawn(server: Server, token: String, max_rate_per_sec: u32) -> Self {
        let addr = server.server_addr().to_ip().expect("TCP で待ち受けている");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                view: None,
                generation: 0,
            }),
            changed: Condvar::new(),
            host: hostname(),
            started: Instant::now(),
            sse: AtomicUsize::new(0),
            last_request: Mutex::new(None),
        });
        let (tx, keys) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("panel".into())
            .spawn(move || serve(server, token, RateLimit::new(max_rate_per_sec), tx, s))
            .expect("panel スレッドを起動できる");
        Self { keys, shared, addr }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 届いているキー。
    pub fn keys(&self) -> TryIter<'_, KeyPress> {
        self.keys.try_iter()
    }

    /// 画面の状態を置く。前と同じなら何もしない(SSE には変わったときだけ送る)。
    pub fn publish(&self, view: Option<PanelView>) {
        let mut st = self.shared.state.lock().unwrap();
        if st.view != view {
            st.view = view;
            st.generation += 1;
            self.shared.changed.notify_all();
        }
    }

    /// 状態ログの `panel=`。SSE を開いているか、直近に要求があれば connected。
    pub fn stat(&self) -> &'static str {
        let recent = self
            .shared
            .last_request
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < CONNECTED_FOR);
        if recent || self.shared.sse.load(Ordering::Relaxed) > 0 {
            "connected"
        } else {
            "idle"
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "-".into())
}

fn peer(req: &Request) -> String {
    req.remote_addr().map_or("-".into(), |a| a.ip().to_string())
}

fn json_response(code: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.to_string().into_bytes())
        .with_status_code(code)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
}

fn error(code: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(code, &json!({ "error": msg }))
}

fn authorized(req: &Request, token: &str) -> bool {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .is_some_and(|t| same_secret(t.trim().as_bytes(), token.as_bytes()))
}

fn serve(
    server: Server,
    token: String,
    mut rate: RateLimit,
    keys: Sender<KeyPress>,
    shared: Arc<Shared>,
) {
    for mut req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or("").to_string();
        let method = req.method().clone();
        if path == "/api/v1/healthz" {
            let _ = req.respond(json_response(200, &json!({ "ok": true })));
            continue;
        }
        if !authorized(&req, &token) {
            eprintln!("panel: {} {method} {path} -> 401", peer(&req));
            let _ = req.respond(error(401, "Authorization: Bearer <token> が必要"));
            continue;
        }
        *shared.last_request.lock().unwrap() = Some(Instant::now());
        let resp = match (&method, path.as_str()) {
            (Method::Get, "/api/v1/status") => match shared.snapshot().0 {
                Some(v) => json_response(200, &v),
                None => error(503, "起動中"),
            },
            (Method::Get, "/api/v1/events") => {
                if shared.sse.load(Ordering::Relaxed) >= MAX_SSE {
                    error(503, "SSE の接続が多すぎる")
                } else {
                    shared.sse.fetch_add(1, Ordering::Relaxed);
                    let s = shared.clone();
                    let who = peer(&req);
                    eprintln!("panel: {who} events 接続");
                    std::thread::spawn(move || {
                        events(req, &s);
                        s.sse.fetch_sub(1, Ordering::Relaxed);
                        eprintln!("panel: {who} events 切断");
                    });
                    continue;
                }
            }
            (Method::Post, "/api/v1/input") => {
                if !rate.allow(Instant::now()) {
                    error(429, "操作が速すぎる")
                } else {
                    let (name, result) = input(&mut req, &keys);
                    let code = result.as_ref().map_or_else(|(c, _)| *c, |_| 204);
                    eprintln!("panel: {} input {name:?} -> {code}", peer(&req));
                    match result {
                        Ok(()) => Response::from_data(Vec::new()).with_status_code(204),
                        Err((code, msg)) => error(code, &msg),
                    }
                }
            }
            (_, "/api/v1/status" | "/api/v1/events" | "/api/v1/input") => {
                error(405, "メソッドが違う")
            }
            _ => error(404, "ない"),
        };
        let _ = req.respond(resp);
    }
}

/// `POST /api/v1/input` の本文を読んでキーを送る。戻り値: (キーの名前(ログ用), 結果)。Err は (状態コード, 本文)。
fn input(req: &mut Request, keys: &Sender<KeyPress>) -> (String, Result<(), (u16, String)>) {
    let shape = || (400, r#"本文は {"key": "next"} の形の JSON"#.to_string());
    let mut body = Vec::new();
    if let Err(e) = req.as_reader().take(MAX_BODY + 1).read_to_end(&mut body) {
        return ("-".into(), Err((400, format!("本文を読めない: {e}"))));
    }
    if body.len() as u64 > MAX_BODY {
        return ("-".into(), Err((413, "本文が大きすぎる".into())));
    }
    let Some(name) = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("key").and_then(Value::as_str).map(str::to_string))
    else {
        return ("-".into(), Err(shape()));
    };
    let result = parse_key(&name).map_err(|msg| (400, msg)).and_then(|key| {
        keys.send(key)
            .map_err(|_| (503, "キオスクが終了中".to_string()))
    });
    (name, result)
}

/// SSE。最初に今の状態を送り、以後は変わるたびに送る。15 秒ごとに keepalive。
fn events(req: Request, shared: &Shared) {
    let mut w = req.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let mut seen = None;
    loop {
        let (json, generation) = shared.snapshot();
        let msg = match json {
            Some(v) if seen != Some(generation) => {
                seen = Some(generation);
                format!("event: status\ndata: {v}\n\n")
            }
            _ => {
                let st = shared.state.lock().unwrap();
                let (st, timeout) = shared
                    .changed
                    .wait_timeout_while(st, KEEPALIVE, |s| {
                        Some(s.generation) == seen || s.view.is_none()
                    })
                    .unwrap();
                drop(st);
                if !timeout.timed_out() {
                    continue;
                }
                ": keepalive\n\n".to_string()
            }
        };
        if w.write_all(msg.as_bytes()).and_then(|_| w.flush()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::ScreenInfo;
    use std::io::BufRead;

    const TOKEN: &str = "test-token-0123456789";

    fn view(current: usize) -> PanelView {
        PanelView {
            screens: vec![
                ScreenInfo {
                    id: "status",
                    title: "STATUS",
                },
                ScreenInfo {
                    id: "photo",
                    title: "PHOTO FRAME",
                },
            ],
            current,
            page: (current == 0).then_some("OVERVIEW"),
            photo: Some((12, 187)),
            feed_online: true,
        }
    }

    fn start(rate: u32) -> (Panel, String) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let p = Panel::spawn(server, TOKEN.into(), rate);
        let base = format!("http://{}", p.addr());
        (p, base)
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into()
    }

    fn post(base: &str, auth: Option<&str>, body: &str) -> u16 {
        let mut r = agent().post(format!("{base}/api/v1/input"));
        if let Some(t) = auth {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        r.content_type("application/json")
            .send(body)
            .unwrap()
            .status()
            .as_u16()
    }

    fn get(base: &str, path: &str, auth: bool) -> (u16, String) {
        let mut r = agent().get(format!("{base}{path}"));
        if auth {
            r = r.header("Authorization", format!("Bearer {TOKEN}"));
        }
        let mut resp = r.call().unwrap();
        let code = resp.status().as_u16();
        (code, resp.body_mut().read_to_string().unwrap_or_default())
    }

    /// キーが実行ループ側に届くまで少し待つ。
    fn recv_key(p: &Panel) -> Option<KeyPress> {
        for _ in 0..100 {
            if let Some(k) = p.keys().next() {
                return Some(k);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn listen_must_name_one_interface() {
        assert!(check_listen("192.168.0.132:8790").is_ok());
        assert!(check_listen("127.0.0.1:8790").is_ok());
        assert!(check_listen("0.0.0.0:8790").is_err());
        assert!(check_listen("[::]:8790").is_err());
        assert!(check_listen("192.168.0.132").is_err(), "ポートが無い");
    }

    #[test]
    fn keys_map_to_the_current_keypress_only() {
        assert_eq!(parse_key("prev"), Ok(KeyPress::Prev));
        assert_eq!(parse_key("next"), Ok(KeyPress::Next));
        assert_eq!(parse_key("select"), Ok(KeyPress::Switch));
        for k in ["up", "down", "menu", "quit", "Next", ""] {
            assert!(parse_key(k).is_err(), "{k}");
        }
    }

    #[test]
    fn rate_limit_resets_after_a_second() {
        let mut r = RateLimit::new(2);
        let t = Instant::now();
        assert!(r.allow(t) && r.allow(t));
        assert!(!r.allow(t + Duration::from_millis(500)));
        assert!(r.allow(t + Duration::from_secs(1)));
    }

    #[test]
    fn token_file_must_be_private_and_long_enough() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("miharashi-panel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("panel.token");
        std::fs::write(&p, format!("{TOKEN}\n")).unwrap();
        let path = p.to_str().unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(path).is_err(), "他人が読める");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token(path).unwrap(), TOKEN, "末尾の改行は除く");
        std::fs::write(&p, "short\n").unwrap();
        assert!(read_token(path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn healthz_is_open_and_everything_else_needs_the_token() {
        let (_p, base) = start(20);
        assert_eq!(
            get(&base, "/api/v1/healthz", false),
            (200, r#"{"ok":true}"#.into())
        );
        assert_eq!(get(&base, "/api/v1/status", false).0, 401);
        assert_eq!(post(&base, None, r#"{"key":"next"}"#), 401);
        assert_eq!(
            post(&base, Some("wrong-token-0123456789"), r#"{"key":"next"}"#),
            401
        );
        assert_eq!(get(&base, "/api/v1/screen", true).0, 404, "P3 まで無い");
        assert_eq!(get(&base, "/api/v1/input", true).0, 405);
    }

    #[test]
    fn input_reaches_the_run_loop() {
        let (p, base) = start(20);
        assert_eq!(p.stat(), "idle");
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"next"}"#), 204);
        assert_eq!(recv_key(&p), Some(KeyPress::Next));
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"select"}"#), 204);
        assert_eq!(recv_key(&p), Some(KeyPress::Switch));
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"up"}"#), 400);
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"quit"}"#), 400);
        assert_eq!(post(&base, Some(TOKEN), "next"), 400);
        assert_eq!(post(&base, Some(TOKEN), &"x".repeat(2000)), 413);
        assert_eq!(recv_key(&p), None, "拒否したものは届かない");
        assert_eq!(p.stat(), "connected");
    }

    #[test]
    fn bursts_get_429() {
        let (p, base) = start(3);
        let codes: Vec<u16> = (0..5)
            .map(|_| post(&base, Some(TOKEN), r#"{"key":"prev"}"#))
            .collect();
        assert_eq!(codes, [204, 204, 204, 429, 429]);
        assert_eq!(p.keys().count(), 3);
    }

    #[test]
    fn status_follows_what_the_run_loop_publishes() {
        let (p, base) = start(20);
        assert_eq!(
            get(&base, "/api/v1/status", true).0,
            503,
            "まだ置かれていない"
        );
        p.publish(Some(view(1)));
        let (code, body) = get(&base, "/api/v1/status", true);
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["screen"]["id"], "photo");
        assert_eq!(v["screen"]["index"], 1);
        assert_eq!(v["screen"]["count"], 2);
        assert_eq!(v["screens"][0]["id"], "status");
        assert_eq!(v["photo"]["position"], 12);
        assert_eq!(v["photo"]["total"], 187);
        assert_eq!(v["feed"], "online");
        p.publish(Some(view(0)));
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(v["screen"]["id"], "status");
        assert_eq!(v["screen"]["page"], "OVERVIEW");
    }

    #[test]
    fn events_send_the_current_status_then_each_change() {
        let (p, base) = start(20);
        p.publish(Some(view(0)));
        let mut s = std::net::TcpStream::connect(p.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET /api/v1/events HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        )
        .unwrap();
        let mut r = std::io::BufReader::new(s);
        let mut data = || loop {
            let mut line = String::new();
            assert!(r.read_line(&mut line).unwrap() > 0, "切れた");
            if let Some(d) = line.strip_prefix("data: ") {
                return serde_json::from_str::<Value>(d).unwrap();
            }
        };
        assert_eq!(data()["screen"]["id"], "status");
        p.publish(Some(view(0)));
        p.publish(Some(view(1)));
        assert_eq!(data()["screen"]["id"], "photo", "同じ状態は送らない");
        assert_eq!(p.stat(), "connected");
        let _ = base;
    }
}
