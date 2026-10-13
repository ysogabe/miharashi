//! 画面の電源制御。Home Assistant の在宅・人感エンティティと手動オーバーライド(input_select)を
//! 別スレッドで問い合わせ、画面を点けるべきかを `ScreenPower::want_on` で返す。
//! 実際に DPMS を切り替えるのは runtime(DRM master を持つ側)。
//!
//! 判定は GEOFRAME の `screen_control.py` / `ha.py` から移植した(GEOFRAME の display-final タグ)。
//! - 優先度は force_off > force_on > auto。
//! - auto では、在宅エンティティがあり、HA と通信でき、不在が `idle_minutes` を超えたときだけ消す。
//!   HA と通信できないときは点けておく(消えたまま戻らないのが最悪の失敗)。
//! - オーバーライドは、HA と通信できない状態が `override_decay_minutes` を超えたら auto に戻す。
//! - auto のとき、表示機のキー操作は在宅の検知として扱う(押せばすぐ点き、不在の時間も数え直す)。
use crate::config::{HomeAssistant, ScreenPower as Settings};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 手動オーバーライドの実効値。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    ForceOn,
    ForceOff,
}

/// HA の生の値を実効モードにする。値は厳密一致だけを受け付け、それ以外(タイポ・unknown・
/// unavailable)は auto に倒す。`stale` は mode エンティティと最後に通信できてからの経過時間。
pub fn effective_mode(raw: &str, stale: Duration, decay_after: Duration) -> Mode {
    let mode = match raw {
        "force_on" => Mode::ForceOn,
        "force_off" => Mode::ForceOff,
        _ => return Mode::Auto,
    };
    // ちょうど閾値の時点ではまだ有効(`>` であって `>=` ではない)
    if stale > decay_after {
        Mode::Auto
    } else {
        mode
    }
}

/// 画面を点けるべきか。`ha_ok` は在宅エンティティを最後に問い合わせた結果(None は起動直後)。
/// 境界の `idle == idle_after` は点灯。
pub fn decide(
    mode: Mode,
    ha_ok: Option<bool>,
    idle: Duration,
    presence_configured: bool,
    idle_after: Duration,
) -> bool {
    match mode {
        Mode::ForceOff => false,
        Mode::ForceOn => true,
        Mode::Auto => !(presence_configured && ha_ok == Some(true) && idle > idle_after),
    }
}

/// トークンを送ってよい接続先か。https か、名前解決を伴わない IP アドレス・localhost の http に限る。
/// 平文 http でホスト名宛てにすると、mDNS/DNS の応答を偽装されたときにトークンが盗まれる。
pub fn check_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return Err(format!(
            "http:// か https:// で始まる URL を指定する({url})"
        ));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        // [::1]:8123
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    if host.is_empty() {
        return Err(format!("ホスト名を解釈できない({url})"));
    }
    if host == "localhost" || host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    Err(format!(
        "平文 http でホスト名『{host}』宛てにトークンを送る設定になっている。IP アドレスか https で指定する"
    ))
}

/// 在宅判定と画面の向きを決める状態。I/O を持たないので、時刻と取得結果を渡してテストできる。
struct Policy {
    presence_configured: bool,
    idle_after: Duration,
    decay_after: Duration,
    /// 在宅エンティティとの最後の通信結果(None は起動直後)
    ha_ok: Option<bool>,
    /// 最後に在宅・人感を検知した時刻(起動時刻から数える)
    last_seen: Instant,
    /// HA から読んだ mode の生の値。取得に失敗しても直前の値を保つ(減衰の起点にするため)
    raw_mode: String,
    /// mode エンティティと最後に通信できた時刻。値の妥当性は問わない
    mode_last_ok: Instant,
}

impl Policy {
    fn new(s: &Settings, now: Instant) -> Self {
        Self {
            presence_configured: !s.presence_entity.is_empty(),
            idle_after: Duration::from_secs(s.idle_minutes * 60),
            decay_after: Duration::from_secs(s.override_decay_minutes * 60),
            ha_ok: None,
            last_seen: now,
            raw_mode: "auto".into(),
            mode_last_ok: now,
        }
    }

    /// 在宅エンティティの取得結果。Err は通信・応答の失敗。
    fn on_presence(&mut self, now: Instant, state: Result<&str, ()>) {
        match state {
            Ok(s) => {
                self.ha_ok = Some(true);
                // 動きを検知(binary_sensor)か在宅(person / device_tracker)
                if matches!(s, "on" | "home") {
                    self.last_seen = now;
                }
            }
            Err(()) => self.ha_ok = Some(false),
        }
    }

    /// mode エンティティの取得結果。失敗時は直前の値を保ち、時間経過で auto に戻す。
    fn on_mode(&mut self, now: Instant, state: Result<&str, ()>) {
        if let Ok(s) = state {
            self.raw_mode = s.to_string();
            self.mode_last_ok = now;
        }
    }

    /// 表示機のキー操作。前に人がいるので、在宅を検知したのと同じに扱う。
    fn on_activity(&mut self, now: Instant) {
        self.last_seen = now;
    }

    fn mode(&self, now: Instant) -> Mode {
        effective_mode(
            &self.raw_mode,
            now.duration_since(self.mode_last_ok),
            self.decay_after,
        )
    }

    fn want_on(&self, now: Instant) -> bool {
        decide(
            self.mode(now),
            self.ha_ok,
            now.duration_since(self.last_seen),
            self.presence_configured,
            self.idle_after,
        )
    }

    fn idle(&self, now: Instant) -> Duration {
        now.duration_since(self.last_seen)
    }
}

/// HA の `GET /api/states/<entity>` の state だけを読むクライアント。
struct Client {
    agent: ureq::Agent,
    url: String,
    auth: String,
}

impl Client {
    fn state(&self, entity: &str) -> Result<String, String> {
        let text = self
            .agent
            .get(&format!("{}/api/states/{entity}", self.url))
            .header("Authorization", &self.auth)
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        body["state"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "state がない応答".into())
    }
}

/// 問い合わせスレッドと実行ループで共有する状態。
#[derive(Default)]
struct Shared {
    want_on: AtomicBool,
    /// 実効モードが auto 以外(キー操作では点けない)
    forced: AtomicBool,
    /// 前回の問い合わせ以降にキー操作があった
    key_pressed: AtomicBool,
}

/// 画面を点けるべきかを別スレッドで判定し続ける。
pub struct ScreenPower {
    shared: Arc<Shared>,
}

impl ScreenPower {
    /// 連携の設定がそろっていれば問い合わせスレッドを起動する。そろっていなければ None(画面は常に点灯)。
    pub fn start(ha: &HomeAssistant, settings: &Settings) -> Option<Self> {
        if ha.url.is_empty() {
            return None;
        }
        if settings.presence_entity.is_empty() && settings.mode_entity.is_empty() {
            eprintln!("screen: home_assistant.url はあるが、エンティティが未設定(画面は常に点灯)");
            return None;
        }
        let token = match std::fs::read_to_string(&ha.token_file) {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            Ok(_) => {
                eprintln!("screen: {} が空(画面は常に点灯)", ha.token_file);
                return None;
            }
            Err(e) => {
                eprintln!(
                    "screen: トークンを読めない {}: {e}(画面は常に点灯)",
                    ha.token_file
                );
                return None;
            }
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let client = Client {
            agent,
            url: ha.url.trim_end_matches('/').to_string(),
            auth: format!("Bearer {token}"),
        };
        eprintln!(
            "screen: Home Assistant 連携 ON({} / 在宅={} / 手動={} / 不在{}分で消灯)",
            client.url,
            or_dash(&settings.presence_entity),
            or_dash(&settings.mode_entity),
            settings.idle_minutes
        );
        let shared = Arc::new(Shared::default());
        shared.want_on.store(true, Ordering::Relaxed);
        let for_thread = shared.clone();
        let settings = settings.clone();
        std::thread::spawn(move || poll(client, settings, for_thread));
        Some(Self { shared })
    }

    pub fn want_on(&self) -> bool {
        self.shared.want_on.load(Ordering::Relaxed)
    }

    /// 表示機でキーが押された。auto なら画面をすぐ点け、次の問い合わせで在宅として数える。
    pub fn on_key(&self) {
        self.shared.key_pressed.store(true, Ordering::Relaxed);
        if !self.shared.forced.load(Ordering::Relaxed)
            && !self.shared.want_on.swap(true, Ordering::Relaxed)
        {
            eprintln!("screen: 点灯(キー操作)");
        }
    }
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

fn poll(client: Client, s: Settings, shared: Arc<Shared>) {
    let mut policy = Policy::new(&s, Instant::now());
    let (mut presence_warned, mut mode_warned) = (false, false);
    let mut prev_mode = Mode::Auto;
    loop {
        let now = Instant::now();
        // 在宅と mode は別々に扱う。mode エンティティのタイポで在宅判定まで止めないため。
        if !s.presence_entity.is_empty() {
            let r = client.state(&s.presence_entity);
            warn_once(&mut presence_warned, &s.presence_entity, &r);
            policy.on_presence(now, r.as_deref().map_err(|_| ()));
        }
        if !s.mode_entity.is_empty() {
            let r = client.state(&s.mode_entity);
            warn_once(&mut mode_warned, &s.mode_entity, &r);
            policy.on_mode(now, r.as_deref().map_err(|_| ()));
        }
        // 判定の直前に取り込む(取り込んだあとに押されたキーは、on_key がすでに点けている)
        if shared.key_pressed.swap(false, Ordering::Relaxed) {
            policy.on_activity(Instant::now());
        }
        let mode = policy.mode(now);
        shared.forced.store(mode != Mode::Auto, Ordering::Relaxed);
        let on = policy.want_on(now);
        if on != shared.want_on.load(Ordering::Relaxed) {
            let what = if on { "点灯" } else { "消灯" };
            if prev_mode != Mode::Auto && mode == Mode::Auto {
                eprintln!("screen: {what}(手動オーバーライドが HA の不通で auto に戻った)");
            } else if mode == Mode::Auto {
                eprintln!(
                    "screen: {what}(不在 {} 分)",
                    policy.idle(now).as_secs() / 60
                );
            } else {
                eprintln!("screen: {what}(手動オーバーライド {mode:?})");
            }
            shared.want_on.store(on, Ordering::Relaxed);
        }
        prev_mode = mode;
        std::thread::sleep(Duration::from_secs(s.poll_secs));
    }
}

/// 取得の失敗は続いている間 1 回だけログに出す。成功したら次の失敗でまた出す。
fn warn_once(warned: &mut bool, entity: &str, r: &Result<String, String>) {
    match r {
        Err(e) if !*warned => {
            *warned = true;
            eprintln!("screen: {entity} を取得できない(再試行を続ける): {e}");
        }
        Ok(_) if *warned => {
            *warned = false;
            eprintln!("screen: {entity} の取得が回復");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn effective_mode_accepts_only_exact_values() {
        assert_eq!(effective_mode("force_off", MIN, 5 * MIN), Mode::ForceOff);
        assert_eq!(effective_mode("force_on", MIN, 5 * MIN), Mode::ForceOn);
        for raw in ["auto", "Force_Off", "unknown", "unavailable", ""] {
            assert_eq!(effective_mode(raw, MIN, 5 * MIN), Mode::Auto, "{raw}");
        }
    }

    #[test]
    fn override_decays_only_after_the_threshold() {
        assert_eq!(
            effective_mode("force_off", 5 * MIN, 5 * MIN),
            Mode::ForceOff
        );
        assert_eq!(
            effective_mode("force_off", 5 * MIN + Duration::from_secs(1), 5 * MIN),
            Mode::Auto
        );
    }

    #[test]
    fn decide_priority_and_auto_rule() {
        let idle_after = 10 * MIN;
        assert!(!decide(
            Mode::ForceOff,
            Some(true),
            Duration::ZERO,
            true,
            idle_after
        ));
        assert!(decide(
            Mode::ForceOn,
            Some(true),
            60 * MIN,
            true,
            idle_after
        ));
        // auto: 不在が閾値を超え、HA と通信できているときだけ消す
        assert!(!decide(Mode::Auto, Some(true), 11 * MIN, true, idle_after));
        assert!(
            decide(Mode::Auto, Some(true), 10 * MIN, true, idle_after),
            "境界は点灯"
        );
        assert!(
            decide(Mode::Auto, Some(false), 60 * MIN, true, idle_after),
            "HA 不通は点灯"
        );
        assert!(
            decide(Mode::Auto, None, 60 * MIN, true, idle_after),
            "起動直後は点灯"
        );
        assert!(
            decide(Mode::Auto, Some(true), 60 * MIN, false, idle_after),
            "在宅エンティティなし"
        );
    }

    #[test]
    fn url_safety() {
        for ok in [
            "https://ha.example.test",
            "http://192.0.2.10:8123",
            "http://127.0.0.1:8123/",
            "http://[::1]:8123",
            "http://localhost:8123",
            "HTTP://192.0.2.10",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for ng in [
            "http://homeassistant.local:8123",
            "http://ha.example.test",
            "ftp://192.0.2.10",
            "192.0.2.10:8123",
            "http://",
        ] {
            assert!(check_url(ng).is_err(), "{ng}");
        }
    }

    fn settings(presence: &str, mode: &str) -> Settings {
        Settings {
            presence_entity: presence.into(),
            mode_entity: mode.into(),
            ..Settings::default()
        }
    }

    #[test]
    fn policy_turns_off_after_absence_and_back_on_when_home() {
        let t0 = Instant::now();
        let mut p = Policy::new(&settings("person.me", ""), t0);
        assert!(p.want_on(t0), "起動直後は点灯");
        p.on_presence(t0, Ok("not_home"));
        assert!(p.want_on(t0 + 10 * MIN), "起動から 10 分は点灯");
        let t1 = t0 + 11 * MIN;
        p.on_presence(t1, Ok("not_home"));
        assert!(!p.want_on(t1));
        p.on_presence(t1, Ok("home"));
        assert!(p.want_on(t1));
    }

    #[test]
    fn key_activity_counts_as_presence() {
        let t0 = Instant::now();
        let mut p = Policy::new(&settings("person.me", ""), t0);
        let t1 = t0 + 11 * MIN;
        p.on_presence(t1, Ok("not_home"));
        assert!(!p.want_on(t1));
        p.on_activity(t1);
        assert!(p.want_on(t1));
        assert!(p.want_on(t1 + 10 * MIN), "キー操作から 10 分は点灯");
        assert!(!p.want_on(t1 + 11 * MIN));
    }

    #[test]
    fn policy_keeps_screen_on_while_ha_is_unreachable() {
        let t0 = Instant::now();
        let mut p = Policy::new(&settings("person.me", ""), t0);
        let t1 = t0 + 30 * MIN;
        p.on_presence(t1, Ok("not_home"));
        assert!(!p.want_on(t1));
        p.on_presence(t1, Err(()));
        assert!(p.want_on(t1), "不通なら点ける");
    }

    #[test]
    fn policy_override_holds_while_reachable_and_decays_when_not() {
        let t0 = Instant::now();
        let mut p = Policy::new(&settings("person.me", "input_select.screen"), t0);
        p.on_presence(t0, Ok("home"));
        p.on_mode(t0, Ok("force_off"));
        assert!(!p.want_on(t0), "在宅でも force_off なら消す");
        // 通信できない間は直前の値を保ち、5 分を超えたら auto(在宅なので点灯)
        p.on_mode(t0 + 5 * MIN, Err(()));
        p.on_presence(t0 + 5 * MIN, Ok("home"));
        assert!(!p.want_on(t0 + 5 * MIN));
        let t1 = t0 + 6 * MIN;
        p.on_mode(t1, Err(()));
        p.on_presence(t1, Ok("home"));
        assert_eq!(p.mode(t1), Mode::Auto);
        assert!(p.want_on(t1));
    }
}
