//! 管理ページの API(docs/admin-web/design.md)。パネルのスレッドは認証とパスの判定だけをして、
//! 管理の `Request` をここの管理ワーカー(1 本)に渡す。本文の読み取り・処理・応答はワーカーで行うので、
//! 遅いクライアントがいても操作パネルのキーは待たされない。
//!
//! 今ある経路: `GET /api/v1/admin/overview`、`POST /api/v1/admin/restart`。
//! `PUT /api/v1/admin/config`(T3)と `/admin*` の静的配信(T5)は、表には入れて今は 404。
use super::{
    AdminSources, HttpError, PanelInput, RateLimit, Reaper, Route, Shared, body_prefetched, error,
    json_response, same_secret,
};
use crate::config::{
    self, Config, LoadedConfig, Overlay, OverlayErrorKind, overlay_generation, remove_boot_marker,
};
use serde_json::{Value, json};
use std::io::Read;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Request};

/// `Panel::start` に渡す、管理ページの材料。
pub struct AdminContext {
    pub loaded: LoadedConfig,
    /// プロセスの起動時刻(再起動の抑止と `uptime_s` の基準)
    pub started: Instant,
    pub sources: AdminSources,
}

/// 本文の上限。これを超えたら 413。
const MAX_BODY: usize = 16 * 1024;
/// 本文を読み切るまでの期限(受信を始めてから、読み取り全体で)。超えたらワーカーは読むのを諦める。
const BODY_DEADLINE: Duration = Duration::from_secs(5);
/// 期限を過ぎても読み取りから戻らない読み取りスレッドの数の上限。超えたら新しい本文は 429 で断る。
const MAX_STALLED_READERS: usize = 4;
/// 起動からこの時間は再起動を受け付けない(systemd の起動制限に掛からないように)。
const RESTART_AFTER: Duration = Duration::from_secs(120);
/// `error` / `overlay.error` の長さの上限(文字数)。
const MAX_ERROR_CHARS: usize = 200;

/// パネルのスレッドが持つ、管理ワーカーへの入口。
pub(super) struct Admin {
    token: String,
    tx: Sender<Job>,
    /// ワーカーが 1 件を受け取ってから応答を終えるまで立つ(待ち行列は作らない)
    busy: Arc<AtomicBool>,
}

struct Job {
    req: Request,
    route: Route,
}

impl Admin {
    pub(super) fn token(&self) -> &str {
        &self.token
    }

    /// ワーカーに渡す。忙しい(処理中)・ワーカーが無いときは要求を返す(呼び出し側が 429 を返す)。
    /// `Request` は大きいので箱に入れて返す。
    pub(super) fn submit(&self, req: Request, route: Route) -> Result<(), Box<Request>> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Box::new(req));
        }
        self.tx.send(Job { req, route }).map_err(|e| {
            self.busy.store(false, Ordering::Release);
            Box::new(e.0.req)
        })
    }
}

/// ワーカーが使うもの。
struct Ctx {
    loaded: LoadedConfig,
    started: Instant,
    #[allow(dead_code)] // T4(feeds)で使う
    sources: AdminSources,
    /// `restart` と、T3 の `PUT config` で共有する(1 秒に 5 回)
    limiter: Mutex<RateLimit>,
    /// 再起動を受け付けて、終了を待っている
    restart_requested: AtomicBool,
    shared: Arc<Shared>,
    keys: Sender<PanelInput>,
    /// 本文を読み残した要求の応答と手放しを任せる(要求ごとのスレッド)
    reaper: Reaper,
    /// 動いている読み取りスレッドの数
    readers: Arc<AtomicUsize>,
}

/// 管理トークンのファイルを読む。`agents::read_token` と同じ厳しい規則: 実行ユーザーの 0600、または
/// root:実行グループの 0640。16 文字以上で 1 行。`euid` / `egid` は実行ユーザーとグループ。
fn read_admin_token(path: &str, euid: u32, egid: u32) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if !crate::agents::token_perms_ok(meta.mode(), meta.uid(), meta.gid(), euid, egid) {
        return Err(format!(
            "{path}: 所有者か権限が規則に合わない(権限 {:o})。実行ユーザーの 0600、または root:実行グループの 0640 にする",
            meta.mode() & 0o777
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let token = text.trim();
    if token.len() < 16 {
        return Err(format!("{path}: トークンが短い(16 文字以上)"));
    }
    if token.contains(['\r', '\n']) {
        return Err(format!("{path}: トークンは 1 行で書く"));
    }
    Ok(token.to_string())
}

/// `[admin]` が有効なら管理トークンを読み、管理ワーカーを起こす。管理トークンが規則を満たさない、
/// またはパネルのトークンと同じ値なら、管理ページだけを無効にして(stderr に 1 行)キオスクは動かす。
/// 戻り値: (入口、無効にした理由)。
pub(super) fn prepare(
    ctx: Option<AdminContext>,
    panel_token: &str,
    shared: &Arc<Shared>,
    keys: &Sender<PanelInput>,
    local: SocketAddr,
    reaper: &Reaper,
) -> (Option<Admin>, Option<String>) {
    let Some(ctx) = ctx.filter(|c| c.loaded.effective.admin.enabled) else {
        return (None, None);
    };
    let path = ctx.loaded.effective.admin.token_file.clone();
    let token = read_admin_token(
        &path,
        crate::agents::effective_uid(),
        crate::agents::effective_gid(),
    )
    .and_then(|t| {
        if same_secret(t.as_bytes(), panel_token.as_bytes()) {
            Err(format!("{path}: パネルのトークンと同じ値(別の値にする)"))
        } else {
            Ok(t)
        }
    });
    let token = match token {
        Ok(t) => t,
        Err(e) => {
            eprintln!("admin: 管理ページを無効にした(キオスクは動かす): {e}");
            return (None, Some(e));
        }
    };
    let (tx, rx) = channel();
    let busy = Arc::new(AtomicBool::new(false));
    let ctx = Arc::new(Ctx {
        loaded: ctx.loaded,
        started: ctx.started,
        sources: ctx.sources,
        limiter: Mutex::new(RateLimit::new(5)),
        restart_requested: AtomicBool::new(false),
        shared: shared.clone(),
        keys: keys.clone(),
        reaper: reaper.clone(),
        readers: Arc::new(AtomicUsize::new(0)),
    });
    let b = busy.clone();
    std::thread::Builder::new()
        .name("admin".into())
        .spawn(move || worker(rx, ctx, b))
        .expect("admin スレッドを起動できる");
    eprintln!("admin: 管理ページが有効({local})");
    (Some(Admin { token, tx, busy }), None)
}

fn worker(rx: Receiver<Job>, ctx: Arc<Ctx>, busy: Arc<AtomicBool>) {
    for job in rx {
        // 1 件の失敗(panic)でワーカーを止めない
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| handle(job, &ctx)));
        if r.is_err() {
            eprintln!("admin: 処理中に panic した");
        }
        busy.store(false, Ordering::Release);
    }
}

fn handle(job: Job, ctx: &Ctx) {
    let Job { req, route } = job;
    let who = super::peer(&req);
    let (req, _body) = match read_body(req, ctx) {
        Body::Done(req, body) => (req, body),
        Body::Refused(req, (code, msg)) => {
            eprintln!("admin: {who} 本文 -> {code}: {msg}");
            // 本文を読み残した接続は使い回さない。読み残しの手放しは後始末スレッドに任せる
            ctx.reaper.reply(
                req,
                error(code, &msg).with_header(Header::from_bytes("Connection", "close").unwrap()),
            );
            return;
        }
        Body::GaveUp => {
            eprintln!(
                "admin: {who} 本文 -> 408: 期限に間に合わなかった(読み取りスレッドが後で応える)"
            );
            return;
        }
    };
    // 今の経路は本文を使わない(T3 の PUT config が使う)
    match route {
        Route::AdminOverview => {
            let _ = req.respond(json_response(200, &overview(ctx)));
        }
        Route::AdminRestart => restart(req, ctx, &who),
        // PUT config(T3)と静的配信(T5)は未実装
        _ => {
            let _ = req.respond(error(404, "ない"));
        }
    }
}

/// 本文を読んだ結果。
enum Body {
    /// 読み切った(要求は手元に戻る。本文は読み切っているので、応答しても手放しても待たない)
    Done(Request, Vec<u8>),
    /// 断る(要求は手元に戻る。呼び出し側が応答を書いて、後始末スレッドへ渡す)
    Refused(Request, HttpError),
    /// 期限までに読み切れなかった。要求は読み取りスレッドの手元に残る(そのスレッドが読み取りから
    /// 戻ったときに 408 を書く)ので、ワーカーはもう触れない
    GaveUp,
}

/// 読み取りスレッドとワーカーの受け渡し。1 回ずつ使い捨てなので、大きさの差は気にしない。
#[allow(clippy::large_enum_variant)]
enum Phase {
    Reading,
    Done(Request, Result<Vec<u8>, HttpError>),
    GaveUp,
}

struct Handoff {
    phase: Mutex<Phase>,
    changed: Condvar,
}

/// 動いている読み取りスレッドの数を数える。
struct ReaderCount(Arc<AtomicUsize>);

impl Drop for ReaderCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 本文を読む(上限 16 KiB は 413、読み始めから 5 秒の絶対期限は 408)。
///
/// tiny_http が読み切っていない本文(`body_prefetched` が偽)は、相手が送るまで `read` が戻らず、
/// 戻るまで `Request` は読み取りの中に居るので、期限が来ても 408 を書けない。そこで読み取りを
/// 要求ごとの別スレッドで行い、ワーカーは期限まで待って諦める(`Body::GaveUp`)。読み取りが戻った
/// ところで、そのスレッドが 408 を書いて手放す(手放しで止まるのもそのスレッドだけ)。
fn read_body(mut req: Request, ctx: &Ctx) -> Body {
    let deadline = Instant::now() + BODY_DEADLINE;
    match req.body_length() {
        Some(0) => return Body::Done(req, Vec::new()),
        Some(n) if n > MAX_BODY => return Body::Refused(req, (413, "本文が大きすぎる".into())),
        _ => {}
    }
    if body_prefetched(&req) {
        // メモリの中にある(または空)。待たずに読める
        return match read_limited(req.as_reader(), || false) {
            Ok(body) => Body::Done(req, body),
            Err(e) => Body::Refused(req, e),
        };
    }
    if ctx.readers.fetch_add(1, Ordering::AcqRel) >= MAX_STALLED_READERS {
        ctx.readers.fetch_sub(1, Ordering::AcqRel);
        return Body::Refused(req, (429, "本文を待っている要求が多い".into()));
    }
    let count = ReaderCount(ctx.readers.clone());
    let hand = Arc::new(Handoff {
        phase: Mutex::new(Phase::Reading),
        changed: Condvar::new(),
    });
    // spawn が失敗したときに要求を取り戻せるよう、箱に入れて渡す
    let slot = Arc::new(Mutex::new(Some(req)));
    let (h, s) = (hand.clone(), slot.clone());
    let spawned = std::thread::Builder::new()
        .name("admin-body".into())
        .spawn(move || {
            let _count = count;
            let Some(mut req) = s.lock().unwrap().take() else {
                return;
            };
            let gave_up = || matches!(*h.phase.lock().unwrap(), Phase::GaveUp);
            let result = read_limited(req.as_reader(), gave_up);
            let mut phase = h.phase.lock().unwrap();
            if matches!(*phase, Phase::GaveUp) {
                // ワーカーは諦めて先へ進んだ。ここで 408 を書いて手放す(読み残しの手放しはここで待つ)
                drop(phase);
                let resp = error(408, "本文の受信が遅い")
                    .with_header(Header::from_bytes("Connection", "close").unwrap());
                let _ = req.respond(resp);
            } else {
                *phase = Phase::Done(req, result);
                h.changed.notify_one();
            }
        });
    if spawned.is_err() {
        let req = slot
            .lock()
            .unwrap()
            .take()
            .expect("起動に失敗した読み取りが要求を持っていない");
        return Body::Refused(req, (503, "読み取りスレッドを起こせない".into()));
    }
    let wait = deadline.saturating_duration_since(Instant::now());
    let phase = hand.phase.lock().unwrap();
    let (mut phase, _) = hand
        .changed
        .wait_timeout_while(phase, wait, |p| matches!(p, Phase::Reading))
        .unwrap();
    // 読み切っていれば受け取り、そうでなければ諦めたと印を付ける(以降は読み取りスレッドの持ち物)
    match std::mem::replace(&mut *phase, Phase::GaveUp) {
        Phase::Done(req, Ok(body)) => Body::Done(req, body),
        Phase::Done(req, Err(e)) => Body::Refused(req, e),
        Phase::Reading | Phase::GaveUp => Body::GaveUp,
    }
}

/// 本文を最後まで読む(上限は `MAX_BODY`)。1 回の `read` は止まりうるので、`gave_up` が真になったら
/// 次の `read` に入る前に切り上げる。
fn read_limited(reader: &mut dyn Read, gave_up: impl Fn() -> bool) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(body),
            Ok(n) => {
                body.extend_from_slice(&buf[..n]);
                if body.len() > MAX_BODY {
                    return Err((413, "本文が大きすぎる".into()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err((400, format!("本文を読めない: {e}"))),
        }
        if gave_up() {
            return Err((408, "本文の受信が遅い".into()));
        }
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_ERROR_CHARS {
        return s.to_string();
    }
    let mut t: String = s.chars().take(MAX_ERROR_CHARS).collect();
    t.push('…');
    t
}

fn rfc3339(t: chrono::DateTime<chrono::Local>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// 管理ページに見せる項目。(キー, 種類)。`home_assistant.url` は表示だけ。
const ITEMS: [(&str, &str); 9] = [
    ("geoframe.url", "url"),
    ("home_assistant.url", "readonly"),
    ("agents.url", "url"),
    ("agents.enabled", "bool"),
    ("aircraft.url", "url"),
    ("aircraft.enabled", "bool"),
    ("room.enabled", "bool"),
    ("cube.enabled", "bool"),
    ("vrm.enabled", "bool"),
];

fn item_note(key: &str) -> Option<&'static str> {
    match key {
        "room.enabled" => Some("HA の url か room.forecast_area が無いと画面は出ない"),
        "vrm.enabled" => Some("有効にするには vrm.model と vrm.screens が設定ファイルに要る"),
        "agents.url" => Some("ホストは allowed_hosts の範囲"),
        "home_assistant.url" => Some("設定ファイルでだけ変えられる"),
        _ => None,
    }
}

/// `Config::value` の文字列を、種類に合った JSON の値にする。
fn typed(kind: &str, s: &str) -> Value {
    if kind == "bool" {
        Value::Bool(s == "true")
    } else {
        Value::String(s.to_string())
    }
}

/// 今の上書きファイル。(存在するか、世代、読めた内容)。読めない・TOML として読めないときの内容は None。
fn read_overlay_file(path: &Path) -> (bool, String, Option<Overlay>) {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, "none".into(), None),
        Err(_) => (true, "none".into(), None),
        Ok(bytes) => {
            let parsed = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| toml::from_str::<Overlay>(t).ok());
            (true, overlay_generation(&bytes), parsed)
        }
    }
}

/// 起動時に上書きを外した(`boot_failed`)のと同じ世代の上書きが、まだ残っているか。
fn boot_failed_same_generation(report: &config::OverlayReport, current: &str) -> bool {
    report.error_kind == Some(OverlayErrorKind::BootFailed)
        && current
            == report
                .boot_failed_generation
                .as_deref()
                .unwrap_or(&report.generation)
}

fn retry_after_s(started: Instant) -> u64 {
    let left = RESTART_AFTER.saturating_sub(started.elapsed());
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// `GET /api/v1/admin/overview` の本文。
fn overview(ctx: &Ctx) -> Value {
    let startup = &ctx.loaded.report;
    let (exists, generation, overlay) = read_overlay_file(&startup.path);
    // 次回起動の値: 今の設定ファイルを読み直し、今の上書きを起動時と同じ規則で重ねる
    let reloaded = Config::reload_with_overlay(ctx.loaded.base_path.as_deref());
    let (next, allowed_hosts, mut error, mut error_kind) = match &reloaded {
        Ok(l) => (
            Some(&l.effective),
            l.base.admin.allowed_hosts.clone(),
            l.report.error.clone(),
            l.report.error_kind,
        ),
        Err(e) => (
            None,
            ctx.loaded.base.admin.allowed_hosts.clone(),
            Some(format!("設定ファイルが読めない: {e}")),
            None,
        ),
    };
    // 起動で外した上書きと同じ世代のままなら、読み直しでは分からない(印は起動時に消した)ので起動時の理由を出す
    let boot_failed = boot_failed_same_generation(startup, &generation);
    if boot_failed {
        error = startup.error.clone();
        error_kind = startup.error_kind;
    }
    let overlay_entries = overlay.as_ref().map(Overlay::entries).unwrap_or_default();
    let mut pending_restart = false;
    let items: Vec<Value> = ITEMS
        .iter()
        .map(|&(key, kind)| {
            let effective = ctx.loaded.effective.value(key).unwrap_or_default();
            let next_value = next.and_then(|c| c.value(key));
            if next_value.as_deref().is_some_and(|n| n != effective) {
                pending_restart = true;
            }
            let ov = overlay_entries
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| typed(kind, v));
            json!({
                "key": key,
                "kind": kind,
                "effective": typed(kind, &effective),
                "file": typed(kind, &ctx.loaded.base.value(key).unwrap_or_default()),
                "overlay": ov,
                "next": next_value.map(|n| typed(kind, &n)),
                "note": item_note(key),
            })
        })
        .collect();
    let screen = ctx
        .shared
        .state
        .lock()
        .unwrap()
        .view
        .as_ref()
        .and_then(|v| {
            v.screens
                .get(v.current)
                .map(|s| json!({"id": s.id, "title": s.title}))
        });
    let modified_at = ctx
        .loaded
        .base_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .map(|t| rfc3339(t.into()));
    let retry = retry_after_s(ctx.started);
    let requested = ctx.restart_requested.load(Ordering::Acquire);
    json!({
        "schema_version": 1,
        "host": ctx.shared.host,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": ctx.started.elapsed().as_secs(),
        "screen": screen,
        "config_file": {
            "path": ctx.loaded.base_path.as_ref().map(|p| p.display().to_string()),
            "modified_at": modified_at,
        },
        "overlay": {
            "path": startup.path.display().to_string(),
            "exists": exists,
            "generation": generation,
            "error": error.as_deref().map(clip),
            "error_kind": error_kind,
            "pending_restart": pending_restart,
        },
        "allowed_hosts": allowed_hosts,
        "items": items,
        "feeds": [],
        "tokens": [],
        "restart": {
            "allowed": retry == 0 && !requested && !boot_failed,
            "retry_after_s": retry,
            "requested": requested,
        },
        "generated_at": rfc3339(chrono::Local::now()),
    })
}

/// `POST /api/v1/admin/restart`。受け付けたら 202 を返してから実行ループに `Restart` を送る。
fn restart(req: Request, ctx: &Ctx, who: &str) {
    let (code, body) = restart_decision(ctx);
    eprintln!("admin: {who} restart -> {code}");
    let accepted = code == 202;
    let _ = req.respond(json_response(code, &body));
    if accepted {
        // 応答を返してから終了させる(先に終わると 202 が届かない)
        let _ = ctx.keys.send(PanelInput::Restart);
    }
}

fn restart_decision(ctx: &Ctx) -> (u16, Value) {
    if !ctx.limiter.lock().unwrap().allow(Instant::now()) {
        return (429, json!({"error": "操作が速すぎる", "retry_after_s": 1}));
    }
    if ctx.shared.state.lock().unwrap().view.is_none() {
        return (503, json!({"error": "起動中"}));
    }
    if ctx.restart_requested.load(Ordering::Acquire) {
        return (
            429,
            json!({"error": "再起動を受け付け済み(終了を待っている)", "retry_after_s": 1}),
        );
    }
    let startup = &ctx.loaded.report;
    let (_, generation, _) = read_overlay_file(&startup.path);
    if boot_failed_same_generation(startup, &generation) {
        return (
            409,
            json!({"error": "起動に失敗した上書きのまま。保存して世代を変えてから"}),
        );
    }
    let retry = retry_after_s(ctx.started);
    if retry > 0 {
        return (
            429,
            json!({
                "error": format!("起動から {} 秒たつまで再起動できない", RESTART_AFTER.as_secs()),
                "retry_after_s": retry,
            }),
        );
    }
    if ctx
        .restart_requested
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return (
            429,
            json!({"error": "再起動を受け付け済み(終了を待っている)", "retry_after_s": 1}),
        );
    }
    // 次の起動で上書きを外させないよう、印は先に消す(終了処理でも消すが、受け付けた時点で確実にする)
    if !startup.applied.is_empty() {
        remove_boot_marker(&config::boot_marker_dir(&startup.path));
    }
    (202, json!({"restart_in_s": 1}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{DisplayStatus, Panel};
    use crate::screen::{PanelKeys, PanelView, ScreenInfo};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    const PANEL_TOKEN: &str = "test-token-0123456789";
    const ADMIN_TOKEN: &str = "admin-token-0123456789ab";
    const A: &str = "http://192.0.2.50/tar1090/data/aircraft.json";
    const B: &str = "http://192.0.2.51/tar1090/data/aircraft.json";

    /// 一時ディレクトリの中に、設定ファイル・上書き・トークンを置く。
    struct Fx {
        dir: PathBuf,
    }

    impl Fx {
        fn new(name: &str) -> Fx {
            let dir = std::env::temp_dir().join(format!(
                "miharashi-admin-test-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let fx = Fx { dir };
            fx.token("admin.token", ADMIN_TOKEN, 0o600);
            fx
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn token(&self, name: &str, value: &str, mode: u32) {
            let p = self.path(name);
            let _ = std::fs::remove_file(&p);
            std::fs::write(&p, format!("{value}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        /// 設定ファイルを書く。`extra` は `[admin]` 以外のセクション。`admin` は `[admin]` の追加の行。
        fn base(&self, admin: &str, extra: &str) -> PathBuf {
            let d = self.dir.display();
            let text = format!(
                "[panel]\nenabled = true\nlisten = \"127.0.0.1:8790\"\ntoken_file = \"{d}/panel.token\"\n\
                 [admin]\nenabled = true\ntoken_file = \"{d}/admin.token\"\noverlay_file = \"{d}/admin.toml\"\n\
                 allowed_hosts = [\"t3.example.test\"]\n{admin}\n\
                 [home_assistant]\nurl = \"http://192.0.2.10:8123\"\n{extra}\n"
            );
            let p = self.path("config.toml");
            std::fs::write(&p, text).unwrap();
            p
        }

        fn overlay(&self, text: &str) {
            std::fs::write(self.path("admin.toml"), text).unwrap();
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn view() -> PanelView {
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
            current: 1,
            page: None,
            photo: None,
            feed_online: true,
            rotation: None,
            menu: None,
            keys: PanelKeys::default(),
        }
    }

    /// 起動から `age_s` 秒たった状態で、管理ページつきのパネルを立てる。
    fn start(loaded: LoadedConfig, age_s: u64) -> (Panel, String) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let ctx = AdminContext {
            loaded,
            started: Instant::now() - Duration::from_secs(age_s),
            sources: AdminSources::default(),
        };
        let p = Panel::spawn(server, PANEL_TOKEN.into(), 20, None, Some(ctx));
        p.publish(Some(view()), DisplayStatus::default());
        let base = format!("http://{}", p.addr());
        (p, base)
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(8)))
            .build()
            .into()
    }

    fn call(method: &str, base: &str, path: &str, token: Option<&str>) -> (u16, Value) {
        let mut b = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{base}{path}"));
        if let Some(t) = token {
            b = b.header("Authorization", format!("Bearer {t}"));
        }
        let mut resp = agent().run(b.body(()).unwrap()).unwrap();
        let code = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        (code, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn get(base: &str, path: &str, token: Option<&str>) -> (u16, Value) {
        call("GET", base, path, token)
    }

    fn overview(base: &str) -> Value {
        let (code, v) = get(base, "/api/v1/admin/overview", Some(ADMIN_TOKEN));
        assert_eq!(code, 200, "{v}");
        v
    }

    fn item<'a>(v: &'a Value, key: &str) -> &'a Value {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == key)
            .unwrap_or_else(|| panic!("{key} が無い"))
    }

    /// 生の HTTP を送り、閉じるまでの応答を全部読む。
    fn raw(addr: SocketAddr, request: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    fn recv(p: &Panel) -> Option<PanelInput> {
        for _ in 0..100 {
            if let Some(k) = p.inputs().next() {
                return Some(k);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn tokens_are_separate() {
        let fx = Fx::new("tokens");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        assert_eq!(p.admin_disabled(), None);
        let path = "/api/v1/admin/overview";
        assert_eq!(get(&base, path, None).0, 401, "トークン無し");
        assert_eq!(
            get(&base, path, Some(PANEL_TOKEN)).0,
            401,
            "パネルのトークン"
        );
        assert_eq!(get(&base, path, Some("wrong-token-0123456789")).0, 401);
        assert_eq!(get(&base, path, Some(ADMIN_TOKEN)).0, 200);
        // 管理トークンでパネルの経路は呼べない
        for (m, p) in [
            ("POST", "/api/v1/input"),
            ("GET", "/api/v1/status"),
            ("POST", "/api/v1/screen"),
        ] {
            assert_eq!(call(m, &base, p, Some(ADMIN_TOKEN)).0, 401, "{m} {p}");
        }
        assert_eq!(get(&base, "/api/v1/status", Some(PANEL_TOKEN)).0, 200);
        // 管理の表に無い経路も、管理トークンがなければ 401(あれば 404)
        assert_eq!(get(&base, "/api/v1/admin/nothing", None).0, 401);
        assert_eq!(
            get(&base, "/api/v1/admin/nothing", Some(PANEL_TOKEN)).0,
            401
        );
        assert_eq!(
            get(&base, "/api/v1/admin/nothing", Some(ADMIN_TOKEN)).0,
            404
        );
        // メソッドが違う経路は、認証のあとに 405
        assert_eq!(
            call("POST", &base, path, Some(ADMIN_TOKEN)).0,
            405,
            "overview は GET だけ"
        );
        assert_eq!(
            call("GET", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN)).0,
            405
        );
        // 管理のリクエストは「パネルがつながっている」を更新しない
        assert_eq!(p.stat(), "connected", "パネルのトークンの GET で更新された");
    }

    #[test]
    fn admin_requests_do_not_mark_the_panel_connected() {
        let fx = Fx::new("connected");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        assert_eq!(p.stat(), "idle");
        overview(&base);
        assert_eq!(get(&base, "/api/v1/admin/overview", None).0, 401);
        assert_eq!(p.stat(), "idle", "管理のリクエストでは変わらない");
        assert_eq!(get(&base, "/api/v1/status", Some(PANEL_TOKEN)).0, 200);
        assert_eq!(p.stat(), "connected");
    }

    #[test]
    fn bad_admin_token_disables_only_the_admin_page() {
        let fx = Fx::new("disabled");
        let base_path = fx.base("", "");
        let cases: [(&str, Option<(&str, u32)>); 5] = [
            ("緩い権限", Some((ADMIN_TOKEN, 0o644))),
            ("グループ書き込み", Some((ADMIN_TOKEN, 0o660))),
            ("パネルと同じ値", Some((PANEL_TOKEN, 0o600))),
            ("短い", Some(("short", 0o600))),
            ("ファイル無し", None),
        ];
        for (name, file) in cases {
            let _ = std::fs::remove_file(fx.path("admin.token"));
            if let Some((value, mode)) = file {
                fx.token("admin.token", value, mode);
            }
            let loaded = Config::load_with_overlay(&base_path, false).unwrap();
            let (p, base) = start(loaded, 200);
            assert!(p.admin_disabled().is_some(), "{name}");
            for t in [None, Some(ADMIN_TOKEN), Some(PANEL_TOKEN)] {
                assert_eq!(get(&base, "/api/v1/admin/overview", t).0, 404, "{name}");
                assert_eq!(get(&base, "/admin", t).0, 404, "{name}");
                assert_eq!(get(&base, "/admin/app.js", t).0, 404, "{name}");
                assert_eq!(
                    call("POST", &base, "/api/v1/admin/restart", t).0,
                    404,
                    "{name}"
                );
            }
            // キオスク(パネル)は動く
            assert_eq!(
                get(&base, "/api/v1/status", Some(PANEL_TOKEN)).0,
                200,
                "{name}"
            );
        }
    }

    #[test]
    fn admin_page_is_off_unless_enabled() {
        let fx = Fx::new("off");
        let base_path = fx.base("", "");
        let text = std::fs::read_to_string(&base_path)
            .unwrap()
            .replace("[admin]\nenabled = true", "[admin]\nenabled = false");
        std::fs::write(&base_path, text).unwrap();
        let loaded = Config::load_with_overlay(&base_path, false).unwrap();
        let (p, base) = start(loaded, 200);
        assert_eq!(p.admin_disabled(), None, "無効にしたのではなく、使わない");
        assert_eq!(
            get(&base, "/api/v1/admin/overview", Some(ADMIN_TOKEN)).0,
            404
        );
        assert_eq!(get(&base, "/admin", None).0, 404);
        assert_eq!(get(&base, "/api/v1/status", Some(PANEL_TOKEN)).0, 200);
    }

    #[test]
    fn admin_token_file_rules() {
        let fx = Fx::new("rules");
        let (euid, egid) = (
            crate::agents::effective_uid(),
            crate::agents::effective_gid(),
        );
        let p = fx.path("admin.token");
        let path = p.to_str().unwrap();
        assert_eq!(read_admin_token(path, euid, egid).unwrap(), ADMIN_TOKEN);
        assert!(
            read_admin_token(path, euid + 1, egid).is_err(),
            "所有者が実行ユーザーではない"
        );
        fx.token("admin.token", ADMIN_TOKEN, 0o640);
        assert!(
            read_admin_token(path, euid, egid).is_err(),
            "0640 は root 所有のときだけ"
        );
        fx.token("admin.token", ADMIN_TOKEN, 0o400);
        assert!(read_admin_token(path, euid, egid).is_err());
        fx.token("admin.token", "a\nb-0123456789abcdef", 0o600);
        assert!(read_admin_token(path, euid, egid).is_err(), "1 行");
        assert!(read_admin_token("/nonexistent/admin.token", euid, egid).is_err());
    }

    #[test]
    fn overview_shows_effective_file_overlay_and_next() {
        let fx = Fx::new("items");
        let base_path = fx.base("", &format!("[aircraft]\nurl = \"{A}\"\n"));
        fx.overlay(&format!("[aircraft]\nurl = \"{B}\"\n"));
        let loaded = Config::load_with_overlay(&base_path, false).unwrap();
        assert_eq!(loaded.effective.aircraft.url, B);
        let (_p, base) = start(loaded, 200);
        let v = overview(&base);
        let it = item(&v, "aircraft.url");
        assert_eq!(it["kind"], "url");
        assert_eq!(it["effective"], B);
        assert_eq!(it["file"], A);
        assert_eq!(it["overlay"], B);
        assert_eq!(it["next"], B);
        assert_eq!(v["overlay"]["exists"], true);
        assert_eq!(v["overlay"]["pending_restart"], false);
        assert_eq!(v["overlay"]["error"], Value::Null);
        assert_eq!(v["overlay"]["generation"].as_str().unwrap().len(), 16);
        // 上書きが無い項目
        let it = item(&v, "geoframe.url");
        assert_eq!(it["overlay"], Value::Null);
        assert_eq!(it["effective"], it["file"]);
        // 表示だけの項目
        let it = item(&v, "home_assistant.url");
        assert_eq!(it["kind"], "readonly");
        assert_eq!(it["effective"], "http://192.0.2.10:8123");
        assert_eq!(it["overlay"], Value::Null);
        assert_eq!(it["note"], "設定ファイルでだけ変えられる");
        // 真偽の項目は JSON の真偽
        let it = item(&v, "vrm.enabled");
        assert_eq!(it["kind"], "bool");
        assert_eq!(it["effective"], false);
        assert!(it["note"].as_str().unwrap().contains("vrm.model"));
        assert!(item(&v, "room.enabled")["note"].is_string());
        assert_eq!(
            item(&v, "agents.url")["note"],
            "ホストは allowed_hosts の範囲"
        );
        assert_eq!(v["items"].as_array().unwrap().len(), 9);
        // そのほかの項目
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["screen"], json!({"id": "photo", "title": "PHOTO FRAME"}));
        assert_eq!(v["allowed_hosts"], json!(["t3.example.test"]));
        assert_eq!(v["feeds"], json!([]));
        assert_eq!(v["tokens"], json!([]));
        assert_eq!(v["config_file"]["path"], base_path.display().to_string());
        assert!(v["config_file"]["modified_at"].is_string());
        assert!(v["uptime_s"].as_u64().unwrap() >= 200);
        assert!(v["generated_at"].is_string());
        assert_eq!(v["restart"]["allowed"], true);
        assert_eq!(v["restart"]["retry_after_s"], 0);
        assert_eq!(v["restart"]["requested"], false);
    }

    #[test]
    fn overview_reads_the_overlay_on_every_request() {
        let fx = Fx::new("reread");
        let base_path = fx.base("", &format!("[aircraft]\nurl = \"{A}\"\n"));
        let loaded = Config::load_with_overlay(&base_path, false).unwrap();
        let (_p, base) = start(loaded, 200);
        let v = overview(&base);
        assert_eq!(v["overlay"]["exists"], false);
        assert_eq!(v["overlay"]["generation"], "none");
        fx.overlay(&format!("[aircraft]\nurl = \"{B}\"\n"));
        let v = overview(&base);
        let it = item(&v, "aircraft.url");
        assert_eq!(
            (it["effective"].as_str(), it["overlay"].as_str()),
            (Some(A), Some(B))
        );
        assert_eq!(it["next"], B);
        assert_eq!(v["overlay"]["exists"], true);
        assert_ne!(v["overlay"]["generation"], "none");
        // 設定ファイルの更新も次の読み直しで効く。読めなければ next は null で理由が出る
        std::fs::write(&base_path, "[panel\n").unwrap();
        let v = overview(&base);
        assert_eq!(item(&v, "aircraft.url")["next"], Value::Null);
        assert_eq!(item(&v, "aircraft.url")["file"], A, "起動時に読んだ値");
        assert!(
            v["overlay"]["error"]
                .as_str()
                .unwrap()
                .starts_with("設定ファイルが読めない")
        );
        assert_eq!(
            v["overlay"]["pending_restart"], false,
            "分からないものは数えない"
        );
    }

    /// 起動時の上書き `at_start` で立て、そのあとファイルを `now` にして overview の
    /// (`pending_restart`、`overlay.error`、`overlay.error_kind`)を返す。
    fn pending(name: &str, at_start: Option<&str>, now: Option<&str>) -> (bool, Value, Value) {
        let fx = Fx::new(name);
        let base_path = fx.base("", &format!("[aircraft]\nurl = \"{A}\"\n"));
        if let Some(t) = at_start {
            fx.overlay(t);
        }
        let loaded = Config::load_with_overlay(&base_path, false).unwrap();
        let (_p, base) = start(loaded, 200);
        match now {
            Some(t) => fx.overlay(t),
            None => {
                let _ = std::fs::remove_file(fx.path("admin.toml"));
            }
        }
        let v = overview(&base);
        (
            v["overlay"]["pending_restart"].as_bool().unwrap(),
            v["overlay"]["error"].clone(),
            v["overlay"]["error_kind"].clone(),
        )
    }

    #[test]
    fn pending_restart_follows_next_vs_effective() {
        let a = format!("[aircraft]\nurl = \"{A}\"\n");
        let b = format!("[aircraft]\nurl = \"{B}\"\n");
        // 効いている値を変える保存 → true
        let (p, e, _) = pending("pend-change", None, Some(&b));
        assert!(p);
        assert_eq!(e, Value::Null);
        // 効いている値と同じ値の保存 → false
        let (p, e, _) = pending("pend-same", None, Some(&a));
        assert!(!p);
        assert_eq!(e, Value::Null);
        // 上書きを取り消して設定ファイルの値が効いている値と同じ → false
        let (p, _, _) = pending("pend-cancel-same", Some(&a), None);
        assert!(!p);
        // 上書き無しのまま → false
        let (p, _, _) = pending("pend-none", None, None);
        assert!(!p);
        // 効いている上書きを取り消す(設定ファイルの値に戻る)→ true
        let (p, _, _) = pending("pend-cancel", Some(&b), None);
        assert!(p);
        // 壊れた上書き: 効いている値が壊れる前の上書きから来ていれば true と理由
        let (p, e, kind) = pending("pend-broken", Some(&b), Some("[aircraft\nurl = \n"));
        assert!(p);
        assert!(e.as_str().unwrap().contains("TOML"), "{e}");
        assert_eq!(kind, "parse");
        // 壊れた上書きでも、効いている値が設定ファイルの値なら false
        let (p, e, _) = pending("pend-broken-base", None, Some("[aircraft\n"));
        assert!(!p);
        assert!(e.is_string());
        // 知らないキー・許されないホストは規則違反として理由が出る
        let (_, _, kind) = pending("pend-unknown", None, Some("[panel]\nlisten = \"x\"\n"));
        assert_eq!(kind, "unknown_key");
        let (_, _, kind) = pending(
            "pend-host",
            None,
            Some("[agents]\nurl = \"http://192.0.2.99:3773\"\n"),
        );
        assert_eq!(kind, "rule");
    }

    #[test]
    fn error_is_clipped_to_200_chars() {
        assert_eq!(clip("short"), "short");
        assert_eq!(clip(&"あ".repeat(200)).chars().count(), 200);
        let long = clip(&"あ".repeat(500));
        assert_eq!(long.chars().count(), 201);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn restart_reaches_the_run_loop_once() {
        let fx = Fx::new("restart");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let (code, v) = call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN));
        assert_eq!((code, v), (202, json!({"restart_in_s": 1})));
        assert_eq!(recv(&p), Some(PanelInput::Restart));
        // 受け付けたあとは 429
        let (code, v) = call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN));
        assert_eq!(code, 429, "{v}");
        assert!(v["retry_after_s"].is_u64());
        assert!(p.inputs().next().is_none(), "2 回目は届かない");
        let v = overview(&base);
        assert_eq!(v["restart"]["requested"], true);
        assert_eq!(v["restart"]["allowed"], false);
        // 認証が無ければ受け付けない
        assert_eq!(call("POST", &base, "/api/v1/admin/restart", None).0, 401);
        assert_eq!(
            call("POST", &base, "/api/v1/admin/restart", Some(PANEL_TOKEN)).0,
            401
        );
    }

    #[test]
    fn restart_waits_120_seconds_after_start() {
        let fx = Fx::new("restart-early");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 10);
        let (code, v) = call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN));
        assert_eq!(code, 429);
        let wait = v["retry_after_s"].as_u64().unwrap();
        assert!((108..=110).contains(&wait), "{wait}");
        assert!(v["error"].as_str().unwrap().contains("120"));
        assert!(p.inputs().next().is_none());
        let v = overview(&base);
        assert_eq!(v["restart"]["allowed"], false);
        assert_eq!(v["restart"]["requested"], false);
        assert!((108..=110).contains(&v["restart"]["retry_after_s"].as_u64().unwrap()));
    }

    #[test]
    fn restart_is_503_before_the_first_publish() {
        let fx = Fx::new("restart-503");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        p.publish(None, DisplayStatus::default());
        // None を置くと state.view は None に戻る
        assert_eq!(
            call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN)).0,
            503
        );
        assert!(p.inputs().next().is_none());
    }

    #[test]
    fn restart_removes_the_boot_marker_and_is_rate_limited() {
        let fx = Fx::new("restart-marker");
        let base_path = fx.base("", "");
        fx.overlay(&format!("[aircraft]\nurl = \"{B}\"\n"));
        let mut loaded = Config::load_with_overlay(&base_path, true).unwrap();
        loaded.arm_boot_marker();
        let marker = config::boot_marker_path(&loaded.report.path);
        assert!(marker.exists(), "上書きで起動した印");
        let (p, base) = start(loaded, 200);
        assert_eq!(
            call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN)).0,
            202
        );
        assert!(!marker.exists(), "受け付けたら印を消す");
        assert_eq!(recv(&p), Some(PanelInput::Restart));

        // 1 秒に 5 回まで(受け付け済みの 429 も数える)
        let codes: Vec<u16> = (0..6)
            .map(|_| call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN)).0)
            .collect();
        assert!(codes.contains(&429));
        assert!(codes.iter().all(|c| *c == 429), "{codes:?}");
    }

    #[test]
    fn restart_is_refused_while_a_failed_overlay_stays() {
        let fx = Fx::new("restart-bootfailed");
        let base_path = fx.base("", &format!("[aircraft]\nurl = \"{A}\"\n"));
        let overlay = format!("[aircraft]\nurl = \"{B}\"\n");
        fx.overlay(&overlay);
        // 上書きで起動して 60 秒以内に落ちた印(書いたプロセスはもういない)
        let generation = overlay_generation(overlay.as_bytes());
        std::fs::write(
            config::boot_marker_path(&fx.path("admin.toml")),
            format!(r#"{{"pid":4000000000,"generation":"{generation}"}}"#),
        )
        .unwrap();
        let loaded = Config::load_with_overlay(&base_path, true).unwrap();
        assert_eq!(loaded.report.error_kind, Some(OverlayErrorKind::BootFailed));
        let (p, base) = start(loaded, 200);
        let v = overview(&base);
        assert_eq!(v["overlay"]["error_kind"], "boot_failed");
        assert!(v["overlay"]["error"].is_string());
        assert_eq!(v["restart"]["allowed"], false);
        assert_eq!(
            item(&v, "aircraft.url")["effective"],
            A,
            "上書きは外れている"
        );
        let (code, body) = call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN));
        assert_eq!(code, 409);
        assert_eq!(
            body["error"],
            "起動に失敗した上書きのまま。保存して世代を変えてから"
        );
        assert!(p.inputs().next().is_none());
        // 保存して世代が変わると解ける
        fx.overlay(&format!("[aircraft]\nurl = \"{A}\"\n"));
        let v = overview(&base);
        assert_eq!(v["overlay"]["error_kind"], Value::Null);
        assert_eq!(v["restart"]["allowed"], true);
        assert_eq!(
            call("POST", &base, "/api/v1/admin/restart", Some(ADMIN_TOKEN)).0,
            202
        );
        assert_eq!(recv(&p), Some(PanelInput::Restart));
    }

    /// 本文を途中までしか送らない接続を開いたまま、パネルの操作が 1 秒以内に通ることを確かめる。
    fn panel_input_is_not_blocked(p: &Panel, base: &str, since: Instant) {
        let r = agent()
            .post(format!("{base}/api/v1/input"))
            .header("Authorization", format!("Bearer {PANEL_TOKEN}"))
            .content_type("application/json")
            .send(r#"{"key":"next"}"#)
            .unwrap();
        assert_eq!(r.status().as_u16(), 204);
        assert!(
            since.elapsed() < Duration::from_secs(1),
            "{:?}",
            since.elapsed()
        );
        assert_eq!(
            recv(p),
            Some(PanelInput::Key(crate::screen::KeyPress::Next))
        );
    }

    /// `Content-Length` に `declared` を書き、本文を 10 バイトだけ送って黙る接続(開いたままにして返す)。
    fn held(addr: SocketAddr, head: &str, declared: usize) -> TcpStream {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            s,
            "{head}\r\nHost: x\r\nContent-Length: {declared}\r\n\r\n0123456789"
        )
        .unwrap();
        s
    }

    fn status_line(s: &mut TcpStream) -> String {
        let mut status = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(s), &mut status).unwrap();
        status
    }

    #[test]
    fn half_sent_bodies_held_open_do_not_block_the_panel() {
        // AC21。tiny_http は 1 KiB までの本文を、要求をパネルに渡す前に接続のスレッドで読む。
        // 途中までしか届かない接続は、その接続のスレッドが待つだけで、パネルのスレッドは塞がらない。
        // `Expect: 100-continue` つきは先読みされず、本文を読み残した要求がパネルに届く(後始末スレッドへ)
        let fx = Fx::new("held");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let auth = format!("Authorization: Bearer {ADMIN_TOKEN}");
        let mut conns = vec![
            held(p.addr(), "POST /api/v1/input HTTP/1.1", 1000),
            held(
                p.addr(),
                "POST /api/v1/input HTTP/1.1\r\nExpect: 100-continue",
                1000,
            ),
            held(
                p.addr(),
                &format!("PUT /api/v1/admin/config HTTP/1.1\r\n{auth}"),
                1000,
            ),
            held(
                p.addr(),
                &format!("PUT /api/v1/admin/config HTTP/1.1\r\n{auth}\r\nExpect: 100-continue"),
                1000,
            ),
        ];
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        // 認証のない Expect つきには 401 が返る(後始末スレッドが応答を先に書く)
        assert!(status_line(&mut conns[1]).starts_with("HTTP/1.1 401"));
        panel_input_is_not_blocked(&p, &base, t0);
        assert_eq!(
            get(&base, "/api/v1/status", Some(PANEL_TOKEN)).0,
            200,
            "status も答える"
        );
        assert!(p.inputs().next().is_none(), "遅い要求は何にもならない");
        drop(conns);
    }

    /// 本文を読み残した要求を `n` 本、開いたまま返す(パネルの経路で、認証が無いので 401 になる)。
    fn many_held(addr: SocketAddr, n: usize) -> Vec<TcpStream> {
        (0..n)
            .map(|_| {
                held(
                    addr,
                    "POST /api/v1/input HTTP/1.1\r\nExpect: 100-continue",
                    1000,
                )
            })
            .collect()
    }

    #[test]
    fn many_unread_bodies_held_open_do_not_block_the_panel() {
        // 本文を読み残した要求が何本溜まっても(後始末は要求ごとのスレッドで、上限も待ち行列も無い)、
        // パネルは止まらず、後続の応答も遅れない
        let fx = Fx::new("reaper-many");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let mut conns = many_held(p.addr(), 40);
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        panel_input_is_not_blocked(&p, &base, t0);
        // 40 本目の 401 も、手前の読み捨てに詰まらず返る
        assert!(status_line(conns.last_mut().unwrap()).starts_with("HTTP/1.1 401"));
        drop(conns);
    }

    #[test]
    fn reaper_threads_are_reclaimed_when_the_peers_close() {
        // 開いたままの接続を相手が全部閉じたら、後始末のスレッドは全部終わる(資源が残らない)
        let fx = Fx::new("reaper-reclaim");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let conns = many_held(p.addr(), 40);
        let t0 = Instant::now();
        let mut peak = 0;
        while t0.elapsed() < Duration::from_secs(5) && peak < 40 {
            peak = peak.max(p.reapers());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(peak, 40, "40 本とも読み捨てを待っている");
        panel_input_is_not_blocked(&p, &base, Instant::now());
        drop(conns);
        let t1 = Instant::now();
        while p.reapers() > 0 && t1.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(p.reapers(), 0, "切断のあとに後始末のスレッドが残っている");
    }

    #[test]
    fn a_slow_large_body_holds_only_the_admin_worker_until_the_deadline() {
        // 1 KiB を超える本文は管理ワーカーが読む(別スレッドで)。読み始めから 5 秒の絶対期限で、
        // ワーカーは諦めて空く。ゆっくり少しずつ送っても期限は延びない。408 は、読み取りが戻ったとき
        // (相手が次に送るか切ったとき)に読み取りスレッドが書く(止まった read には書き込めない)
        let fx = Fx::new("slow-large");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let mut slow = held(
            p.addr(),
            &format!("POST /api/v1/admin/restart HTTP/1.1\r\nAuthorization: Bearer {ADMIN_TOKEN}"),
            5000,
        );
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        panel_input_is_not_blocked(&p, &base, t0);
        // ワーカーが塞がっている間の管理の要求は 429
        let (code, v) = get(&base, "/api/v1/admin/overview", Some(ADMIN_TOKEN));
        assert_eq!(code, 429);
        assert_eq!(v, json!({"error": "管理の処理中", "retry_after_s": 1}));
        // 開始から 4.9 秒に少量送ってまた黙る
        std::thread::sleep(Duration::from_millis(4900).saturating_sub(t0.elapsed()));
        slow.write_all(b"abcdef").unwrap();
        // 5 秒の期限で、ワーカーは空く(残りを待ち続けない)
        let mut freed = None;
        while t0.elapsed() < Duration::from_secs(7) {
            if get(&base, "/api/v1/admin/overview", Some(ADMIN_TOKEN)).0 == 200 {
                freed = Some(t0.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let freed = freed.expect("ワーカーが空かない");
        assert!(
            (Duration::from_millis(4800)..Duration::from_secs(6)).contains(&freed),
            "5 秒ほどで空く: {freed:?}"
        );
        panel_input_is_not_blocked(&p, &base, Instant::now());
        assert!(p.inputs().next().is_none(), "遅い要求は再起動にならない");
        // 接続は開いたまま。次に何か届くと、読み取りスレッドが 408 を書く
        slow.write_all(b"g").unwrap();
        let status = status_line(&mut slow);
        assert!(status.starts_with("HTTP/1.1 408"), "{status}");
        assert!(p.inputs().next().is_none(), "遅い要求は再起動にならない");
    }

    #[test]
    fn a_rejected_body_is_answered_and_does_not_block_the_panel() {
        // 認証が無くて断る要求は 401 を返し、読み残した本文の読み捨てでパネルのスレッドを止めない
        let fx = Fx::new("slow-401");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let mut s = held(p.addr(), "PUT /nothing HTTP/1.1", 5000);
        let t0 = Instant::now();
        let status = status_line(&mut s);
        assert!(status.starts_with("HTTP/1.1 401"), "{status}");
        panel_input_is_not_blocked(&p, &base, t0);
        // 認証済みの大きい本文は、読まずに 413(応答は後始末スレッドが先の読み捨てから戻ったあと)。
        // どちらにせよパネルは止まらない
        let _s2 = held(
            p.addr(),
            &format!("POST /api/v1/input HTTP/1.1\r\nAuthorization: Bearer {PANEL_TOKEN}"),
            5000,
        );
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        panel_input_is_not_blocked(&p, &base, t0);
        // 認証済みの Expect つき(パネルの経路)も、パネルのスレッドで読まずに断る
        let _s3 = held(
            p.addr(),
            &format!(
                "POST /api/v1/input HTTP/1.1\r\nAuthorization: Bearer {PANEL_TOKEN}\r\nExpect: 100-continue"
            ),
            100,
        );
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300));
        panel_input_is_not_blocked(&p, &base, t0);
    }

    #[test]
    fn events_survive_the_receive_timeout() {
        // 6 秒黙っていても、SSE の接続(読む側は黙ったまま)が切れないこと
        let fx = Fx::new("sse");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, _base) = start(loaded, 200);
        let mut s = TcpStream::connect(p.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET /api/v1/events HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {PANEL_TOKEN}\r\n\r\n"
        )
        .unwrap();
        let mut r = std::io::BufReader::new(s);
        let mut next_data = || loop {
            let mut line = String::new();
            assert!(
                std::io::BufRead::read_line(&mut r, &mut line).unwrap() > 0,
                "切れた"
            );
            if let Some(d) = line.strip_prefix("data: ") {
                return serde_json::from_str::<Value>(d).unwrap();
            }
        };
        assert_eq!(next_data()["screen"]["id"], "photo");
        std::thread::sleep(Duration::from_secs(6));
        p.publish(
            Some(PanelView {
                current: 0,
                ..view()
            }),
            DisplayStatus::default(),
        );
        assert_eq!(next_data()["screen"]["id"], "status", "6 秒たっても届く");
    }

    #[test]
    fn oversized_bodies_are_413() {
        let fx = Fx::new("big");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, _base) = start(loaded, 200);
        let r = raw(
            p.addr(),
            &format!(
                "POST /api/v1/admin/restart HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                 Authorization: Bearer {ADMIN_TOKEN}\r\nContent-Length: 20000\r\n\r\n"
            ),
        );
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        assert!(p.inputs().next().is_none());
    }

    #[test]
    fn cross_site_requests_cannot_reach_the_admin_api() {
        let fx = Fx::new("csrf");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (p, base) = start(loaded, 200);
        let lower = |r: &str| r.to_ascii_lowercase();
        // Origin 付きで Authorization 無し
        for (m, path) in [
            ("POST", "/api/v1/admin/restart"),
            ("PUT", "/api/v1/admin/config"),
            ("GET", "/api/v1/admin/overview"),
        ] {
            let r = raw(
                p.addr(),
                &format!(
                    "{m} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                     Origin: http://evil.example.test\r\nContent-Length: 0\r\n\r\n"
                ),
            );
            assert!(r.starts_with("HTTP/1.1 401"), "{m} {path}: {r}");
            assert!(!lower(&r).contains("access-control-allow"), "{r}");
        }
        // 事前確認はどれも 405
        for path in [
            "/api/v1/admin/overview",
            "/api/v1/admin/config",
            "/api/v1/admin/restart",
            "/api/v1/input",
            "/api/v1/status",
            "/admin",
            "/nothing",
        ] {
            let r = raw(
                p.addr(),
                &format!(
                    "OPTIONS {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                     Origin: http://evil.example.test\r\n\
                     Access-Control-Request-Method: POST\r\n\
                     Access-Control-Request-Headers: authorization\r\n\r\n"
                ),
            );
            assert!(r.starts_with("HTTP/1.1 405"), "{path}: {r}");
            assert!(!lower(&r).contains("access-control-allow"), "{path}: {r}");
        }
        // トークンはクエリでは受け取らない
        let (code, _) = get(
            &base,
            &format!("/api/v1/admin/overview?token={ADMIN_TOKEN}"),
            None,
        );
        assert_eq!(code, 401);
        let (code, _) = get(&base, &format!("/api/v1/status?token={PANEL_TOKEN}"), None);
        assert_eq!(code, 401);
        let (code, _) = call(
            "POST",
            &base,
            &format!("/api/v1/admin/restart?token={ADMIN_TOKEN}"),
            None,
        );
        assert_eq!(code, 401);
        assert!(p.inputs().next().is_none());
        // 正しい認証の応答にも Access-Control-Allow-* は付かず、JSON には nosniff が付く
        let r = raw(
            p.addr(),
            &format!(
                "GET /api/v1/admin/overview HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                 Origin: http://evil.example.test\r\nAuthorization: Bearer {ADMIN_TOKEN}\r\n\r\n"
            ),
        );
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        assert!(!lower(&r).contains("access-control-allow"));
        assert!(lower(&r).contains("x-content-type-options: nosniff"), "{r}");
        let r = raw(
            p.addr(),
            &format!(
                "GET /api/v1/status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
                 Authorization: Bearer {PANEL_TOKEN}\r\n\r\n"
            ),
        );
        assert!(
            lower(&r).contains("x-content-type-options: nosniff"),
            "既存の JSON にも: {r}"
        );
        let r = raw(
            p.addr(),
            "GET /api/v1/healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(lower(&r).contains("x-content-type-options: nosniff"), "{r}");
    }

    #[test]
    fn static_pages_are_not_served_yet() {
        let fx = Fx::new("pages");
        let loaded = Config::load_with_overlay(&fx.base("", ""), false).unwrap();
        let (_p, base) = start(loaded, 200);
        // T5 まで: 管理が有効でも 404(認証は要らない)。PUT config は T3
        for path in ["/admin", "/admin/app.js", "/admin/app.css"] {
            assert_eq!(get(&base, path, None).0, 404, "{path}");
        }
        assert_eq!(
            call("PUT", &base, "/api/v1/admin/config", Some(ADMIN_TOKEN)).0,
            404
        );
        assert_eq!(call("PUT", &base, "/api/v1/admin/config", None).0, 401);
    }
}
