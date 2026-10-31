//! 管理ページの `feeds` と `tokens`(docs/admin-web/design.md の対応表)。取得スレッドが持つ状態を
//! 短いロックで写して `FeedReport` にする。取得の動きは変えない(読むだけ)。
use super::{AdminSources, FeedReport, FeedState, TokenState};
use crate::agents::{AgentConnection, AgentSnapshot};
use crate::aircraft::Health;
use crate::env::FeedStatus;
use crate::photos::Link;
use chrono::{DateTime, Local};

/// `error` の長さの上限(文字数)。
pub(super) const MAX_ERROR_CHARS: usize = 200;

/// 外部に出す文字列にする。URL のクエリ(`?` から空白や引用符まで)は落とし、200 文字で切る。
pub(super) fn clip_error(s: &str) -> String {
    let s = drop_url_queries(s);
    if s.chars().count() <= MAX_ERROR_CHARS {
        return s;
    }
    let mut t: String = s.chars().take(MAX_ERROR_CHARS).collect();
    t.push('…');
    t
}

/// URL のクエリの終わりとみなす文字(空白のほか、括弧・引用符・句点。全角を含む)。
const QUERY_END: [char; 18] = [
    '"', '\'', '<', '>', ')', ']', '）', '］', '」', '』', '】', '〉', '》', '“', '”', '。', '、',
    '（',
];

fn drop_url_queries(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut in_url, mut in_query) = (false, false);
    for c in s.chars() {
        if c.is_whitespace() || QUERY_END.contains(&c) {
            in_url = false;
            in_query = false;
        } else if in_url && !in_query && matches!(c, '?' | '#') {
            in_query = true;
        }
        if in_query {
            continue;
        }
        out.push(c);
        if !in_url && out.ends_with("://") {
            in_url = true;
        }
    }
    out
}

fn rfc3339(t: DateTime<Local>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn report(
    id: &'static str,
    label: &'static str,
    state: FeedState,
    last_ok: Option<DateTime<Local>>,
    error: Option<&str>,
) -> FeedReport {
    FeedReport {
        id,
        label,
        state,
        last_ok: last_ok.map(rfc3339),
        error: error.map(clip_error),
    }
}

/// 取得が動いていない(`AdminSources` を作る側が決めた理由を `error` に出す)。
pub(super) fn not_running(id: &'static str, label: &'static str, reason: &str) -> FeedReport {
    report(id, label, FeedState::NotRunning, None, Some(reason))
}

/// HA 室内・気象庁(予報・警報)。`failing` なら失敗、`last_ok` が無ければ初回前、それ以外は ok。
pub(super) fn from_feed_status(
    id: &'static str,
    label: &'static str,
    s: &FeedStatus,
) -> FeedReport {
    let (state, error) = if s.failing {
        (FeedState::Failing, s.error.as_deref())
    } else if s.last_ok.is_none() {
        (FeedState::Connecting, None)
    } else {
        (FeedState::Ok, None)
    };
    report(id, label, state, s.last_ok, error)
}

pub(super) fn from_link(l: &Link) -> FeedReport {
    use crate::photos::FeedStatus as Photo;
    let (state, error) = match &l.status {
        None => (FeedState::Connecting, None),
        Some(Photo::Online) => (FeedState::Ok, None),
        Some(Photo::Retrying { error, .. }) => (FeedState::Failing, Some(error.as_str())),
    };
    report("geoframe", "GEOFRAME(写真)", state, l.last_ok, error)
}

pub(super) fn from_aircraft(h: &Health) -> FeedReport {
    let (state, error) = match (&h.last_error, h.last_ok) {
        (Some((_, e)), _) => (FeedState::Failing, Some(e.as_str())),
        (None, Some(_)) => (FeedState::Ok, None),
        (None, None) => (FeedState::Connecting, None),
    };
    report("aircraft", "tar1090(航空機)", state, h.last_ok, error)
}

pub(super) fn from_agents(s: &AgentSnapshot) -> FeedReport {
    let state = match s.connection {
        AgentConnection::Connecting => FeedState::Connecting,
        AgentConnection::Live => FeedState::Ok,
        AgentConnection::Retrying | AgentConnection::ApiError => FeedState::Failing,
        AgentConnection::AuthenticationRequired => FeedState::AuthRequired,
        AgentConnection::Forbidden => FeedState::Forbidden,
    };
    let error = if state == FeedState::Ok {
        None
    } else {
        s.error_summary.as_deref()
    };
    report(
        "agents",
        "T3(エージェント)",
        state,
        s.last_success.map(DateTime::<Local>::from),
        error,
    )
}

impl AdminSources {
    /// 取得元ごとの状態。ロックは状態を写す間だけ(写してから変換する)。
    pub fn feeds(&self) -> Vec<FeedReport> {
        const HA: (&str, &str) = ("home_assistant", "Home Assistant(室内)");
        const WEATHER: (&str, &str) = ("weather", "気象庁(予報)");
        const DISASTER: (&str, &str) = ("disaster", "気象庁(警報)");
        vec![
            match &self.geoframe {
                Ok(l) => from_link(&l.lock().map(|g| g.clone()).unwrap_or_default()),
                Err(why) => not_running("geoframe", "GEOFRAME(写真)", why),
            },
            match &self.home_assistant {
                Ok(e) => from_feed_status(
                    HA.0,
                    HA.1,
                    &e.lock().map(|g| g.status.clone()).unwrap_or_default(),
                ),
                Err(why) => not_running(HA.0, HA.1, why),
            },
            match &self.weather {
                Ok(w) => from_feed_status(
                    WEATHER.0,
                    WEATHER.1,
                    &w.lock().map(|g| g.status.clone()).unwrap_or_default(),
                ),
                Err(why) => not_running(WEATHER.0, WEATHER.1, why),
            },
            match &self.disaster {
                Ok(d) => from_feed_status(
                    DISASTER.0,
                    DISASTER.1,
                    &d.lock().map(|g| g.status.clone()).unwrap_or_default(),
                ),
                Err(why) => not_running(DISASTER.0, DISASTER.1, why),
            },
            match &self.aircraft {
                Ok(h) => from_aircraft(&h.lock().map(|g| g.clone()).unwrap_or_default()),
                Err(why) => not_running("aircraft", "tar1090(航空機)", why),
            },
            match &self.agents {
                Ok(a) => from_agents(&a.read().map(|g| g.clone()).unwrap_or_default()),
                Err(why) => not_running("agents", "T3(エージェント)", why),
            },
        ]
    }
}

/// トークンファイルの状態。中身は読まず、`open` の成否と `metadata` だけを見る。
/// 規則は `agents::token_perms_ok` と同じ(`uid` / `gid` は実行ユーザーとグループ。テストでは偽の値を渡す):
/// 実行ユーザー所有の 0600、または root 所有でグループが実行グループの 0640。
/// 所有者が規則の外なら `WrongOwner`。所有者は規則内で権限が規則に合わない(その他に読み取りがある、
/// グループに読み取りがあるのに root 所有でない、など)なら `LoosePermissions`。
pub(super) fn check_token_file(path: &str, uid: u32, gid: u32) -> TokenState {
    use std::os::unix::fs::MetadataExt;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return TokenState::Missing,
        Err(_) => return TokenState::Unreadable,
    };
    let Ok(meta) = file.metadata() else {
        return TokenState::Unreadable;
    };
    if !meta.is_file() {
        return TokenState::Unreadable;
    }
    if crate::agents::token_perms_ok(meta.mode(), meta.uid(), meta.gid(), uid, gid) {
        return TokenState::Ok;
    }
    let owner_ok = meta.uid() == uid || (meta.uid() == 0 && meta.gid() == gid);
    if owner_ok {
        TokenState::LoosePermissions
    } else {
        TokenState::WrongOwner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photos::FeedStatus as Photo;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, SystemTime};

    fn at(s: &str) -> DateTime<Local> {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Local)
    }

    fn status(failing: bool, last_ok: Option<&str>, error: Option<&str>) -> FeedStatus {
        FeedStatus {
            last_ok: last_ok.map(at),
            failing,
            error: error.map(str::to_string),
            ..FeedStatus::default()
        }
    }

    #[test]
    fn ha_style_feed_maps_to_connecting_ok_and_failing() {
        let r = from_feed_status("weather", "気象庁(予報)", &status(false, None, None));
        assert_eq!(
            (r.state, r.last_ok, r.error),
            (FeedState::Connecting, None, None)
        );
        let r = from_feed_status(
            "weather",
            "x",
            &status(false, Some("2026-10-30T10:40:12+09:00"), None),
        );
        assert_eq!(r.state, FeedState::Ok);
        assert!(r.last_ok.is_some());
        // 失敗中は、前に取れた時刻を残したまま failing と理由
        let r = from_feed_status(
            "weather",
            "x",
            &status(
                true,
                Some("2026-10-30T10:20:00+09:00"),
                Some("接続できない"),
            ),
        );
        assert_eq!(r.state, FeedState::Failing);
        assert_eq!(r.error.as_deref(), Some("接続できない"));
        assert!(r.last_ok.is_some());
        // 初回から失敗しているときも failing(connecting にしない)
        let r = from_feed_status("weather", "x", &status(true, None, Some("e")));
        assert_eq!(r.state, FeedState::Failing);
    }

    #[test]
    fn geoframe_maps_online_retrying_and_before_the_first_fetch() {
        let r = from_link(&Link::default());
        assert_eq!((r.id, r.state), ("geoframe", FeedState::Connecting));
        let ok = Link {
            last_ok: Some(at("2026-10-30T10:40:12+09:00")),
            status: Some(Photo::Online),
        };
        let r = from_link(&ok);
        assert_eq!(r.state, FeedState::Ok);
        assert_eq!(r.label, "GEOFRAME(写真)");
        assert!(r.last_ok.unwrap().starts_with("2026-10-30T"));
        // 止めると failing。最後に取れた時刻は残る。戻ると ok
        let failing = Link {
            status: Some(Photo::Retrying {
                attempt: 2,
                retry_in: Duration::from_secs(4),
                error: "接続できない".into(),
            }),
            ..ok.clone()
        };
        let r = from_link(&failing);
        assert_eq!(r.state, FeedState::Failing);
        assert_eq!(r.error.as_deref(), Some("接続できない"));
        assert!(r.last_ok.is_some());
    }

    #[test]
    fn aircraft_maps_connecting_ok_and_failing() {
        let r = from_aircraft(&Health::default());
        assert_eq!(r.state, FeedState::Connecting);
        let ok = Health {
            last_ok: Some(at("2026-10-30T10:40:12+09:00")),
            last_error: None,
        };
        assert_eq!(from_aircraft(&ok).state, FeedState::Ok);
        let failing = Health {
            last_error: Some((at("2026-10-30T10:41:00+09:00"), "取得できない".into())),
            ..ok
        };
        let r = from_aircraft(&failing);
        assert_eq!(r.state, FeedState::Failing);
        assert_eq!(r.error.as_deref(), Some("取得できない"));
        assert!(r.last_ok.is_some(), "最後に取れた時刻は失敗中も出す");
        // 一度も取れずに失敗
        let r = from_aircraft(&Health {
            last_ok: None,
            last_error: failing.last_error,
        });
        assert_eq!((r.state, r.last_ok), (FeedState::Failing, None));
    }

    #[test]
    fn agents_map_each_connection() {
        let snap = |connection, err: Option<&str>| AgentSnapshot {
            connection,
            error_summary: err.map(str::to_string),
            last_success: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            ..AgentSnapshot::default()
        };
        for (c, want) in [
            (AgentConnection::Connecting, FeedState::Connecting),
            (AgentConnection::Live, FeedState::Ok),
            (AgentConnection::Retrying, FeedState::Failing),
            (AgentConnection::ApiError, FeedState::Failing),
            (
                AgentConnection::AuthenticationRequired,
                FeedState::AuthRequired,
            ),
            (AgentConnection::Forbidden, FeedState::Forbidden),
        ] {
            let r = from_agents(&snap(c, Some("401")));
            assert_eq!(r.state, want, "{c:?}");
            assert_eq!(r.id, "agents");
            assert!(r.last_ok.is_some());
            // ok のときは古いエラーを出さない
            assert_eq!(r.error.is_some(), want != FeedState::Ok, "{c:?}");
        }
        let r = from_agents(&AgentSnapshot::default());
        assert_eq!((r.state, r.last_ok), (FeedState::Connecting, None));
    }

    #[test]
    fn sources_map_none_with_reason_to_not_running() {
        let feeds = AdminSources::default().feeds();
        assert_eq!(feeds.len(), 6);
        for f in &feeds {
            assert_eq!(f.state, FeedState::NotRunning, "{}", f.id);
            assert_eq!(f.error.as_deref(), Some("取得元が登録されていない"));
            assert_eq!(f.last_ok, None);
        }
        let s = AdminSources {
            aircraft: Err("aircraft.enabled = false".into()),
            agents: Ok(Arc::new(RwLock::new(AgentSnapshot {
                connection: AgentConnection::AuthenticationRequired,
                error_summary: Some("T3 token file を確認できません".into()),
                ..AgentSnapshot::default()
            }))),
            home_assistant: Ok(Arc::new(Mutex::new(crate::env::EnvData::default()))),
            ..AdminSources::default()
        };
        let by = |id: &str| s.feeds().into_iter().find(|f| f.id == id).unwrap();
        assert_eq!(
            by("aircraft").error.as_deref(),
            Some("aircraft.enabled = false")
        );
        assert_eq!(by("agents").state, FeedState::AuthRequired);
        assert_eq!(by("home_assistant").state, FeedState::Connecting);
    }

    #[test]
    fn feed_json_uses_snake_case() {
        let v = serde_json::to_value(not_running("aircraft", "tar1090(航空機)", "r")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"id":"aircraft","label":"tar1090(航空機)","state":"not_running","last_ok":null,"error":"r"})
        );
        let v = serde_json::to_value(from_agents(&AgentSnapshot {
            connection: AgentConnection::AuthenticationRequired,
            ..AgentSnapshot::default()
        }))
        .unwrap();
        assert_eq!(v["state"], "auth_required");
    }

    #[test]
    fn errors_lose_url_queries_and_are_clipped() {
        assert_eq!(clip_error("短い"), "短い");
        assert_eq!(
            clip_error("http://192.0.2.9:8080/a?token=SECRET&x=1: connection failed"),
            "http://192.0.2.9:8080/a connection failed"
        );
        assert_eq!(
            clip_error("GET \"https://example.test/p?k=v#f\" に失敗 (?は残る)"),
            "GET \"https://example.test/p\" に失敗 (?は残る)"
        );
        assert_eq!(
            clip_error("取得失敗（https://example.test/a?x=1）再試行します"),
            "取得失敗（https://example.test/a）再試行します"
        );
        assert_eq!(
            clip_error("「https://example.test/a?x=1」と(http://h/b?y=2)と[http://h/c?z=3]と。"),
            "「https://example.test/a」と(http://h/b)と[http://h/c]と。"
        );
        let long = clip_error(&"あ".repeat(500));
        assert_eq!(long.chars().count(), 201);
        assert!(long.ends_with('…'));
        // 長いエラーは切ってから出る(feeds の経路)
        let r = from_link(&Link {
            status: Some(Photo::Retrying {
                attempt: 1,
                retry_in: Duration::from_secs(1),
                error: "e".repeat(1000),
            }),
            ..Link::default()
        });
        assert_eq!(r.error.unwrap().chars().count(), 201);
    }

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "miharashi-feeds-test-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
        fn file(&self, name: &str, mode: u32) -> String {
            let p = self.0.join(name);
            std::fs::write(&p, "secret-value-0123456789\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            p.to_str().unwrap().to_string()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ac16_token_file_states() {
        let d = Dir::new("tokens");
        let (uid, gid) = (
            crate::agents::effective_uid(),
            crate::agents::effective_gid(),
        );
        // 規則どおり(0600・自分の所有)
        assert_eq!(
            check_token_file(&d.file("ok", 0o600), uid, gid),
            TokenState::Ok
        );
        // その他に読み取りがある
        assert_eq!(
            check_token_file(&d.file("open", 0o644), uid, gid),
            TokenState::LoosePermissions
        );
        assert_eq!(
            check_token_file(&d.file("grp", 0o660), uid, gid),
            TokenState::LoosePermissions
        );
        // 無い
        let missing = d.0.join("missing");
        assert_eq!(
            check_token_file(missing.to_str().unwrap(), uid, gid),
            TokenState::Missing
        );
        assert_eq!(check_token_file("", uid, gid), TokenState::Missing);
        // 開けない(0000)。root は権限を無視して開けるので、root 以外のときだけ確かめる
        let none = d.file("none", 0o000);
        if uid != 0 {
            assert_eq!(check_token_file(&none, uid, gid), TokenState::Unreadable);
        }
        // ディレクトリは読めるファイルではない
        assert_eq!(
            check_token_file(d.0.to_str().unwrap(), uid, gid),
            TokenState::Unreadable
        );
        // 所有者違い: 実行ユーザーを偽の値にして判定する(0600 でも 0640 でも)
        let own = d.file("own", 0o600);
        let fake = uid.wrapping_add(12345);
        assert_eq!(check_token_file(&own, fake, gid), TokenState::WrongOwner);
        let own640 = d.file("own640", 0o640);
        assert_eq!(check_token_file(&own640, fake, gid), TokenState::WrongOwner);
        // 0640 は root 所有でグループが実行グループのときだけ規則どおり。自分の所有の 0640 は緩い
        assert_eq!(
            check_token_file(&own640, uid, gid),
            TokenState::LoosePermissions
        );
        // 実行ユーザーが root だと偽っても、自分の所有のファイルは規則の所有者ではない
        if uid != 0 {
            assert_eq!(check_token_file(&own, 0, gid), TokenState::WrongOwner);
        }
        // 値は結果に含まれない(TokenState は状態の名前だけ)
        assert!(!format!("{:?}", check_token_file(&own, uid, gid)).contains("secret"));
    }
}
