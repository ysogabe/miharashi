//! 待ち受けと 1 要求の処理。接続は 1 本ずつ順に処理する(書き込みも自然に直列になる。`feed` のロックは別途ある)。
//!
//! 順序: accept → 送信元 IP の照合(外れたら何も読まずに 403 で切る)→ 要求行 + ヘッダー → 経路(404 / 405)→
//! 認証(401)→ レート制限(429)→ 本文(認証を通ったときだけ読む)→ 検証(400)→ 書き込み。
//! レート制限には、許可リストを通った送信元の要求を、認証に成功した `GET /api/v1/health` を除いてすべて数える
//! (認証前に終わる要求も含む)。枠が空なら、認証前に終わる要求の応答も 429 に置き換わる。

use crate::config::{Config, normalize_ip};
use crate::feed::{self, FeedError};
use crate::http::{
    self, HttpError, IO_DEADLINE, MAX_BODY_BYTES, Response, TimedRead, read_body, read_head,
    write_response,
};
use crate::notice::{self, rfc3339, truncate_secs};
use crate::ratelimit::RateLimiter;
use crate::token::{self, ct_eq};
use chrono::{DateTime, FixedOffset, Local};
use serde_json::json;
use std::io::Read;
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PATH_NOTICES: &str = "/api/v1/notices";
pub const PATH_HEALTH: &str = "/api/v1/health";
/// 応答のあと、読み残しを捨てるために待つ長さと量の上限。
const DRAIN_WAIT: Duration = Duration::from_secs(1);
const DRAIN_MAX_BYTES: usize = 16 * 1024;

pub struct Server {
    listener: TcpListener,
    cfg: Config,
    euid: u32,
    limiter: Mutex<RateLimiter>,
}

/// 1 要求の結果(ログと応答に使う)。
struct Handled {
    method: String,
    path: String,
    response: Response,
    id: Option<String>,
}

/// `Server::screen` の結果。
enum Screened {
    /// 認証前に終わった(読み取りの失敗、404、405、ヘッダーの不正、401、503)。数える。
    Done(Handled),
    /// 認証に成功した `GET /api/v1/health`。数えない。
    Health(Handled),
    /// 認証に成功した `POST /api/v1/notices`。数えてから本文を読む。
    Post(http::Head, Vec<u8>),
}

impl Server {
    /// 設定を確かめ、トークンファイルが規則に合うことを確かめて、待ち受けを始める。
    pub fn bind(cfg: Config) -> Result<Self, String> {
        cfg.validate()?;
        let euid = token::effective_uid();
        token::read_token(&cfg.token_file, euid).map_err(|e| e.message().to_string())?;
        let listener = TcpListener::bind(cfg.listen)
            .map_err(|e| format!("{} で待ち受けられません: {e}", cfg.listen))?;
        let limiter = Mutex::new(RateLimiter::new(cfg.burst, cfg.per_minute));
        Ok(Self {
            listener,
            cfg,
            euid,
            limiter,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// 接続を受けて 1 本ずつ処理し続ける。
    pub fn run(&self) -> ! {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => self.serve(stream),
                Err(e) => {
                    eprintln!("accept に失敗: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    fn serve(&self, mut stream: TcpStream) {
        let deadline = Instant::now() + IO_DEADLINE;
        let _ = stream.set_write_timeout(Some(IO_DEADLINE));
        let Ok(peer) = stream.peer_addr().map(|a| normalize_ip(a.ip())) else {
            return;
        };
        // 何も読む前に送信元を照合する
        if !self.cfg.allow.iter().any(|a| normalize_ip(*a) == peer) {
            let r = Response::error(403, "forbidden");
            let _ = write_response(&mut stream, &r);
            log(peer, "-", "-", r.status, None);
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        let h = self.handle(peer, &mut stream, deadline);
        let _ = write_response(&mut stream, &h.response);
        log(peer, &h.method, &h.path, h.response.status, h.id.as_deref());
        finish(stream);
    }

    /// 1 要求の処理。許可リストを通った送信元の要求は、認証に成功した `GET /api/v1/health` を除いて、
    /// 必ず 1 件として数える。数える場所はここ 1 か所に集めた(`screen` が `Screened::Done` を返したら数え、
    /// `Screened::Post` を返したら本文を読む前に数える)。`screen` の中でどこから抜けても数え漏れない。
    fn handle<S: TimedRead>(&self, peer: IpAddr, s: &mut S, deadline: Instant) -> Handled {
        let (head, prefix) = match self.screen(s, deadline) {
            Screened::Health(h) => return h,
            Screened::Done(h) => {
                return match self.charge(peer) {
                    Ok(()) => h,
                    Err(r) => Handled {
                        response: r,
                        id: None,
                        ..h
                    },
                };
            }
            Screened::Post(head, prefix) => (head, prefix),
        };
        let done = |response: Response| Handled {
            method: head.method.clone(),
            path: head.path.clone(),
            response,
            id: None,
        };
        if let Err(r) = self.charge(peer) {
            return done(r);
        }
        let (method, path) = (head.method.as_str(), head.path.as_str());

        // POST /api/v1/notices
        match head.single("content-type") {
            Ok(Some(ct)) if is_json_media_type(ct) => {}
            Ok(_) => {
                return done(Response::error(
                    415,
                    "content-type must be application/json",
                ));
            }
            Err(e) => return done(bad(e)),
        }
        let len = match head.single("content-length") {
            Ok(Some(v)) => match http::parse_content_length(v) {
                Ok(n) if n <= MAX_BODY_BYTES => n,
                Ok(_) => return done(bad(HttpError::BodyTooLarge)),
                Err(e) => return done(bad(e)),
            },
            Ok(None) => {
                return done(Response::error(411, "content-length required"));
            }
            Err(e) => return done(bad(e)),
        };
        let body = match read_body(s, prefix, len, deadline) {
            Ok(b) => b,
            Err(e) => return done(bad(e)),
        };
        let now = now_secs();
        let new = match notice::parse_request(&body, now) {
            Ok(n) => n,
            Err(msg) => return done(Response::error(400, msg)),
        };
        match feed::append_notice(&self.cfg.feed, &new, now) {
            Ok(p) => Handled {
                method: method.to_string(),
                path: path.to_string(),
                response: Response::json(
                    201,
                    json!({"id": p.id, "posted_at": p.posted_at, "expires_at": p.expires_at})
                        .to_string(),
                ),
                id: Some(p.id),
            },
            Err(e) => done(feed_error_response(e)),
        }
    }

    /// 要求行 + ヘッダーの読み取り、経路(404 / 405)、認証までを行う。ここでは数えない(`handle` が数える)。
    fn screen<S: TimedRead>(&self, s: &mut S, deadline: Instant) -> Screened {
        let done = |method: &str, path: &str, response: Response| {
            Screened::Done(Handled {
                method: method.to_string(),
                path: path.to_string(),
                response,
                id: None,
            })
        };
        let (head, prefix) = match read_head(s, deadline) {
            Ok(v) => v,
            Err(e) => {
                let r = e.response().map_or_else(
                    || Response::error(400, "bad request"),
                    |(c, m)| Response::error(c, m),
                );
                return done("-", "-", r);
            }
        };
        let (method, path) = (head.method.as_str(), head.path.as_str());
        let allowed = match path {
            PATH_NOTICES => "POST",
            PATH_HEALTH => "GET",
            _ => return done(method, path, Response::error(404, "not found")),
        };
        if method != allowed {
            let r = Response::error(405, "method not allowed").with_header("Allow", allowed.into());
            return done(method, path, r);
        }

        // 認証。トークンは要求ごとに読み直す(差し替えがすぐ効く)。読めないときは通さない
        let token = match token::read_token(&self.cfg.token_file, self.euid) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("token ファイルを使えません: {}", e.message());
                return done(method, path, Response::error(503, "token unavailable"));
            }
        };
        let presented = match head.single("authorization") {
            Ok(v) => v.and_then(bearer_token),
            Err(e) => return done(method, path, bad(e)),
        };
        let authed = ct_eq(presented.unwrap_or("").as_bytes(), token.as_bytes());
        drop(token);
        if !authed {
            let r = Response::error(401, "unauthorized")
                .with_header("WWW-Authenticate", "Bearer".into());
            return done(method, path, r);
        }
        if path == PATH_HEALTH {
            // 認証に成功した疎通確認だけは数えない
            return Screened::Health(Handled {
                method: method.to_string(),
                path: path.to_string(),
                response: Response::json(200, json!({"ok": true}).to_string()),
                id: None,
            });
        }
        Screened::Post(head, prefix)
    }

    /// 送信元 IP のバケットから 1 件ぶんを使う。足りなければ 429(`Retry-After` つき)。
    fn charge(&self, peer: IpAddr) -> Result<(), Response> {
        let mut l = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
        l.acquire(peer, Instant::now()).map_err(|wait| {
            Response::error(429, "too many requests")
                .with_header("Retry-After", wait.as_secs().saturating_add(1).to_string())
        })
    }
}

fn bad(e: HttpError) -> Response {
    let (code, msg) = e.response().unwrap_or((400, "bad request"));
    Response::error(code, msg)
}

pub fn feed_error_response(e: FeedError) -> Response {
    match e {
        FeedError::Unreadable => Response::error(503, "feed unreadable"),
        FeedError::Full => Response::error(409, "feed full"),
        FeedError::TooLarge => Response::error(413, "feed too large"),
        FeedError::Busy => Response::error(503, "feed busy"),
        FeedError::Io => Response::error(500, "feed write failed"),
    }
}

/// `Authorization: Bearer <token>`(スキームは大文字小文字を問わない)から token を取り出す。
fn bearer_token(v: &str) -> Option<&str> {
    let (scheme, rest) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| rest.trim_matches(' '))
}

/// `application/json`(`; charset=utf-8` などのパラメーター付きも可)か。
fn is_json_media_type(v: &str) -> bool {
    v.split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

fn now_secs() -> DateTime<FixedOffset> {
    truncate_secs(Local::now().fixed_offset())
}

/// 応答を送り終えたら、書き込み側を閉じて、読み残し(本文など)を少しだけ捨てる。
/// 読み残しがあるまま閉じると、相手に RST が届いて応答が消えることがある。
fn finish(mut stream: TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Instant::now() + DRAIN_WAIT;
    let mut buf = [0u8; 1024];
    let mut total = 0;
    while total < DRAIN_MAX_BYTES {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

/// 1 要求 1 行。ヘッダー・トークン・本文は出さない。メソッドとパスは相手が決める文字列なので、
/// 印字できる ASCII だけ残して短くする。
fn log(peer: IpAddr, method: &str, path: &str, status: u16, id: Option<&str>) {
    eprintln!(
        "{} {} {} {} {} {}",
        rfc3339(&now_secs()),
        peer,
        sanitize(method, 16),
        sanitize(path, 64),
        status,
        id.unwrap_or("-")
    );
}

fn sanitize(s: &str, max: usize) -> String {
    if s.is_empty() {
        return "-".into();
    }
    s.chars()
        .take(max)
        .map(|c| if c.is_ascii_graphic() { c } else { '?' })
        .collect()
}
