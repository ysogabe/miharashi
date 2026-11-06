//! T3 の閲覧用 shell snapshot を取得し、画面へ渡す。
//! HTTP・認証ファイル・JSON 解析は専用 worker に閉じ込める。
use crate::config::ConfigAgents;
use crate::screen::{Provider, World};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const BODY_LIMIT: u64 = 4 * 1024 * 1024;
const THREAD_LIMIT: usize = 2048;
const ENTRY_TEXT_LIMIT: usize = 512;
const CHANGE_LIMIT: usize = 64;
/// 対応する T3 の orchestration protocol(T3 v0.1.0 以降の V2)。
const PROTOCOL_VERSION: u64 = 2;
const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";

/// 1 接続の取得結果。接続ごとに worker が 1 本あり、それぞれが自分の `Shared` だけを書く。
pub type Shared = Arc<RwLock<AgentSnapshot>>;

/// 設定上の安定した接続の識別子(`primary` と `[[agents.extra_servers]]` の `id`)。
pub type ServerId = Arc<str>;

/// スレッドの識別。T3 の thread id は接続の中でだけ一意なので、接続の識別子と組にして構造的に持つ
/// (文字列を連結して 1 本の ID にはしない)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadKey {
    pub server: ServerId,
    pub thread: String,
}

/// プロジェクトの識別。project id も接続の中でだけ一意。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectKey {
    pub server: ServerId,
    pub project: String,
}

/// 変更通知の識別。`AgentChange::id` は接続ごとの連番なので、カーソルの比較は同じ接続の中だけで行う。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangeKey {
    pub server: ServerId,
    pub id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentConnection {
    #[default]
    Connecting,
    Live,
    Retrying,
    AuthenticationRequired,
    Forbidden,
    ApiError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AgentState {
    Starting,
    Running,
    Monitoring,
    Approval,
    Input,
    Plan,
    Error,
    Interrupted,
    Stopped,
    Completed,
    Idle,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentChangeKind {
    New,
    State,
    Flags,
    Goal,
    Title,
    Error,
    TurnCompleted,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentEntry {
    /// このスレッドを持つ接続。worker が取得時に付ける(画面は ServerSnapshot の接続で付け直す)。
    pub server_id: ServerId,
    pub thread_id: String,
    pub project_id: String,
    pub project_title: String,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub state: AgentState,
    /// thread.status(最新 run の状態、run が無ければ idle)。
    pub raw_status: Option<String>,
    /// thread.activityRunStatus(活動中の run の状態)。
    pub raw_activity_status: Option<String>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    /// 入力待ちのうち、T3 が provider の認証のし直しを待っている(auth_refresh)。
    pub needs_reauth: bool,
    pub has_actionable_proposed_plan: bool,
    /// 完了を引き留めるバックグラウンドの仕事(subagent・monitor・background_task)の数。
    pub background_tasks: usize,
    pub active_run_id: Option<String>,
    pub latest_run_id: Option<String>,
    /// 進行中の provider の目標(goal.objective)。完了した目標は持たない。
    pub goal: Option<String>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub updated_at: Option<String>,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_summary: Option<String>,
    /// `lineage.relationshipToParent` が `subagent` のときの親スレッド ID。fork や不明な関係は親子として扱わない。
    pub parent_thread_id: Option<String>,
    /// 親スレッドのタイトル。親が一覧の対象外(片付け済み・古い・アーカイブ)でも、shell にあれば残す。
    pub parent_title: Option<String>,
}

impl AgentEntry {
    pub fn key(&self) -> ThreadKey {
        ThreadKey {
            server: self.server_id.clone(),
            thread: self.thread_id.clone(),
        }
    }
    pub fn project_key(&self) -> ProjectKey {
        ProjectKey {
            server: self.server_id.clone(),
            project: self.project_id.clone(),
        }
    }
    /// 親スレッド。親子は同じ接続の中だけで結ぶ。
    pub fn parent_key(&self) -> Option<ThreadKey> {
        self.parent_thread_id.as_ref().map(|parent| ThreadKey {
            server: self.server_id.clone(),
            thread: parent.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChange {
    pub server_id: ServerId,
    pub id: u64,
    pub thread_id: String,
    pub kind: AgentChangeKind,
    pub before_state: Option<AgentState>,
    pub after_state: Option<AgentState>,
    pub before_goal: Option<String>,
    pub after_goal: Option<String>,
    pub before_title: Option<String>,
    pub after_title: Option<String>,
    pub observed_at: SystemTime,
}

/// 1 接続ぶんの取得結果。sequence / cursor / generation / last_success / error は接続ごとに持ち、
/// 全体の stale や合成した sequence は作らない。
#[derive(Debug, Clone, Default)]
pub struct AgentSnapshot {
    pub environment_id: Option<String>,
    pub environment_label: Option<String>,
    pub server_version: Option<String>,
    pub protocol_version: Option<u64>,
    pub sequence: Option<u64>,
    pub entries: Arc<[AgentEntry]>,
    pub changes: Arc<[AgentChange]>,
    pub connection: AgentConnection,
    pub last_success: Option<SystemTime>,
    pub last_success_mono: Option<Instant>,
    pub change_cursor: u64,
    pub resync_generation: u64,
    pub synchronized: bool,
    pub error_summary: Option<String>,
}

/// World に載せる 1 接続ぶん。接続は設定の順(`primary` が先頭)に並ぶ。
#[derive(Debug, Clone, Default)]
pub struct ServerSnapshot {
    pub server_id: ServerId,
    /// 画面に出す名前(設定の label。descriptor 由来の名前は識別に使わない)。
    pub label: String,
    /// 同じ environment_id が別の接続からも返っている(重複接続。自動では統合しない)。
    pub duplicate_environment: bool,
    pub snapshot: AgentSnapshot,
}

impl ServerSnapshot {
    pub fn new(server_id: &str, label: &str, snapshot: AgentSnapshot) -> Self {
        Self {
            server_id: server_id.into(),
            label: label.into(),
            duplicate_environment: false,
            snapshot,
        }
    }

    /// 旧設定と同じ 1 台だけの構成(`primary`)。demo・fixture が使う。
    pub fn primary(snapshot: AgentSnapshot) -> Vec<Self> {
        vec![Self::new(
            crate::config::PRIMARY_SERVER_ID,
            crate::config::PRIMARY_SERVER_ID,
            snapshot,
        )]
    }
}

/// 起動した 1 接続。管理ページは `shared` を接続ごとに読む。
#[derive(Clone)]
pub struct Connection {
    pub server_id: ServerId,
    pub label: String,
    pub shared: Shared,
}

/// 接続ごとの共有値を 1 本に集める。各 worker は自分の `Shared` だけを書き、
/// ここで接続の順に並べて `World::agents` を作る(1 台の遅れや失敗が他の接続の値を変えない)。
pub struct AgentProvider {
    connections: Vec<Connection>,
    last: Vec<ServerSnapshot>,
    warned_duplicates: Vec<ServerId>,
}

impl AgentProvider {
    pub fn new(connections: Vec<Connection>) -> Self {
        let last = connections
            .iter()
            .map(|c| ServerSnapshot {
                server_id: c.server_id.clone(),
                label: c.label.clone(),
                duplicate_environment: false,
                snapshot: AgentSnapshot::default(),
            })
            .collect();
        Self {
            connections,
            last,
            warned_duplicates: Vec::new(),
        }
    }
}

impl Provider for AgentProvider {
    fn update(&mut self, world: &mut World) {
        for (slot, connection) in self.last.iter_mut().zip(&self.connections) {
            // 書き込み中の接続は前回の値のまま(他の接続の更新は止めない)。
            if let Ok(snapshot) = connection.shared.try_read() {
                slot.snapshot = snapshot.clone();
            }
        }
        let duplicates = duplicate_environments(&self.last);
        for slot in &mut self.last {
            slot.duplicate_environment = duplicates.contains(&slot.server_id);
        }
        if duplicates != self.warned_duplicates {
            if !duplicates.is_empty() {
                eprintln!(
                    "miharashi: T3 接続 {} が同じ environment_id を返しています(重複接続。統合はしません)",
                    duplicates
                        .iter()
                        .map(|id| id.as_ref())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            self.warned_duplicates = duplicates;
        }
        world.agents = self.last.clone();
    }
}

/// 同じ environment_id を返している接続の id(設定の順)。
fn duplicate_environments(servers: &[ServerSnapshot]) -> Vec<ServerId> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for s in servers {
        if let Some(id) = s.snapshot.environment_id.as_deref() {
            *seen.entry(id).or_default() += 1;
        }
    }
    servers
        .iter()
        .filter(|s| {
            s.snapshot
                .environment_id
                .as_deref()
                .is_some_and(|id| seen.get(id).copied().unwrap_or(0) > 1)
        })
        .map(|s| s.server_id.clone())
        .collect()
}

/// `enabled` のときだけ呼び出す。接続(`primary` と `extra_servers`)ごとに worker を 1 本起動し、
/// それぞれの共有値を設定の順に返す。
pub fn start(cfg: &ConfigAgents) -> Vec<Connection> {
    start_selected(cfg, None)
}

/// `start` と同じだが、`only` があればその id の接続だけを起動する(診断用の `agents-check`)。
/// 設定の順は保つ。worker 本体は `start` と同じ関数なので、ここで確かめた動きは本番の worker と同じ。
pub fn start_selected(cfg: &ConfigAgents, only: Option<&[String]>) -> Vec<Connection> {
    cfg.servers()
        .into_iter()
        .filter(|server| only.is_none_or(|ids| ids.contains(&server.id)))
        .enumerate()
        .map(|(index, server)| {
            let shared = Arc::new(RwLock::new(AgentSnapshot::default()));
            let server_id: ServerId = server.id.as_str().into();
            let thread_shared = shared.clone();
            // この接続だけの URL / token_file を持つ設定にする(他の接続の値は渡さない)。
            let mut settings = cfg.clone();
            settings.url = server.url;
            settings.token_file = server.token_file;
            settings.extra_servers = Vec::new();
            let worker_id = server_id.clone();
            if let Err(e) = thread::Builder::new()
                .name(format!("t3-agents-{index}"))
                .spawn(move || worker(settings, worker_id, thread_shared))
            {
                set_connection(
                    &shared,
                    AgentConnection::Retrying,
                    Some(format!("worker を起動できない: {e}")),
                );
            }
            Connection {
                server_id,
                label: server.label,
                shared,
            }
        })
        .collect()
}

/// 認証情報を送る URL は HTTPS、または名前解決をしない HTTP IP / localhost に限る。
pub fn check_url(value: &str) -> Result<(), String> {
    let (scheme, rest) = value
        .split_once("://")
        .ok_or("http:// または https:// が必要")?;
    if !matches!(scheme, "http" | "https") {
        return Err("http:// または https:// が必要".into());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || rest.contains(['?', '#']) {
        return Err("URL の host が不正".into());
    }
    let suffix = &rest[authority.len()..];
    if !suffix.is_empty() && suffix != "/" {
        return Err("T3 URL は origin のみ指定してください".into());
    }
    if scheme == "http" {
        let host = if let Some(v6) = authority.strip_prefix('[') {
            v6.split_once(']').map(|(h, _)| h).unwrap_or("")
        } else {
            authority.rsplit_once(':').map_or(authority, |(h, _)| h)
        };
        let ip = host.parse::<IpAddr>().ok();
        if host != "localhost" && !matches!(ip, Some(IpAddr::V4(_)) | Some(IpAddr::V6(_))) {
            return Err("平文 HTTP は IP アドレスか localhost に限る".into());
        }
    }
    Ok(())
}

fn worker(cfg: ConfigAgents, server_id: ServerId, shared: Shared) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(cfg.timeout_secs)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let base = cfg.url.trim_end_matches('/');
    let endpoint = format!("{base}/api/orchestration/shell");
    let descriptor_endpoint = format!("{base}/.well-known/t3/environment");
    let mut token = String::new();
    let mut token_fingerprint = 0u64;
    let mut auth_blocked = false;
    let mut last_token_check = Instant::now() - Duration::from_secs(30);
    let mut backoff = 5u64;
    let mut was_connected = false;
    let mut environment: Option<Environment> = None;
    let mut next_poll = Instant::now();
    loop {
        let now = Instant::now();
        if auth_blocked && now.duration_since(last_token_check) < Duration::from_secs(30) {
            thread::sleep(Duration::from_millis(250));
            continue;
        }
        if now < next_poll {
            thread::sleep((next_poll - now).min(Duration::from_millis(250)));
            continue;
        }
        if auth_blocked
            || token.is_empty()
            || now.duration_since(last_token_check) >= Duration::from_secs(30)
        {
            last_token_check = Instant::now();
            match read_token(&cfg.token_file) {
                Ok(next) => {
                    let next_fingerprint = fingerprint(&next);
                    if !auth_retry_allowed(auth_blocked, token_fingerprint, next_fingerprint) {
                        next_poll = Instant::now() + Duration::from_secs(30);
                        continue;
                    }
                    token = next;
                    token_fingerprint = next_fingerprint;
                    auth_blocked = false;
                }
                Err(e) => {
                    set_connection(&shared, AgentConnection::AuthenticationRequired, Some(e));
                    next_poll = Instant::now() + Duration::from_secs(30);
                    continue;
                }
            }
        }
        let descriptor = if was_connected {
            environment
                .clone()
                .ok_or_else(|| FetchError::Invalid("T3 environment descriptor がありません".into()))
        } else {
            fetch_environment(&agent, &descriptor_endpoint)
                .inspect(|descriptor| environment = Some(descriptor.clone()))
        };
        let result = descriptor.and_then(|descriptor| {
            fetch(&agent, &endpoint, &token, cfg.recent_hours).map(|mut snapshot| {
                snapshot.environment_id = descriptor.environment_id;
                snapshot.environment_label = descriptor.environment_label;
                snapshot.server_version = descriptor.server_version;
                snapshot.protocol_version = descriptor.protocol_version;
                snapshot
            })
        });
        match result {
            Ok(remote) => {
                let had_success = shared.read().map(|s| s.synchronized).unwrap_or(false);
                let is_resync = had_success && !was_connected;
                install_snapshot(&shared, &server_id, remote, is_resync);
                was_connected = true;
                backoff = 5;
                next_poll = Instant::now() + Duration::from_secs(cfg.poll_secs);
            }
            Err(FetchError::Unauthorized) => {
                auth_blocked = true;
                was_connected = false;
                set_connection(
                    &shared,
                    AgentConnection::AuthenticationRequired,
                    Some("T3 が認証を拒否しました (401)".into()),
                );
                next_poll = Instant::now() + Duration::from_secs(30);
            }
            Err(FetchError::Forbidden) => {
                auth_blocked = true;
                was_connected = false;
                set_connection(
                    &shared,
                    AgentConnection::Forbidden,
                    Some("T3 が閲覧を許可しません (403)".into()),
                );
                next_poll = Instant::now() + Duration::from_secs(30);
            }
            Err(FetchError::RateLimited(delay)) => {
                was_connected = false;
                set_connection(
                    &shared,
                    AgentConnection::Retrying,
                    Some("T3 が取得を制限しています (429)".into()),
                );
                next_poll = Instant::now() + delay;
            }
            Err(FetchError::Invalid(e)) => {
                was_connected = false;
                set_connection(&shared, AgentConnection::ApiError, Some(e));
                next_poll = Instant::now() + Duration::from_secs(backoff);
                backoff = (backoff * 2).min(30);
            }
            Err(FetchError::Transport(e)) => {
                was_connected = false;
                set_connection(&shared, AgentConnection::Retrying, Some(e));
                next_poll = Instant::now() + Duration::from_secs(backoff);
                backoff = (backoff * 2).min(30);
            }
        }
    }
}

fn set_connection(shared: &Shared, connection: AgentConnection, error: Option<String>) {
    if let Ok(mut snapshot) = shared.write() {
        snapshot.connection = connection;
        snapshot.error_summary = error;
    }
}

fn install_snapshot(
    shared: &Shared,
    server_id: &ServerId,
    mut remote: RemoteSnapshot,
    resync: bool,
) {
    let now_wall = SystemTime::now();
    let now_mono = Instant::now();
    let (old_entries, old_changes, synchronized, mut cursor, old_environment) = match shared.read()
    {
        Ok(current) => (
            current.entries.clone(),
            current.changes.clone(),
            current.synchronized,
            current.change_cursor,
            current.environment_id.clone(),
        ),
        Err(_) => return,
    };
    for entry in &mut remote.entries {
        entry.server_id = server_id.clone();
    }
    // 再接続で取り直した descriptor の environment_id が変わった(別の T3 に入れ替わった)ときは、
    // この接続の履歴を引き継がない。再同期と同じく、変更通知を出さずに世代を進める。
    let environment_changed = matches!(
        (&old_environment, &remote.environment_id),
        (Some(old), Some(new)) if old != new
    );
    let resync = resync || environment_changed;
    let sync_reset = synchronized && resync;
    let changes = if synchronized && !resync {
        let mut history = old_changes.to_vec();
        history.extend(diff(
            server_id,
            &old_entries,
            &remote.entries,
            &mut cursor,
            now_wall,
        ));
        if history.len() > CHANGE_LIMIT {
            history.drain(..history.len() - CHANGE_LIMIT);
        }
        history
    } else {
        Vec::new()
    };
    if let Ok(mut current) = shared.write() {
        if sync_reset {
            current.resync_generation = current.resync_generation.saturating_add(1);
        }
        current.change_cursor = cursor;
        current.environment_id = remote.environment_id;
        current.environment_label = remote.environment_label;
        current.server_version = remote.server_version;
        current.protocol_version = remote.protocol_version;
        current.sequence = remote.sequence;
        current.entries = remote.entries.into();
        current.changes = changes.into();
        current.connection = AgentConnection::Live;
        current.last_success = Some(now_wall);
        current.last_success_mono = Some(now_mono);
        current.synchronized = true;
        current.error_summary = None;
    }
}

#[derive(Debug)]
enum FetchError {
    Unauthorized,
    Forbidden,
    RateLimited(Duration),
    Invalid(String),
    Transport(String),
}

#[derive(Default)]
struct RemoteSnapshot {
    environment_id: Option<String>,
    environment_label: Option<String>,
    server_version: Option<String>,
    protocol_version: Option<u64>,
    sequence: Option<u64>,
    entries: Vec<AgentEntry>,
}

fn fetch_environment(agent: &ureq::Agent, endpoint: &str) -> Result<Environment, FetchError> {
    let mut response = agent
        .get(endpoint)
        .call()
        .map_err(|e| FetchError::Transport(format!("T3 の環境情報を取得できません: {e}")))?;
    if response.status().as_u16() != 200 {
        return Err(FetchError::Invalid(format!(
            "T3 の環境情報が HTTP {} を返しました",
            response.status().as_u16()
        )));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_vec()
        .map_err(|e| FetchError::Invalid(format!("T3 の環境情報を読み取れません: {e}")))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| FetchError::Invalid("T3 の環境情報が JSON ではありません".into()))?;
    let protocol = value
        .get("orchestrationProtocolVersion")
        .and_then(Value::as_u64);
    if protocol != Some(PROTOCOL_VERSION) {
        return Err(FetchError::Invalid(format!(
            "未対応の orchestration protocol ({})",
            protocol.map_or_else(|| "不明".into(), |v| v.to_string())
        )));
    }
    Ok(Environment {
        environment_id: field_str(&value, "environmentId")
            .or_else(|| field_str(&value, "id"))
            .map(|s| sanitize(s, 128)),
        environment_label: field_str(&value, "environmentLabel")
            .or_else(|| field_str(&value, "label"))
            .or_else(|| field_str(&value, "name"))
            .map(|s| sanitize(s, 128)),
        server_version: field_str(&value, "serverVersion").map(|s| sanitize(s, 64)),
        protocol_version: protocol,
    })
}

#[derive(Clone, Default)]
struct Environment {
    environment_id: Option<String>,
    environment_label: Option<String>,
    server_version: Option<String>,
    protocol_version: Option<u64>,
}

fn fetch(
    agent: &ureq::Agent,
    endpoint: &str,
    token: &str,
    recent_hours: u64,
) -> Result<RemoteSnapshot, FetchError> {
    let mut response = agent
        .get(endpoint)
        .header("Authorization", &format!("Bearer {token}"))
        .header(PROTOCOL_HEADER, PROTOCOL_VERSION.to_string())
        .call()
        .map_err(|e| FetchError::Transport(format!("T3 へ接続できません: {e}")))?;
    match response.status().as_u16() {
        200 => {}
        401 => return Err(FetchError::Unauthorized),
        403 => return Err(FetchError::Forbidden),
        429 => {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse::<u64>().ok().or_else(|| {
                        DateTime::parse_from_rfc2822(v).ok().map(|date| {
                            (date.with_timezone(&Utc) - Utc::now()).num_seconds().max(0) as u64
                        })
                    })
                })
                .unwrap_or(5)
                .clamp(1, 300);
            return Err(FetchError::RateLimited(Duration::from_secs(delay)));
        }
        code => {
            return Err(FetchError::Invalid(format!(
                "T3 API が HTTP {code} を返しました"
            )));
        }
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_vec()
        .map_err(|e| FetchError::Invalid(format!("T3 応答を読み取れません: {e}")))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| FetchError::Invalid("T3 API の JSON 形式を確認してください".into()))?;
    parse_shell_recent(&value, recent_hours, SystemTime::now()).map_err(FetchError::Invalid)
}

#[cfg(test)]
fn parse_shell(root: &Value) -> Result<RemoteSnapshot, String> {
    parse_shell_recent(root, 24, SystemTime::now())
}

fn parse_shell_recent(
    root: &Value,
    recent_hours: u64,
    now: SystemTime,
) -> Result<RemoteSnapshot, String> {
    let obj = root
        .as_object()
        .ok_or("shell response が object ではありません")?;
    let projects = obj
        .get("projects")
        .and_then(Value::as_array)
        .ok_or("projects が配列ではありません")?;
    // schemaVersion はサーバー内部の保存形式の版で(v0.1.0 で 2)、通信の形とは別に上がる。
    // T3 の client も確かめていないので、ここでも見ない。通信の形は protocol ヘッダーで決まる。
    let threads = obj
        .get("threads")
        .and_then(Value::as_array)
        .ok_or("threads が配列ではありません")?;
    if projects.len() > THREAD_LIMIT * 2 {
        return Err(format!(
            "project 数が上限 {} を超えています",
            THREAD_LIMIT * 2
        ));
    }
    if threads.len() > THREAD_LIMIT {
        return Err(format!("thread 数が上限 {THREAD_LIMIT} を超えています"));
    }
    let mut project_names = HashMap::with_capacity(projects.len());
    for project in projects {
        let id = field_str(project, "id").ok_or("project id がありません")?;
        if id.is_empty() {
            return Err("空の project id があります".into());
        }
        let title = field_str(project, "title").unwrap_or(id);
        project_names.insert(sanitize(id, 256), sanitize(title, ENTRY_TEXT_LIMIT));
    }
    let mut entries = Vec::with_capacity(threads.len());
    let mut seen_ids = std::collections::HashSet::with_capacity(threads.len());
    // 除外した親のタイトルも子の見出しに使うので、対象かどうかの前に集める。
    let mut titles: HashMap<String, String> = HashMap::with_capacity(threads.len());
    for thread in threads {
        validate_thread(thread)?;
        let id = field_str(thread, "id").ok_or("thread id がありません")?;
        if id.is_empty() || !seen_ids.insert(id) {
            return Err("thread id が空、または重複しています".into());
        }
        let project_id = field_str(thread, "projectId").ok_or("thread projectId がありません")?;
        if project_id.is_empty() {
            return Err("空の thread projectId があります".into());
        }
        let selection = thread.get("modelSelection").filter(|v| v.is_object());
        let request = thread
            .get("pendingRuntimeRequest")
            .filter(|v| v.is_object());
        let raw_status = field_str(thread, "status");
        let raw_activity_status = field_str(thread, "activityRunStatus");
        let plan = bool_field(thread, "hasActionableProposedPlan");
        let background_tasks = holding_background_tasks(thread);
        let state = normalize_state(
            raw_status,
            raw_activity_status,
            request.and_then(|r| field_str(r, "kind")),
            plan,
            background_tasks > 0,
        );
        titles.insert(
            sanitize(id, 256),
            field_str(thread, "title").map_or_else(
                || "無題".into(),
                |v| nonempty(sanitize(v, ENTRY_TEXT_LIMIT), "無題"),
            ),
        );
        let archived = thread.get("archivedAt").is_some_and(|v| !v.is_null());
        // T3 のサーバー・client と同じく、settledOverride だけで片付け済みかを決める。
        let settled = field_str(thread, "settledOverride") == Some("settled");
        let updated = field_str(thread, "updatedAt");
        if archived || !is_target(state, settled, updated, recent_hours, now) {
            continue;
        }
        let project_title = project_names
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| short_id(project_id));
        let goal = thread
            .get("goal")
            .filter(|g| g.is_object() && field_str(g, "status") != Some("complete"))
            .and_then(|g| field_str(g, "objective"))
            .map(|s| sanitize(s, ENTRY_TEXT_LIMIT))
            .filter(|s| !s.is_empty());
        entries.push(AgentEntry {
            // 接続は install_snapshot が付ける(parse は 1 つの shell 応答だけを見る)。
            server_id: ServerId::default(),
            thread_id: sanitize(id, 256),
            project_id: sanitize(project_id, 256),
            project_title,
            title: field_str(thread, "title").map_or_else(
                || "無題".into(),
                |v| nonempty(sanitize(v, ENTRY_TEXT_LIMIT), "無題"),
            ),
            provider: field_str(thread, "providerInstanceId")
                .map(|s| provider_label(&sanitize(s, 128))),
            model: nested_str(selection, "model").map(|s| sanitize(&s, 128)),
            state,
            raw_status: raw_status.map(|s| sanitize(s, 64)),
            raw_activity_status: raw_activity_status.map(|s| sanitize(s, 64)),
            has_pending_approvals: state == AgentState::Approval,
            has_pending_user_input: state == AgentState::Input,
            needs_reauth: request.and_then(|r| field_str(r, "kind")) == Some("auth_refresh"),
            has_actionable_proposed_plan: plan,
            background_tasks,
            active_run_id: field_str(thread, "activeRunId").map(|s| sanitize(s, 256)),
            latest_run_id: field_str(thread, "latestRunId").map(|s| sanitize(s, 256)),
            goal,
            branch: field_str(thread, "branch").map(|s| sanitize(s, 256)),
            worktree_path: field_str(thread, "worktreePath").map(|s| sanitize(s, 512)),
            updated_at: updated.map(|s| sanitize(s, 64)),
            requested_at: field_str(thread, "latestRunRequestedAt").map(|s| sanitize(s, 64)),
            started_at: field_str(thread, "latestRunStartedAt").map(|s| sanitize(s, 64)),
            completed_at: field_str(thread, "latestRunCompletedAt").map(|s| sanitize(s, 64)),
            error_summary: error_summary(thread),
            parent_thread_id: subagent_parent(thread),
            parent_title: None,
        });
    }
    for entry in &mut entries {
        entry.parent_title = entry
            .parent_thread_id
            .as_ref()
            .and_then(|parent| titles.get(parent).cloned());
    }
    entries.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
    Ok(RemoteSnapshot {
        environment_id: None,
        environment_label: None,
        server_version: None,
        protocol_version: None,
        sequence: obj.get("snapshotSequence").and_then(Value::as_u64),
        entries,
    })
}

/// V2 の一覧は provider の表示名を持たず instance ID だけなので、既定の instance は名前に直す。
/// 利用者が追加した instance(例: `codex_work`)は ID のまま出す。
fn provider_label(instance_id: &str) -> String {
    match instance_id {
        "claudeAgent" => "Claude",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "opencode" => "OpenCode",
        "grok" => "Grok",
        "pi" => "Pi",
        other => other,
    }
    .to_string()
}

/// 利用上限で止まった run は、いつ再開できるかを添える。
/// `lineage` が `subagent` の子なら親スレッドの ID。`fork`・null・未知の関係は独立したスレッドとして None。
fn subagent_parent(thread: &Value) -> Option<String> {
    let lineage = thread.get("lineage").filter(|v| v.is_object())?;
    if field_str(lineage, "relationshipToParent") != Some("subagent") {
        return None;
    }
    field_str(lineage, "parentThreadId")
        .filter(|id| !id.is_empty())
        .map(|id| sanitize(id, 256))
}

fn error_summary(thread: &Value) -> Option<String> {
    if field_str(thread, "lastErrorClass") == Some("usage_limit") {
        return Some(match field_str(thread, "usageLimitResetAt") {
            Some(at) => format!("利用上限 · {} に再開", sanitize(at, 64)),
            None => "利用上限".into(),
        });
    }
    field_str(thread, "lastError")
        .map(|s| sanitize(s, 240))
        .filter(|s| !s.is_empty())
}

/// T3 の client と同じく、command は完了を引き留めない(dev server など、誰も待っていないことが多い)。
fn holding_background_tasks(thread: &Value) -> usize {
    thread
        .get("pendingBackgroundTasks")
        .and_then(Value::as_array)
        .map_or(0, |tasks| {
            tasks
                .iter()
                .filter(|t| field_str(t, "kind") != Some("command"))
                .count()
        })
}

fn is_target(
    state: AgentState,
    settled: bool,
    updated_at: Option<&str>,
    recent_hours: u64,
    now: SystemTime,
) -> bool {
    let live = matches!(
        state,
        AgentState::Starting
            | AgentState::Running
            | AgentState::Monitoring
            | AgentState::Approval
            | AgentState::Input
            | AgentState::Plan
    );
    // 片付け済み(settled)の失敗は、もう注意を引かない。
    if live || state == AgentState::Error && !settled {
        return true;
    }
    let Some(raw) = updated_at else {
        return false;
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(raw) else {
        return false;
    };
    let age = DateTime::<Utc>::from(now).signed_duration_since(parsed.with_timezone(&Utc));
    age.num_seconds() >= 0
        && age <= chrono::Duration::hours(i64::try_from(recent_hours.min(8760)).unwrap_or(8760))
}

fn validate_thread(thread: &Value) -> Result<(), String> {
    if !thread.is_object() {
        return Err("thread item が object ではありません".into());
    }
    for key in ["status", "providerInstanceId"] {
        if !thread.get(key).is_some_and(Value::is_string) {
            return Err(format!("thread.{key} が文字列ではありません"));
        }
    }
    for key in [
        "title",
        "projectId",
        "branch",
        "worktreePath",
        "updatedAt",
        "activityRunStatus",
        "lastError",
        "lastErrorClass",
        "usageLimitResetAt",
        "latestRunId",
        "activeRunId",
        "latestRunRequestedAt",
        "latestRunStartedAt",
        "latestRunCompletedAt",
        "settledOverride",
    ] {
        validate_string_or_null(thread, key)?;
    }
    if let Some(value) = thread.get("hasActionableProposedPlan")
        && !value.is_boolean()
    {
        return Err("thread.hasActionableProposedPlan が boolean ではありません".into());
    }
    for key in ["modelSelection", "pendingRuntimeRequest", "goal", "lineage"] {
        if let Some(value) = thread.get(key)
            && !value.is_null()
            && !value.is_object()
        {
            return Err(format!("thread.{key} が object または null ではありません"));
        }
    }
    for (parent, fields) in [
        ("modelSelection", &["instanceId", "model"][..]),
        ("pendingRuntimeRequest", &["kind"][..]),
        ("goal", &["objective", "status"][..]),
        (
            "lineage",
            &["parentThreadId", "relationshipToParent", "rootThreadId"][..],
        ),
    ] {
        let Some(value) = thread.get(parent).filter(|v| v.is_object()) else {
            continue;
        };
        for field in fields {
            validate_string_or_null(value, field)
                .map_err(|e| format!("thread.{parent}.{field}: {e}"))?;
        }
    }
    if let Some(tasks) = thread.get("pendingBackgroundTasks")
        && !tasks.is_null()
    {
        let tasks = tasks
            .as_array()
            .ok_or("thread.pendingBackgroundTasks が配列ではありません")?;
        if tasks
            .iter()
            .any(|t| !t.get("kind").is_some_and(Value::is_string))
        {
            return Err("thread.pendingBackgroundTasks に kind の無い項目があります".into());
        }
    }
    Ok(())
}

fn validate_string_or_null(value: &Value, key: &str) -> Result<(), String> {
    if let Some(field) = value.get(key)
        && !field.is_null()
        && !field.is_string()
    {
        return Err(format!("{key} が文字列または null ではありません"));
    }
    Ok(())
}

/// T3 v0.1.0 の client(`presentThreadShell`・`shellRuntime`)と同じ優先順で状態を決める。
fn normalize_state(
    status: Option<&str>,
    activity: Option<&str>,
    request_kind: Option<&str>,
    plan: bool,
    holds_background: bool,
) -> AgentState {
    match request_kind {
        // auth_refresh は手元で認証し直すまで進まないので、入力待ちとして扱う。
        Some("user_input" | "auth_refresh") => return AgentState::Input,
        Some(_) => return AgentState::Approval,
        None => {}
    }
    // 計画を受け入れて run が動き出したら、旗が残っていても実行中として扱う。
    if plan && activity.is_none() {
        return AgentState::Plan;
    }
    if holds_background && status != Some("failed") {
        return AgentState::Monitoring;
    }
    match activity.or(status) {
        Some("preparing" | "queued" | "starting") => AgentState::Starting,
        Some("running") => AgentState::Running,
        // run の中で subagent や monitor の結果を待っている。
        Some("waiting") => AgentState::Monitoring,
        Some("completed") => AgentState::Completed,
        Some("interrupted") => AgentState::Interrupted,
        Some("failed") => AgentState::Error,
        Some("cancelled" | "rolled_back") => AgentState::Stopped,
        Some("idle") => AgentState::Idle,
        _ => AgentState::Unknown,
    }
}

fn diff(
    server_id: &ServerId,
    old: &[AgentEntry],
    new: &[AgentEntry],
    cursor: &mut u64,
    now: SystemTime,
) -> Vec<AgentChange> {
    let old_by_id: HashMap<&str, &AgentEntry> =
        old.iter().map(|e| (e.thread_id.as_str(), e)).collect();
    let mut result = Vec::new();
    for entry in new {
        let Some(previous) = old_by_id.get(entry.thread_id.as_str()).copied() else {
            push_change(
                &mut result,
                server_id,
                cursor,
                now,
                entry,
                AgentChangeKind::New,
                None,
            );
            continue;
        };
        let kind = if previous.state != entry.state {
            Some(if entry.state == AgentState::Completed {
                AgentChangeKind::TurnCompleted
            } else if entry.state == AgentState::Running
                && matches!(
                    previous.state,
                    AgentState::Approval
                        | AgentState::Input
                        | AgentState::Plan
                        | AgentState::Interrupted
                )
            {
                AgentChangeKind::Resumed
            } else if entry.state == AgentState::Error {
                AgentChangeKind::Error
            } else {
                AgentChangeKind::State
            })
        } else if entry.state == AgentState::Error && previous.error_summary != entry.error_summary
        {
            Some(AgentChangeKind::Error)
        } else if previous.active_run_id != entry.active_run_id
            || previous.latest_run_id != entry.latest_run_id
        {
            // 新しい run が始まった。preparing → queued → starting のような同じ状態の中の移り変わりは知らせない。
            Some(AgentChangeKind::Resumed)
        } else if previous.has_pending_approvals != entry.has_pending_approvals
            || previous.has_pending_user_input != entry.has_pending_user_input
            || previous.has_actionable_proposed_plan != entry.has_actionable_proposed_plan
        {
            Some(AgentChangeKind::Flags)
        } else if previous.goal != entry.goal {
            Some(AgentChangeKind::Goal)
        } else if previous.title != entry.title {
            Some(AgentChangeKind::Title)
        } else {
            None
        };
        if let Some(kind) = kind {
            push_change(
                &mut result,
                server_id,
                cursor,
                now,
                entry,
                kind,
                Some(previous),
            );
        }
    }
    // Removed entries naturally disappear from the complete snapshot; no entry allocation is retained.
    if result.len() > CHANGE_LIMIT {
        result.drain(..result.len() - CHANGE_LIMIT);
    }
    result
}

fn push_change(
    out: &mut Vec<AgentChange>,
    server_id: &ServerId,
    cursor: &mut u64,
    now: SystemTime,
    entry: &AgentEntry,
    kind: AgentChangeKind,
    previous: Option<&AgentEntry>,
) {
    *cursor = cursor.saturating_add(1);
    out.push(AgentChange {
        server_id: server_id.clone(),
        id: *cursor,
        thread_id: entry.thread_id.clone(),
        kind,
        before_state: previous.map(|e| e.state),
        after_state: Some(entry.state),
        before_goal: previous.and_then(|e| e.goal.clone()),
        after_goal: entry.goal.clone(),
        before_title: previous.map(|e| e.title.clone()),
        after_title: Some(entry.title.clone()),
        observed_at: now,
    });
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}
fn nested_str(value: Option<&Value>, key: &str) -> Option<String> {
    field_str(value?, key).map(str::to_owned)
}
fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn sanitize(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}
fn nonempty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}
fn short_id(value: &str) -> String {
    let prefix: String = value.chars().take(12).collect();
    if value.chars().count() > 12 {
        format!("{prefix}…")
    } else if value.is_empty() {
        "不明なプロジェクト".into()
    } else {
        prefix
    }
}

fn fingerprint(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

fn auth_retry_allowed(blocked: bool, previous_fingerprint: u64, next_fingerprint: u64) -> bool {
    !blocked || previous_fingerprint != next_fingerprint
}

/// worker が使う token の読み込み。リンクを追跡せず、開いた FD で通常ファイル・所有者・権限・サイズ・1 行の
/// 形式を検査してから読む(`token_file` モジュール)。エラーには値を含めない。
fn read_token(path: &str) -> Result<String, String> {
    crate::token_file::read_private_token(path, effective_uid(), effective_gid())
        .map_err(|e| e.message().to_string())
}

/// `probe_shell` の失敗。値(token)は含まない。
#[derive(Debug, PartialEq)]
pub(crate) enum ProbeError {
    BadUrl(String),
    EnvironmentMismatch,
    Unauthorized,
    Forbidden,
    /// リダイレクト・未対応の protocol など、想定外の応答
    Rejected(String),
    Transport(String),
}

/// `probe_shell` が成功したときの、接続先の環境。
#[derive(Debug)]
pub(crate) struct ProbeOk {
    pub environment_id: Option<String>,
}

/// 接続先の T3 に、この token で一覧 API(`GET /api/orchestration/shell`)が読めるかを 1 回だけ確かめる
/// (token の受取ツール用)。worker と同じ規則: URL は `check_url`、リダイレクトは追跡しない。
/// 先に認証なしで環境情報を取り、`expect_environment_id` があって違えば token を送らずに止める。
pub(crate) fn probe_shell(
    url: &str,
    token: &str,
    timeout_secs: u64,
    expect_environment_id: Option<&str>,
) -> Result<ProbeOk, ProbeError> {
    check_url(url).map_err(ProbeError::BadUrl)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let base = url.trim_end_matches('/');
    let to_probe = |e: FetchError| match e {
        FetchError::Unauthorized => ProbeError::Unauthorized,
        FetchError::Forbidden => ProbeError::Forbidden,
        FetchError::RateLimited(_) => {
            ProbeError::Rejected("T3 が取得を制限しています (429)".into())
        }
        FetchError::Invalid(w) => ProbeError::Rejected(w),
        FetchError::Transport(w) => ProbeError::Transport(w),
    };
    let environment = fetch_environment(&agent, &format!("{base}/.well-known/t3/environment"))
        .map_err(to_probe)?;
    if let Some(expected) = expect_environment_id
        && environment.environment_id.as_deref() != Some(expected)
    {
        return Err(ProbeError::EnvironmentMismatch);
    }
    fetch(
        &agent,
        &format!("{base}/api/orchestration/shell"),
        token,
        24,
    )
    .map_err(to_probe)?;
    Ok(ProbeOk {
        environment_id: environment.environment_id,
    })
}

/// トークンファイルの所有者と権限が規則どおりか: 実行ユーザーの 0600、または root:実行グループの 0640。
/// `euid` / `egid` は実行ユーザーとグループ(テストでは偽の値を渡す)。
pub(crate) fn token_perms_ok(mode: u32, uid: u32, gid: u32, euid: u32, egid: u32) -> bool {
    match mode & 0o777 {
        0o600 => uid == euid,
        0o640 => uid == 0 && gid == egid,
        _ => false,
    }
}

#[cfg(unix)]
pub(crate) fn effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}
#[cfg(unix)]
pub(crate) fn effective_gid() -> u32 {
    unsafe extern "C" {
        fn getegid() -> u32;
    }
    unsafe { getegid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};

    fn sid() -> ServerId {
        "primary".into()
    }

    fn shell(threads: Vec<Value>, sequence: u64) -> Value {
        serde_json::json!({
            "schemaVersion": 2,
            "snapshotSequence": sequence,
            "projects":[{"id":"p1", "title":"Project"}], "threads":threads,
            "archivedThreads":[]
        })
    }

    /// T3 v0.1.0 の OrchestrationV2ThreadShell のうち、miharashi が読む項目。
    fn mock_thread(id: &str, status: &str, updated: &str) -> Value {
        let updated = if DateTime::parse_from_rfc3339(updated).is_ok() {
            updated.to_owned()
        } else {
            Utc::now().to_rfc3339()
        };
        let active = matches!(
            status,
            "preparing" | "queued" | "starting" | "running" | "waiting"
        );
        serde_json::json!({
            "id":id, "projectId":"p1", "title":"Thread", "updatedAt":updated,
            "providerInstanceId":"codex",
            "modelSelection":{"instanceId":"codex","model":"gpt","options":[]},
            "status":status,
            "activityRunStatus": if active { Value::String(status.into()) } else { Value::Null },
            "latestRunId":"run-1",
            "activeRunId": if active { Value::String("run-1".into()) } else { Value::Null },
            "latestRunRequestedAt":"2026-10-15T00:00:00Z",
            "latestRunStartedAt":"2026-10-15T00:00:01Z",
            "latestRunCompletedAt":null,
            "lastError":null, "lastErrorClass":null, "usageLimitResetAt":null,
            "pendingRuntimeRequest":null, "hasActionableProposedPlan":false,
            "pendingBackgroundTasks":[], "goal":null,
            "lineage":null,
            "archivedAt":null, "settledOverride":null, "settledAt":null
        })
    }

    fn state_of(thread: Value) -> AgentState {
        parse_shell(&shell(vec![thread], 1)).unwrap().entries[0].state
    }

    #[test]
    fn maps_schema_prioritizes_requests_and_sanitizes_strings() {
        let mut running = mock_thread("t1", "running", "today");
        running["title"] = Value::String("Thr\u{7}ead".into());
        running["goal"] = serde_json::json!({"objective":"テストを通す","status":"active"});
        let parsed = parse_shell(&shell(vec![running], 42)).unwrap();
        assert_eq!(parsed.sequence, Some(42));
        let e = &parsed.entries[0];
        assert_eq!(e.project_title, "Project");
        assert_eq!(e.title, "Thread");
        assert_eq!(e.state, AgentState::Running);
        assert_eq!(e.provider.as_deref(), Some("Codex"));
        assert_eq!(e.model.as_deref(), Some("gpt"));
        assert_eq!(e.active_run_id.as_deref(), Some("run-1"));
        assert_eq!(e.goal.as_deref(), Some("テストを通す"));

        let mut approval = mock_thread("t1", "running", "today");
        approval["pendingRuntimeRequest"] =
            serde_json::json!({"id":"r1","kind":"command","createdAt":"2026-10-15T00:00:00Z"});
        let parsed = parse_shell(&shell(vec![approval], 43)).unwrap();
        assert_eq!(parsed.entries[0].state, AgentState::Approval);
        assert!(parsed.entries[0].has_pending_approvals);

        for kind in ["user_input", "auth_refresh"] {
            let mut input = mock_thread("t1", "waiting", "today");
            input["pendingRuntimeRequest"] =
                serde_json::json!({"id":"r1","kind":kind,"createdAt":"2026-10-15T00:00:00Z"});
            assert_eq!(state_of(input), AgentState::Input, "{kind}");
        }

        let mut reauth = mock_thread("t1", "waiting", "today");
        reauth["pendingRuntimeRequest"] =
            serde_json::json!({"id":"r1","kind":"auth_refresh","createdAt":"2026-10-15T00:00:00Z"});
        assert!(parse_shell(&shell(vec![reauth], 1)).unwrap().entries[0].needs_reauth);

        let mut plan = mock_thread("t1", "completed", "today");
        plan["hasActionableProposedPlan"] = Value::Bool(true);
        assert_eq!(state_of(plan), AgentState::Plan);
        // 計画を受け入れて run が動き出したら、旗が残っていても実行中。
        let mut accepted = mock_thread("t1", "running", "today");
        accepted["hasActionableProposedPlan"] = Value::Bool(true);
        assert_eq!(state_of(accepted), AgentState::Running);
    }

    #[test]
    fn missing_provider_is_rejected() {
        let mut no_provider = mock_thread("t", "running", "now");
        no_provider["providerInstanceId"] = Value::Null;
        assert!(parse_shell(&shell(vec![no_provider], 1)).is_err());
    }

    #[test]
    fn steps_inside_one_state_and_stale_errors_are_not_reported() {
        let parse = |thread: Value| parse_shell(&shell(vec![thread], 1)).unwrap().entries;
        let mut cursor = 0;
        // preparing → starting は同じ「起動中」で、同じ run なので知らせない。
        let preparing = parse(mock_thread("t", "preparing", "now"));
        let starting = parse(mock_thread("t", "starting", "now"));
        assert!(
            diff(
                &sid(),
                &preparing,
                &starting,
                &mut cursor,
                SystemTime::now()
            )
            .is_empty()
        );
        // 終わった thread の lastError が消えても、異常として知らせない。
        let mut with_error = mock_thread("t", "completed", "now");
        with_error["lastError"] = Value::String("old".into());
        let cleared = parse(mock_thread("t", "completed", "now"));
        assert!(
            diff(
                &sid(),
                &parse(with_error),
                &cleared,
                &mut cursor,
                SystemTime::now()
            )
            .is_empty()
        );
    }

    #[test]
    fn run_statuses_map_like_the_t3_client() {
        for (status, expected) in [
            ("idle", AgentState::Idle),
            ("preparing", AgentState::Starting),
            ("queued", AgentState::Starting),
            ("starting", AgentState::Starting),
            ("running", AgentState::Running),
            ("waiting", AgentState::Monitoring),
            ("completed", AgentState::Completed),
            ("interrupted", AgentState::Interrupted),
            ("failed", AgentState::Error),
            ("cancelled", AgentState::Stopped),
            ("rolled_back", AgentState::Stopped),
            ("mystery", AgentState::Unknown),
        ] {
            assert_eq!(
                state_of(mock_thread("t", status, "now")),
                expected,
                "{status}"
            );
        }
        // activityRunStatus は、最新 run の status より優先する(wake で続きの仕事をしている)。
        let mut woke = mock_thread("t", "completed", "now");
        woke["activityRunStatus"] = Value::String("running".into());
        assert_eq!(state_of(woke), AgentState::Running);
    }

    #[test]
    fn background_work_holds_completion_except_commands_and_failures() {
        let mut monitor = mock_thread("t", "completed", "now");
        monitor["pendingBackgroundTasks"] = serde_json::json!([{"taskId":"m1","kind":"monitor"}]);
        let parsed = parse_shell(&shell(vec![monitor.clone()], 1)).unwrap();
        assert_eq!(parsed.entries[0].state, AgentState::Monitoring);
        assert_eq!(parsed.entries[0].background_tasks, 1);

        let mut command = mock_thread("t", "completed", "now");
        command["pendingBackgroundTasks"] = serde_json::json!([{"taskId":"c1","kind":"command"}]);
        assert_eq!(state_of(command), AgentState::Completed);

        monitor["status"] = Value::String("failed".into());
        assert_eq!(state_of(monitor), AgentState::Error);
    }

    #[test]
    fn usage_limit_error_shows_reset_time() {
        let mut limited = mock_thread("t", "failed", "now");
        limited["lastError"] = Value::String("rate limited".into());
        limited["lastErrorClass"] = Value::String("usage_limit".into());
        limited["usageLimitResetAt"] = Value::String("2026-10-16T09:00:00.000Z".into());
        let e = &parse_shell(&shell(vec![limited], 1)).unwrap().entries[0];
        assert_eq!(e.state, AgentState::Error);
        assert_eq!(
            e.error_summary.as_deref(),
            Some("利用上限 · 2026-10-16T09:00:00.000Z に再開")
        );

        let mut failed = mock_thread("t", "failed", "now");
        failed["lastError"] = Value::String("boom\n".into());
        assert_eq!(
            parse_shell(&shell(vec![failed], 1)).unwrap().entries[0]
                .error_summary
                .as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn semantic_diff_ignores_updated_at_and_repeated_snapshot() {
        let first = parse_shell(&shell(vec![mock_thread("t1", "running", "a")], 1)).unwrap();
        let mut cursor = 0;
        let initial = diff(&sid(), &[], &first.entries, &mut cursor, SystemTime::now());
        assert_eq!(initial[0].kind, AgentChangeKind::New);
        let second = parse_shell(&shell(vec![mock_thread("t1", "running", "b")], 2)).unwrap();
        assert!(
            diff(
                &sid(),
                &first.entries,
                &second.entries,
                &mut cursor,
                SystemTime::now()
            )
            .is_empty()
        );
        let third = parse_shell(&shell(vec![mock_thread("t1", "completed", "c")], 3)).unwrap();
        assert_eq!(
            diff(
                &sid(),
                &first.entries,
                &third.entries,
                &mut cursor,
                SystemTime::now()
            )[0]
            .kind,
            AgentChangeKind::TurnCompleted
        );
    }

    #[test]
    fn new_run_is_reported_even_when_running_state_is_unchanged() {
        let a = AgentEntry {
            thread_id: "t".into(),
            state: AgentState::Running,
            active_run_id: Some("run-a".into()),
            ..AgentEntry::default()
        };
        let mut b = a.clone();
        b.active_run_id = Some("run-b".into());
        let mut cursor = 0;
        let changes = diff(
            &sid(),
            std::slice::from_ref(&a),
            &[b],
            &mut cursor,
            SystemTime::now(),
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, AgentChangeKind::Resumed);
    }

    #[test]
    fn goal_change_is_its_own_change_kind() {
        let a = AgentEntry {
            thread_id: "t".into(),
            state: AgentState::Running,
            goal: Some("前".into()),
            ..AgentEntry::default()
        };
        let mut b = a.clone();
        b.goal = Some("後".into());
        let mut cursor = 0;
        let changes = diff(
            &sid(),
            std::slice::from_ref(&a),
            &[b],
            &mut cursor,
            SystemTime::now(),
        );
        assert_eq!(changes[0].kind, AgentChangeKind::Goal);
        assert_eq!(changes[0].before_goal.as_deref(), Some("前"));
        assert_eq!(changes[0].after_goal.as_deref(), Some("後"));
    }

    #[test]
    fn archived_settled_and_old_threads_are_filtered_but_attention_items_remain() {
        let now = SystemTime::now();
        let recent = DateTime::<Utc>::from(now).to_rfc3339();
        let old = DateTime::<Utc>::from(now - Duration::from_secs(48 * 3600)).to_rfc3339();
        let mut archived = mock_thread("archived", "running", &recent);
        archived["archivedAt"] = Value::String(recent.clone());
        let old_running = mock_thread("running", "running", &old);
        let old_idle = mock_thread("idle-old", "idle", &old);
        let recent_idle = mock_thread("idle-recent", "idle", &recent);
        let old_failed = mock_thread("failed-old", "failed", &old);
        let mut old_failed_settled = mock_thread("failed-settled", "failed", &old);
        old_failed_settled["settledOverride"] = Value::String("settled".into());
        // settledAt だけ残っていても、settledOverride が settled でなければ片付け済みではない。
        let mut old_failed_reopened = mock_thread("failed-reopened", "failed", &old);
        old_failed_reopened["settledAt"] = Value::String(old.clone());
        old_failed_reopened["settledOverride"] = Value::String("active".into());
        let parsed = parse_shell_recent(
            &shell(
                vec![
                    archived,
                    old_running,
                    old_idle,
                    recent_idle,
                    old_failed,
                    old_failed_settled,
                    old_failed_reopened,
                ],
                1,
            ),
            24,
            now,
        )
        .unwrap();
        let ids: Vec<_> = parsed
            .entries
            .iter()
            .map(|e| e.thread_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["failed-old", "failed-reopened", "idle-recent", "running"]
        );
    }

    fn lineage(parent: &str, relationship: &str) -> Value {
        serde_json::json!({
            "parentThreadId": parent,
            "relationshipToParent": relationship,
            "rootThreadId": "root",
        })
    }

    #[test]
    fn subagent_lineage_gives_the_parent_and_its_title() {
        let mut parent = mock_thread("parent", "running", "today");
        parent["title"] = Value::String("親のスレッド".into());
        let mut child = mock_thread("child", "completed", "today");
        child["lineage"] = lineage("parent", "subagent");
        let mut solo = mock_thread("solo", "idle", "today");
        solo["lineage"] = serde_json::json!({
            "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "solo"
        });
        let parsed = parse_shell(&shell(vec![parent, child, solo], 1)).unwrap();
        let by_id = |id: &str| parsed.entries.iter().find(|e| e.thread_id == id).unwrap();
        assert_eq!(by_id("child").parent_thread_id.as_deref(), Some("parent"));
        assert_eq!(by_id("child").parent_title.as_deref(), Some("親のスレッド"));
        // lineage が null(または無い)スレッドと、親を持たない lineage は独立したスレッド。
        assert_eq!(by_id("parent").parent_thread_id, None);
        assert_eq!(by_id("solo").parent_thread_id, None);
        assert_eq!(by_id("solo").parent_title, None);
    }

    #[test]
    fn fork_and_unknown_relationships_stay_independent() {
        for relationship in ["fork", "something-new", ""] {
            let mut child = mock_thread("child", "running", "today");
            child["lineage"] = lineage("parent", relationship);
            let parsed = parse_shell(&shell(vec![child], 1)).unwrap();
            assert_eq!(parsed.entries[0].parent_thread_id, None, "{relationship}");
            assert_eq!(parsed.entries[0].parent_title, None, "{relationship}");
        }
        let mut no_lineage = mock_thread("child", "running", "today");
        no_lineage.as_object_mut().unwrap().remove("lineage");
        assert_eq!(
            parse_shell(&shell(vec![no_lineage], 1)).unwrap().entries[0].parent_thread_id,
            None
        );
    }

    #[test]
    fn malformed_lineage_is_rejected_like_other_fields() {
        let mut not_object = mock_thread("t", "running", "today");
        not_object["lineage"] = Value::String("parent".into());
        assert!(parse_shell(&shell(vec![not_object], 1)).is_err());
        for key in ["parentThreadId", "relationshipToParent", "rootThreadId"] {
            let mut bad = mock_thread("t", "running", "today");
            bad["lineage"] = lineage("parent", "subagent");
            bad["lineage"][key] = serde_json::json!(5);
            assert!(parse_shell(&shell(vec![bad], 1)).is_err(), "{key}");
        }
    }

    #[test]
    fn an_excluded_parent_still_names_its_children() {
        let now = SystemTime::now();
        let recent = DateTime::<Utc>::from(now).to_rfc3339();
        let old = DateTime::<Utc>::from(now - Duration::from_secs(48 * 3600)).to_rfc3339();
        let mut old_parent = mock_thread("old-parent", "idle", &old);
        old_parent["title"] = Value::String("古い親".into());
        let mut archived_parent = mock_thread("archived-parent", "running", &recent);
        archived_parent["title"] = Value::String("片付けた親".into());
        archived_parent["archivedAt"] = Value::String(recent.clone());
        let mut a = mock_thread("a", "running", &recent);
        a["lineage"] = lineage("old-parent", "subagent");
        let mut b = mock_thread("b", "running", &recent);
        b["lineage"] = lineage("archived-parent", "subagent");
        let mut c = mock_thread("c", "running", &recent);
        c["lineage"] = lineage("missing-parent", "subagent");
        let parsed = parse_shell_recent(
            &shell(vec![old_parent, archived_parent, a, b, c], 1),
            24,
            now,
        )
        .unwrap();
        let ids: Vec<_> = parsed
            .entries
            .iter()
            .map(|e| e.thread_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(parsed.entries[0].parent_title.as_deref(), Some("古い親"));
        assert_eq!(
            parsed.entries[1].parent_title.as_deref(),
            Some("片付けた親")
        );
        // 親が shell に無ければタイトルは None(親 ID は残る)。
        assert_eq!(
            parsed.entries[2].parent_thread_id.as_deref(),
            Some("missing-parent")
        );
        assert_eq!(parsed.entries[2].parent_title, None);
    }

    #[test]
    fn malformed_ids_and_v1_shapes_are_not_silently_accepted() {
        let duplicate = mock_thread("same", "running", "now");
        assert!(parse_shell(&shell(vec![duplicate.clone(), duplicate], 1)).is_err());
        // V1 の thread(session・latestTurn だけで status が無い)は受け付けない。
        let v1 = serde_json::json!({
            "id":"v1", "projectId":"p1", "title":"Thread",
            "session":{"status":"running"}, "latestTurn":{"id":"turn-1","state":"running"}
        });
        assert!(parse_shell(&shell(vec![v1], 1)).is_err());
        let mut malformed = mock_thread("bad", "running", "now");
        malformed["pendingRuntimeRequest"] = Value::String("partial parse".into());
        assert!(parse_shell(&shell(vec![malformed], 1)).is_err());
        let mut malformed_flag = mock_thread("bad-flag", "running", "now");
        malformed_flag["hasActionableProposedPlan"] = Value::String("false".into());
        assert!(parse_shell(&shell(vec![malformed_flag], 1)).is_err());
        let mut malformed_tasks = mock_thread("bad-tasks", "running", "now");
        malformed_tasks["pendingBackgroundTasks"] = serde_json::json!([{"taskId":"x"}]);
        assert!(parse_shell(&shell(vec![malformed_tasks], 1)).is_err());
    }

    #[test]
    fn initial_and_resync_snapshots_do_not_emit_a_flood() {
        let shared = Arc::new(RwLock::new(AgentSnapshot::default()));
        let remote = parse_shell(&shell(
            vec![
                mock_thread("t1", "running", "a"),
                mock_thread("t2", "idle", "a"),
            ],
            1,
        ))
        .unwrap();
        install_snapshot(&shared, &sid(), remote, false);
        assert!(shared.read().unwrap().changes.is_empty());
        let remote = parse_shell(&shell(
            vec![
                mock_thread("t1", "completed", "b"),
                mock_thread("t2", "idle", "b"),
            ],
            2,
        ))
        .unwrap();
        install_snapshot(&shared, &sid(), remote, true);
        assert!(shared.read().unwrap().changes.is_empty());
        assert_eq!(shared.read().unwrap().resync_generation, 1);
    }

    #[test]
    fn mock_http_server_checks_bearer_and_parses_complete_response() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let body = shell(vec![mock_thread("mock-1", "running", "now")], 7).to_string();
        let server_body = body.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer mock-token"));
            assert!(request.contains("x-t3-orchestration-protocol: 2"));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", server_body.len(), server_body).unwrap();
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let got = fetch(
            &agent,
            &format!("http://{address}/api/orchestration/shell"),
            "mock-token",
            24,
        )
        .unwrap();
        assert_eq!(got.sequence, Some(7));
        assert_eq!(got.entries[0].thread_id, "mock-1");
        server.join().unwrap();
    }

    #[test]
    fn environment_descriptor_is_checked_separately_from_shell() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /.well-known/t3/environment "));
            assert!(!request.contains("authorization:"));
            let body = r#"{"environmentId":"env-a","label":"T3 host","serverVersion":"0.1.0","orchestrationProtocolVersion":2}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let environment = fetch_environment(
            &agent,
            &format!("http://{address}/.well-known/t3/environment"),
        )
        .unwrap();
        assert_eq!(environment.environment_id.as_deref(), Some("env-a"));
        assert_eq!(environment.environment_label.as_deref(), Some("T3 host"));
        assert_eq!(environment.protocol_version, Some(2));
        server.join().unwrap();
    }

    #[test]
    fn response_limit_rejects_oversized_shell_without_partial_snapshot() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = vec![b'x'; BODY_LIMIT as usize + 1];
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let result = fetch(&agent, &format!("http://{address}/shell"), "token", 24);
        assert!(matches!(result, Err(FetchError::Invalid(_))));
        server.join().unwrap();
    }

    #[test]
    fn auth_statuses_are_distinct_and_redirect_is_not_followed() {
        for (status, expected) in [(401, 401), (403, 403), (302, 302)] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let reason = if status == 302 { "Found" } else { "Error" };
                write!(stream, "HTTP/1.1 {status} {reason}\r\nLocation: http://127.0.0.1:1/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            });
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into();
            let result = fetch(&agent, &format!("http://{address}/shell"), "token", 24);
            match expected {
                401 => assert!(matches!(result, Err(FetchError::Unauthorized))),
                403 => assert!(matches!(result, Err(FetchError::Forbidden))),
                _ => assert!(matches!(result, Err(FetchError::Invalid(_)))),
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn validates_secure_endpoints() {
        assert!(check_url("http://192.0.2.1:3773").is_ok());
        assert!(check_url("http://[::1]:3773").is_ok());
        assert!(check_url("https://t3.example.test").is_ok());
        assert!(check_url("http://t3.local:3773").is_err());
        assert!(check_url("http://user:secret@192.0.2.1").is_err());
        assert!(check_url("http://192.0.2.1/path?token=secret").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn token_file_permissions_reject_world_readable_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("miharashi-t3-token-{}", std::process::id()));
        fs::write(&path, "test-token\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token(path.to_str().unwrap()).unwrap(), "test-token");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(path.to_str().unwrap()).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejected_token_stays_blocked_until_file_content_changes() {
        let old = fingerprint("old-token");
        assert!(!auth_retry_allowed(true, old, fingerprint("old-token")));
        assert!(auth_retry_allowed(true, old, fingerprint("new-token")));
        assert!(auth_retry_allowed(false, old, old));
    }

    /// 独立した 2 台の mock T3。要求ごとに「この token でなければ 401」「応答を遅らせる」を切り替えられる。
    #[derive(Clone)]
    enum Mode {
        Ok,
        Slow(Duration),
        Unauthorized,
    }

    struct MockT3 {
        url: String,
        mode: Arc<std::sync::Mutex<Mode>>,
        sequence: Arc<std::sync::atomic::AtomicU64>,
        env_id: Arc<std::sync::Mutex<String>>,
    }

    fn mock_t3(token: &'static str, threads: Vec<Value>) -> MockT3 {
        use std::io::BufRead;
        use std::sync::atomic::Ordering;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mode = Arc::new(std::sync::Mutex::new(Mode::Ok));
        let sequence = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let env_id = Arc::new(std::sync::Mutex::new(format!("env-{token}")));
        let (m, q, e) = (mode.clone(), sequence.clone(), env_id.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (m, q, e, threads) = (m.clone(), q.clone(), e.clone(), threads.clone());
                thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap_or(0);
                    let mut auth = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                            break;
                        }
                        if line.to_ascii_lowercase().starts_with("authorization:") {
                            auth = line.trim().to_string();
                        }
                    }
                    let mode = m.lock().unwrap().clone();
                    let reply = |stream: &mut std::net::TcpStream, code: &str, body: String| {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    };
                    if first.contains("/.well-known/t3/environment") {
                        let body = serde_json::json!({
                            "orchestrationProtocolVersion": 2,
                            "environmentId": e.lock().unwrap().clone(),
                            "environmentLabel": "mock",
                        })
                        .to_string();
                        reply(&mut stream, "200 OK", body);
                        return;
                    }
                    if let Mode::Slow(d) = mode {
                        thread::sleep(d);
                    }
                    let authorized = auth
                        .to_ascii_lowercase()
                        .ends_with(&format!("bearer {token}"));
                    if matches!(mode, Mode::Unauthorized) || !authorized {
                        reply(&mut stream, "401 Unauthorized", "{}".into());
                    } else {
                        let body = shell(threads, q.load(Ordering::SeqCst)).to_string();
                        reply(&mut stream, "200 OK", body);
                    }
                });
            }
        });
        MockT3 {
            url,
            mode,
            sequence,
            env_id,
        }
    }

    #[cfg(unix)]
    fn token_file(name: &str, token: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("miharashi-multi-{}-{name}", std::process::id()));
        fs::write(&path, format!("{token}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while !ok() {
            assert!(Instant::now() < end, "待ち切れなかった: {what}");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn server_state(world: &World, id: &str) -> (AgentConnection, Option<u64>, Vec<String>) {
        let s = world.agents.iter().find(|s| &*s.server_id == id).unwrap();
        (
            s.snapshot.connection,
            s.snapshot.sequence,
            s.snapshot
                .entries
                .iter()
                .map(|e| e.thread_id.clone())
                .collect(),
        )
    }

    /// 独立した 2 台: 片方の遅延(timeout)・401・復帰の間も、もう片方の取得は止まらず、
    /// それぞれ自分の token だけで認証し、同じ thread / project ID でも接続ごとに別のスレッドとして届く。
    #[cfg(unix)]
    #[test]
    fn one_connection_failing_never_stops_the_other() {
        use std::sync::atomic::Ordering;
        let a = mock_t3("token-a", vec![mock_thread("t1", "running", "now")]);
        let b = mock_t3("token-b", vec![mock_thread("t1", "waiting", "now")]);
        let cfg = ConfigAgents {
            enabled: true,
            url: a.url.clone(),
            token_file: token_file("a", "token-a"),
            poll_secs: 1,
            timeout_secs: 1,
            stale_secs: 30,
            recent_hours: 24,
            extra_servers: vec![crate::config::ConfigAgentServer {
                id: "container".into(),
                label: Some("コンテナ".into()),
                url: b.url.clone(),
                token_file: token_file("b", "token-b"),
            }],
        };
        let connections = start(&cfg);
        assert_eq!(connections.len(), 2);
        assert_eq!(&*connections[0].server_id, "primary");
        assert_eq!(connections[1].label, "コンテナ");
        let mut provider = AgentProvider::new(connections);
        let mut world = World::default();
        let mut poll = |world: &mut World| provider.update(world);

        // 1. 両方とも、自分の token で取得できる。同じ thread id "t1" は接続ごとの別のエントリ。
        wait_for("両方の接続が Live", 10, || {
            poll(&mut world);
            world.agents.len() == 2
                && world
                    .agents
                    .iter()
                    .all(|s| s.snapshot.connection == AgentConnection::Live)
        });
        let ea = &world.agents[0].snapshot.entries[0];
        let eb = &world.agents[1].snapshot.entries[0];
        assert_eq!((ea.thread_id.as_str(), eb.thread_id.as_str()), ("t1", "t1"));
        assert_eq!((&*ea.server_id, &*eb.server_id), ("primary", "container"));
        assert_ne!(ea.key(), eb.key());
        assert_ne!(ea.state, eb.state, "接続ごとの中身が混ざらない");
        assert_ne!(
            world.agents[0].snapshot.environment_id,
            world.agents[1].snapshot.environment_id
        );

        // 2. B が応答を遅らせる(timeout)。B は再接続中になるが、A は更新され続ける。
        *b.mode.lock().unwrap() = Mode::Slow(Duration::from_secs(3));
        wait_for("B が再接続中", 10, || {
            poll(&mut world);
            server_state(&world, "container").0 == AgentConnection::Retrying
        });
        let before = server_state(&world, "primary").1;
        a.sequence.store(40, Ordering::SeqCst);
        wait_for("B の障害中も A が更新される", 10, || {
            poll(&mut world);
            let a_now = server_state(&world, "primary");
            a_now.0 == AgentConnection::Live && a_now.1 == Some(40) && a_now.1 != before
        });
        assert_eq!(
            server_state(&world, "container").2,
            ["t1"],
            "B の最後のデータは障害中も残る(画面が「前回取得時」と示す)"
        );

        // 3. B が復帰する。A の履歴は B の再同期に巻き込まれない。
        *b.mode.lock().unwrap() = Mode::Ok;
        b.sequence.store(9, Ordering::SeqCst);
        wait_for("B が復帰", 20, || {
            poll(&mut world);
            let b_now = server_state(&world, "container");
            b_now.0 == AgentConnection::Live && b_now.1 == Some(9)
        });
        let gen_a = world.agents[0].snapshot.resync_generation;
        assert_eq!(gen_a, 0, "A は再同期していない");
        assert_eq!(
            world.agents[1].snapshot.resync_generation, 1,
            "B だけが再同期した"
        );

        // 4. 再接続で取り直した descriptor の environment_id が変わると、その接続の履歴だけが捨てられる。
        *b.mode.lock().unwrap() = Mode::Slow(Duration::from_secs(3));
        wait_for("B が再接続中(2 回目)", 15, || {
            poll(&mut world);
            server_state(&world, "container").0 == AgentConnection::Retrying
        });
        *b.env_id.lock().unwrap() = "env-replaced".into();
        *b.mode.lock().unwrap() = Mode::Ok;
        let gen_b = world.agents[1].snapshot.resync_generation;
        wait_for("B の environment_id が入れ替わる", 40, || {
            poll(&mut world);
            let b_now = &world.agents[1].snapshot;
            b_now.environment_id.as_deref() == Some("env-replaced")
                && b_now.connection == AgentConnection::Live
        });
        assert!(world.agents[1].snapshot.resync_generation > gen_b);
        assert!(world.agents[1].snapshot.changes.is_empty());
        assert_eq!(world.agents[0].snapshot.resync_generation, 0);

        // 5. B が 401 になる。B は認証が必要になるが、A は Live のまま。
        *b.mode.lock().unwrap() = Mode::Unauthorized;
        wait_for("B が 401", 10, || {
            poll(&mut world);
            server_state(&world, "container").0 == AgentConnection::AuthenticationRequired
        });
        a.sequence.store(41, Ordering::SeqCst);
        wait_for("B の 401 中も A が更新される", 10, || {
            poll(&mut world);
            let a_now = server_state(&world, "primary");
            a_now.0 == AgentConnection::Live && a_now.1 == Some(41)
        });
    }

    /// 受けた要求(パス, Authorization ヘッダー)を記録する mock。`redirect_shell_to` があれば一覧 API は 302 で返す。
    type RequestLog = Arc<std::sync::Mutex<Vec<(String, String)>>>;

    fn recording_mock(redirect_shell_to: Option<String>) -> (String, RequestLog) {
        use std::io::BufRead;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let log: RequestLog = Default::default();
        let l = log.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (l, redirect) = (l.clone(), redirect_shell_to.clone());
                thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap_or(0);
                    let mut auth = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                            break;
                        }
                        if line.to_ascii_lowercase().starts_with("authorization:") {
                            auth = line.trim().to_string();
                        }
                    }
                    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                    l.lock().unwrap().push((path.clone(), auth));
                    if path.contains("/.well-known/t3/environment") {
                        let body = r#"{"orchestrationProtocolVersion":2,"environmentId":"env-x"}"#;
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    } else if let Some(to) = redirect {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nLocation: {to}/api/orchestration/shell\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                    } else {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                        );
                    }
                });
            }
        });
        (url, log)
    }

    /// 2 台のうち一方がリダイレクトで他方の origin を指しても、その token は他方に届かない。
    /// token_file がリンクに差し替えられた接続は、何も送らずに認証が必要になる。
    #[cfg(unix)]
    #[test]
    fn redirects_and_swapped_links_never_move_a_token_to_another_origin() {
        let (a_url, a_log) = recording_mock(None);
        let (b_url, b_log) = recording_mock(Some(a_url.clone()));
        // C: token_file がリンク(正しい token を指している)。C の mock は何も受けないはず
        let (c_url, c_log) = recording_mock(None);
        let real = token_file("c-real", "token-c");
        let link =
            std::env::temp_dir().join(format!("miharashi-multi-{}-c-link", std::process::id()));
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let cfg = ConfigAgents {
            enabled: true,
            url: a_url.clone(),
            token_file: token_file("a2", "token-a"),
            poll_secs: 1,
            timeout_secs: 2,
            stale_secs: 30,
            recent_hours: 24,
            extra_servers: vec![
                crate::config::ConfigAgentServer {
                    id: "container".into(),
                    label: None,
                    url: b_url,
                    token_file: token_file("b2", "token-b"),
                },
                crate::config::ConfigAgentServer {
                    id: "linked".into(),
                    label: None,
                    url: c_url,
                    token_file: link.to_str().unwrap().to_string(),
                },
            ],
        };
        let mut provider = AgentProvider::new(start(&cfg));
        let mut world = World::default();
        wait_for("B と C が失敗として現れる", 15, || {
            provider.update(&mut world);
            let st = |id: &str| {
                world
                    .agents
                    .iter()
                    .find(|s| &*s.server_id == id)
                    .map(|s| s.snapshot.connection)
            };
            st("container")
                .is_some_and(|c| c != AgentConnection::Connecting && c != AgentConnection::Live)
                && st("linked") == Some(AgentConnection::AuthenticationRequired)
        });
        thread::sleep(Duration::from_millis(500));
        let linked = world
            .agents
            .iter()
            .find(|s| &*s.server_id == "linked")
            .unwrap();
        assert!(
            linked
                .snapshot
                .error_summary
                .as_deref()
                .is_some_and(|e| e.contains("シンボリックリンク")),
            "{:?}",
            linked.snapshot.error_summary
        );
        assert!(!format!("{:?}", linked.snapshot.error_summary).contains("token-c"));
        // A の mock に届いた Authorization は、A 自身の token だけ。B の token は一度も来ない
        for (path, auth) in a_log.lock().unwrap().iter() {
            assert!(
                !auth.contains("token-b"),
                "A に B の token が届いた: {path}"
            );
            assert!(
                !auth.contains("token-c"),
                "A に C の token が届いた: {path}"
            );
        }
        // B は自分の origin にだけ token を送り、リダイレクトは追跡していない
        assert!(
            b_log
                .lock()
                .unwrap()
                .iter()
                .any(|(_, a)| a.contains("token-b"))
        );
        // C(リンク)には環境情報の問い合わせも含め、token を載せた要求が 1 件も届かない
        assert!(c_log.lock().unwrap().iter().all(|(_, a)| a.is_empty()));
    }

    #[test]
    fn duplicate_environment_ids_are_flagged_not_merged() {
        let mk = |id: &str, env: Option<&str>| {
            ServerSnapshot::new(
                id,
                id,
                AgentSnapshot {
                    environment_id: env.map(str::to_string),
                    ..AgentSnapshot::default()
                },
            )
        };
        let servers = vec![
            mk("primary", Some("e1")),
            mk("b", Some("e1")),
            mk("c", Some("e2")),
            mk("d", None),
        ];
        let dup = duplicate_environments(&servers);
        let ids: Vec<&str> = dup.iter().map(|d| d.as_ref()).collect();
        assert_eq!(ids, ["primary", "b"]);
    }

    #[test]
    fn a_replaced_environment_resets_only_its_own_history() {
        let shared: Shared = Default::default();
        let id = sid();
        let remote = |env: &str, threads: Vec<Value>, seq| {
            let mut r = parse_shell(&shell(threads, seq)).unwrap();
            r.environment_id = Some(env.into());
            r
        };
        install_snapshot(
            &shared,
            &id,
            remote("e1", vec![mock_thread("a", "running", "now")], 1),
            false,
        );
        install_snapshot(
            &shared,
            &id,
            remote("e1", vec![mock_thread("a", "waiting", "now")], 2),
            false,
        );
        assert_eq!(shared.read().unwrap().changes.len(), 1);
        // 同じ environment_id なら履歴を引き継ぐ。変わったら、変更通知を出さず世代を進める。
        let generation = shared.read().unwrap().resync_generation;
        install_snapshot(
            &shared,
            &id,
            remote("e2", vec![mock_thread("b", "running", "now")], 3),
            false,
        );
        let s = shared.read().unwrap();
        assert_eq!(s.resync_generation, generation + 1);
        assert!(s.changes.is_empty(), "別環境の差分を新着として出さない");
        assert_eq!(s.entries[0].thread_id, "b");
        assert_eq!(&*s.entries[0].server_id, "primary");
    }
}
