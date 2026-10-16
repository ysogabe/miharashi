//! 手元の操作パネル(Echo Show 5 など)向けの HTTP API(docs/panel/DESIGN.md §5.2)。
//!
//! パネルはキーボードと同じ `KeyPress` を送るだけで、画面の論理はキオスク側にある。
//! サーバーは別スレッド 1 本(写真の `Feed` と同じ形)。キーは mpsc で実行ループへ渡し、
//! 実行ループは画面が変わったときに `publish` で状態を置く。SSE の接続はそれぞれ別スレッドで待つ。
//!
//! キーは `prev` / `next` / `select` / `up` / `down` / `menu`(テンキーの 4 / 6 / 5 / 8 / 2 / 0)。
//! `POST /api/v1/screen` で画面を直接選べる(P3、#5 の S2)。
//! `POST /api/v1/rotation` で自動巡回を止めたままにする・再開する(#57)。`/status` の
//! `rotation.control` はこれを受け付けるか(巡回が有効なら常に true)。
//!
//! モニターの消灯・点灯(`POST /api/v1/display`)は、HA の手動オーバーライド(input_select)を書き換える。
//! キオスクの中だけで切り替えると HA の画面と食い違うので、実際の切り替えは `DisplayControl` に任せる。
use crate::config;
use crate::screen::{KeyPress, PanelRotation, PanelView};
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
        "select" => Ok(KeyPress::Select),
        "up" => Ok(KeyPress::Up),
        "down" => Ok(KeyPress::Down),
        "menu" => Ok(KeyPress::Menu),
        _ => Err(format!(
            "未知のキー {name:?}。使えるのは prev / next / select / up / down / menu"
        )),
    }
}

/// パネルから実行ループへ渡す操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelInput {
    Key(KeyPress),
    /// 画面の id(登録されているものだけ)
    Screen(&'static str),
    /// 自動巡回を再開する(true)・止めたままにする(false)
    Rotation(bool),
}

/// モニターの手動オーバーライド(auto / force_on / force_off)を切り替える先。実体は `ScreenPower`。
pub trait DisplayControl: Send + Sync {
    fn set_mode(&self, mode: &str) -> Result<(), String>;
}

/// `/api/v1/status` の `display`。実行ループが知っている、モニターの今の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DisplayStatus {
    /// 点いているか(DPMS)
    pub on: bool,
    /// 手動オーバーライドの実効値。HA の mode エンティティが未設定なら None
    pub mode: Option<&'static str>,
}

/// 長さに依らない比較(トークンを 1 文字ずつ当てられないように)。
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// トークンのファイルを読む。誰でも読めるファイルは使わない(ha-token と同じ root:hermes 0640 か 0600)。
fn read_token(path: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("{path}: {e}"))?
            .permissions()
            .mode();
        if mode & 0o007 != 0 {
            return Err(format!(
                "{path}: 権限が {:o}。0640(root:hermes)か 0600 にする",
                mode & 0o777
            ));
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
    display: DisplayStatus,
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
    /// モニターを切り替えられるか(HA の mode エンティティが設定してあるか)
    display_control: bool,
}

impl Shared {
    fn status_json(&self, view: &PanelView, display: DisplayStatus) -> Value {
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
            "rotation": view.rotation.map(|r| rotation_json(r, Instant::now())),
            "menu": view.menu.and_then(|i| view.screens.get(i).map(|s| json!({"index": i, "id": s.id}))),
            "keys": {
                "prev": view.keys.prev,
                "next": view.keys.next,
                "select": view.keys.select,
                "up": view.keys.up,
                "down": view.keys.down,
                "menu": view.keys.menu,
            },
            "display": {
                "on": display.on,
                "mode": display.mode,
                "control": self.display_control && display.mode.is_some(),
            },
            "uptime_s": self.started.elapsed().as_secs(),
            "generated_at": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        })
    }

    /// 今の状態の JSON と世代。まだ一度も置かれていなければ None。
    fn snapshot(&self) -> (Option<Value>, u64) {
        let st = self.state.lock().unwrap();
        (
            st.view.as_ref().map(|v| self.status_json(v, st.display)),
            st.generation,
        )
    }
}

/// `/status` の `rotation`。戻るまでの残り秒数は切り上げ(過ぎていて、まだ戻していなければ 0)。
/// `control` は `POST /api/v1/rotation` を受け付けるか。巡回が有効なら(= ここに来るなら)常に true。
/// これが無い古いキオスクでは、パネルは止める・再開するボタンを灰色にする。
fn rotation_json(r: PanelRotation, now: Instant) -> Value {
    match r {
        PanelRotation::On => json!({ "state": "on", "resume_in_s": null, "control": true }),
        PanelRotation::Off => json!({ "state": "off", "resume_in_s": null, "control": true }),
        PanelRotation::Paused { until } => {
            let left = until.saturating_duration_since(now);
            let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
            json!({ "state": "paused", "resume_in_s": secs, "control": true })
        }
    }
}

/// 実行ループ側の持ち手。
pub struct Panel {
    keys: Receiver<PanelInput>,
    shared: Arc<Shared>,
    addr: SocketAddr,
}

impl Panel {
    /// 設定どおりに待ち受けを始める。失敗したら Err(キオスクはパネルなしで動き続ける)。
    /// `display` はモニターの切り替え先(HA 連携が無ければ None。`/api/v1/display` は 409)。
    pub fn start(
        cfg: &config::Panel,
        display: Option<Arc<dyn DisplayControl>>,
    ) -> Result<Self, String> {
        let addr = check_listen(&cfg.listen)?;
        let token = read_token(&cfg.token_file)?;
        let server = Server::http(addr).map_err(|e| format!("{addr} で待ち受けられない: {e}"))?;
        Ok(Self::spawn(server, token, cfg.max_rate_per_sec, display))
    }

    fn spawn(
        server: Server,
        token: String,
        max_rate_per_sec: u32,
        display: Option<Arc<dyn DisplayControl>>,
    ) -> Self {
        let addr = server.server_addr().to_ip().expect("TCP で待ち受けている");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                view: None,
                display: DisplayStatus::default(),
                generation: 0,
            }),
            changed: Condvar::new(),
            host: hostname(),
            started: Instant::now(),
            sse: AtomicUsize::new(0),
            last_request: Mutex::new(None),
            display_control: display.is_some(),
        });
        let (tx, keys) = channel();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("panel".into())
            .spawn(move || {
                let rate = RateLimit::new(max_rate_per_sec);
                serve(server, token, rate, tx, display, s)
            })
            .expect("panel スレッドを起動できる");
        Self { keys, shared, addr }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 届いている操作。
    pub fn inputs(&self) -> TryIter<'_, PanelInput> {
        self.keys.try_iter()
    }

    /// 画面とモニターの状態を置く。前と同じなら何もしない(SSE には変わったときだけ送る)。
    pub fn publish(&self, view: Option<PanelView>, display: DisplayStatus) {
        let mut st = self.shared.state.lock().unwrap();
        if st.view != view || st.display != display {
            st.view = view;
            st.display = display;
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
    keys: Sender<PanelInput>,
    display: Option<Arc<dyn DisplayControl>>,
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
            (Method::Post, "/api/v1/screen") => {
                if !rate.allow(Instant::now()) {
                    error(429, "操作が速すぎる")
                } else {
                    let (id, result) = select_screen(&mut req, &keys, &shared);
                    let code = result.as_ref().map_or_else(|(c, _)| *c, |_| 204);
                    eprintln!("panel: {} screen {id:?} -> {code}", peer(&req));
                    match result {
                        Ok(()) => Response::from_data(Vec::new()).with_status_code(204),
                        Err((code, msg)) => error(code, &msg),
                    }
                }
            }
            (Method::Post, "/api/v1/display") => {
                if !rate.allow(Instant::now()) {
                    error(429, "操作が速すぎる")
                } else {
                    let (mode, result) = set_display(&mut req, display.as_deref());
                    let code = result.as_ref().map_or_else(|(c, _)| *c, |_| 204);
                    eprintln!("panel: {} display {mode:?} -> {code}", peer(&req));
                    match result {
                        Ok(()) => Response::from_data(Vec::new()).with_status_code(204),
                        Err((code, msg)) => error(code, &msg),
                    }
                }
            }
            (Method::Post, "/api/v1/rotation") => {
                if !rate.allow(Instant::now()) {
                    error(429, "操作が速すぎる")
                } else {
                    let (mode, result) = set_rotation(&mut req, &keys, &shared);
                    let code = result.as_ref().map_or_else(|(c, _)| *c, |_| 204);
                    eprintln!("panel: {} rotation {mode:?} -> {code}", peer(&req));
                    match result {
                        Ok(()) => Response::from_data(Vec::new()).with_status_code(204),
                        Err((code, msg)) => error(code, &msg),
                    }
                }
            }
            (
                _,
                "/api/v1/status" | "/api/v1/events" | "/api/v1/input" | "/api/v1/screen"
                | "/api/v1/display" | "/api/v1/rotation",
            ) => error(405, "メソッドが違う"),
            _ => error(404, "ない"),
        };
        let _ = req.respond(resp);
    }
}

type HttpError = (u16, String);

/// 本文 `{"<field>": "<値>"}` の値を読む。
fn read_field(req: &mut Request, field: &str, example: &str) -> Result<String, HttpError> {
    let mut body = Vec::new();
    req.as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut body)
        .map_err(|e| (400, format!("本文を読めない: {e}")))?;
    if body.len() as u64 > MAX_BODY {
        return Err((413, "本文が大きすぎる".into()));
    }
    serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get(field).and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| {
            (
                400,
                format!(r#"本文は {{"{field}": "{example}"}} の形の JSON"#),
            )
        })
}

/// `POST /api/v1/display` の本文を読んで、モニターの手動オーバーライドを切り替える。
fn set_display(
    req: &mut Request,
    display: Option<&dyn DisplayControl>,
) -> (String, Result<(), HttpError>) {
    let mode = match read_field(req, "mode", "force_off") {
        Ok(m) => m,
        Err(e) => return ("-".into(), Err(e)),
    };
    let result = if !matches!(mode.as_str(), "auto" | "force_on" | "force_off") {
        Err((400, "mode は auto / force_on / force_off".into()))
    } else if let Some(d) = display {
        d.set_mode(&mode)
            .map_err(|e| (502, format!("Home Assistant を切り替えられない: {e}")))
    } else {
        Err((
            409,
            "モニターを切り替えられない(home_assistant と screen_power.mode_entity が未設定)"
                .into(),
        ))
    };
    (mode, result)
}

/// `POST /api/v1/rotation` の本文を読んで、自動巡回を再開する(on)・止めたままにする(off)。
/// 巡回していない(`[rotation] enabled = false` か、回せる画面が無い)なら 409。戻り値: (mode(ログ用), 結果)。
fn set_rotation(
    req: &mut Request,
    keys: &Sender<PanelInput>,
    shared: &Shared,
) -> (String, Result<(), HttpError>) {
    let mode = match read_field(req, "mode", "off") {
        Ok(m) => m,
        Err(e) => return ("-".into(), Err(e)),
    };
    let on = match mode.as_str() {
        "on" => true,
        "off" => false,
        _ => return (mode, Err((400, "mode は on / off".into()))),
    };
    let enabled = match &shared.state.lock().unwrap().view {
        None => Err((503, "起動中".to_string())),
        Some(v) if v.rotation.is_none() => Err((
            409,
            "巡回していない([rotation] enabled = false か、回せる画面が無い)".to_string(),
        )),
        Some(_) => Ok(()),
    };
    let result = enabled.and_then(|()| {
        keys.send(PanelInput::Rotation(on))
            .map_err(|_| (503, "キオスクが終了中".to_string()))
    });
    (mode, result)
}

/// `POST /api/v1/input` の本文を読んでキーを送る。戻り値: (キーの名前(ログ用), 結果)。Err は (状態コード, 本文)。
fn input(req: &mut Request, keys: &Sender<PanelInput>) -> (String, Result<(), HttpError>) {
    let name = match read_field(req, "key", "next") {
        Ok(n) => n,
        Err(e) => return ("-".into(), Err(e)),
    };
    let result = parse_key(&name).map_err(|msg| (400, msg)).and_then(|key| {
        keys.send(PanelInput::Key(key))
            .map_err(|_| (503, "キオスクが終了中".to_string()))
    });
    (name, result)
}

/// `POST /api/v1/screen` の本文を読んで、登録されている画面なら切り替えを送る。戻り値: (id(ログ用), 結果)。
fn select_screen(
    req: &mut Request,
    keys: &Sender<PanelInput>,
    shared: &Shared,
) -> (String, Result<(), HttpError>) {
    let id = match read_field(req, "id", "photo") {
        Ok(i) => i,
        Err(e) => return ("-".into(), Err(e)),
    };
    let found = {
        let st = shared.state.lock().unwrap();
        match &st.view {
            None => Err((503, "起動中".to_string())),
            Some(v) => v
                .screens
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.id)
                .ok_or_else(|| (404, format!("登録されていない画面 {id:?}"))),
        }
    };
    let result = found.and_then(|sid| {
        keys.send(PanelInput::Screen(sid))
            .map_err(|_| (503, "キオスクが終了中".to_string()))
    });
    (id, result)
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
    use crate::screen::{PanelKeys, ScreenInfo};
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
            rotation: None,
            menu: None,
            keys: PanelKeys::default(),
        }
    }

    fn start(rate: u32) -> (Panel, String) {
        start_with(rate, None)
    }

    fn start_with(rate: u32, display: Option<Arc<dyn DisplayControl>>) -> (Panel, String) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let p = Panel::spawn(server, TOKEN.into(), rate, display);
        let base = format!("http://{}", p.addr());
        (p, base)
    }

    /// HA の代わり。受け取ったモードを記録し、`fail` なら失敗する。
    #[derive(Default)]
    struct FakeDisplay {
        modes: Mutex<Vec<String>>,
        fail: bool,
    }

    impl DisplayControl for FakeDisplay {
        fn set_mode(&self, mode: &str) -> Result<(), String> {
            if self.fail {
                return Err("connection refused".into());
            }
            self.modes.lock().unwrap().push(mode.into());
            Ok(())
        }
    }

    fn post_display(base: &str, body: &str) -> u16 {
        agent()
            .post(format!("{base}/api/v1/display"))
            .header("Authorization", format!("Bearer {TOKEN}"))
            .content_type("application/json")
            .send(body)
            .unwrap()
            .status()
            .as_u16()
    }

    #[test]
    fn display_mode_goes_to_the_control() {
        let fake = Arc::new(FakeDisplay::default());
        let (_p, base) = start_with(20, Some(fake.clone()));
        assert_eq!(post_display(&base, r#"{"mode":"force_off"}"#), 204);
        assert_eq!(post_display(&base, r#"{"mode":"auto"}"#), 204);
        assert_eq!(post_display(&base, r#"{"mode":"off"}"#), 400);
        assert_eq!(post_display(&base, r#"{"key":"next"}"#), 400);
        assert_eq!(*fake.modes.lock().unwrap(), ["force_off", "auto"]);

        let failing = Arc::new(FakeDisplay {
            fail: true,
            ..Default::default()
        });
        let (_p, base) = start_with(20, Some(failing));
        assert_eq!(
            post_display(&base, r#"{"mode":"force_on"}"#),
            502,
            "HA に届かない"
        );

        let (_p, base) = start(20);
        assert_eq!(
            post_display(&base, r#"{"mode":"force_on"}"#),
            409,
            "HA 連携なし"
        );
    }

    #[test]
    fn status_reports_the_monitor() {
        let (p, base) = start_with(20, Some(Arc::new(FakeDisplay::default())));
        p.publish(
            Some(view(1)),
            DisplayStatus {
                on: false,
                mode: Some("force_off"),
            },
        );
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(
            v["display"],
            json!({"on": false, "mode": "force_off", "control": true})
        );

        // HA 連携が無ければ切り替えられない
        let (p, base) = start(20);
        p.publish(
            Some(view(1)),
            DisplayStatus {
                on: true,
                mode: None,
            },
        );
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(
            v["display"],
            json!({"on": true, "mode": null, "control": false})
        );
    }

    #[test]
    fn status_reports_the_menu() {
        let (p, base) = start(20);
        let menu = |p: &Panel, m| {
            p.publish(
                Some(PanelView { menu: m, ..view(0) }),
                DisplayStatus::default(),
            );
            let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
            v["menu"].clone()
        };
        assert_eq!(menu(&p, None), Value::Null, "閉じている");
        assert_eq!(menu(&p, Some(1)), json!({"index": 1, "id": "photo"}));
        assert_eq!(menu(&p, Some(0)), json!({"index": 0, "id": "status"}));
    }

    #[test]
    fn status_reports_the_keys() {
        let (p, base) = start(20);
        p.publish(
            Some(PanelView {
                keys: PanelKeys {
                    prev: Some("前の画面"),
                    next: Some("次の画面"),
                    select: Some("詳細"),
                    up: None,
                    down: None,
                    menu: Some("メニュー"),
                },
                ..view(0)
            }),
            DisplayStatus::default(),
        );
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(
            v["keys"],
            json!({"prev": "前の画面", "next": "次の画面", "select": "詳細", "up": null, "down": null, "menu": "メニュー"})
        );
    }

    #[test]
    fn status_reports_the_rotation() {
        let (p, base) = start(20);
        let rotation = |p: &Panel, r| {
            p.publish(
                Some(PanelView {
                    rotation: r,
                    ..view(1)
                }),
                DisplayStatus::default(),
            );
            let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
            v["rotation"].clone()
        };
        assert_eq!(rotation(&p, None), Value::Null, "巡回が無効");
        assert_eq!(
            rotation(&p, Some(PanelRotation::On)),
            json!({"state": "on", "resume_in_s": null, "control": true})
        );
        let until = Instant::now() + Duration::from_secs(240);
        let v = rotation(&p, Some(PanelRotation::Paused { until }));
        assert_eq!(v["state"], "paused");
        assert_eq!(v["control"], true);
        let left = v["resume_in_s"].as_u64().unwrap();
        assert!((1..=240).contains(&left), "{left}");
        assert_eq!(
            rotation(&p, Some(PanelRotation::Off)),
            json!({"state": "off", "resume_in_s": null, "control": true}),
            "止めたまま"
        );
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(v["schema_version"], 1, "項目と値を足すだけ");
    }

    #[test]
    fn resume_in_s_rounds_up_and_stops_at_zero() {
        let now = Instant::now();
        let at = |ms| {
            rotation_json(
                PanelRotation::Paused {
                    until: now + Duration::from_millis(ms),
                },
                now,
            )["resume_in_s"]
                .clone()
        };
        assert_eq!(at(240_000), 240);
        assert_eq!(at(239_001), 240, "切り上げ");
        assert_eq!(at(1), 1);
        assert_eq!(at(0), 0);
        let past = rotation_json(
            PanelRotation::Paused { until: now },
            now + Duration::from_secs(3),
        );
        assert_eq!(past["resume_in_s"], 0, "過ぎていても 0 未満にしない");
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into()
    }

    fn post(base: &str, auth: Option<&str>, body: &str) -> u16 {
        post_to(base, "/api/v1/input", auth, body)
    }

    fn post_to(base: &str, path: &str, auth: Option<&str>, body: &str) -> u16 {
        let mut r = agent().post(format!("{base}{path}"));
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
    fn recv_key(p: &Panel) -> Option<PanelInput> {
        for _ in 0..100 {
            if let Some(k) = p.inputs().next() {
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
    fn keys_map_to_keypress() {
        assert_eq!(parse_key("prev"), Ok(KeyPress::Prev));
        assert_eq!(parse_key("next"), Ok(KeyPress::Next));
        assert_eq!(parse_key("select"), Ok(KeyPress::Select));
        assert_eq!(parse_key("up"), Ok(KeyPress::Up));
        assert_eq!(parse_key("down"), Ok(KeyPress::Down));
        assert_eq!(parse_key("menu"), Ok(KeyPress::Menu));
        for k in ["quit", "Next", ""] {
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
        assert!(read_token(path).is_err(), "誰でも読める");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_token(path).is_ok(), "グループ(hermes)には読ませてよい");
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
        assert_eq!(get(&base, "/api/v1/screen", true).0, 405);
        assert_eq!(
            post_to(&base, "/api/v1/screen", None, r#"{"id":"photo"}"#),
            401
        );
        assert_eq!(get(&base, "/api/v1/nothing", true).0, 404);
        assert_eq!(get(&base, "/api/v1/input", true).0, 405);
        assert_eq!(get(&base, "/api/v1/rotation", true).0, 405);
        assert_eq!(
            post_to(&base, "/api/v1/rotation", None, r#"{"mode":"off"}"#),
            401
        );
    }

    #[test]
    fn input_reaches_the_run_loop() {
        let (p, base) = start(20);
        assert_eq!(p.stat(), "idle");
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"next"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Key(KeyPress::Next)));
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"select"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Key(KeyPress::Select)));
        assert_eq!(post(&base, Some(TOKEN), r#"{"key":"menu"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Key(KeyPress::Menu)));
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
        assert_eq!(p.inputs().count(), 3);
    }

    #[test]
    fn screen_selects_only_registered_ids() {
        let (p, base) = start(20);
        let screen = |body: &str| post_to(&base, "/api/v1/screen", Some(TOKEN), body);
        assert_eq!(screen(r#"{"id":"photo"}"#), 503, "画面の一覧がまだない");
        p.publish(Some(view(0)), DisplayStatus::default());
        assert_eq!(screen(r#"{"id":"photo"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Screen("photo")));
        assert_eq!(screen(r#"{"id":"room"}"#), 404, "登録されていない");
        assert_eq!(screen(r#"{"key":"photo"}"#), 400);
        assert_eq!(recv_key(&p), None, "拒否したものは届かない");
    }

    #[test]
    fn rotation_on_off_reaches_the_run_loop() {
        let (p, base) = start(20);
        let rotation = |body: &str| post_to(&base, "/api/v1/rotation", Some(TOKEN), body);
        assert_eq!(rotation(r#"{"mode":"off"}"#), 503, "まだ置かれていない");
        p.publish(Some(view(0)), DisplayStatus::default());
        assert_eq!(rotation(r#"{"mode":"off"}"#), 409, "巡回が無効");
        assert_eq!(recv_key(&p), None, "拒否したものは届かない");
        p.publish(
            Some(PanelView {
                rotation: Some(PanelRotation::On),
                ..view(0)
            }),
            DisplayStatus::default(),
        );
        assert_eq!(rotation(r#"{"mode":"off"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Rotation(false)));
        assert_eq!(rotation(r#"{"mode":"on"}"#), 204);
        assert_eq!(recv_key(&p), Some(PanelInput::Rotation(true)));
        assert_eq!(rotation(r#"{"mode":"paused"}"#), 400, "知らない mode");
        assert_eq!(rotation(r#"{"mode":"Off"}"#), 400);
        assert_eq!(rotation(r#"{"key":"off"}"#), 400, "項目が違う");
        assert_eq!(rotation(&"x".repeat(2000)), 413);
        assert_eq!(recv_key(&p), None, "拒否したものは届かない");
    }

    #[test]
    fn rotation_bursts_get_429() {
        let (p, base) = start(2);
        p.publish(
            Some(PanelView {
                rotation: Some(PanelRotation::On),
                ..view(0)
            }),
            DisplayStatus::default(),
        );
        let codes: Vec<u16> = (0..3)
            .map(|_| post_to(&base, "/api/v1/rotation", Some(TOKEN), r#"{"mode":"off"}"#))
            .collect();
        assert_eq!(codes, [204, 204, 429]);
        assert_eq!(p.inputs().count(), 2);
    }

    #[test]
    fn status_follows_what_the_run_loop_publishes() {
        let (p, base) = start(20);
        assert_eq!(
            get(&base, "/api/v1/status", true).0,
            503,
            "まだ置かれていない"
        );
        p.publish(Some(view(1)), DisplayStatus::default());
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
        p.publish(Some(view(0)), DisplayStatus::default());
        let v: Value = serde_json::from_str(&get(&base, "/api/v1/status", true).1).unwrap();
        assert_eq!(v["screen"]["id"], "status");
        assert_eq!(v["screen"]["page"], "OVERVIEW");
    }

    #[test]
    fn events_send_the_current_status_then_each_change() {
        let (p, base) = start(20);
        p.publish(Some(view(0)), DisplayStatus::default());
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
        p.publish(Some(view(0)), DisplayStatus::default());
        p.publish(Some(view(1)), DisplayStatus::default());
        assert_eq!(data()["screen"]["id"], "photo", "同じ状態は送らない");
        let on = DisplayStatus {
            on: true,
            mode: Some("auto"),
        };
        p.publish(Some(view(1)), on);
        assert_eq!(data()["display"]["on"], true, "モニターだけ変わっても送る");
        let menu = PanelView {
            menu: Some(0),
            ..view(1)
        };
        p.publish(Some(menu), on);
        assert_eq!(
            data()["menu"],
            json!({"index": 0, "id": "status"}),
            "メニューの状態だけ変わっても送る"
        );
        p.publish(Some(view(1)), on);
        assert_eq!(data()["menu"], Value::Null, "閉じたときも送る");
        let keys = PanelView {
            keys: PanelKeys {
                select: Some("概要"),
                ..PanelKeys::default()
            },
            ..view(1)
        };
        p.publish(Some(keys), on);
        assert_eq!(
            data()["keys"]["select"],
            "概要",
            "キーの意味だけ変わっても送る"
        );
        p.publish(Some(view(1)), on);
        assert_eq!(data()["keys"]["select"], Value::Null);
        let until = Instant::now() + Duration::from_secs(300);
        let paused = |until| PanelView {
            rotation: Some(PanelRotation::Paused { until }),
            ..view(1)
        };
        p.publish(Some(paused(until)), on);
        let v = data();
        assert_eq!(
            v["rotation"]["state"], "paused",
            "巡回の状態だけ変わっても送る"
        );
        let first = v["rotation"]["resume_in_s"].as_u64().unwrap();
        let generation = p.shared.state.lock().unwrap().generation;
        std::thread::sleep(Duration::from_millis(1100));
        p.publish(Some(paused(until)), on);
        assert_eq!(
            p.shared.state.lock().unwrap().generation,
            generation,
            "止めている間、時間が進むだけでは送らない"
        );
        p.publish(Some(paused(until + Duration::from_secs(5))), on);
        let v = data();
        assert_eq!(v["rotation"]["state"], "paused");
        assert!(
            v["rotation"]["resume_in_s"].as_u64().unwrap() > first,
            "もう一度操作して止める時間が延びたら送る"
        );
        let rotation = |r| PanelView {
            rotation: Some(r),
            ..view(1)
        };
        p.publish(Some(rotation(PanelRotation::On)), on);
        assert_eq!(data()["rotation"]["state"], "on");
        p.publish(Some(rotation(PanelRotation::Off)), on);
        assert_eq!(
            data()["rotation"],
            json!({"state": "off", "resume_in_s": null, "control": true}),
            "止めたままにしたら送る"
        );
        p.publish(Some(rotation(PanelRotation::On)), on);
        assert_eq!(data()["rotation"]["state"], "on", "再開したら送る");
        assert_eq!(p.stat(), "connected");
        let _ = base;
    }
}
