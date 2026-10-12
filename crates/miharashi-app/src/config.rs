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
    pub ui: Ui,
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
pub struct Ui {
    /// 日本語フォントファイル。空ならシステムフォントを使う。
    pub font: String,
    /// status / photo
    pub start_screen: String,
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

impl Default for Ui {
    fn default() -> Self {
        Self {
            font: "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
            start_screen: "status".into(),
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
        if self.geoframe.photo_interval_secs == 0 || self.metrics.interval_secs == 0 {
            return Err("geoframe.photo_interval_secs と metrics.interval_secs は 1 以上".into());
        }
        if !self.power.price_per_kwh.is_finite() || self.power.price_per_kwh < 0.0 {
            return Err("power.price_per_kwh は 0 以上の数(0 = 電気代を表示しない)".into());
        }
        if !matches!(self.ui.start_screen.as_str(), "status" | "photo") {
            return Err("ui.start_screen は status か photo".into());
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
        assert!(Config::parse("[metrics]\ngpu = \"rtx\"\n").is_err());
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
    fn shipped_example_parses() {
        let text = include_str!("../../../config/example.toml");
        Config::parse(text).expect("config/example.toml が読み込めること");
    }
}
