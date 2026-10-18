//! TOML 設定。未指定の項目は既定値を使い、未知のキーは typo として拒否する。
use serde::Deserialize;
use std::path::{Path, PathBuf};

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

/// GL の状態をわざと汚す試験画面(`dirty-a`・`dirty-b`。実機の確認用)。既定は無効。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GlCheck {
    pub enabled: bool,
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

    /// 明示パス → `$XDG_CONFIG_HOME/miharashi/config.toml` → `/etc/miharashi/config.toml` → 既定値。
    pub fn discover(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>), String> {
        if let Some(p) = explicit {
            return Ok((Self::load(p)?, Some(p.to_path_buf())));
        }
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        let candidates = xdg
            .into_iter()
            .map(|d| d.join("miharashi/config.toml"))
            .chain([PathBuf::from("/etc/miharashi/config.toml")]);
        for p in candidates {
            if p.exists() {
                return Ok((Self::load(&p)?, Some(p)));
            }
        }
        Ok((Self::default(), None))
    }

    fn validate(&self) -> Result<(), String> {
        miharashi_linux_validate(self)?;
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
