//! `miharashi agents-check`: 画面を使わず、実際の worker(`agents::worker`)で T3 への接続を確かめる診断コマンド。
//!
//! 用途:
//! - token の配送・更新のあと、置いた token_file を **worker と同じ読み方・同じ取得処理** で使えるかを確かめる
//!   (`systemd/miharashi-t3-remote-token` が、古いセッションを失効させる前の最後の確認に使う)。
//! - 複数の接続を同時に動かし、片方の失敗・復帰が他方に影響しないことを実サーバーで確かめる。
//!
//! 画面・DRM・パネル・管理ページは起動しない(ディスプレイを占有しない)。起動中の miharashi とは別プロセスで、
//! 起動中の worker の状態は変えない。token の値は読むだけで、出力・ログに出さない。
//!
//! ```text
//! miharashi agents-check [--config FILE] [--server ID]... [--expect-environment SERVER=ENVIRONMENT_ID]...
//!                        [--timeout SECS] [--watch SECS]
//! ```
//! - `--server` を省略すると設定の全接続(`primary` と `extra_servers`)を動かす。繰り返して選べる。
//! - `--expect-environment`: その接続の environmentId が違えば失敗(接続先の取り違え)。
//! - 選んだ全接続が「接続中(LIVE)かつ 1 回取得できた」状態になるまで `--timeout` 秒(既定 60)待つ。
//!   全部そうなれば 0。タイムアウトなら 1(状態ごとの理由を stderr に出す)。引数・設定の誤りは 2。
//! - `--watch SECS`: 成功のあと SECS 秒そのまま動かし、状態が変わるたびに 1 行出す(片側停止・復帰の観察用)。
//!   終了コードは最初の成功で決まる。
//!
//! 出力は 1 行ずつ: `agents-check: t=<秒> server=<id> state=<状態> synchronized=<bool> threads=<件数> env=<ok|mismatch|unknown> [error=<要約>]`。

use crate::agents::{self, AgentConnection, AgentSnapshot, Connection};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const USAGE: &str = "使い方: miharashi agents-check [--config FILE] [--server ID]... [--expect-environment SERVER=ENVIRONMENT_ID]... [--timeout SECS] [--watch SECS]";

#[derive(Debug, PartialEq)]
struct Args {
    config: Option<std::path::PathBuf>,
    servers: Vec<String>,
    expect_env: HashMap<String, String>,
    timeout: u64,
    watch: u64,
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut out = Args {
        config: None,
        servers: Vec::new(),
        expect_env: HashMap::new(),
        timeout: 60,
        watch: 0,
    };
    let mut it = args.into_iter();
    while let Some(k) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} に値が要ります"));
        match k.as_str() {
            "--config" => out.config = Some(val("--config")?.into()),
            "--server" => out.servers.push(val("--server")?),
            "--expect-environment" => {
                let v = val("--expect-environment")?;
                let (server, env) = v
                    .split_once('=')
                    .filter(|(s, e)| !s.is_empty() && !e.is_empty())
                    .ok_or("--expect-environment は SERVER=ENVIRONMENT_ID の形")?;
                if out
                    .expect_env
                    .insert(server.to_string(), env.to_string())
                    .is_some()
                {
                    return Err(format!("--expect-environment {server} が重複しています"));
                }
            }
            "--timeout" => {
                out.timeout = val("--timeout")?
                    .parse::<u64>()
                    .ok()
                    .filter(|s| (1..=3600).contains(s))
                    .ok_or("--timeout は 1〜3600 秒")?;
            }
            "--watch" => {
                out.watch = val("--watch")?
                    .parse::<u64>()
                    .ok()
                    .filter(|s| *s <= 86_400)
                    .ok_or("--watch は 0〜86400 秒")?;
            }
            other => return Err(format!("引数が不正です: {other}")),
        }
    }
    Ok(out)
}

/// 1 接続の判定。`env` は `ok`(期待どおり、または期待なしで取得済み)/ `mismatch` / `unknown`(まだ分からない)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Verdict {
    state: &'static str,
    synchronized: bool,
    threads: usize,
    env: &'static str,
    error: Option<String>,
}

fn state_name(c: AgentConnection) -> &'static str {
    match c {
        AgentConnection::Connecting => "connecting",
        AgentConnection::Live => "live",
        AgentConnection::Retrying => "retrying",
        AgentConnection::AuthenticationRequired => "auth-required",
        AgentConnection::Forbidden => "forbidden",
        AgentConnection::ApiError => "api-error",
    }
}

fn verdict(s: &AgentSnapshot, expected_env: Option<&str>) -> Verdict {
    let env = match (expected_env, s.environment_id.as_deref()) {
        (_, None) => "unknown",
        (None, Some(_)) => "ok",
        (Some(want), Some(got)) if want == got => "ok",
        (Some(_), Some(_)) => "mismatch",
    };
    Verdict {
        state: state_name(s.connection),
        synchronized: s.synchronized,
        threads: s.entries.len(),
        env,
        error: s.error_summary.clone(),
    }
}

impl Verdict {
    /// 取得に成功している(接続中・1 回以上同期・接続先が期待どおり)
    fn healthy(&self) -> bool {
        self.state == "live" && self.synchronized && self.env == "ok"
    }
}

fn line(t: u64, server: &str, v: &Verdict) -> String {
    let mut out = format!(
        "agents-check: t={t} server={server} state={} synchronized={} threads={} env={}",
        v.state, v.synchronized, v.threads, v.env
    );
    if let Some(e) = &v.error {
        // 1 行に収める(エラー要約に認証情報は含まれない)
        let flat: String = e
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(160)
            .collect();
        out.push_str(&format!(" error={flat}"));
    }
    out
}

pub fn run(args: Vec<String>) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("agents-check: {e}\n{USAGE}");
            return 2;
        }
    };
    let loaded = match crate::config::Config::discover_with_overlay(args.config.as_deref(), false) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agents-check: 設定を読めません: {e}");
            return 2;
        }
    };
    let agents_cfg = &loaded.effective.agents;
    let known: Vec<String> = agents_cfg.servers().into_iter().map(|s| s.id).collect();
    for id in args.servers.iter().chain(args.expect_env.keys()) {
        if !known.contains(id) {
            eprintln!("agents-check: 設定に接続 {id:?} がありません\n{USAGE}");
            return 2;
        }
    }
    let only = (!args.servers.is_empty()).then_some(args.servers.as_slice());
    let connections: Vec<Connection> = agents::start_selected(agents_cfg, only);
    if connections.is_empty() {
        eprintln!("agents-check: 動かす接続がありません");
        return 2;
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.timeout);
    let mut last: Vec<Option<Verdict>> = vec![None; connections.len()];
    let mut ever_healthy = vec![false; connections.len()];
    let mut succeeded_at: Option<Instant> = None;
    let mut code = 1;
    loop {
        for (i, c) in connections.iter().enumerate() {
            let snap = c.shared.read().map(|s| s.clone()).unwrap_or_default();
            let v = verdict(
                &snap,
                args.expect_env
                    .get(c.server_id.as_ref())
                    .map(String::as_str),
            );
            if v.healthy() {
                ever_healthy[i] = true;
            }
            if last[i].as_ref() != Some(&v) {
                println!("{}", line(started.elapsed().as_secs(), &c.server_id, &v));
                last[i] = Some(v);
            }
        }
        if succeeded_at.is_none()
            && last.iter().flatten().all(Verdict::healthy)
            && last.iter().all(Option::is_some)
        {
            succeeded_at = Some(Instant::now());
            code = 0;
            println!(
                "agents-check: ok: {} 件の接続すべてで取得に成功しました",
                connections.len()
            );
            if args.watch == 0 {
                return 0;
            }
        }
        let end = match succeeded_at {
            Some(t) => t + Duration::from_secs(args.watch),
            None => deadline,
        };
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if code != 0 {
        for (i, c) in connections.iter().enumerate() {
            if !ever_healthy[i] {
                let why = last[i]
                    .as_ref()
                    .map(|v| format!("state={} env={}", v.state, v.env))
                    .unwrap_or_default();
                eprintln!(
                    "agents-check: 失敗: 接続 {} が時間内に取得できませんでした({why})",
                    c.server_id
                );
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_arguments() {
        let p = parse(a(&[
            "--config",
            "c.toml",
            "--server",
            "x",
            "--server",
            "primary",
            "--expect-environment",
            "x=env-1",
            "--timeout",
            "30",
            "--watch",
            "5",
        ]))
        .unwrap();
        assert_eq!(p.servers, ["x", "primary"]);
        assert_eq!(p.expect_env["x"], "env-1");
        assert_eq!((p.timeout, p.watch), (30, 5));
        assert_eq!(parse(a(&[])).unwrap().timeout, 60);
    }

    #[test]
    fn rejects_bad_arguments() {
        for bad in [
            a(&["--timeout", "0"]),
            a(&["--timeout", "x"]),
            a(&["--watch", "99999999"]),
            a(&["--expect-environment", "noequals"]),
            a(&["--expect-environment", "=e"]),
            a(&["--expect-environment", "s=e", "--expect-environment", "s=f"]),
            a(&["--server"]),
            a(&["--bogus"]),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn verdict_requires_live_synchronized_and_expected_environment() {
        let mut s = AgentSnapshot {
            connection: AgentConnection::Live,
            synchronized: true,
            environment_id: Some("env-1".into()),
            ..Default::default()
        };
        assert!(verdict(&s, Some("env-1")).healthy());
        assert!(verdict(&s, None).healthy());
        assert_eq!(verdict(&s, Some("env-2")).env, "mismatch");
        assert!(!verdict(&s, Some("env-2")).healthy());
        s.synchronized = false;
        assert!(!verdict(&s, None).healthy(), "同期前は成功にしない");
        s.synchronized = true;
        s.connection = AgentConnection::AuthenticationRequired;
        assert!(!verdict(&s, None).healthy());
        assert_eq!(verdict(&s, None).state, "auth-required");
        s.connection = AgentConnection::Live;
        s.environment_id = None;
        assert!(
            !verdict(&s, None).healthy(),
            "接続先が分からないうちは成功にしない"
        );
    }

    #[test]
    fn output_line_is_single_line_and_has_no_secrets_shape() {
        let v = Verdict {
            state: "retrying",
            synchronized: true,
            threads: 3,
            env: "ok",
            error: Some("a\nb".into()),
        };
        let l = line(7, "container", &v);
        assert!(!l.contains('\n'));
        assert!(l.starts_with("agents-check: t=7 server=container state=retrying"));
    }
}
