//! T3 の閲覧用 shell snapshot を取得し、画面へ渡す。
//! HTTP・認証ファイル・JSON 解析は専用 worker に閉じ込める。
use crate::config::ConfigAgents;
use crate::screen::{Provider, World};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const BODY_LIMIT: u64 = 4 * 1024 * 1024;
const THREAD_LIMIT: usize = 2048;
const ENTRY_TEXT_LIMIT: usize = 512;
const CHANGE_LIMIT: usize = 64;

pub type Shared = Arc<RwLock<AgentSnapshot>>;

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
    Progress,
    Title,
    Error,
    TurnCompleted,
    Resumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanProgress {
    pub step: Option<String>,
    pub completed_steps: Option<u32>,
    pub total_steps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentEntry {
    pub thread_id: String,
    pub project_id: String,
    pub project_title: String,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub state: AgentState,
    pub raw_session_status: Option<String>,
    pub raw_turn_state: Option<String>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    pub has_actionable_proposed_plan: bool,
    pub background_liveness: Option<String>,
    pub active_turn_id: Option<String>,
    pub latest_turn_id: Option<String>,
    pub plan_progress: Option<PlanProgress>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub updated_at: Option<String>,
    pub requested_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChange {
    pub id: u64,
    pub thread_id: String,
    pub kind: AgentChangeKind,
    pub before_state: Option<AgentState>,
    pub after_state: Option<AgentState>,
    pub before_progress: Option<PlanProgress>,
    pub after_progress: Option<PlanProgress>,
    pub before_title: Option<String>,
    pub after_title: Option<String>,
    pub observed_at: SystemTime,
}

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

pub struct AgentProvider(Shared);

impl AgentProvider {
    pub fn new(shared: Shared) -> Self {
        Self(shared)
    }
}

impl Provider for AgentProvider {
    fn update(&mut self, world: &mut World) {
        if let Ok(snapshot) = self.0.try_read() {
            world.agents = snapshot.clone();
        }
    }
}

/// `enabled` のときだけ呼び出す。返した共有値は Provider と screen で使える。
pub fn start(cfg: &ConfigAgents) -> Shared {
    let shared = Arc::new(RwLock::new(AgentSnapshot::default()));
    let thread_shared = shared.clone();
    let settings = cfg.clone();
    if let Err(e) = thread::Builder::new()
        .name("t3-agents".into())
        .spawn(move || worker(settings, thread_shared))
    {
        set_connection(
            &shared,
            AgentConnection::Retrying,
            Some(format!("worker を起動できない: {e}")),
        );
    }
    shared
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

fn worker(cfg: ConfigAgents, shared: Shared) {
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
                install_snapshot(&shared, remote, is_resync);
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

fn install_snapshot(shared: &Shared, remote: RemoteSnapshot, resync: bool) {
    let now_wall = SystemTime::now();
    let now_mono = Instant::now();
    let (old_entries, old_changes, synchronized, mut cursor) = match shared.read() {
        Ok(current) => (
            current.entries.clone(),
            current.changes.clone(),
            current.synchronized,
            current.change_cursor,
        ),
        Err(_) => return,
    };
    let sync_reset = synchronized && resync;
    let changes = if synchronized && !resync {
        let mut history = old_changes.to_vec();
        history.extend(diff(&old_entries, &remote.entries, &mut cursor, now_wall));
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
    if protocol != Some(1) {
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
        let session = thread.get("session").filter(|v| v.is_object());
        let latest = thread.get("latestTurn").filter(|v| v.is_object());
        let selection = thread.get("modelSelection").filter(|v| v.is_object());
        let raw_session_status = nested_str(session, "status");
        let raw_turn_state = nested_str(latest, "state");
        let active_turn_id = nested_str(session, "activeTurnId");
        let latest_turn_id = nested_str(latest, "id").or_else(|| nested_str(latest, "turnId"));
        let approval = bool_field(thread, "hasPendingApprovals");
        let input = bool_field(thread, "hasPendingUserInput");
        let plan = bool_field(thread, "hasActionableProposedPlan");
        let background_liveness = field_str(thread, "backgroundLiveness").map(|s| sanitize(s, 64));
        let state = normalize_state(
            raw_session_status.as_deref(),
            raw_turn_state.as_deref(),
            active_turn_id.as_deref(),
            latest_turn_id.as_deref(),
            approval,
            input,
            plan,
            background_liveness.as_deref(),
        );
        let archived = thread.get("archivedAt").is_some_and(|v| !v.is_null());
        let updated = field_str(thread, "updatedAt");
        if archived || !is_target(state, updated, recent_hours, now) {
            continue;
        }
        let progress = thread.get("planProgress").and_then(parse_progress);
        let project_title = project_names
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| short_id(project_id));
        let error_summary = nested_str(session, "lastError")
            .map(|s| sanitize(&s, 240))
            .filter(|s| !s.is_empty());
        entries.push(AgentEntry {
            thread_id: sanitize(id, 256),
            project_id: sanitize(project_id, 256),
            project_title,
            title: field_str(thread, "title").map_or_else(
                || "無題".into(),
                |v| nonempty(sanitize(v, ENTRY_TEXT_LIMIT), "無題"),
            ),
            provider: nested_str(session, "providerName")
                .or_else(|| nested_str(selection, "instanceId"))
                .map(|s| sanitize(&s, 128)),
            model: nested_str(selection, "model").map(|s| sanitize(&s, 128)),
            state,
            raw_session_status: raw_session_status.map(|s| sanitize(&s, 64)),
            raw_turn_state: raw_turn_state.map(|s| sanitize(&s, 64)),
            has_pending_approvals: approval,
            has_pending_user_input: input,
            has_actionable_proposed_plan: plan,
            background_liveness,
            active_turn_id: active_turn_id.map(|s| sanitize(&s, 256)),
            latest_turn_id: latest_turn_id.map(|s| sanitize(&s, 256)),
            plan_progress: progress,
            branch: field_str(thread, "branch").map(|s| sanitize(s, 256)),
            worktree_path: field_str(thread, "worktreePath").map(|s| sanitize(s, 512)),
            updated_at: field_str(thread, "updatedAt").map(|s| sanitize(s, 64)),
            requested_at: nested_str(latest, "requestedAt").map(|s| sanitize(&s, 64)),
            started_at: nested_str(latest, "startedAt").map(|s| sanitize(&s, 64)),
            completed_at: nested_str(latest, "completedAt").map(|s| sanitize(&s, 64)),
            error_summary,
        });
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

fn is_target(
    state: AgentState,
    updated_at: Option<&str>,
    recent_hours: u64,
    now: SystemTime,
) -> bool {
    if matches!(
        state,
        AgentState::Starting
            | AgentState::Running
            | AgentState::Monitoring
            | AgentState::Approval
            | AgentState::Input
            | AgentState::Plan
            | AgentState::Error
    ) {
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
    for key in [
        "title",
        "projectId",
        "branch",
        "worktreePath",
        "updatedAt",
        "backgroundLiveness",
    ] {
        validate_string_or_null(thread, key)?;
    }
    for key in [
        "hasPendingApprovals",
        "hasPendingUserInput",
        "hasActionableProposedPlan",
    ] {
        if let Some(value) = thread.get(key)
            && !value.is_boolean()
        {
            return Err(format!("thread.{key} が boolean ではありません"));
        }
    }
    for key in ["session", "latestTurn", "modelSelection", "planProgress"] {
        if let Some(value) = thread.get(key)
            && !value.is_null()
            && !value.is_object()
        {
            return Err(format!("thread.{key} が object または null ではありません"));
        }
    }
    for (parent, fields) in [
        (
            "session",
            &["status", "providerName", "activeTurnId", "lastError"][..],
        ),
        (
            "latestTurn",
            &[
                "id",
                "turnId",
                "state",
                "requestedAt",
                "startedAt",
                "completedAt",
            ][..],
        ),
        ("modelSelection", &["instanceId", "model"][..]),
        ("planProgress", &["step"][..]),
    ] {
        let Some(value) = thread.get(parent).filter(|v| v.is_object()) else {
            continue;
        };
        for field in fields {
            validate_string_or_null(value, field)
                .map_err(|e| format!("thread.{parent}.{field}: {e}"))?;
        }
    }
    if let Some(progress) = thread.get("planProgress").filter(|v| v.is_object()) {
        for key in ["completedSteps", "totalSteps"] {
            if let Some(value) = progress.get(key)
                && !value.is_null()
                && value.as_u64().is_none()
            {
                return Err(format!(
                    "thread.planProgress.{key} が非負整数ではありません"
                ));
            }
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

fn parse_progress(value: &Value) -> Option<PlanProgress> {
    if !value.is_object() || value.as_object().is_some_and(|obj| obj.is_empty()) {
        return None;
    }
    let completed = value
        .get("completedSteps")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let total = value
        .get("totalSteps")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    if let (Some(c), Some(t)) = (completed, total)
        && (t == 0 || c > t)
    {
        return None;
    }
    Some(PlanProgress {
        step: field_str(value, "step").map(|s| sanitize(s, 256)),
        completed_steps: completed,
        total_steps: total,
    })
}

// Server status dimensions are independent and their precedence is intentionally explicit.
#[allow(clippy::too_many_arguments)]
fn normalize_state(
    session: Option<&str>,
    turn: Option<&str>,
    active: Option<&str>,
    latest: Option<&str>,
    approval: bool,
    input: bool,
    plan: bool,
    background: Option<&str>,
) -> AgentState {
    if approval {
        return AgentState::Approval;
    }
    if input {
        return AgentState::Input;
    }
    if plan {
        return AgentState::Plan;
    }
    if session.is_some_and(|v| {
        !matches!(
            v,
            "idle" | "starting" | "running" | "ready" | "interrupted" | "stopped" | "error"
        )
    }) || turn.is_some_and(|v| !matches!(v, "running" | "interrupted" | "completed" | "error"))
        || background.is_some_and(|v| !matches!(v, "working" | "monitoring"))
    {
        return AgentState::Unknown;
    }
    let newer_active = active.is_some() && latest.is_some() && active != latest;
    if session == Some("error") && !newer_active || turn == Some("error") && !newer_active {
        return AgentState::Error;
    }
    if session == Some("starting") {
        return AgentState::Starting;
    }
    if background == Some("monitoring") {
        return AgentState::Monitoring;
    }
    if session == Some("running") || active.is_some() || background == Some("working") {
        return AgentState::Running;
    }
    if session == Some("interrupted") || turn == Some("interrupted") {
        return AgentState::Interrupted;
    }
    if session == Some("stopped") {
        return AgentState::Stopped;
    }
    if turn == Some("completed") {
        return AgentState::Completed;
    }
    if matches!(session, Some("idle" | "ready")) {
        return AgentState::Idle;
    }
    AgentState::Unknown
}

fn diff(
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
            push_change(&mut result, cursor, now, entry, AgentChangeKind::New, None);
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
        } else if previous.error_summary != entry.error_summary {
            Some(AgentChangeKind::Error)
        } else if previous.active_turn_id != entry.active_turn_id
            || previous.raw_session_status != entry.raw_session_status
            || previous.raw_turn_state != entry.raw_turn_state
        {
            Some(AgentChangeKind::Resumed)
        } else if previous.has_pending_approvals != entry.has_pending_approvals
            || previous.has_pending_user_input != entry.has_pending_user_input
            || previous.has_actionable_proposed_plan != entry.has_actionable_proposed_plan
        {
            Some(AgentChangeKind::Flags)
        } else if previous.plan_progress != entry.plan_progress {
            Some(AgentChangeKind::Progress)
        } else if previous.title != entry.title {
            Some(AgentChangeKind::Title)
        } else {
            None
        };
        if let Some(kind) = kind {
            push_change(&mut result, cursor, now, entry, kind, Some(previous));
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
    cursor: &mut u64,
    now: SystemTime,
    entry: &AgentEntry,
    kind: AgentChangeKind,
    previous: Option<&AgentEntry>,
) {
    *cursor = cursor.saturating_add(1);
    out.push(AgentChange {
        id: *cursor,
        thread_id: entry.thread_id.clone(),
        kind,
        before_state: previous.map(|e| e.state),
        after_state: Some(entry.state),
        before_progress: previous.and_then(|e| e.plan_progress.clone()),
        after_progress: entry.plan_progress.clone(),
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

fn read_token(path: &str) -> Result<String, String> {
    let path_ref = Path::new(path);
    let metadata =
        fs::metadata(path_ref).map_err(|e| format!("T3 token file を確認できません: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode() & 0o777;
        let safe_owner = mode == 0o600 && metadata.uid() == effective_uid()
            || mode == 0o640 && metadata.uid() == 0 && metadata.gid() == effective_gid();
        if !safe_owner || !matches!(mode, 0o600 | 0o640) {
            return Err("T3 token file は実行ユーザーの 0600、または root:実行グループの 0640 にしてください".into());
        }
    }
    let text =
        fs::read_to_string(path_ref).map_err(|e| format!("T3 token file を読めません: {e}"))?;
    let token = text.trim();
    if token.is_empty() || token.contains(['\r', '\n']) || token.len() > 8192 {
        return Err("T3 token file の形式が不正です".into());
    }
    Ok(token.to_string())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}
#[cfg(unix)]
fn effective_gid() -> u32 {
    unsafe extern "C" {
        fn getegid() -> u32;
    }
    unsafe { getegid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};

    fn shell(threads: Vec<Value>, sequence: u64) -> Value {
        serde_json::json!({
            "snapshotSequence": sequence,
            "environment": {"id":"env-1", "label":"test T3", "serverVersion":"0.0.45"},
            "projects":[{"id":"p1", "title":"Project"}], "threads":threads
        })
    }

    fn mock_thread(id: &str, status: &str, updated: &str) -> Value {
        let updated = if DateTime::parse_from_rfc3339(updated).is_ok() {
            updated.to_owned()
        } else {
            Utc::now().to_rfc3339()
        };
        serde_json::json!({
            "id":id, "projectId":"p1", "title":"Thread", "updatedAt":updated,
            "session":{"status":status,"providerName":"Codex","activeTurnId":null,"lastError":null},
            "latestTurn":{"id":"turn-1","state":"running","requestedAt":"2026-10-15T00:00:00Z"},
            "modelSelection":{"model":"gpt"}, "hasPendingApprovals":false,
            "hasPendingUserInput":false,"hasActionableProposedPlan":false,
            "planProgress":{"step":"step 1","completedSteps":1,"totalSteps":3}
        })
    }

    #[test]
    fn maps_schema_prioritizes_flags_and_sanitizes_strings() {
        let value = shell(vec![mock_thread("t1", "running", "today")], 42);
        let parsed = parse_shell(&value).unwrap();
        assert_eq!(parsed.sequence, Some(42));
        let e = &parsed.entries[0];
        assert_eq!(e.project_title, "Project");
        assert_eq!(e.state, AgentState::Running);
        assert_eq!(e.provider.as_deref(), Some("Codex"));
        assert_eq!(e.plan_progress.as_ref().unwrap().total_steps, Some(3));

        let mut approval = mock_thread("t1", "running", "today");
        approval["hasPendingApprovals"] = Value::Bool(true);
        assert_eq!(
            parse_shell(&shell(vec![approval], 43)).unwrap().entries[0].state,
            AgentState::Approval
        );
    }

    #[test]
    fn semantic_diff_ignores_updated_at_and_repeated_snapshot() {
        let first = parse_shell(&shell(vec![mock_thread("t1", "running", "a")], 1)).unwrap();
        let mut cursor = 0;
        let initial = diff(&[], &first.entries, &mut cursor, SystemTime::now());
        assert_eq!(initial[0].kind, AgentChangeKind::New);
        let second = parse_shell(&shell(vec![mock_thread("t1", "running", "b")], 2)).unwrap();
        assert!(
            diff(
                &first.entries,
                &second.entries,
                &mut cursor,
                SystemTime::now()
            )
            .is_empty()
        );
        let mut completed = mock_thread("t1", "ready", "c");
        completed["latestTurn"]["state"] = Value::String("completed".into());
        let third = parse_shell(&shell(vec![completed], 3)).unwrap();
        assert_eq!(
            diff(
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
    fn active_turn_change_is_reported_even_when_running_state_is_unchanged() {
        let a = AgentEntry {
            thread_id: "t".into(),
            state: AgentState::Running,
            active_turn_id: Some("turn-a".into()),
            ..AgentEntry::default()
        };
        let mut b = a.clone();
        b.active_turn_id = Some("turn-b".into());
        let mut cursor = 0;
        let changes = diff(
            std::slice::from_ref(&a),
            &[b],
            &mut cursor,
            SystemTime::now(),
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, AgentChangeKind::Resumed);
    }

    #[test]
    fn archived_and_old_settled_threads_are_filtered_but_attention_items_remain() {
        let now = SystemTime::now();
        let recent = DateTime::<Utc>::from(now).to_rfc3339();
        let old = DateTime::<Utc>::from(now - Duration::from_secs(48 * 3600)).to_rfc3339();
        let mut archived = mock_thread("archived", "running", &recent);
        archived["archivedAt"] = Value::String(recent.clone());
        let mut old_running = mock_thread("running", "running", &old);
        old_running["session"]["activeTurnId"] = Value::String("turn-1".into());
        let old_idle = mock_thread("idle-old", "idle", &old);
        let recent_idle = mock_thread("idle-recent", "idle", &recent);
        let parsed = parse_shell_recent(
            &shell(vec![archived, old_running, old_idle, recent_idle], 1),
            24,
            now,
        )
        .unwrap();
        let ids: Vec<_> = parsed
            .entries
            .iter()
            .map(|e| e.thread_id.as_str())
            .collect();
        assert_eq!(ids, ["idle-recent", "running"]);
    }

    #[test]
    fn malformed_ids_and_unknown_enums_are_not_silently_accepted() {
        let duplicate = mock_thread("same", "running", "now");
        assert!(parse_shell(&shell(vec![duplicate.clone(), duplicate], 1)).is_err());
        let mut unknown = mock_thread("unknown", "mystery", "now");
        unknown["latestTurn"]["state"] = Value::String("completed".into());
        assert_eq!(
            parse_shell(&shell(vec![unknown], 1)).unwrap().entries[0].state,
            AgentState::Unknown
        );
        assert!(parse_progress(&serde_json::json!({})).is_none());
        assert!(parse_progress(&Value::Null).is_none());
        assert!(parse_progress(&serde_json::json!({"completedSteps":3,"totalSteps":0})).is_none());
        let mut malformed = mock_thread("bad", "running", "now");
        malformed["session"] = Value::String("partial parse".into());
        assert!(parse_shell(&shell(vec![malformed], 1)).is_err());
        let mut malformed_flag = mock_thread("bad-flag", "running", "now");
        malformed_flag["hasPendingApprovals"] = Value::String("false".into());
        assert!(parse_shell(&shell(vec![malformed_flag], 1)).is_err());
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
        install_snapshot(&shared, remote, false);
        assert!(shared.read().unwrap().changes.is_empty());
        let remote = parse_shell(&shell(
            vec![
                mock_thread("t1", "ready", "b"),
                mock_thread("t2", "idle", "b"),
            ],
            2,
        ))
        .unwrap();
        install_snapshot(&shared, remote, true);
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
            let body = r#"{"environmentId":"env-a","label":"T3 host","serverVersion":"0.0.45","orchestrationProtocolVersion":1}"#;
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
        assert_eq!(environment.protocol_version, Some(1));
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
}
