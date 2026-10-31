//! TOML 設定。未指定の項目は既定値を使い、未知のキーは typo として拒否する。
use serde::Deserialize;
use serde::Serialize;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use ureq::http::Uri;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Config {
    pub runtime: Runtime,
    pub display: Display,
    pub input: Input,
    pub geoframe: Geoframe,
    pub metrics: Metrics,
    pub power: Power,
    pub home_assistant: HomeAssistant,
    pub screen_power: ScreenPower,
    pub alerts: Alerts,
    pub aircraft: Aircraft,
    pub room: Room,
    pub rotation: Rotation,
    pub ui: Ui,
    pub panel: Panel,
    pub agents: ConfigAgents,
    pub cube: Cube,
    pub gl_check: GlCheck,
    pub vrm: Vrm,
    pub admin: Admin,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Runtime {
    /// auto / libseat / direct
    pub seat: String,
    /// この秒数で自動終了(0 = 無期限)。試験用。
    pub max_seconds: u64,
    /// 状態ログ(RSS・フレーム時間など)を出す間隔の秒数。journal に残る。
    pub stats_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    /// auto / /dev/dri/cardN / pci:0000:01:00.0 / driver:amdgpu
    pub card: String,
    /// auto / HDMI-A-1 など
    pub connector: String,
    /// preferred / 1920x1080 / 1920x1080@60
    pub mode: String,
    /// auto / cpu / gles。auto は GLES(表示カードの GPU)を試し、使えなければ CPU(dumb buffer)に戻る。
    pub renderer: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Input {
    /// デバイス名に含まれる文字列。空なら 4/6 か矢印キーを持つ全キーボード。
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Geoframe {
    pub url: String,
    pub photo_interval_secs: u64,
    pub reconnect_min_secs: u64,
    pub reconnect_max_secs: u64,
    /// 自宅からこの距離(km)以上離れた写真を「旅」、近い写真を「いつもの暮らし」(季節ごと)にする。
    pub home_radius_km: f64,
    /// 旅・季節の切り替わりに挟むカードを出す秒数。
    pub card_secs: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Metrics {
    /// 実測値の取得間隔(秒)。
    pub interval_secs: u64,
    /// 監視する GPU。auto / NVML のインデックス / pci:0000:01:00.0。表示先 GPU とは独立。
    pub gpu: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Power {
    /// 電力量(今日・累計)の保存先 JSON。空なら保存しない。
    pub state_file: String,
    /// root ヘルパーが書き出す CPU の RAPL 値("<energy_uj> <max_energy_range_uj>")。
    pub rapl_helper_file: String,
    /// 電気代の単価 [円/kWh]。0 なら電気代を表示しない。
    pub price_per_kwh: f32,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HomeAssistant {
    /// 接続先(例: http://192.0.2.10:8123)。空なら連携しない。
    /// 平文 http はトークンが盗まれないよう IP アドレスか localhost に限る。
    pub url: String,
    /// 長期アクセストークンを 1 行で書いたファイル。設定ファイルには書かない。
    pub token_file: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenPower {
    /// 在宅・人感のエンティティ(person.xxx / binary_sensor.xxx)。state が home か on なら在宅。
    pub presence_entity: String,
    /// 手動オーバーライド用の input_select(auto / force_on / force_off)。空なら auto 固定。
    pub mode_entity: String,
    /// 不在がこの分数を超えたら画面を消す。
    pub idle_minutes: u64,
    /// HA と通信できない状態がこの分数を超えたら、オーバーライドを捨てて auto に戻す。
    pub override_decay_minutes: u64,
    /// HA に問い合わせる間隔(秒)。
    pub poll_secs: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Alerts {
    /// 地震アラート(住んでいる地方で震度 4 以上、全国で震度 5 弱以上)。
    pub quake: bool,
    /// 気象庁の地震情報を読む間隔(秒)。
    pub quake_poll_secs: u64,
    /// 災害アラート(自宅から半径 disaster_radius_km の市町村に特別警報・土砂災害危険警報)。
    pub disaster: bool,
    /// 気象庁の警報を読む間隔(秒)。変わっていなければ 304 で済む。
    pub disaster_poll_secs: u64,
    /// 災害アラートの対象にする半径(km)。
    pub disaster_radius_km: f64,
    /// アラートを出し続ける分数。キーを押せば先に閉じる。
    pub show_minutes: u64,
    /// 0 より大きければ、起動からこの秒数後にテスト表示を 1 回出す(動作確認用)。
    pub demo_after_secs: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// 日本語フォントファイル。空ならシステムフォントを使う。
    pub font: String,
    /// status / photo / room / aircraft
    pub start_screen: String,
    /// 焼き付き対策: この分数ごとに、`refresh_screens` の画面をグリッチで暗転して描き直す。0 なら行わない
    pub refresh_minutes: u64,
    /// 焼き付き対策の演出を入れる画面の id(動かない線が多い画面)
    pub refresh_screens: Vec<String>,
}

/// 手元の操作パネル(Echo Show 5 など)向けの API(docs/panel/DESIGN.md §5.2)。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Panel {
    pub enabled: bool,
    /// 待ち受けるアドレス。LAN のインターフェースの IP を指定する(0.0.0.0 / :: は不可)。
    pub listen: String,
    /// Bearer トークンを 1 行で書いたファイル(ha-token と同じく root:hermes 0640 か 0600)。
    pub token_file: String,
    /// `POST /api/v1/input` を 1 秒にこの回数まで受け付ける。超えたら 429。
    pub max_rate_per_sec: u32,
}

/// ブラウザから見る管理ページ(docs/admin-web/design.md)。`[panel]` が有効なときだけ動く。既定は無効。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Admin {
    pub enabled: bool,
    /// 管理トークンを 1 行で書いたファイル(`panel.token_file` とは別のファイル・別の値)。
    pub token_file: String,
    /// 管理ページから保存した値の置き場所。設定ファイルより優先する。
    pub overlay_file: String,
    /// 管理ページから `agents.url` に使える接続先(host か host:port)。
    pub allowed_hosts: Vec<String>,
}

impl Default for Admin {
    fn default() -> Self {
        Self {
            enabled: false,
            token_file: "/etc/miharashi/admin.token".into(),
            overlay_file: "/var/lib/miharashi/admin.toml".into(),
            allowed_hosts: Vec::new(),
        }
    }
}

/// 管理ページが書く上書きファイル(`admin.toml`)。変えられる項目だけで、全部 `Option`(無い = 上書きしない)。
/// 内側の構造体にも `deny_unknown_fields` を付ける(外側だけでは内側の知らないキーが黙って捨てられる)。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    pub geoframe: OverlayUrl,
    pub agents: OverlayScreenUrl,
    pub aircraft: OverlayScreenUrl,
    pub room: OverlayScreen,
    pub cube: OverlayScreen,
    pub vrm: OverlayScreen,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayUrl {
    pub url: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayScreenUrl {
    pub enabled: Option<bool>,
    pub url: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayScreen {
    pub enabled: Option<bool>,
}

impl Overlay {
    /// Some の項目だけ `cfg` へ代入する。
    pub fn apply(&self, cfg: &mut Config) {
        if let Some(v) = &self.geoframe.url {
            cfg.geoframe.url = v.clone();
        }
        if let Some(v) = self.agents.enabled {
            cfg.agents.enabled = v;
        }
        if let Some(v) = &self.agents.url {
            cfg.agents.url = v.clone();
        }
        if let Some(v) = self.aircraft.enabled {
            cfg.aircraft.enabled = v;
        }
        if let Some(v) = &self.aircraft.url {
            cfg.aircraft.url = v.clone();
        }
        if let Some(v) = self.room.enabled {
            cfg.room.enabled = v;
        }
        if let Some(v) = self.cube.enabled {
            cfg.cube.enabled = v;
        }
        if let Some(v) = self.vrm.enabled {
            cfg.vrm.enabled = v;
        }
    }

    /// 入っている項目の一覧(ログ・check-config 用)。例: `("aircraft.enabled", "false")`。
    pub fn entries(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(x) = &self.geoframe.url {
            v.push(("geoframe.url", x.clone()));
        }
        if let Some(x) = self.agents.enabled {
            v.push(("agents.enabled", x.to_string()));
        }
        if let Some(x) = &self.agents.url {
            v.push(("agents.url", x.clone()));
        }
        if let Some(x) = self.aircraft.enabled {
            v.push(("aircraft.enabled", x.to_string()));
        }
        if let Some(x) = &self.aircraft.url {
            v.push(("aircraft.url", x.clone()));
        }
        if let Some(x) = self.room.enabled {
            v.push(("room.enabled", x.to_string()));
        }
        if let Some(x) = self.cube.enabled {
            v.push(("cube.enabled", x.to_string()));
        }
        if let Some(x) = self.vrm.enabled {
            v.push(("vrm.enabled", x.to_string()));
        }
        v
    }

    /// 上書きに固有の規則。読み込みと保存の両方から呼ぶ(1 か所に書く)。
    /// - 3 つの url: 共通の文字の検査(制御文字・空白・`"`・`\`・`@` を拒否、authority は ASCII だけで `%` も不可、
    ///   http(s) で始まり、`Uri` で読めて host が空でなく、port があれば数字)
    /// - agents.url: 画面が無効でも `agents::check_url` を通す。さらに origin(scheme・host・port)が次のどれかと一致すること:
    ///   (a) 設定ファイルの文面に書かれた agents.url の origin(`base_agents_url`。プログラムの既定値は使わない)
    ///   (b) `base.admin.allowed_hosts` の各要素(host か host:port。port が無ければ scheme の既定ポートだけ)
    ///   設定ファイルの agents.url が https なら、上書きは (b) に書かれていても https だけ(http への格下げは認めない。PRD R5)
    /// - aircraft.url / geoframe.url: 共通の文字の検査だけ(トークンを送らない)
    pub fn check(&self, base: &Config, base_agents_url: Option<&str>) -> Result<(), String> {
        if let Some(u) = &self.geoframe.url {
            parse_url_strict("geoframe.url", u)?;
        }
        if let Some(u) = &self.aircraft.url {
            parse_url_strict("aircraft.url", u)?;
        }
        let Some(u) = &self.agents.url else {
            return Ok(());
        };
        let (_, origin) = parse_url_strict("agents.url", u)?;
        // 画面が無効でも検査する。check_url は origin だけを受け付ける(パス・クエリ・fragment 付きは Err)
        crate::agents::check_url(u).map_err(|e| format!("agents.url: {e}"))?;
        let from_file = base_agents_url
            .and_then(|b| b.parse::<Uri>().ok())
            .and_then(|b| origin_of(&b));
        // 設定ファイルの agents.url が https なら、allowed_hosts に書いてあっても http にはできない(格下げの検査が先)
        if from_file.as_ref().is_some_and(|f| f.https) && !origin.https {
            return Err(format!(
                "agents.url: 設定ファイルの agents.url が https なので、http への格下げはできない(allowed_hosts に書いても不可): {u}"
            ));
        }
        if from_file.as_ref() == Some(&origin) {
            return Ok(());
        }
        let allowed = base.admin.allowed_hosts.iter().any(|e| {
            parse_allowed_host(e).is_ok_and(|(host, port)| {
                host == origin.host
                    && port.unwrap_or_else(|| default_port(origin.https)) == origin.port
            })
        });
        if allowed {
            return Ok(());
        }
        Err(format!(
            "agents.url: 接続先 {u} は許可されていない(allowed_hosts か設定ファイルの agents.url と同じ接続先だけ)"
        ))
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 接続先の照合に使うホストの形。IP は `IpAddr` に直して比べ、名前は小文字にそろえる(末尾のドット・ワイルドカードは無い)。
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostKey {
    Ip(IpAddr),
    Name(String),
}

/// `Uri::host()` が返した値(IPv6 は `[]` 付き)を照合用の形にする。
fn host_key(host: &str) -> HostKey {
    let h = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match h.parse::<IpAddr>() {
        Ok(ip) => HostKey::Ip(ip),
        Err(_) => HostKey::Name(h.to_ascii_lowercase()),
    }
}

/// 接続先(scheme・host・port)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    https: bool,
    host: HostKey,
    port: u16,
}

fn default_port(https: bool) -> u16 {
    if https { 443 } else { 80 }
}

/// 読めた `Uri` が http(s) で host が空でなければ、その host を返す(userinfo の有無・port は見ない)。
fn http_host(uri: &Uri) -> Option<(bool, &str)> {
    let https = match uri.scheme_str()? {
        "https" => true,
        "http" => false,
        _ => return None,
    };
    let host = uri.host().filter(|h| !h.is_empty())?;
    Some((https, host))
}

/// 読めた `Uri` から origin を作る。http(s) でない・host が空・port が数字でない(`Uri` は黙って捨てる)なら None。
/// authority は userinfo を持たない前提(上書きの検査は `@` を先に拒否する)。
fn origin_of(uri: &Uri) -> Option<Origin> {
    let (https, host) = http_host(uri)?;
    // host の後ろは「無し」か「:数字」だけ(host で始まる)
    let rest = uri.authority()?.as_str().strip_prefix(host)?;
    let port = match rest {
        "" => default_port(https),
        r => {
            let digits = r.strip_prefix(':')?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse::<u16>().ok().filter(|p| *p != 0)?
        }
    };
    Some(Origin {
        https,
        host: host_key(host),
        port,
    })
}

/// 上書きの URL に共通の検査。journal・TOML・スクリプトでの読み方が食い違う文字を拒否する。
fn parse_url_strict(label: &str, url: &str) -> Result<(Uri, Origin), String> {
    if let Some(c) = url
        .chars()
        .find(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\\' | '@'))
    {
        return Err(format!(
            "{label}: 使えない文字 {c:?} を含む(制御文字・空白・引用符・バックスラッシュ・@ は不可)"
        ));
    }
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(format!("{label}: http:// か https:// で始まる URL にする"));
    }
    let uri: Uri = url
        .parse()
        .map_err(|e| format!("{label}: URL として読めない({e})"))?;
    let authority = uri
        .authority()
        .map(|a| a.as_str())
        .ok_or_else(|| format!("{label}: host が無い"))?;
    if !authority.is_ascii() || authority.contains('%') {
        return Err(format!("{label}: host は ASCII だけ(% エンコードは不可)"));
    }
    let origin = origin_of(&uri)
        .ok_or_else(|| format!("{label}: host が空、または port が 1〜65535 の数字でない"))?;
    Ok((uri, origin))
}

/// `geoframe.url` と `aircraft.url` の最小の検査(`validate` 用)。http(s) で始まり、host が解釈できること。
fn check_http_url(label: &str, url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err(format!(
            "{label}: http:// か https:// で始まる URL にする({url})"
        ));
    }
    let uri: Result<Uri, _> = url.parse();
    match uri {
        // userinfo や port の検査は上書きにだけ掛ける(base の既存の書き方は通す)
        Ok(u) if http_host(&u).is_some() => Ok(()),
        _ => Err(format!("{label}: host を解釈できない({url})")),
    }
}

/// `admin.allowed_hosts` の 1 要素(host か host:port。IPv6 は `[]` で囲む)を読む。
fn parse_allowed_host(entry: &str) -> Result<(HostKey, Option<u16>), String> {
    let bad = || {
        format!(
            "admin.allowed_hosts の {entry:?} は host か host:port(ホスト名は英数字・-・.、IPv6 は [] で囲む)で書く"
        )
    };
    let (host, port) = if let Some(v6) = entry.strip_prefix('[') {
        let (inner, rest) = v6.split_once(']').ok_or_else(bad)?;
        let ip: std::net::Ipv6Addr = inner.parse().map_err(|_| bad())?;
        let port = match rest {
            "" => None,
            r => Some(r.strip_prefix(':').ok_or_else(bad)?),
        };
        (HostKey::Ip(IpAddr::V6(ip)), port)
    } else {
        let (h, port) = match entry.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (entry, None),
        };
        let key = if let Ok(ip) = h.parse::<IpAddr>() {
            HostKey::Ip(ip)
        } else if !h.is_empty()
            && h.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            HostKey::Name(h.to_ascii_lowercase())
        } else {
            return Err(bad());
        };
        (key, port)
    };
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            Some(p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad)?)
        }
        Some(_) => return Err(bad()),
    };
    Ok((host, port))
}

/// 上書きファイルを読んだ結果で、無視したときの理由の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayErrorKind {
    Parse,
    UnknownKey,
    Rule,
    Validate,
    BootFailed,
    MarkerUnwritable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayReport {
    pub path: PathBuf,
    pub exists: bool,
    /// 読んだ時点の上書きファイルの世代("none" = 無い)
    pub generation: String,
    /// 効いた項目
    pub applied: Vec<(&'static str, String)>,
    /// 無視した理由
    pub error: Option<String>,
    pub error_kind: Option<OverlayErrorKind>,
    /// BootFailed のとき、印に書かれていた上書きの世代
    pub boot_failed_generation: Option<String>,
}

/// 起動時に読んだもの一式。実行ループと panel が持ち回る。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub base_path: Option<PathBuf>,
    /// 設定ファイルの文面に書かれた agents.url(未記載・空なら None。allowed_hosts の照合用)
    pub base_agents_url: Option<String>,
    /// 上書き前(管理ページの「ファイルの値」)
    pub base: Config,
    /// 実際に使う(上書き後。無視したときは base と同じ)
    pub effective: Config,
    pub report: OverlayReport,
    /// 起動の印を書いた時刻(印を書いていなければ None)。印を消す 60 秒はここから数える
    pub marker_armed_at: Option<std::time::Instant>,
}

/// 起動の印(`admin.boot`)のファイル名。`overlay_file` と同じディレクトリに置く。
const BOOT_MARKER: &str = "admin.boot";

/// 印のパス(`overlay_file` と同じディレクトリの `admin.boot`)。
pub fn boot_marker_path(overlay_file: &Path) -> PathBuf {
    overlay_file.with_file_name(BOOT_MARKER)
}

pub fn boot_marker_dir(overlay_file: &Path) -> PathBuf {
    match overlay_file.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// 今の起動の ID(電源断・再起動で変わる)。読めなければ None(判定では pid だけを見る)。
fn current_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Serialize, Deserialize)]
struct MarkerFile {
    pid: u32,
    #[serde(default)]
    boot_id: Option<String>,
    generation: String,
    #[serde(default)]
    started_at: String,
}

/// 上書きで起動したことを示す印を `dir` に書く(一時ファイル → `sync_all` → `rename`)。
/// 中身は 1 行の JSON `{"pid","boot_id","generation","started_at"}`。
pub fn write_boot_marker(dir: &Path, generation: &str) -> Result<(), String> {
    use std::io::Write;
    let line = serde_json::to_string(&MarkerFile {
        pid: std::process::id(),
        boot_id: current_boot_id(),
        generation: generation.to_string(),
        started_at: chrono::Local::now().to_rfc3339(),
    })
    .map_err(|e| format!("印を作れない: {e}"))?;
    let path = dir.join(BOOT_MARKER);
    let tmp = dir.join(format!("{BOOT_MARKER}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{} に印を書けない: {e}", path.display())
    })
}

/// 印を消す(無ければ何もしない)。
pub fn remove_boot_marker(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(BOOT_MARKER));
}

/// 上書きで起動した印を消すまでの時間(docs/admin-web/design.md の R9。印を書いた時点から数える)。
pub const BOOT_MARKER_SECS: std::time::Duration = std::time::Duration::from_secs(60);

/// 印を書いた時刻(`armed_at`)から `ttl` たったら印を消すスレッド(`boot-marker`)を起こす。消したら終わる。
/// 初期化(表示・GPU・取得スレッドの起動)が長引いて実行ループに入る前に異常終了しても、
/// 起動から `ttl` を超えて動いていた印は残さない。実行ループ側の削除と重なっても、無いファイルの削除は無視する。
fn spawn_boot_marker_timer(
    dir: PathBuf,
    armed_at: std::time::Instant,
    ttl: std::time::Duration,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("boot-marker".into())
        .spawn(move || {
            std::thread::sleep(ttl.saturating_sub(armed_at.elapsed()));
            remove_boot_marker(&dir);
        })
}

/// 印の判定結果。
enum MarkerState {
    None,
    /// 電源断・再起動をまたいだ古い印(失敗と見ない)
    OtherBoot,
    /// 印を書いたプロセスがまだ動いている(失敗と見ない)
    Alive,
    /// 上書きで起動した直後に終了した。中身から世代が分かれば Some
    Failed(Option<String>),
}

fn read_marker(path: &Path) -> MarkerState {
    read_marker_with(path, current_boot_id().as_deref())
}

/// `now_boot` は今の boot_id(読めなければ None)。両方が取れて違うときだけ別の起動と見る。
fn read_marker_with(path: &Path, now_boot: Option<&str>) -> MarkerState {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return MarkerState::None,
        Err(_) => return MarkerState::Failed(None),
    };
    let Ok(m) = serde_json::from_str::<MarkerFile>(text.trim()) else {
        return MarkerState::Failed(None);
    };
    if matches!((m.boot_id.as_deref(), now_boot), (Some(a), Some(b)) if a != b) {
        MarkerState::OtherBoot
    } else if Path::new("/proc").join(m.pid.to_string()).exists() {
        MarkerState::Alive
    } else {
        MarkerState::Failed(Some(m.generation))
    }
}

/// 上書きファイルの世代(内容の SHA-256 の先頭 16 桁)。
pub(crate) fn overlay_generation(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 残った `<overlay>.tmp-*` を消す。
fn remove_stale_tmp(overlay_file: &Path) {
    let (Some(name), dir) = (
        overlay_file.file_name().and_then(|n| n.to_str()),
        boot_marker_dir(overlay_file),
    ) else {
        return;
    };
    let prefix = format!("{name}.tmp-");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 上書きファイルの先頭に付ける 1 行。
const OVERLAY_HEADER: &str = "# miharashi の管理ページが書くファイル。手で直してもよいが、ここに無い項目は書けない(起動時に無視される)。\n";

/// 保存の書き込みの途中経過のファイル名(`<overlay>.tmp-<pid>`)。
pub fn overlay_tmp_path(overlay_file: &Path) -> PathBuf {
    sibling(overlay_file, &format!("tmp-{}", std::process::id()))
}

/// 直前の上書き(`<overlay>.prev`)。
pub fn overlay_prev_path(overlay_file: &Path) -> PathBuf {
    sibling(overlay_file, "prev")
}

/// `overlay_file` と同じディレクトリの `<名前>.<suffix>`。
fn sibling(overlay_file: &Path, suffix: &str) -> PathBuf {
    let name = overlay_file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "admin.toml".into());
    overlay_file.with_file_name(format!("{name}.{suffix}"))
}

/// 上書きファイルの中身(先頭の 1 行コメント + TOML)。
pub fn render_overlay(overlay: &Overlay) -> Result<String, String> {
    // 何も入っていないテーブル(`[room]` だけ)は書かない
    let mut table =
        toml::Table::try_from(overlay).map_err(|e| format!("上書きを TOML にできない: {e}"))?;
    table.retain(|_, v| v.as_table().is_none_or(|t| !t.is_empty()));
    let body = toml::to_string(&table).map_err(|e| format!("上書きを TOML にできない: {e}"))?;
    Ok(format!("{OVERLAY_HEADER}{body}"))
}

/// 手順 1: `<overlay>.tmp-<pid>` を `create_new`(0600)で作って書き、`sync_all` する。
/// 既にあれば失敗する。書けなければ作ったファイルは消す。
pub fn write_overlay_tmp(overlay_file: &Path, text: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    let tmp = overlay_tmp_path(overlay_file);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts
        .open(&tmp)
        .map_err(|e| format!("{} を作れない: {e}", tmp.display()))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{} に書けない: {e}", tmp.display())
        })?;
    Ok(tmp)
}

/// 手順 2: 今の上書きファイルがあれば、`.prev` を消してからハードリンクで残す(コピーではない)。
/// 旧内容を残せなければ Err(置き換えない)。今の上書きファイルが無ければ何もしない(`.prev` は作らない)。
pub fn link_overlay_prev(overlay_file: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(overlay_file).is_err() {
        return Ok(());
    }
    let prev = overlay_prev_path(overlay_file);
    match std::fs::remove_file(&prev) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{} を消せない: {e}", prev.display())),
    }
    std::fs::hard_link(overlay_file, &prev)
        .map_err(|e| format!("{} に直前の内容を残せない: {e}", prev.display()))
}

/// 手順 3・4: `rename(tmp, overlay_file)` を 1 回して、ディレクトリを `sync_all` する。
pub fn commit_overlay(tmp: &Path, overlay_file: &Path) -> Result<(), String> {
    std::fs::rename(tmp, overlay_file).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        format!("{} に置き換えられない: {e}", overlay_file.display())
    })?;
    sync_dir(overlay_file)
}

/// `overlay_file` のあるディレクトリを `sync_all` する。
fn sync_dir(overlay_file: &Path) -> Result<(), String> {
    let dir = boot_marker_dir(overlay_file);
    std::fs::File::open(&dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{} を同期できない: {e}", dir.display()))
}

/// 上書きを保存する(管理ページの `PUT`)。どの時点で止まっても `overlay_file` は前の内容か新しい内容のどちらか。
/// 残った `<overlay>.tmp-*` は先に消す。`overlay` が空なら `.prev` を作ったあと `overlay_file` を削除する(tmp は作らない)。
/// 既存の上書きファイルがあるのに `.prev` を作れなければ、何も置き換えず Err(AC5)。
pub fn save_overlay(overlay_file: &Path, overlay: &Overlay) -> Result<(), String> {
    remove_stale_tmp(overlay_file);
    if overlay.is_empty() {
        if std::fs::symlink_metadata(overlay_file).is_err() {
            return Ok(());
        }
        link_overlay_prev(overlay_file)?;
        std::fs::remove_file(overlay_file)
            .map_err(|e| format!("{} を消せない: {e}", overlay_file.display()))?;
        return sync_dir(overlay_file);
    }
    let tmp = write_overlay_tmp(overlay_file, &render_overlay(overlay)?)?;
    if let Err(e) = link_overlay_prev(overlay_file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    commit_overlay(&tmp, overlay_file)
}

/// toml のエラーを 1 行にする(切り出された本文の抜粋は入れない)。
fn toml_error_line(e: &toml::de::Error, text: &str) -> String {
    match e.span() {
        Some(s) => format!(
            "{} 行目: {}",
            text[..s.start.min(text.len())].matches('\n').count() + 1,
            e.message()
        ),
        None => e.message().to_string(),
    }
}

impl OverlayReport {
    /// 上書きが効いているときの表示(check-config の stdout・起動ログ)。効いていなければ None。
    /// 例: `上書き: /var/lib/miharashi/admin.toml(2 項目)` の後に、2 文字下げた `key = value` が続く。
    pub fn applied_text(&self) -> Option<String> {
        if self.applied.is_empty() {
            return None;
        }
        let mut s = format!(
            "上書き: {}({} 項目)",
            self.path.display(),
            self.applied.len()
        );
        for (k, v) in &self.applied {
            s.push_str(&format!("\n  {k} = {v}"));
        }
        Some(s)
    }

    /// 上書きを無視したときの 1 行(stderr)。無視していなければ None。
    pub fn ignored_text(&self) -> Option<String> {
        let e = self.error.as_ref()?;
        Some(format!("上書き: {} を無視した: {e}", self.path.display()))
    }
}

impl LoadedConfig {
    /// `run` 用。上書きが効いているときは起動の印を書く。書けなければ上書きを使わず(effective = base)、
    /// `MarkerUnwritable` として理由を入れる(印なしで上書きを使うと、落ちる上書きで systemd が止まるため)。
    pub fn arm_boot_marker(&mut self) {
        if self.report.applied.is_empty() {
            return;
        }
        let dir = boot_marker_dir(&self.report.path);
        if let Err(e) = write_boot_marker(&dir, &self.report.generation) {
            self.effective = self.base.clone();
            self.report.applied.clear();
            self.report.error = Some(format!("起動の印を書けないので上書きを使わない: {e}"));
            self.report.error_kind = Some(OverlayErrorKind::MarkerUnwritable);
            return;
        }
        let armed_at = std::time::Instant::now();
        self.marker_armed_at = Some(armed_at);
        // 初期化と独立に 60 秒で消す(detach)。起こせなければ実行ループ側の削除に任せる
        if let Err(e) = spawn_boot_marker_timer(dir, armed_at, BOOT_MARKER_SECS) {
            eprintln!("印を消すスレッドを起こせない(実行ループで消す): {e}");
        }
    }
}

/// 設定ファイルの文面に書かれた agents.url(未記載・空なら None)。プログラムの既定値は含めない。
fn base_agents_url_of(base_text: &str) -> Option<String> {
    toml::from_str::<toml::Table>(base_text)
        .ok()
        .and_then(|t| {
            t.get("agents")?
                .as_table()?
                .get("url")?
                .as_str()
                .map(str::to_string)
        })
        .filter(|u| !u.is_empty())
}

/// base を読んだあとの共通部分。上書きを重ねて `LoadedConfig` を作る(失敗したら effective = base)。
fn build_loaded(
    base_path: Option<PathBuf>,
    base: Config,
    base_text: &str,
    consume_marker: bool,
) -> LoadedConfig {
    let base_agents_url = base_agents_url_of(base_text);
    let overlay_path = PathBuf::from(&base.admin.overlay_file);
    let marker_path = boot_marker_path(&overlay_path);
    let mut report = OverlayReport {
        path: overlay_path.clone(),
        exists: false,
        generation: "none".into(),
        applied: Vec::new(),
        error: None,
        error_kind: None,
        boot_failed_generation: None,
    };
    let mut loaded = LoadedConfig {
        base_path,
        base_agents_url,
        effective: base.clone(),
        base,
        report: report.clone(),
        marker_armed_at: None,
    };

    if consume_marker {
        remove_stale_tmp(&overlay_path);
    }
    let marker = read_marker(&marker_path);
    if consume_marker && matches!(marker, MarkerState::OtherBoot | MarkerState::Failed(_)) {
        let _ = std::fs::remove_file(&marker_path);
    }

    let fail = |report: &mut OverlayReport, kind: OverlayErrorKind, msg: String| {
        report.error = Some(msg);
        report.error_kind = Some(kind);
    };
    match std::fs::read(&overlay_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            report.exists = true;
            fail(
                &mut report,
                OverlayErrorKind::Parse,
                format!("読めない: {e}"),
            );
        }
        Ok(bytes) => {
            report.exists = true;
            report.generation = overlay_generation(&bytes);
            if let MarkerState::Failed(g) = marker {
                report.boot_failed_generation = g;
                fail(
                    &mut report,
                    OverlayErrorKind::BootFailed,
                    "上書きで起動した直後に終了した(原因が上書きとは限らない)ので、上書きを外して設定ファイルだけで起動した。直すか取り消してから再起動する".into(),
                );
            } else {
                match apply_overlay_bytes(&loaded, &bytes) {
                    Ok((cfg, applied)) => {
                        loaded.effective = cfg;
                        report.applied = applied;
                    }
                    Err((kind, msg)) => fail(&mut report, kind, msg),
                }
            }
        }
    }
    loaded.report = report;
    loaded
}

/// 上書きファイルの中身を読み、check → apply → validate を通した Config と効いた項目を返す。
fn apply_overlay_bytes(
    loaded: &LoadedConfig,
    bytes: &[u8],
) -> Result<(Config, Vec<(&'static str, String)>), (OverlayErrorKind, String)> {
    use OverlayErrorKind::*;
    let text =
        std::str::from_utf8(bytes).map_err(|e| (Parse, format!("UTF-8 として読めない: {e}")))?;
    let overlay: Overlay = toml::from_str(text).map_err(|e| {
        let line = toml_error_line(&e, text);
        if e.message().contains("unknown field") {
            (UnknownKey, format!("知らないキーがある({line})"))
        } else {
            (Parse, format!("TOML として読めない({line})"))
        }
    })?;
    overlay
        .check(&loaded.base, loaded.base_agents_url.as_deref())
        .map_err(|e| (Rule, e))?;
    let mut cfg = loaded.base.clone();
    overlay.apply(&mut cfg);
    cfg.validate()
        .map_err(|e| (Validate, format!("合わせた設定が検証に通らない: {e}")))?;
    Ok((cfg, overlay.entries()))
}

/// T3 の閲覧専用 Agent 一覧。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigAgents {
    pub enabled: bool,
    pub url: String,
    pub token_file: String,
    pub poll_secs: u64,
    pub timeout_secs: u64,
    pub stale_secs: u64,
    pub recent_hours: u64,
}

/// キューブ画面(Issue #74。回る立方体を GL で描く 3D の画面)。既定は無効。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Cube {
    pub enabled: bool,
    /// full(画面全体、id = cube)/ third(画面中央の 1/3 幅、id = cube-third)/ both(両方)
    pub layout: String,
    /// 動いている間の描画の上限 fps(0 = 制限せず vsync だけ)
    pub fps: u32,
}

/// VRM のキャラクターを写真・Agent 画面の脇に載せる(Issue #71 B。GLES のときだけ。既定は無効)。
/// モデルはリポジトリに置かず、`model` でパスを指定する。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Vrm {
    pub enabled: bool,
    /// VRM 1.0 の glb ファイルのパス。有効にするなら必須。ファイルが無い・壊れていても、キオスクは 3D なしで動く
    pub model: String,
    /// 載せる画面の id(photo / agents)。agents は agents.enabled = false なら出ない
    pub screens: Vec<String>,
    /// キャラクターを描く領域(画面の 1/3 幅): left / center / right
    pub region: String,
    /// 動いている間の描画の上限 fps(1〜60)
    pub fps: u32,
}

/// GL の状態をわざと汚す試験画面(`dirty-a`・`dirty-b`。実機の確認用)。既定は無効。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GlCheck {
    pub enabled: bool,
}

impl Vrm {
    /// キャラクターを描く領域。`region` は検証済みの前提(それ以外は left)。
    pub fn stage_region(&self) -> miharashi_vrm::StageRegion {
        use miharashi_vrm::StageRegion;
        match self.region.as_str() {
            "center" => StageRegion::Center,
            "right" => StageRegion::Right,
            _ => StageRegion::Left,
        }
    }
}

impl Default for Vrm {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            screens: vec!["photo".into(), "agents".into()],
            region: "left".into(),
            fps: 30,
        }
    }
}

impl Default for Cube {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: "both".into(),
            fps: 30,
        }
    }
}

impl Cube {
    /// (全面を出すか, 1/3 幅を出すか)。`layout` は検証済みの前提。
    pub fn layouts(&self) -> (bool, bool) {
        match self.layout.as_str() {
            "full" => (true, false),
            "third" => (false, true),
            _ => (true, true),
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            seat: "auto".into(),
            max_seconds: 0,
            stats_interval_secs: 60,
        }
    }
}

impl Default for Display {
    fn default() -> Self {
        Self {
            card: "auto".into(),
            connector: "auto".into(),
            mode: "preferred".into(),
            renderer: "cpu".into(),
        }
    }
}

impl Default for Geoframe {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8080".into(),
            photo_interval_secs: 30,
            reconnect_min_secs: 1,
            reconnect_max_secs: 60,
            home_radius_km: 50.0,
            card_secs: 10,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            interval_secs: 1,
            gpu: "auto".into(),
        }
    }
}

impl Default for Power {
    fn default() -> Self {
        Self {
            state_file: "/var/lib/miharashi/energy.json".into(),
            rapl_helper_file: "/run/miharashi-rapl/energy".into(),
            price_per_kwh: 0.0,
        }
    }
}

impl Default for HomeAssistant {
    fn default() -> Self {
        Self {
            url: String::new(),
            token_file: "/etc/miharashi/ha-token".into(),
        }
    }
}

impl Default for ScreenPower {
    fn default() -> Self {
        Self {
            presence_entity: String::new(),
            mode_entity: String::new(),
            idle_minutes: 10,
            override_decay_minutes: 5,
            poll_secs: 10,
        }
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            quake: true,
            quake_poll_secs: 30,
            disaster: true,
            disaster_poll_secs: 60,
            disaster_radius_km: 50.0,
            show_minutes: 5,
            demo_after_secs: 0,
        }
    }
}

/// 航空機の画面(写真の合間に挟む)。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Aircraft {
    pub enabled: bool,
    /// readsb/tar1090 の aircraft.json(受信機の位置は同じ場所の receiver.json)
    pub url: String,
    pub poll_secs: u64,
    /// 受信機からこの距離(km)以内の機体を出す
    pub radius_km: f64,
    /// 使わない(#5 の S3 で航空機の画面を独立させ、写真の合間に挟むのをやめた)。古い設定ファイルが
    /// 起動できるよう読むだけ読み、警告を出す
    pub every_photos: Option<u32>,
    /// 使わない(every_photos と同じ)
    pub show_secs: Option<u64>,
}

impl Default for Aircraft {
    fn default() -> Self {
        Self {
            enabled: true,
            url: "https://ads-b.dam-lab.work/tar1090/data/aircraft.json".into(),
            poll_secs: 5,
            radius_km: 50.0,
            every_photos: None,
            show_secs: None,
        }
    }
}

/// 室内環境と天気の画面(docs/DESIGN-screens.md §2.1・§6)。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Room {
    /// false なら画面ごと出さない。true でも、HA の url も forecast_area も無ければ出さない
    pub enabled: bool,
    /// 室内センサーのタイル(表示順。最大 6 枚)
    pub tiles: Vec<RoomTile>,
    /// 折れ線に出す過去の時間
    pub history_hours: u64,
    /// 天気予報の地域(気象庁 area.json の class10 のコード。"130010" など)。空なら自宅の位置から決める
    pub forecast_area: String,
    /// センサーの値を読む間隔(秒)
    pub poll_secs: u64,
    /// 折れ線の履歴を読み直す間隔(分)
    pub history_minutes: u64,
    /// 天気予報を読む間隔(分)。気象庁の発表は 1 日 3 回
    pub forecast_minutes: u64,
}

/// 室内センサーのタイル 1 枚。単位は HA のもの(mbar は hPa と書く)。
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RoomTile {
    pub label: String,
    pub entity: String,
    /// 値の右に並べる 2 つ目の entity(玄関の湿度など)
    #[serde(default)]
    pub second: String,
    /// この値以上で黄
    pub warn: Option<f64>,
    /// この値以上で赤
    pub alert: Option<f64>,
    /// この時間前からの変化で上昇・下降を出す(気圧)
    pub trend_hours: Option<u64>,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            enabled: true,
            tiles: Vec::new(),
            history_hours: 24,
            forecast_area: String::new(),
            poll_secs: 60,
            history_minutes: 10,
            forecast_minutes: 30,
        }
    }
}

/// 無人のときの自動巡回(docs/DESIGN-screens.md §4.2)。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Rotation {
    /// true なら起動時から巡回する(`ui.start_screen` ではなく最初の手順の画面から)
    pub enabled: bool,
    /// 画面 id と表示する秒数。上から順に繰り返す
    pub steps: Vec<RotationStep>,
    /// キー・操作パネルで操作したら、この秒数のあいだ巡回を止める
    pub pause_after_input_secs: u64,
}

/// 巡回の手順 1 つ。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RotationStep {
    pub screen: String,
    pub secs: u64,
    /// 画面に出すものが無ければ飛ばす(航空機の画面で、半径内に機体がいないとき)
    #[serde(default)]
    pub skip_if_empty: bool,
}

impl Default for Rotation {
    fn default() -> Self {
        let step = |screen: &str, secs, skip_if_empty| RotationStep {
            screen: screen.into(),
            secs,
            skip_if_empty,
        };
        Self {
            enabled: false,
            steps: vec![
                step("photo", 600, false),
                step("room", 60, false),
                step("photo", 600, false),
                step("aircraft", 60, true),
                step("status", 30, false),
            ],
            pause_after_input_secs: 300,
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            font: "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
            start_screen: "status".into(),
            refresh_minutes: 5,
            refresh_screens: vec!["aircraft".into()],
        }
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: String::new(),
            token_file: "/etc/miharashi/panel.token".into(),
            max_rate_per_sec: 20,
        }
    }
}

impl Default for ConfigAgents {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://192.168.0.132:3773".into(),
            token_file: "/etc/miharashi/t3-agent.token".into(),
            poll_secs: 5,
            timeout_secs: 3,
            stale_secs: 30,
            recent_hours: 24,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| format!("設定の読み込みに失敗: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// 設定ファイルの場所。明示パス → `$XDG_CONFIG_HOME/miharashi/config.toml` → `/etc/miharashi/config.toml`。
    /// 明示パスは存在を確かめずに返す。どれも無ければ None(既定値)。
    fn discover_path(explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit {
            return Some(p.to_path_buf());
        }
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        xdg.into_iter()
            .map(|d| d.join("miharashi/config.toml"))
            .chain([PathBuf::from("/etc/miharashi/config.toml")])
            .find(|p| p.exists())
    }

    /// 明示パス → `$XDG_CONFIG_HOME/miharashi/config.toml` → `/etc/miharashi/config.toml` → 既定値。
    pub fn discover(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>), String> {
        match Self::discover_path(explicit) {
            Some(p) => Ok((Self::load(&p)?, Some(p))),
            None => Ok((Self::default(), None)),
        }
    }

    /// `discover` と同じ場所の探し方で、上書きまで重ねて読む(設定ファイルが無ければ既定値)。
    pub fn discover_with_overlay(
        explicit: Option<&Path>,
        consume_marker: bool,
    ) -> Result<LoadedConfig, String> {
        match Self::discover_path(explicit) {
            Some(p) => Self::load_with_overlay(&p, consume_marker),
            None => Ok(build_loaded(None, Self::default(), "", consume_marker)),
        }
    }

    /// base を読んで検証し、`base.admin.overlay_file` があれば check → apply → validate して重ねる。
    /// どこかで失敗したら effective = base にし、理由を `report` に入れる(base 自体の失敗はそのまま Err)。
    /// 印(`overlay_file` と同じディレクトリの `admin.boot`)が残っていて、書いたプロセスが死んでいる
    /// (前回、上書きで起動して 60 秒以内に異常終了した)なら、上書きを読まずに無視する。
    /// `consume_marker` = true(run)なら印と残った tmp を消す。false(check-config、config-get)なら触らない。
    /// 印を書くのは呼び出し側(`LoadedConfig::arm_boot_marker`)。
    pub fn load_with_overlay(base: &Path, consume_marker: bool) -> Result<LoadedConfig, String> {
        let text = std::fs::read_to_string(base).map_err(|e| format!("{}: {e}", base.display()))?;
        let cfg = Self::parse(&text).map_err(|e| format!("{}: {e}", base.display()))?;
        Ok(build_loaded(
            Some(base.to_path_buf()),
            cfg,
            &text,
            consume_marker,
        ))
    }

    /// 設定ファイルだけを読み直す(管理ページの保存。上書きは重ねない)。戻り値: (base、文面の agents.url)。
    /// 設定ファイルが無い起動(`base_path` が None)は既定値を base にする。
    pub fn reload_base(base_path: Option<&Path>) -> Result<(Config, Option<String>), String> {
        let Some(p) = base_path else {
            return Ok((Self::default(), None));
        };
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let cfg = Self::parse(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok((cfg, base_agents_url_of(&text)))
    }

    /// `load_with_overlay` を今のファイルでやり直す(管理ページの「次回起動の値」)。印には触らない。
    /// 設定ファイルが無い起動(`base_path` が None)は既定値を base にする。
    pub fn reload_with_overlay(base_path: Option<&Path>) -> Result<LoadedConfig, String> {
        match base_path {
            Some(p) => Self::load_with_overlay(p, false),
            None => Ok(build_loaded(None, Self::default(), "", false)),
        }
    }

    /// 管理ページが扱うキー(`aircraft.enabled` など)と、`miharashi-t3-token` が読む `agents.token_file` の値を文字列で返す。知らないキーは None。
    pub fn value(&self, key: &str) -> Option<String> {
        Some(match key {
            "geoframe.url" => self.geoframe.url.clone(),
            "home_assistant.url" => self.home_assistant.url.clone(),
            "agents.url" => self.agents.url.clone(),
            "agents.token_file" => self.agents.token_file.clone(),
            "agents.enabled" => self.agents.enabled.to_string(),
            "aircraft.url" => self.aircraft.url.clone(),
            "aircraft.enabled" => self.aircraft.enabled.to_string(),
            "room.enabled" => self.room.enabled.to_string(),
            "cube.enabled" => self.cube.enabled.to_string(),
            "vrm.enabled" => self.vrm.enabled.to_string(),
            _ => return None,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        miharashi_linux_validate(self)?;
        if self.admin.enabled && !self.panel.enabled {
            return Err("admin.enabled = true には panel.enabled = true が必要".into());
        }
        if self.geoframe.reconnect_min_secs == 0
            || self.geoframe.reconnect_max_secs < self.geoframe.reconnect_min_secs
        {
            return Err(
                "geoframe: reconnect_min_secs は 1 以上、reconnect_max_secs は min 以上".into(),
            );
        }
        let a = &self.aircraft;
        if a.poll_secs < 2 || !(1.0..=400.0).contains(&a.radius_km) {
            return Err("aircraft.poll_secs は 2 以上、radius_km は 1〜400".into());
        }
        if a.every_photos.is_some() || a.show_secs.is_some() {
            eprintln!(
                "config: aircraft.every_photos と show_secs は使わなくなった(航空機は独立した画面)。消してよい"
            );
        }
        if self.geoframe.card_secs == 0 || !(1.0..=1000.0).contains(&self.geoframe.home_radius_km) {
            return Err("geoframe.card_secs は 1 以上、home_radius_km は 1〜1000".into());
        }
        if self.geoframe.photo_interval_secs == 0 || self.metrics.interval_secs == 0 {
            return Err("geoframe.photo_interval_secs と metrics.interval_secs は 1 以上".into());
        }
        if !self.power.price_per_kwh.is_finite() || self.power.price_per_kwh < 0.0 {
            return Err("power.price_per_kwh は 0 以上の数(0 = 電気代を表示しない)".into());
        }
        if !matches!(self.display.renderer.as_str(), "auto" | "cpu" | "gles") {
            return Err("display.renderer は auto / cpu / gles".into());
        }
        const START_SCREENS: [&str; 5] = ["status", "photo", "room", "aircraft", "agents"];
        // 3D の画面(キューブ、状態を汚す試験画面)。有効にしたものだけ起動時の画面に選べる
        const GL_SCREENS: [&str; 4] = ["cube", "cube-third", "dirty-a", "dirty-b"];
        let (full, third) = self.cube.layouts();
        let gl_enabled = |id: &str| match id {
            "cube" => self.cube.enabled && full,
            "cube-third" => self.cube.enabled && third,
            _ => self.gl_check.enabled,
        };
        if !matches!(self.cube.layout.as_str(), "full" | "third" | "both") {
            return Err("cube.layout は full / third / both".into());
        }
        if self.cube.fps > 240 {
            return Err("cube.fps は 0〜240(0 = 制限なし)".into());
        }
        // VRM は無効でも値の形は確かめる(有効にしたときに初めて typo が分かるのを避ける)。
        // モデルのパスは、有効なのに無いときだけエラー(panel.enabled の token_file と同じ)。
        // ファイルがあるかは確かめない: 無い・壊れているモデルは 3D なしで動かす約束で、起動を止めない
        let v = &self.vrm;
        if let Some(id) = v
            .screens
            .iter()
            .find(|id| !matches!(id.as_str(), "photo" | "agents"))
        {
            return Err(format!("vrm.screens に未知の画面 {id:?}(photo / agents)"));
        }
        if !matches!(v.region.as_str(), "left" | "center" | "right") {
            return Err("vrm.region は left / center / right".into());
        }
        if !(1..=60).contains(&v.fps) {
            return Err("vrm.fps は 1〜60".into());
        }
        if v.enabled && (v.model.is_empty() || v.screens.is_empty()) {
            return Err("vrm.enabled = true には vrm.model と vrm.screens が要る".into());
        }
        let start = self.ui.start_screen.as_str();
        if GL_SCREENS.contains(&start) {
            if !gl_enabled(start) {
                return Err(format!(
                    "ui.start_screen = {start} には cube.enabled / gl_check.enabled などで画面を有効にする必要がある"
                ));
            }
        } else if !START_SCREENS.contains(&start) {
            return Err("ui.start_screen は status / photo / room / aircraft / agents / cube / cube-third / dirty-a / dirty-b".into());
        }
        if self.ui.start_screen == "agents" && !self.agents.enabled {
            return Err("ui.start_screen = agents には agents.enabled = true が必要".into());
        }
        const REFRESH_SCREENS: [&str; 4] = ["status", "photo", "room", "aircraft"];
        if let Some(id) = self
            .ui
            .refresh_screens
            .iter()
            .find(|id| !REFRESH_SCREENS.contains(&id.as_str()))
        {
            return Err(format!(
                "ui.refresh_screens に未知または未対応の画面 {id:?}(status / photo / room / aircraft)"
            ));
        }
        let r = &self.room;
        if r.poll_secs < 10
            || !(1..=168).contains(&r.history_hours)
            || r.history_minutes == 0
            || r.forecast_minutes < 5
        {
            return Err(
                "room.poll_secs は 10 以上、history_hours は 1〜168、history_minutes は 1 以上、forecast_minutes は 5 以上"
                    .into(),
            );
        }
        if !r.forecast_area.is_empty()
            && !(r.forecast_area.len() == 6 && r.forecast_area.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(
                "room.forecast_area は気象庁の class10 のコード(6 桁。\"130010\" など)か空".into(),
            );
        }
        if let Some(t) = r
            .tiles
            .iter()
            .find(|t| t.label.is_empty() || t.entity.is_empty())
        {
            return Err(format!("room.tiles の label と entity は必須({t:?})"));
        }
        if r.tiles.len() > miharashi_render::room::MAX_TILES {
            eprintln!(
                "config: room.tiles は {} 枚まで出す(残りは出さない)",
                miharashi_render::room::MAX_TILES
            );
        }
        let rot = &self.rotation;
        if let Some(st) = rot.steps.iter().find(|st| {
            !START_SCREENS.contains(&st.screen.as_str())
                && !GL_SCREENS.contains(&st.screen.as_str())
        }) {
            return Err(format!(
                "rotation.steps に未知の画面 {:?}(status / photo / room / aircraft / agents / cube / cube-third / dirty-a / dirty-b)",
                st.screen
            ));
        }
        if rot.steps.iter().any(|st| st.secs < 5) {
            return Err("rotation.steps の secs は 5 以上".into());
        }
        if rot.enabled && rot.steps.is_empty() {
            return Err("rotation.enabled = true なら steps が要る".into());
        }
        check_http_url("geoframe.url", &self.geoframe.url)?;
        if !self.aircraft.url.is_empty() {
            check_http_url("aircraft.url", &self.aircraft.url)?;
        }
        for h in &self.admin.allowed_hosts {
            parse_allowed_host(h)?;
        }
        if !self.home_assistant.url.is_empty() {
            crate::screen_power::check_url(&self.home_assistant.url)
                .map_err(|e| format!("home_assistant.url: {e}"))?;
        }
        if self.alerts.quake_poll_secs < 10 || self.alerts.show_minutes == 0 {
            return Err("alerts.quake_poll_secs は 10 以上、show_minutes は 1 以上".into());
        }
        if self.alerts.disaster_poll_secs < 30
            || !(1.0..=300.0).contains(&self.alerts.disaster_radius_km)
        {
            return Err(
                "alerts.disaster_poll_secs は 30 以上、disaster_radius_km は 1〜300".into(),
            );
        }
        let sp = &self.screen_power;
        if sp.idle_minutes == 0 || sp.override_decay_minutes == 0 || sp.poll_secs == 0 {
            return Err(
                "screen_power の idle_minutes・override_decay_minutes・poll_secs は 1 以上".into(),
            );
        }
        if self.panel.enabled {
            crate::panel::check_listen(&self.panel.listen)
                .map_err(|e| format!("panel.listen: {e}"))?;
            if self.panel.max_rate_per_sec == 0 || self.panel.token_file.is_empty() {
                return Err("panel.max_rate_per_sec は 1 以上、token_file は必須".into());
            }
        }
        if self.agents.enabled {
            crate::agents::check_url(&self.agents.url).map_err(|e| format!("agents.url: {e}"))?;
            if self.agents.token_file.is_empty()
                || !(1..=3600).contains(&self.agents.poll_secs)
                || !(1..=60).contains(&self.agents.timeout_secs)
                || !(1..=86400).contains(&self.agents.stale_secs)
                || !(1..=8760).contains(&self.agents.recent_hours)
                || self.agents.stale_secs < self.agents.poll_secs
            {
                return Err("agents.token_file は必須、poll_secs は 1〜3600、timeout_secs は 1〜60、stale_secs は poll_secs 以上かつ 86400 以下、recent_hours は 1〜8760".into());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn miharashi_linux_validate(c: &Config) -> Result<(), String> {
    miharashi_linux::seat::SeatMode::parse(&c.runtime.seat)?;
    miharashi_linux::drm_select::ModeSel::parse(&c.display.mode)?;
    miharashi_linux::metrics::GpuSel::parse(&c.metrics.gpu)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn miharashi_linux_validate(_: &Config) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_defaults_and_rules() {
        let a = Config::parse("").unwrap().admin;
        assert!(!a.enabled);
        assert_eq!(a.token_file, "/etc/miharashi/admin.token");
        assert_eq!(a.overlay_file, "/var/lib/miharashi/admin.toml");
        assert!(a.allowed_hosts.is_empty());
        let e = Config::parse("[admin]\nenabled = true\n").unwrap_err();
        assert!(e.contains("panel.enabled"), "{e}");
        assert!(Config::parse("[admin]\nnope = 1\n").is_err());
        let ok = "[panel]\nenabled = true\nlisten = \"192.0.2.5:8095\"\n[admin]\nenabled = true\n";
        assert!(Config::parse(ok).unwrap().admin.enabled);
    }

    #[test]
    fn overlay_apply_entries_and_is_empty() {
        assert!(Overlay::default().is_empty());
        assert!(Overlay::default().entries().is_empty());
        let o: Overlay = toml::from_str(
            "[aircraft]\nenabled = false\n[agents]\nurl = \"http://192.0.2.30:3773\"\n[cube]\nenabled = true\n",
        )
        .unwrap();
        assert!(!o.is_empty());
        let mut c = Config::default();
        o.apply(&mut c);
        assert!(!c.aircraft.enabled);
        assert_eq!(c.agents.url, "http://192.0.2.30:3773");
        assert!(c.cube.enabled);
        assert_eq!(c.geoframe.url, Config::default().geoframe.url);
        assert_eq!(
            o.entries(),
            vec![
                ("agents.url", "http://192.0.2.30:3773".to_string()),
                ("aircraft.enabled", "false".to_string()),
                ("cube.enabled", "true".to_string()),
            ]
        );
        // 内側の知らないキーも拒否する
        assert!(toml::from_str::<Overlay>("[room]\nurl = \"x\"\n").is_err());
        assert!(toml::from_str::<Overlay>("[home_assistant]\nurl = \"x\"\n").is_err());
    }

    #[test]
    fn config_value_keys() {
        let c = Config::default();
        for k in [
            "geoframe.url",
            "home_assistant.url",
            "agents.url",
            "agents.enabled",
            "aircraft.url",
            "aircraft.enabled",
            "room.enabled",
            "cube.enabled",
            "vrm.enabled",
        ] {
            assert!(c.value(k).is_some(), "{k}");
        }
        assert_eq!(c.value("aircraft.enabled").as_deref(), Some("true"));
        assert_eq!(c.value("agents.enabled").as_deref(), Some("false"));
        assert_eq!(c.value("panel.token_file"), None);
    }

    // ---- 管理ページの上書き(docs/admin-web/design.md) ----

    /// 一時ディレクトリ。Drop で消す。
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let d =
                std::env::temp_dir().join(format!("miharashi-cfg-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
        fn path(&self, f: &str) -> PathBuf {
            self.0.join(f)
        }
        fn write(&self, f: &str, text: &str) -> PathBuf {
            let p = self.path(f);
            std::fs::write(&p, text).unwrap();
            p
        }
        /// overlay_file をこのディレクトリに向けた設定ファイルを書く(`extra` は他のセクション)
        fn base(&self, extra: &str) -> PathBuf {
            let text = format!(
                "[admin]\noverlay_file = \"{}\"\n{extra}",
                self.path("admin.toml").display()
            );
            self.write("config.toml", &text)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 存在しない pid(pid_max より大きい)
    const DEAD_PID: u32 = 2_000_000_000;

    fn marker_json(pid: u32, boot_id: &str, generation: &str) -> String {
        format!(
            "{{\"pid\":{pid},\"boot_id\":\"{boot_id}\",\"generation\":\"{generation}\",\"started_at\":\"2026-10-31T00:00:00+09:00\"}}\n"
        )
    }

    #[test]
    fn ac3_overlay_wins_over_base() {
        let t = Tmp::new("ac3");
        let a = "https://adsb-a.example.test/tar1090/data/aircraft.json";
        let b = "https://adsb-b.example.test/tar1090/data/aircraft.json";
        let base = t.base(&format!("[aircraft]\nurl = \"{a}\"\n"));
        t.write("admin.toml", &format!("[aircraft]\nurl = \"{b}\"\n"));
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!(l.effective.aircraft.url, b);
        assert_eq!(l.base.aircraft.url, a);
        assert_eq!(l.report.applied, vec![("aircraft.url", b.to_string())]);
        assert!(l.report.exists);
        assert_eq!(l.report.error, None);
        assert_eq!(l.report.generation.len(), 16);
        assert_eq!(l.base_path.as_deref(), Some(base.as_path()));
        // 上書きが無ければ base と同じ
        std::fs::remove_file(t.path("admin.toml")).unwrap();
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!(l.effective, l.base);
        assert!(!l.report.exists && l.report.applied.is_empty());
        assert_eq!(l.report.generation, "none");
    }

    #[test]
    fn overlay_generation_follows_the_content() {
        let t = Tmp::new("gen");
        let base = t.base("");
        t.write("admin.toml", "[room]\nenabled = false\n");
        let g1 = Config::load_with_overlay(&base, false)
            .unwrap()
            .report
            .generation;
        t.write("admin.toml", "[room]\nenabled = true\n");
        let g2 = Config::load_with_overlay(&base, false)
            .unwrap()
            .report
            .generation;
        assert_ne!(g1, g2);
        assert!(g1.bytes().all(|b| b.is_ascii_hexdigit()));
        // SHA-256("") の先頭 16 桁
        assert_eq!(overlay_generation(b""), "e3b0c44298fc1c14");
    }

    fn overlay(text: &str) -> Overlay {
        toml::from_str(text).unwrap()
    }

    fn check_agents(base_url: Option<&str>, allowed: &[&str], url: &str) -> Result<(), String> {
        let mut base = Config::default();
        base.admin.allowed_hosts = allowed.iter().map(|s| s.to_string()).collect();
        overlay(&format!("[agents]\nurl = \"{url}\"\n")).check(&base, base_url)
    }

    #[test]
    fn ac6_overlay_rejects_unknown_keys_and_bad_urls() {
        // 知らないキー(外側・内側・home_assistant)
        for t in [
            "[panel]\nlisten = \"192.0.2.5:8095\"\n",
            "[agents]\ntoken_file = \"/x\"\n",
            "[geoframe]\nphoto_interval_secs = 1\n",
            "[home_assistant]\nurl = \"http://192.0.2.10:8123\"\n",
        ] {
            let e = toml::from_str::<Overlay>(t).unwrap_err();
            assert!(e.message().contains("unknown field"), "{t}: {e}");
        }
        let base = Config::default();
        // 共通の文字の検査(3 つの url すべて)。TOML の文字列にはせず、構造体に直接入れる
        let with = |field: &str, url: &str| {
            let mut o = Overlay::default();
            match field {
                "geoframe" => o.geoframe.url = Some(url.into()),
                "aircraft" => o.aircraft.url = Some(url.into()),
                _ => o.agents.url = Some(url.into()),
            }
            o
        };
        for bad in [
            "http://u:p@192.0.2.1/",
            "http://192.0.2.1/a b",
            "http://192.0.2.1/\"x",
            "http://192.0.2.1/\u{1}",
            "http://192.0.2.1\\x",
            "ftp://192.0.2.1/",
            "192.0.2.1",
            "",
            "http://",
            "https://:443",
            "http://192.0.2.1:",
            "http://192.0.2.1:abc/",
            "http://192.0.2.1:99999/",
            "http://exa%6Dple.test/",
            "http://ex\u{e4}mple.test/",
        ] {
            for field in ["geoframe", "aircraft"] {
                assert!(
                    with(field, bad).check(&base, None).is_err(),
                    "{field}: {bad:?}"
                );
            }
            let o = with("agents", bad);
            assert!(
                o.check(&base, Some("http://192.0.2.30:3773")).is_err(),
                "agents: {bad:?}"
            );
        }
        // 良い url
        for field in ["geoframe", "aircraft"] {
            let o = overlay(&format!(
                "[{field}]\nurl = \"https://adsb.example.test:8443/a/b.json\"\n"
            ));
            o.check(&base, None).unwrap();
        }
        // agents.enabled = false でも agents.url は検査される(平文 http のホスト名は check_url が拒否)
        let o = overlay("[agents]\nenabled = false\nurl = \"http://t3.example.test:3773\"\n");
        let mut b = Config::default();
        b.admin.allowed_hosts = vec!["t3.example.test:3773".into()];
        let e = o.check(&b, None).unwrap_err();
        assert!(e.contains("agents.url") && e.contains("IP"), "{e}");
        // 無効な画面でも、パス・クエリ・fragment 付きの T3 の URL は拒否する
        for ng in [
            "http://192.0.2.30:3773/x",
            "http://192.0.2.30:3773?x",
            "http://192.0.2.30:3773#x",
        ] {
            let o = overlay(&format!("[agents]\nenabled = false\nurl = \"{ng}\"\n"));
            let e = o
                .check(&Config::default(), Some("http://192.0.2.30:3773"))
                .unwrap_err();
            assert!(e.contains("agents.url"), "{ng}: {e}");
        }
        // 許可されない origin
        let e = check_agents(
            Some("http://192.0.2.30:3773"),
            &[],
            "http://192.0.2.31:3773",
        )
        .unwrap_err();
        assert!(
            e.contains("agents.url") && e.contains("許可されていない"),
            "{e}"
        );
    }

    #[test]
    fn ac7_agents_url_origin_rules() {
        let base = Some("http://192.0.2.30:3773");
        let allowed = ["t3.example.test", "192.0.2.40:4000"];
        // Ok
        for ok in [
            "http://192.0.2.30:3773/",
            "http://192.0.2.30:3773",
            "https://t3.example.test/",
            "http://192.0.2.40:4000",
            "https://192.0.2.40:4000",
        ] {
            check_agents(base, &allowed, ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        // Err: ポート違い・別ホスト・port 無しの要素は既定ポートだけ。T3 の URL は origin だけ(パス・クエリ・fragment 付きも不可)
        for ng in [
            "http://192.0.2.30:3773/x",
            "http://192.0.2.30:3773/x/",
            "http://192.0.2.30:3773?x",
            "http://192.0.2.30:3773/?x",
            "http://192.0.2.30:3773#x",
            "https://t3.example.test/x",
            "http://192.0.2.30:4000",
            "http://192.0.2.31:3773",
            "https://t3.example.test:8443",
            "https://192.0.2.30:3773",
            "http://192.0.2.40:4001",
        ] {
            assert!(check_agents(base, &allowed, ng).is_err(), "{ng}");
        }
        // 設定ファイルが https のとき http は格下げで Err
        let e = check_agents(Some("https://192.0.2.30"), &[], "http://192.0.2.30").unwrap_err();
        assert!(e.contains("格下げ"), "{e}");
        check_agents(Some("https://192.0.2.30"), &[], "https://192.0.2.30:443/").unwrap();
        // allowed_hosts に書いてあっても、設定ファイルが https なら http にはできない(同じホストも別のホストも)。https なら通る
        let https_base = Some("https://192.0.2.40:4000");
        let allowed_https = ["192.0.2.40:4000", "192.0.2.41"];
        for ng in ["http://192.0.2.40:4000", "http://192.0.2.41"] {
            let e = check_agents(https_base, &allowed_https, ng).unwrap_err();
            assert!(e.contains("格下げ"), "{ng}: {e}");
        }
        check_agents(https_base, &allowed_https, "https://192.0.2.41").unwrap();
        // 設定ファイルが http なら、allowed_hosts に書いた http はこれまでどおり通る
        check_agents(base, &allowed_https, "http://192.0.2.40:4000").unwrap();
        // [agents] が無い(None)と url = ""(None)では、既定値のホストは使えず allowed_hosts だけが通る
        // (既定のホストは IP のリテラルを書かず、プログラムの既定値から取る)
        let default_host = ConfigAgents::default().url;
        assert!(check_agents(None, &[], &default_host).is_err());
        assert!(check_agents(None, &["192.0.2.40:4000"], &default_host).is_err());
        check_agents(None, &["192.0.2.40:4000"], "http://192.0.2.40:4000").unwrap();
        // 空ホスト
        assert!(check_agents(base, &allowed, "https://:443").is_err());
        // 表記ゆれ
        assert!(check_agents(base, &allowed, "http://192.000.002.030:3773").is_err());
        assert!(check_agents(base, &allowed, "http://192.0.2.30.:3773").is_err());
        assert!(check_agents(base, &allowed, "http://[::ffff:192.0.2.30]:3773").is_err());
        assert!(check_agents(base, &["t3.example.test"], "https://t3.example.test./").is_err());
        check_agents(base, &["T3.Example.Test"], "https://t3.EXAMPLE.test/").unwrap();
        // IPv6 は IpAddr で比べる
        check_agents(
            Some("http://[2001:db8::1]:3773"),
            &[],
            "http://[2001:db8:0::1]:3773",
        )
        .unwrap();
        check_agents(
            Some("http://[2001:db8:0::1]:3773"),
            &[],
            "http://[2001:DB8::1]:3773",
        )
        .unwrap();
        check_agents(
            None,
            &["[2001:db8::2]:4000"],
            "http://[2001:db8:0:0::2]:4000",
        )
        .unwrap();
        assert!(
            check_agents(
                Some("http://[2001:db8::1]:3773"),
                &[],
                "http://[2001:db8::2]:3773"
            )
            .is_err()
        );
    }

    #[test]
    fn ac7_load_ignores_a_disallowed_overlay_with_a_reason() {
        let t = Tmp::new("ac7load");
        let base = t.base("[agents]\nurl = \"http://192.0.2.30:3773\"\n");
        t.write(
            "admin.toml",
            "[agents]\nurl = \"http://192.0.2.31:3773\"\n[room]\nenabled = false\n",
        );
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!(l.report.error_kind, Some(OverlayErrorKind::Rule));
        assert!(
            l.report
                .error
                .as_deref()
                .unwrap()
                .contains("許可されていない")
        );
        assert_eq!(l.effective, l.base);
        assert!(l.report.applied.is_empty());
        assert_eq!(l.base_agents_url.as_deref(), Some("http://192.0.2.30:3773"));
        // 許可された origin は効く(base の agents.url が空のときは None)
        t.write(
            "admin.toml",
            "[agents]\nurl = \"http://192.0.2.30:3773/\"\n",
        );
        let l = Config::load_with_overlay(&base, false).unwrap();
        // 末尾の "/" だけは origin と同じに扱われる
        assert_eq!(l.report.error, None, "{:?}", l.report.error);
        let base = t.base("[agents]\nurl = \"\"\n");
        assert_eq!(
            Config::load_with_overlay(&base, false)
                .unwrap()
                .base_agents_url,
            None
        );
        let base = t.base("");
        assert_eq!(
            Config::load_with_overlay(&base, false)
                .unwrap()
                .base_agents_url,
            None
        );
        // 設定ファイルが https なら、allowed_hosts に書いた http の上書きも無視する(読み込み時にも格下げは不可)
        let https_base = t.base(
            "allowed_hosts = [\"192.0.2.40:4000\"]\n[agents]\nurl = \"https://192.0.2.40:4000\"\n",
        );
        t.write(
            "admin.toml",
            "[agents]\nurl = \"http://192.0.2.40:4000\"\n[room]\nenabled = false\n",
        );
        let l = Config::load_with_overlay(&https_base, false).unwrap();
        assert_eq!(l.report.error_kind, Some(OverlayErrorKind::Rule));
        assert!(l.report.error.as_deref().unwrap().contains("格下げ"));
        assert_eq!(l.effective, l.base);
        assert!(l.report.applied.is_empty());
    }

    #[test]
    fn ac12_broken_overlays_fall_back_to_base() {
        let t = Tmp::new("ac12");
        let base = t.base("");
        let cases = [
            ("[room\nenabled = ", OverlayErrorKind::Parse),
            ("[room]\nenabled = \"yes\"\n", OverlayErrorKind::Parse),
            (
                "[agents]\ntoken_file = \"/x\"\n",
                OverlayErrorKind::UnknownKey,
            ),
            (
                "[home_assistant]\nurl = \"http://192.0.2.10\"\n",
                OverlayErrorKind::UnknownKey,
            ),
            // agents を有効にするのに token_file などは通るが、ui.start_screen との整合が壊れる
            ("[agents]\nenabled = false\n", OverlayErrorKind::Validate),
        ];
        for (text, kind) in cases {
            t.write("admin.toml", text);
            let extra = if kind == OverlayErrorKind::Validate {
                "[agents]\nenabled = true\n[ui]\nstart_screen = \"agents\"\n"
            } else {
                ""
            };
            let base = if extra.is_empty() {
                base.clone()
            } else {
                t.base(extra)
            };
            let l = Config::load_with_overlay(&base, false).unwrap();
            assert_eq!(
                l.report.error_kind,
                Some(kind),
                "{text}: {:?}",
                l.report.error
            );
            let e = l.report.error.as_deref().unwrap();
            assert!(!e.is_empty() && !e.contains('\n'), "{e}");
            assert_eq!(l.effective, l.base, "{text}");
            assert!(l.report.applied.is_empty());
            assert!(l.report.exists);
            let line = l.report.ignored_text().unwrap();
            assert_eq!(
                line,
                format!("上書き: {} を無視した: {e}", t.path("admin.toml").display())
            );
        }
        // Validate の理由は検証の文を含む
        let l = Config::load_with_overlay(
            &t.base("[agents]\nenabled = true\n[ui]\nstart_screen = \"agents\"\n"),
            false,
        )
        .unwrap();
        assert!(
            l.report.error.as_deref().unwrap().contains("agents"),
            "{:?}",
            l.report.error
        );
        // base が壊れていれば今までどおり Err
        let bad = t.write("bad.toml", "nope = 1\n");
        assert!(Config::load_with_overlay(&bad, false).is_err());
        assert!(Config::load_with_overlay(&t.path("missing.toml"), false).is_err());
    }

    fn read_overlay_text(t: &Tmp, name: &str) -> String {
        std::fs::read_to_string(t.path(name)).unwrap()
    }

    fn overlay_of(text: &str) -> Overlay {
        toml::from_str(text).unwrap()
    }

    fn tmp_files(t: &Tmp) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&t.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("admin.toml.tmp-"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn ac5_render_has_the_comment_and_only_the_set_keys() {
        let text = render_overlay(&overlay_of(
            "[aircraft]\nenabled = false\n[geoframe]\nurl = \"http://192.0.2.20:8080\"\n",
        ))
        .unwrap();
        assert!(
            text.starts_with("# miharashi の管理ページが書くファイル。"),
            "{text}"
        );
        assert!(text.contains("enabled = false") && text.contains("192.0.2.20"));
        assert!(!text.contains("room") && !text.contains("cube"), "{text}");
        assert_eq!(
            overlay_of(&text),
            overlay_of(
                "[aircraft]\nenabled = false\n[geoframe]\nurl = \"http://192.0.2.20:8080\"\n"
            )
        );
        // 空でもコメントだけで、TOML として読める
        let empty = render_overlay(&Overlay::default()).unwrap();
        assert!(overlay_of(&empty).is_empty(), "{empty}");
    }

    #[test]
    fn ac5_every_stage_of_the_write_leaves_the_old_or_the_new_overlay() {
        use std::os::unix::fs::MetadataExt;
        let t = Tmp::new("ac5-stages");
        let base = t.base("");
        let file = t.path("admin.toml");
        let prev = overlay_prev_path(&file);
        let old = overlay_of("[room]\nenabled = false\n");
        let new = overlay_of("[room]\nenabled = true\n[cube]\nenabled = true\n");
        save_overlay(&file, &old).unwrap();
        let old_text = read_overlay_text(&t, "admin.toml");
        assert!(!prev.exists(), "最初の保存では .prev は作らない");
        let room = |l: &LoadedConfig| (l.effective.room.enabled, l.effective.cube.enabled);

        // (a) tmp を書いた直後: 上書きファイルは前の内容のまま
        let tmp = write_overlay_tmp(&file, &render_overlay(&new).unwrap()).unwrap();
        assert_eq!(tmp, overlay_tmp_path(&file));
        assert_eq!(read_overlay_text(&t, "admin.toml"), old_text);
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!((l.report.error.clone(), room(&l)), (None, (false, false)));
        assert_eq!(tmp_files(&t).len(), 1, "読むだけでは tmp を消さない");
        assert_eq!(std::fs::metadata(&tmp).unwrap().mode() & 0o777, 0o600);
        // tmp が既にあれば、書き込みは失敗する(create_new)
        assert!(write_overlay_tmp(&file, "x").is_err());
        assert_eq!(
            std::fs::read_to_string(&tmp).unwrap(),
            render_overlay(&new).unwrap()
        );
        // 起動(印を扱う run)が残った tmp を消す
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!((l.report.error.clone(), room(&l)), (None, (false, false)));
        assert!(tmp_files(&t).is_empty());

        // (b) .prev を作った直後: まだ前の内容。.prev は前の内容
        let tmp = write_overlay_tmp(&file, &render_overlay(&new).unwrap()).unwrap();
        link_overlay_prev(&file).unwrap();
        assert_eq!(read_overlay_text(&t, "admin.toml"), old_text);
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), old_text);
        assert_eq!(
            std::fs::metadata(&prev).unwrap().ino(),
            std::fs::metadata(&file).unwrap().ino(),
            "コピーではなくリンク"
        );
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!((l.report.error.clone(), room(&l)), (None, (false, false)));

        // (c) rename 後: 新しい内容。.prev は前の内容のまま、tmp は無い
        commit_overlay(&tmp, &file).unwrap();
        assert!(tmp_files(&t).is_empty());
        assert_eq!(overlay_of(&read_overlay_text(&t, "admin.toml")), new);
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), old_text);
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!((l.report.error.clone(), room(&l)), (None, (true, true)));
    }

    #[test]
    fn ac5_save_overlay_keeps_the_previous_content_and_clears_stale_tmp() {
        let t = Tmp::new("ac5-save");
        let file = t.path("admin.toml");
        let a = overlay_of("[room]\nenabled = false\n");
        let b = overlay_of("[room]\nenabled = true\n");
        // 同じ pid の tmp と別の pid の tmp が残っていても、保存は成功して消える
        t.write(&format!("admin.toml.tmp-{}", std::process::id()), "stale");
        t.write("admin.toml.tmp-1", "stale");
        t.write("admin.toml.other", "keep");
        save_overlay(&file, &a).unwrap();
        assert!(tmp_files(&t).is_empty());
        assert!(
            t.path("admin.toml.other").exists(),
            "関係ないファイルには触らない"
        );
        let a_text = read_overlay_text(&t, "admin.toml");
        save_overlay(&file, &b).unwrap();
        assert_eq!(overlay_of(&read_overlay_text(&t, "admin.toml")), b);
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), a_text);
        // 古い .prev があっても置き換わる
        let b_text = read_overlay_text(&t, "admin.toml");
        save_overlay(&file, &a).unwrap();
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), b_text);
        // 空 → admin.toml を消し、.prev は消す直前の内容。tmp は作らない
        let a_text = read_overlay_text(&t, "admin.toml");
        save_overlay(&file, &Overlay::default()).unwrap();
        assert!(!file.exists());
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), a_text);
        assert!(tmp_files(&t).is_empty());
        // 上書きが無いところへ空を保存しても何も起きず、.prev も変わらない
        save_overlay(&file, &Overlay::default()).unwrap();
        assert!(!file.exists());
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), a_text);
        // ディレクトリが無ければ失敗し、何も残さない
        let gone = t.path("no-such-dir/admin.toml");
        assert!(save_overlay(&gone, &a).is_err());
        assert!(!t.path("no-such-dir").exists());
    }

    #[test]
    fn ac5_save_fails_without_replacing_when_prev_cannot_be_made() {
        let t = Tmp::new("ac5-prev-fail");
        let file = t.path("admin.toml");
        let a = overlay_of("[room]\nenabled = false\n");
        let b = overlay_of("[room]\nenabled = true\n");
        // 既存の admin.toml が無ければ .prev は作らないまま保存できる
        save_overlay(&file, &a).unwrap();
        assert!(!overlay_prev_path(&file).exists());
        let a_text = read_overlay_text(&t, "admin.toml");
        // .prev が(空でない)ディレクトリで消せない: 保存は Err、admin.toml は前の内容のまま、tmp も残らない
        let prev = overlay_prev_path(&file);
        std::fs::create_dir(&prev).unwrap();
        std::fs::write(prev.join("keep"), "x").unwrap();
        let e = save_overlay(&file, &b).unwrap_err();
        assert!(e.contains("admin.toml.prev"), "{e}");
        assert_eq!(read_overlay_text(&t, "admin.toml"), a_text);
        assert!(tmp_files(&t).is_empty());
        assert!(prev.join("keep").exists());
        // 空の保存(取り消し)も、.prev を残せなければ admin.toml を消さない
        assert!(save_overlay(&file, &Overlay::default()).is_err());
        assert_eq!(read_overlay_text(&t, "admin.toml"), a_text);
        // .prev が直れば保存できる
        std::fs::remove_dir_all(&prev).unwrap();
        save_overlay(&file, &b).unwrap();
        assert_eq!(overlay_of(&read_overlay_text(&t, "admin.toml")), b);
        assert_eq!(read_overlay_text(&t, "admin.toml.prev"), a_text);
    }

    #[test]
    fn ac13_boot_marker_write_remove_and_verdict() {
        let t = Tmp::new("ac13");
        let base = t.base("");
        t.write("admin.toml", "[room]\nenabled = false\n");
        let marker = boot_marker_path(&t.path("admin.toml"));
        assert_eq!(marker, t.path("admin.boot"));
        let boot = current_boot_id().unwrap_or_else(|| "test-boot".into());

        // (b) consume = false では残る。(a) consume = true で BootFailed と世代、印が消える
        std::fs::write(&marker, marker_json(DEAD_PID, &boot, "abc123")).unwrap();
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!(l.report.error_kind, Some(OverlayErrorKind::BootFailed));
        assert_eq!(l.report.boot_failed_generation.as_deref(), Some("abc123"));
        assert!(
            l.report
                .error
                .as_deref()
                .unwrap()
                .contains("原因が上書きとは限らない")
        );
        assert_eq!(l.effective, l.base);
        assert!(l.report.applied.is_empty());
        assert!(marker.exists(), "consume = false は印に触らない");
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error_kind, Some(OverlayErrorKind::BootFailed));
        assert!(!marker.exists(), "consume = true は印を消す");
        // 次の起動は上書きが効く
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error, None);
        assert!(!l.effective.room.enabled);

        // (c) 生きている pid(自分)は失敗と見ない。印も消さない
        std::fs::write(&marker, marker_json(std::process::id(), &boot, "abc123")).unwrap();
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error, None);
        assert!(!l.effective.room.enabled);
        assert!(marker.exists());

        // (d) boot_id が違う印は失敗と見ない(consume なら消す)
        std::fs::write(
            &marker,
            marker_json(DEAD_PID, "00000000-other-boot", "abc123"),
        )
        .unwrap();
        let l = Config::load_with_overlay(&base, false).unwrap();
        assert_eq!(l.report.error, None);
        assert!(marker.exists());
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error, None);
        assert!(!marker.exists());

        // 壊れた印は失敗と見る(世代は分からない)
        std::fs::write(&marker, "not json").unwrap();
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error_kind, Some(OverlayErrorKind::BootFailed));
        assert_eq!(l.report.boot_failed_generation, None);

        // 上書きが無ければ印があっても何も無視しない
        std::fs::write(&marker, marker_json(DEAD_PID, &boot, "abc123")).unwrap();
        std::fs::remove_file(t.path("admin.toml")).unwrap();
        let l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.error, None);
        assert!(!marker.exists());
    }

    #[test]
    fn ac13_unreadable_boot_id_falls_back_to_pid() {
        let t = Tmp::new("ac13b");
        let marker = t.path("admin.boot");
        // 今の boot_id が取れない(None)ときは OtherBoot にせず pid で判定する
        std::fs::write(&marker, marker_json(DEAD_PID, "some-boot", "abc123")).unwrap();
        assert!(matches!(
            read_marker_with(&marker, None),
            MarkerState::Failed(Some(g)) if g == "abc123"
        ));
        std::fs::write(
            &marker,
            marker_json(std::process::id(), "some-boot", "abc123"),
        )
        .unwrap();
        assert!(matches!(
            read_marker_with(&marker, None),
            MarkerState::Alive
        ));
        // 印に boot_id が無い(null・欠落)ときも pid で判定する
        std::fs::write(
            &marker,
            format!("{{\"pid\":{DEAD_PID},\"boot_id\":null,\"generation\":\"abc123\"}}\n"),
        )
        .unwrap();
        assert!(matches!(
            read_marker_with(&marker, Some("now-boot")),
            MarkerState::Failed(Some(_))
        ));
        // 両方が取れて違うときだけ OtherBoot
        std::fs::write(&marker, marker_json(DEAD_PID, "some-boot", "abc123")).unwrap();
        assert!(matches!(
            read_marker_with(&marker, Some("now-boot")),
            MarkerState::OtherBoot
        ));
        assert!(matches!(
            read_marker_with(&marker, Some("some-boot")),
            MarkerState::Failed(Some(_))
        ));
    }

    #[test]
    fn t1_base_url_with_userinfo_loads_but_overlay_rejects_it() {
        // base の geoframe.url / aircraft.url は userinfo 付きでも読める(上書きが無くても起動エラーにしない)
        let c = Config::parse(
            "[geoframe]\nurl = \"http://user:pass@192.0.2.20:8080\"\n[aircraft]\nurl = \"https://u:p@192.0.2.21:8443/a.json\"\n",
        )
        .unwrap();
        assert_eq!(c.geoframe.url, "http://user:pass@192.0.2.20:8080");
        // 同じ URL を上書きに書くと拒否される
        for field in ["geoframe", "aircraft"] {
            let o = overlay(&format!(
                "[{field}]\nurl = \"http://user:pass@192.0.2.20:8080\"\n"
            ));
            let e = o.check(&Config::default(), None).unwrap_err();
            assert!(e.contains(&format!("{field}.url")), "{e}");
        }
    }

    #[test]
    fn ac13_write_boot_marker_roundtrip_and_unwritable() {
        let t = Tmp::new("ac13w");
        write_boot_marker(&t.0, "0123456789abcdef").unwrap();
        let text = std::fs::read_to_string(t.path("admin.boot")).unwrap();
        assert_eq!(text.lines().count(), 1);
        let m: MarkerFile = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(m.pid, std::process::id());
        assert_eq!(m.boot_id, current_boot_id());
        assert_eq!(m.generation, "0123456789abcdef");
        assert!(!m.started_at.is_empty());
        // 書いた印は自分の pid が生きているので失敗と見ない
        assert!(matches!(
            read_marker(&t.path("admin.boot")),
            MarkerState::Alive
        ));
        remove_boot_marker(&t.0);
        assert!(!t.path("admin.boot").exists());
        remove_boot_marker(&t.0); // 無くても何も起きない
        assert!(
            !std::fs::read_dir(&t.0).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("tmp"))
        );

        // (e) 書けないディレクトリ
        assert!(write_boot_marker(&t.path("no/such/dir"), "x").is_err());
        let base = t.base("");
        t.write("admin.toml", "[room]\nenabled = false\n");
        let mut l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.report.applied.len(), 1);
        // 印のディレクトリを消して書けなくする → 上書きを使わず MarkerUnwritable
        let ro = Tmp::new("ac13ro");
        let ro_overlay = ro.path("sub/admin.toml");
        l.report.path = ro_overlay;
        l.arm_boot_marker();
        assert_eq!(
            l.report.error_kind,
            Some(OverlayErrorKind::MarkerUnwritable)
        );
        assert!(l.report.applied.is_empty());
        assert_eq!(l.effective, l.base);
        assert!(l.report.error.is_some());
        assert_eq!(l.marker_armed_at, None, "書けなければ時刻も記録しない");
        // 書けるときは印が置かれ、上書きは効いたまま。印を書いた時刻が記録される(60 秒はここから数える)
        let mut l = Config::load_with_overlay(&base, true).unwrap();
        assert_eq!(l.marker_armed_at, None, "読んだだけでは印を書いていない");
        let before = std::time::Instant::now();
        l.arm_boot_marker();
        assert!(l.marker_armed_at.is_some_and(|a| a >= before));
        assert_eq!(l.report.error, None);
        assert!(!l.effective.room.enabled);
        assert!(t.path("admin.boot").exists());
        // 上書きが効いていなければ印を書かない
        std::fs::remove_file(t.path("admin.boot")).unwrap();
        std::fs::remove_file(t.path("admin.toml")).unwrap();
        let mut l = Config::load_with_overlay(&base, true).unwrap();
        l.arm_boot_marker();
        assert!(!t.path("admin.boot").exists());
        assert_eq!(l.marker_armed_at, None);
    }

    #[test]
    fn boot_marker_timer_removes_the_marker_after_ttl_and_tolerates_a_missing_one() {
        let t = Tmp::new("bmtimer");
        write_boot_marker(&t.0, "0123456789abcdef").unwrap();
        let ttl = std::time::Duration::from_millis(200);
        let armed_at = std::time::Instant::now();
        let h = spawn_boot_marker_timer(t.0.clone(), armed_at, ttl).unwrap();
        assert_eq!(h.thread().name(), Some("boot-marker"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(t.path("admin.boot").exists(), "ttl の前には消さない");
        h.join().unwrap();
        assert!(armed_at.elapsed() >= ttl, "印を書いた時点から数える");
        assert!(!t.path("admin.boot").exists());

        // 既に消えている(実行ループや再起動が先に消した)ときもパニックしない
        let h = spawn_boot_marker_timer(t.0.clone(), std::time::Instant::now(), ttl).unwrap();
        h.join().unwrap();
        // ttl より前の時刻から数えていれば、待たずに消す
        write_boot_marker(&t.0, "x").unwrap();
        let old = std::time::Instant::now() - std::time::Duration::from_secs(1);
        spawn_boot_marker_timer(t.0.clone(), old, ttl)
            .unwrap()
            .join()
            .unwrap();
        assert!(!t.path("admin.boot").exists());
    }

    #[test]
    fn stale_overlay_tmp_files_are_removed_only_when_consuming() {
        let t = Tmp::new("tmp");
        let base = t.base("");
        t.write("admin.toml.tmp-123", "x");
        t.write("admin.toml.tmp-456", "y");
        t.write("other.txt", "z");
        Config::load_with_overlay(&base, false).unwrap();
        assert!(t.path("admin.toml.tmp-123").exists());
        Config::load_with_overlay(&base, true).unwrap();
        assert!(!t.path("admin.toml.tmp-123").exists());
        assert!(!t.path("admin.toml.tmp-456").exists());
        assert!(t.path("other.txt").exists());
    }

    #[test]
    fn overlay_error_kind_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&OverlayErrorKind::BootFailed).unwrap(),
            "\"boot_failed\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayErrorKind::MarkerUnwritable).unwrap(),
            "\"marker_unwritable\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayErrorKind::UnknownKey).unwrap(),
            "\"unknown_key\""
        );
    }

    #[test]
    fn report_texts() {
        let mut r = OverlayReport {
            path: "/var/lib/miharashi/admin.toml".into(),
            exists: false,
            generation: "none".into(),
            applied: vec![],
            error: None,
            error_kind: None,
            boot_failed_generation: None,
        };
        assert_eq!(r.applied_text(), None);
        assert_eq!(r.ignored_text(), None);
        r.applied = vec![
            ("aircraft.enabled", "false".into()),
            ("agents.url", "http://192.0.2.30:3773".into()),
        ];
        assert_eq!(
            r.applied_text().unwrap(),
            "上書き: /var/lib/miharashi/admin.toml(2 項目)\n  aircraft.enabled = false\n  agents.url = http://192.0.2.30:3773"
        );
        r.error = Some("理由".into());
        assert_eq!(
            r.ignored_text().unwrap(),
            "上書き: /var/lib/miharashi/admin.toml を無視した: 理由"
        );
    }

    #[test]
    fn validate_checks_geoframe_aircraft_urls_and_allowed_hosts() {
        for ok in [
            "[geoframe]\nurl = \"http://127.0.0.1:8080\"\n",
            "[geoframe]\nurl = \"https://geo.example.test/\"\n",
            "[aircraft]\nurl = \"\"\n",
            "[aircraft]\nurl = \"https://adsb.example.test/tar1090/data/aircraft.json\"\n",
            "[admin]\nallowed_hosts = [\"192.0.2.30\", \"192.0.2.30:3773\", \"t3.example.test\", \"t3.example.test:443\", \"[2001:db8::1]\", \"[2001:db8::1]:3773\"]\n",
        ] {
            Config::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for ng in [
            "[geoframe]\nurl = \"\"\n",
            "[geoframe]\nurl = \"geo.example.test\"\n",
            "[geoframe]\nurl = \"ftp://geo.example.test\"\n",
            "[geoframe]\nurl = \"http://\"\n",
            "[aircraft]\nurl = \"adsb.example.test\"\n",
            "[aircraft]\nurl = \"https://:443/x\"\n",
            "[admin]\nallowed_hosts = [\"\"]\n",
            "[admin]\nallowed_hosts = [\"t3.example.test:\"]\n",
            "[admin]\nallowed_hosts = [\"t3.example.test:abc\"]\n",
            "[admin]\nallowed_hosts = [\"t3.example.test:0\"]\n",
            "[admin]\nallowed_hosts = [\"t3.example.test:99999\"]\n",
            "[admin]\nallowed_hosts = [\"2001:db8::1\"]\n",
            "[admin]\nallowed_hosts = [\"[2001:db8::1\"]\n",
            "[admin]\nallowed_hosts = [\"*.example.test\"]\n",
            "[admin]\nallowed_hosts = [\"http://t3.example.test\"]\n",
            "[admin]\nallowed_hosts = [\"t3.example.test/x\"]\n",
            "[admin]\nallowed_hosts = [\"user@t3.example.test\"]\n",
        ] {
            assert!(Config::parse(ng).is_err(), "{ng}");
        }
    }

    #[test]
    fn empty_config_uses_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    /// 写真の合間の航空機(S3 でやめた)の設定が残っていても起動できる。
    #[test]
    fn old_aircraft_interlude_keys_are_still_accepted() {
        let c = Config::parse("[aircraft]\nevery_photos = 10\nshow_secs = 30\n").unwrap();
        assert_eq!(c.aircraft.every_photos, Some(10));
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let c = Config::parse(
            "[display]\ncard = \"pci:0000:0e:00.0\"\n[geoframe]\nphoto_interval_secs = 10\n",
        )
        .unwrap();
        assert_eq!(c.display.card, "pci:0000:0e:00.0");
        assert_eq!(c.display.connector, "auto");
        assert_eq!(c.geoframe.photo_interval_secs, 10);
        assert_eq!(c.geoframe.url, Config::default().geoframe.url);
    }

    #[test]
    fn display_and_monitor_gpu_are_independent() {
        let c = Config::parse(
            "[display]\ncard = \"driver:amdgpu\"\n[metrics]\ngpu = \"pci:0000:01:00.0\"\n",
        )
        .unwrap();
        assert_eq!(c.display.card, "driver:amdgpu");
        assert_eq!(c.metrics.gpu, "pci:0000:01:00.0");
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        assert!(Config::parse("[display]\ncrad = \"x\"\n").is_err(), "typo");
        assert!(Config::parse("[runtime]\nseat = \"wayland\"\n").is_err());
        assert!(Config::parse("[display]\nmode = \"huge\"\n").is_err());
        assert!(
            Config::parse("[geoframe]\nreconnect_min_secs = 10\nreconnect_max_secs = 5\n").is_err()
        );
        assert!(Config::parse("[ui]\nstart_screen = \"menu\"\n").is_err());
        assert!(Config::parse("[ui]\nstart_screen = \"aircraft\"\n").is_ok());
        assert!(Config::parse("[ui]\nstart_screen = \"agents\"\n").is_err());
        assert!(Config::parse(
            "[agents]\nenabled = true\nurl = \"http://192.0.2.10:3773\"\ntoken_file = \"/tmp/token\"\n[ui]\nstart_screen = \"agents\"\n"
        ).is_ok());
        assert!(Config::parse("[ui]\nrefresh_screens = [\"agents\"]\n").is_err());
        assert!(Config::parse(
            "[agents]\nenabled = true\nurl = \"http://192.0.2.10:3773\"\ntoken_file = \"/tmp/token\"\n[ui]\nrefresh_screens = [\"agents\"]\n"
        ).is_err());
        assert!(Config::parse("[ui]\nrefresh_screens = [\"status\", \"radar\"]\n").is_err());
        let c = Config::parse(
            "[ui]\nrefresh_minutes = 0\nrefresh_screens = [\"status\", \"aircraft\"]\n",
        )
        .unwrap();
        assert_eq!((c.ui.refresh_minutes, c.ui.refresh_screens.len()), (0, 2));
        assert!(Config::parse("[metrics]\ngpu = \"rtx\"\n").is_err());
        assert!(Config::parse("[display]\nrenderer = \"vulkan\"\n").is_err());
    }

    #[test]
    fn power_section_defaults_and_overrides() {
        let d = Config::default().power;
        assert_eq!(d.state_file, "/var/lib/miharashi/energy.json");
        assert_eq!(d.rapl_helper_file, "/run/miharashi-rapl/energy");
        assert_eq!(d.price_per_kwh, 0.0);
        let c = Config::parse("[power]\nprice_per_kwh = 31.5\nstate_file = \"\"\n").unwrap();
        assert_eq!(c.power.price_per_kwh, 31.5);
        assert_eq!(c.power.state_file, "");
        assert_eq!(c.power.rapl_helper_file, d.rapl_helper_file);
    }

    #[test]
    fn power_section_rejects_bad_values() {
        assert!(Config::parse("[power]\nprice_per_kwh = -1.0\n").is_err());
        assert!(Config::parse("[power]\nprice_per_kwh = nan\n").is_err());
        assert!(
            Config::parse("[power]\nstatefile = \"x\"\n").is_err(),
            "typo"
        );
    }

    #[test]
    fn screen_power_defaults_and_overrides() {
        let d = Config::default();
        assert_eq!(d.home_assistant.url, "");
        assert_eq!(d.home_assistant.token_file, "/etc/miharashi/ha-token");
        assert_eq!(d.screen_power.idle_minutes, 10);
        assert_eq!(d.screen_power.override_decay_minutes, 5);
        let c = Config::parse(
            "[home_assistant]\nurl = \"http://192.0.2.10:8123\"\n\
             [screen_power]\npresence_entity = \"person.me\"\nidle_minutes = 3\n",
        )
        .unwrap();
        assert_eq!(c.home_assistant.url, "http://192.0.2.10:8123");
        assert_eq!(c.screen_power.presence_entity, "person.me");
        assert_eq!(c.screen_power.idle_minutes, 3);
        assert_eq!(c.screen_power.poll_secs, 10);
    }

    #[test]
    fn screen_power_rejects_bad_values() {
        // 平文 http でホスト名宛てにトークンを送る設定は拒否する
        assert!(
            Config::parse("[home_assistant]\nurl = \"http://homeassistant.local:8123\"\n").is_err()
        );
        assert!(Config::parse("[home_assistant]\nurl = \"https://ha.example.test\"\n").is_ok());
        assert!(Config::parse("[screen_power]\nidle_minutes = 0\n").is_err());
        assert!(
            Config::parse("[screen_power]\npresense_entity = \"x\"\n").is_err(),
            "typo"
        );
    }

    #[test]
    fn alerts_defaults_and_bad_values() {
        let a = Config::default().alerts;
        assert!(a.quake);
        assert_eq!(
            (a.quake_poll_secs, a.show_minutes, a.demo_after_secs),
            (30, 5, 0)
        );
        assert!(
            Config::parse("[alerts]\nquake_poll_secs = 1\n").is_err(),
            "気象庁を叩きすぎない"
        );
        assert!(Config::parse("[alerts]\nshow_minutes = 0\n").is_err());
        assert!(Config::parse("[alerts]\nquake = false\n").is_ok());
        assert!(a.disaster && a.disaster_poll_secs == 60 && a.disaster_radius_km == 50.0);
        assert!(Config::parse("[alerts]\ndisaster_poll_secs = 5\n").is_err());
        assert!(Config::parse("[alerts]\ndisaster_radius_km = 0.0\n").is_err());
        assert!(Config::parse("[alerts]\ndisaster_radius_km = 30.0\n").is_ok());
    }

    #[test]
    fn room_section_defaults_and_bad_values() {
        let c = Config::default();
        assert!(c.room.enabled && c.room.tiles.is_empty());
        assert_eq!((c.room.history_hours, c.room.poll_secs), (24, 60));
        let c = Config::parse(
            "[room]\nforecast_area = \"130010\"\ntiles = [\n  { label = \"CO₂\", entity = \"sensor.co2\", warn = 1000, alert = 1500 },\n  { label = \"玄関\", entity = \"sensor.t\", second = \"sensor.h\" },\n  { label = \"気圧\", entity = \"sensor.p\", trend_hours = 3 },\n]\n[ui]\nstart_screen = \"room\"\nrefresh_screens = [\"room\"]\n",
        )
        .unwrap();
        assert_eq!(c.room.tiles.len(), 3);
        assert_eq!(c.room.tiles[0].alert, Some(1500.0));
        assert_eq!(c.room.tiles[1].second, "sensor.h");
        assert_eq!(c.room.tiles[2].trend_hours, Some(3));
        for bad in [
            "[room]\nforecast_area = \"東京\"\n",
            "[room]\nforecast_area = \"13001\"\n",
            "[room]\npoll_secs = 5\n",
            "[room]\nhistory_hours = 0\n",
            "[room]\nforecast_minutes = 1\n",
            "[room]\ntiles = [{ label = \"x\", entity = \"\" }]\n",
            "[room]\ntiles = [{ label = \"x\", entity = \"sensor.x\", colour = 1 }]\n",
        ] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rotation_defaults_and_bad_values() {
        let r = Config::default().rotation;
        assert!(!r.enabled, "既定は巡回しない(今までどおり)");
        let ids: Vec<&str> = r.steps.iter().map(|s| s.screen.as_str()).collect();
        assert_eq!(ids, ["photo", "room", "photo", "aircraft", "status"]);
        assert!(r.steps[3].skip_if_empty);
        assert_eq!(r.pause_after_input_secs, 300);
        let c = Config::parse(
            "[rotation]\nenabled = true\npause_after_input_secs = 60\nsteps = [{ screen = \"status\", secs = 10 }]\n",
        )
        .unwrap();
        assert_eq!(
            (c.rotation.steps.len(), c.rotation.pause_after_input_secs),
            (1, 60)
        );
        for bad in [
            "[rotation]\nsteps = [{ screen = \"radar\", secs = 10 }]\n",
            "[rotation]\nsteps = [{ screen = \"photo\", secs = 1 }]\n",
            "[rotation]\nsteps = [{ screen = \"photo\" }]\n",
            "[rotation]\nenabled = true\nsteps = []\n",
            "[rotation]\nsteps = [{ screen = \"photo\", secs = 10, skip = true }]\n",
        ] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn shipped_example_parses() {
        let text = include_str!("../../../config/example.toml");
        Config::parse(text).expect("config/example.toml が読み込めること");
    }

    #[test]
    fn panel_is_off_by_default_and_needs_a_specific_address() {
        assert!(!Config::default().panel.enabled);
        let on = |listen: &str| {
            Config::parse(&format!("[panel]\nenabled = true\nlisten = \"{listen}\"\n"))
        };
        assert!(on("192.168.0.132:8790").is_ok());
        assert!(on("0.0.0.0:8790").is_err(), "全インターフェース");
        assert!(on("[::]:8790").is_err(), "全インターフェース(IPv6)");
        assert!(on("").is_err());
        assert!(on("geoframe-n100:8790").is_err(), "名前ではなく IP");
        // 無効なら listen が空でもよい
        assert!(Config::parse("[panel]\nmax_rate_per_sec = 5\n").is_ok());
    }

    /// VRM は既定で無効(設定を足さなければ本番の動きは変わらない)。有効にするにはモデルのパスが要り、
    /// 値の形は無効でも確かめる。ファイルがあるかは確かめない(無くても 3D なしで動く)。
    #[test]
    fn vrm_is_off_by_default_and_validates() {
        let c = Config::default();
        assert!(!c.vrm.enabled && c.vrm.model.is_empty());
        assert_eq!(c.vrm.screens, ["photo", "agents"]);
        assert_eq!((c.vrm.region.as_str(), c.vrm.fps), ("left", 30));
        assert!(Config::parse("").is_ok());

        let ok = "[vrm]\nenabled = true\nmodel = \"/var/lib/miharashi/vrm/model.vrm\"\n";
        let c = Config::parse(ok).unwrap();
        assert!(c.vrm.enabled);
        assert_eq!(c.vrm.model, "/var/lib/miharashi/vrm/model.vrm");
        assert!(
            Config::parse("[vrm]\nenabled = true\n").is_err(),
            "有効なのにモデルが無い"
        );
        assert!(
            Config::parse(&format!("{ok}screens = []\n")).is_err(),
            "有効なのに載せる画面が無い"
        );
        assert!(Config::parse(&format!("{ok}screens = [\"photo\"]\n")).is_ok());
        assert!(Config::parse(&format!("{ok}screens = [\"agents\"]\n")).is_ok());
        assert!(
            Config::parse(&format!("{ok}screens = [\"room\"]\n")).is_err(),
            "未知の画面"
        );
        assert!(Config::parse("[vrm]\nscreens = [\"status\"]\n").is_err());
        for region in ["left", "center", "right"] {
            assert!(Config::parse(&format!("{ok}region = \"{region}\"\n")).is_ok());
        }
        assert!(Config::parse(&format!("{ok}region = \"full\"\n")).is_err());
        for fps in [1, 30, 60] {
            assert!(
                Config::parse(&format!("{ok}fps = {fps}\n")).is_ok(),
                "{fps}"
            );
        }
        for fps in [0, 61, 240] {
            assert!(
                Config::parse(&format!("{ok}fps = {fps}\n")).is_err(),
                "{fps}"
            );
        }
        assert!(
            Config::parse("[vrm]\nfps = 100\n").is_err(),
            "無効でも形は確かめる"
        );
        assert!(Config::parse("[vrm]\nenabled = false\nfpss = 30\n").is_err());
        // agents が無効でも agents を載せる設定は通る(既定が両方なので、agents を使わない構成で困らない)
        assert!(Config::parse(&format!("{ok}screens = [\"photo\", \"agents\"]\n")).is_ok());
    }

    #[test]
    fn cube_and_gl_check_are_off_by_default_and_validate() {
        let c = Config::default();
        assert!(!c.cube.enabled && !c.gl_check.enabled);
        assert_eq!((c.cube.layout.as_str(), c.cube.fps), ("both", 30));
        assert_eq!(c.cube.layouts(), (true, true));
        assert!(Config::parse("[cube]\nlayout = \"quarter\"\n").is_err());
        assert!(Config::parse("[cube]\nfps = 241\n").is_err());
        assert!(Config::parse("[cube]\nfps = 0\n").is_ok());
        assert!(Config::parse("[cube]\nfpss = 30\n").is_err());
        // 巡回の手順には書ける(出していない画面は飛ばす)が、起動時の画面は有効にしたものだけ
        assert!(
            Config::parse(
                "[rotation]\nenabled = true\nsteps = [{ screen = \"cube\", secs = 5 }]\n"
            )
            .is_ok()
        );
        assert!(Config::parse("[ui]\nstart_screen = \"cube\"\n").is_err());
        assert!(
            Config::parse(
                "[cube]\nenabled = true\nlayout = \"third\"\n[ui]\nstart_screen = \"cube\"\n"
            )
            .is_err()
        );
        assert!(
            Config::parse("[cube]\nenabled = true\n[ui]\nstart_screen = \"cube-third\"\n").is_ok()
        );
        assert!(
            Config::parse("[gl_check]\nenabled = true\n[ui]\nstart_screen = \"dirty-a\"\n").is_ok()
        );
    }

    #[test]
    fn t3_agents_are_off_by_default_and_validate_private_http_settings() {
        assert!(!Config::default().agents.enabled);
        assert!(Config::parse("[agents]\nurl = \"http://t3.local:3773\"\n").is_ok());
        assert!(
            Config::parse("[agents]\nenabled = true\nurl = \"http://t3.local:3773\"\n").is_err()
        );
        assert!(
            Config::parse("[agents]\nenabled = true\nurl = \"http://192.0.2.10:3773\"\n").is_ok()
        );
        assert!(Config::parse("[agents]\nenabled = true\npoll_secs = 0\n").is_err());
        assert!(
            Config::parse("[agents]\nenabled = true\npoll_secs = 60\nstale_secs = 30\n").is_err()
        );
        assert!(Config::parse("[agents]\ntoken_flie = \"x\"\n").is_err());
    }
}
