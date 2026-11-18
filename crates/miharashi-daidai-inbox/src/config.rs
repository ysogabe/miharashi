//! コマンドライン引数と、起動時に決める設定。
//!
//! `miharashi-daidai-inbox --listen <IP:PORT> --token-file <path> --feed <path> --allow <IP> [--allow <IP>...]`

use crate::ratelimit::{DEFAULT_BURST, DEFAULT_PER_MINUTE};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

pub const DEFAULT_FEED: &str = "/var/lib/miharashi/daidai-feed.json";

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub feed: PathBuf,
    /// 送信元 IP の許可リスト(1 つ以上)
    pub allow: Vec<IpAddr>,
    /// レート制限(送信元 IP ごと)。試験で緩めるため設定にしてある
    pub burst: u32,
    pub per_minute: u32,
}

pub const USAGE: &str = "使い方: miharashi-daidai-inbox --listen <IP:PORT> --token-file <path> [--feed <path>] --allow <IP> [--allow <IP>...]
  --listen      待ち受ける具体的な IP とポート(0.0.0.0 / :: は拒否)
  --token-file  トークンのファイル(実行ユーザー所有の 0600、32 文字以上の 1 行)
  --feed        書き込むフィード(既定 /var/lib/miharashi/daidai-feed.json)
  --allow       許可する送信元 IP(1 つ以上、繰り返し可)";

/// IPv4 射影の IPv6 アドレス(`::ffff:a.b.c.d`)は IPv4 として扱う。
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

impl Config {
    pub fn new(listen: SocketAddr, token_file: PathBuf, feed: PathBuf, allow: Vec<IpAddr>) -> Self {
        Self {
            listen,
            token_file,
            feed,
            allow,
            burst: DEFAULT_BURST,
            per_minute: DEFAULT_PER_MINUTE,
        }
    }

    /// 起動してよい設定か。インターネットや LAN 全体に出さないため、未指定アドレスでの待ち受けは拒否する。
    pub fn validate(&self) -> Result<(), String> {
        if normalize_ip(self.listen.ip()).is_unspecified() {
            return Err(
                "--listen には具体的な IP を指定してください(0.0.0.0 / :: は使えません)".into(),
            );
        }
        if self.allow.is_empty() {
            return Err("--allow を 1 つ以上指定してください".into());
        }
        if self
            .allow
            .iter()
            .any(|ip| normalize_ip(*ip).is_unspecified())
        {
            return Err("--allow に未指定アドレス(0.0.0.0 / ::)は指定できません".into());
        }
        Ok(())
    }
}

/// 引数(実行ファイル名を除く)を読む。`--help` のときは `Ok(None)`。
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Option<Config>, String> {
    let mut listen = None;
    let mut token_file = None;
    let mut feed = None;
    let mut allow = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(None);
        }
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} に値がありません"));
        match arg.as_str() {
            "--listen" => {
                let v = value("--listen")?;
                listen = Some(
                    v.parse::<SocketAddr>()
                        .map_err(|_| "--listen は IP:PORT の形で指定してください".to_string())?,
                );
            }
            "--token-file" => token_file = Some(PathBuf::from(value("--token-file")?)),
            "--feed" => feed = Some(PathBuf::from(value("--feed")?)),
            "--allow" => {
                let v = value("--allow")?;
                allow.push(
                    v.parse::<IpAddr>()
                        .map_err(|_| "--allow は IP アドレスで指定してください".to_string())?,
                );
            }
            _ => return Err("知らない引数があります(--help で使い方)".into()),
        }
    }
    let cfg = Config::new(
        listen.ok_or("--listen が必要です")?,
        token_file.ok_or("--token-file が必要です")?,
        feed.unwrap_or_else(|| PathBuf::from(DEFAULT_FEED)),
        allow,
    );
    cfg.validate()?;
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_a_full_command_line() {
        let c = parse_args(args(
            "--listen 100.64.0.1:8097 --token-file /t --feed /f --allow 100.64.0.2 --allow fd7a::2",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(c.listen, "100.64.0.1:8097".parse().unwrap());
        assert_eq!(c.token_file, PathBuf::from("/t"));
        assert_eq!(c.feed, PathBuf::from("/f"));
        assert_eq!(c.allow.len(), 2);
        let d = parse_args(args(
            "--listen 127.0.0.1:1 --token-file /t --allow 127.0.0.1",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(d.feed, PathBuf::from(DEFAULT_FEED));
    }

    #[test]
    fn refuses_unspecified_listen_addresses() {
        for l in ["0.0.0.0:8097", "[::]:8097", "[::ffff:0.0.0.0]:8097"] {
            let r = parse_args(args(&format!(
                "--listen {l} --token-file /t --allow 127.0.0.1"
            )));
            assert!(r.is_err(), "{l}");
        }
        // 引数なしの --listen も、アドレスだけ(ポートなし)も拒否
        assert!(parse_args(args("--listen --token-file /t --allow 127.0.0.1")).is_err());
        assert!(
            parse_args(args(
                "--listen 100.64.0.1 --token-file /t --allow 127.0.0.1"
            ))
            .is_err()
        );
        assert!(
            Config::new(
                "0.0.0.0:1".parse().unwrap(),
                "/t".into(),
                "/f".into(),
                vec!["127.0.0.1".parse().unwrap()]
            )
            .validate()
            .is_err()
        );
    }

    #[test]
    fn requires_listen_token_and_at_least_one_allow() {
        assert!(parse_args(args("--token-file /t --allow 127.0.0.1")).is_err());
        assert!(parse_args(args("--listen 127.0.0.1:1 --allow 127.0.0.1")).is_err());
        assert!(parse_args(args("--listen 127.0.0.1:1 --token-file /t")).is_err());
        assert!(parse_args(args("--listen 127.0.0.1:1 --token-file /t --allow 0.0.0.0")).is_err());
        assert!(parse_args(args("--listen 127.0.0.1:1 --token-file /t --allow nope")).is_err());
        assert!(parse_args(args("--bogus")).is_err());
        assert!(parse_args(args("--help")).unwrap().is_none());
    }

    #[test]
    fn ipv4_mapped_addresses_are_normalized() {
        let m: IpAddr = "::ffff:100.64.0.2".parse().unwrap();
        assert_eq!(normalize_ip(m), "100.64.0.2".parse::<IpAddr>().unwrap());
        let v6: IpAddr = "fd7a::2".parse().unwrap();
        assert_eq!(normalize_ip(v6), v6);
    }
}
