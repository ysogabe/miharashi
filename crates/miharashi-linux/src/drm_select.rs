//! DRM カードとコネクターの検出・選択。sysfs だけを読むので権限もデバイスのオープンも不要。
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorDesc {
    /// "HDMI-A-1" など。DRM のコネクター名と一致する。
    pub name: String,
    pub connected: bool,
    /// sysfs の `modes` の先頭から順に ("1920x1080" など)。
    pub modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDesc {
    /// `/dev/dri/cardN`
    pub path: PathBuf,
    pub driver: String,
    /// PCI アドレス ("0000:01:00.0")。PCI でないデバイスは None。
    pub pci: Option<String>,
    pub vendor_id: Option<u16>,
    pub boot_vga: bool,
    pub connectors: Vec<ConnectorDesc>,
}

impl CardDesc {
    pub fn connected(&self) -> impl Iterator<Item = &ConnectorDesc> {
        self.connectors.iter().filter(|c| c.connected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardSel {
    /// 接続済みコネクターを持つ最初のカード(boot_vga でないものを優先)。
    Auto,
    /// `/dev/dri/cardN`
    Path(PathBuf),
    /// `pci:0000:01:00.0`
    Pci(String),
    /// `driver:amdgpu`
    Driver(String),
}

impl CardSel {
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s.is_empty() || s == "auto" {
            Self::Auto
        } else if let Some(p) = s.strip_prefix("pci:") {
            Self::Pci(p.to_ascii_lowercase())
        } else if let Some(d) = s.strip_prefix("driver:") {
            Self::Driver(d.to_string())
        } else {
            Self::Path(PathBuf::from(s))
        }
    }
}

/// `/sys/class/drm` を走査して全カードを返す。
pub fn scan() -> Vec<CardDesc> {
    scan_at(Path::new("/sys/class/drm"), Path::new("/dev/dri"))
}

pub fn scan_at(sys_drm: &Path, dev_dri: &Path) -> Vec<CardDesc> {
    let mut cards = Vec::new();
    let Ok(rd) = std::fs::read_dir(sys_drm) else {
        return cards;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(num) = name.strip_prefix("card") else {
            continue;
        };
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let dir = entry.path();
        let dev = dir.join("device");
        let driver = std::fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let pci = std::fs::read_link(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|n| n.contains(':') && n.contains('.'));
        let read_hex = |f: &str| {
            std::fs::read_to_string(dev.join(f))
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        };
        let boot_vga = std::fs::read_to_string(dev.join("boot_vga")).is_ok_and(|s| s.trim() == "1");
        let mut connectors = Vec::new();
        let prefix = format!("{name}-");
        if let Ok(rd) = std::fs::read_dir(sys_drm) {
            for c in rd.flatten() {
                let cname = c.file_name().to_string_lossy().into_owned();
                let Some(short) = cname.strip_prefix(&prefix) else {
                    continue;
                };
                let status = std::fs::read_to_string(c.path().join("status")).unwrap_or_default();
                let modes = std::fs::read_to_string(c.path().join("modes"))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect();
                connectors.push(ConnectorDesc {
                    name: short.to_string(),
                    connected: status.trim() == "connected",
                    modes,
                });
            }
        }
        connectors.sort_by(|a, b| a.name.cmp(&b.name));
        cards.push(CardDesc {
            path: dev_dri.join(&name),
            driver,
            pci,
            vendor_id: read_hex("vendor"),
            boot_vga,
            connectors,
        });
    }
    cards.sort_by(|a, b| a.path.cmp(&b.path));
    cards
}

/// 設定に合うカードを選ぶ。`Auto` 以外は指定どおりのカードがなければエラー。
pub fn choose_card<'a>(cards: &'a [CardDesc], sel: &CardSel) -> Result<&'a CardDesc, String> {
    match sel {
        CardSel::Auto => cards
            .iter()
            .filter(|c| c.connected().next().is_some())
            .min_by_key(|c| c.boot_vga as u8)
            .ok_or_else(|| "接続済みのコネクターを持つ DRM カードがない".to_string()),
        CardSel::Path(p) => cards
            .iter()
            .find(|c| &c.path == p)
            .ok_or_else(|| format!("DRM カード {} がない", p.display())),
        CardSel::Pci(addr) => cards
            .iter()
            .find(|c| {
                c.pci
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case(addr))
            })
            .ok_or_else(|| format!("PCI {addr} の DRM カードがない")),
        CardSel::Driver(d) => cards
            .iter()
            .filter(|c| &c.driver == d)
            .min_by_key(|c| c.connected().next().is_none() as u8)
            .ok_or_else(|| format!("ドライバー {d} の DRM カードがない")),
    }
}

/// `auto` なら接続済みの最初のコネクター、名前指定ならそのコネクター(未接続はエラー)。
pub fn choose_connector<'a>(card: &'a CardDesc, name: &str) -> Result<&'a ConnectorDesc, String> {
    let name = name.trim();
    if name.is_empty() || name == "auto" {
        return card
            .connected()
            .next()
            .ok_or_else(|| format!("{} に接続済みのコネクターがない", card.path.display()));
    }
    let c = card
        .connectors
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            format!(
                "コネクター {name} がない({} のコネクター: {})",
                card.path.display(),
                card.connectors
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    if !c.connected {
        return Err(format!("コネクター {name} は未接続"));
    }
    Ok(c)
}

/// "preferred" / "1920x1080" / "1920x1080@60"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSel {
    Preferred,
    Size {
        w: u16,
        h: u16,
        refresh: Option<u32>,
    },
}

impl ModeSel {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() || s == "preferred" {
            return Ok(Self::Preferred);
        }
        let (size, refresh) = match s.split_once('@') {
            Some((a, b)) => (
                a,
                Some(
                    b.parse::<u32>()
                        .map_err(|_| format!("モード {s}: リフレッシュレートが不正"))?,
                ),
            ),
            None => (s, None),
        };
        let (w, h) = size
            .split_once('x')
            .ok_or_else(|| format!("モード {s}: WxH[@Hz] の形式で指定する"))?;
        Ok(Self::Size {
            w: w.parse().map_err(|_| format!("モード {s}: 幅が不正"))?,
            h: h.parse().map_err(|_| format!("モード {s}: 高さが不正"))?,
            refresh,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(
        path: &str,
        driver: &str,
        pci: &str,
        boot_vga: bool,
        conns: &[(&str, bool)],
    ) -> CardDesc {
        CardDesc {
            path: path.into(),
            driver: driver.into(),
            pci: Some(pci.into()),
            vendor_id: None,
            boot_vga,
            connectors: conns
                .iter()
                .map(|(n, c)| ConnectorDesc {
                    name: n.to_string(),
                    connected: *c,
                    modes: vec!["1920x1080".into()],
                })
                .collect(),
        }
    }

    fn two_gpus() -> Vec<CardDesc> {
        vec![
            card(
                "/dev/dri/card0",
                "amdgpu",
                "0000:0e:00.0",
                false,
                &[("HDMI-A-1", false)],
            ),
            card(
                "/dev/dri/card1",
                "nvidia-drm",
                "0000:01:00.0",
                true,
                &[("HDMI-A-1", true), ("DP-1", false)],
            ),
        ]
    }

    #[test]
    fn auto_picks_card_with_connected_connector_not_card0() {
        let cards = two_gpus();
        assert_eq!(
            choose_card(&cards, &CardSel::Auto).unwrap().path,
            PathBuf::from("/dev/dri/card1")
        );
    }

    #[test]
    fn auto_prefers_non_boot_vga_when_both_connected() {
        let mut cards = two_gpus();
        cards[0].connectors[0].connected = true;
        assert_eq!(
            choose_card(&cards, &CardSel::Auto).unwrap().driver,
            "amdgpu"
        );
    }

    #[test]
    fn explicit_selectors() {
        let cards = two_gpus();
        assert_eq!(
            choose_card(&cards, &CardSel::parse("pci:0000:0E:00.0"))
                .unwrap()
                .driver,
            "amdgpu"
        );
        assert_eq!(
            choose_card(&cards, &CardSel::parse("driver:nvidia-drm"))
                .unwrap()
                .driver,
            "nvidia-drm"
        );
        assert_eq!(
            choose_card(&cards, &CardSel::parse("/dev/dri/card0"))
                .unwrap()
                .driver,
            "amdgpu"
        );
        assert!(choose_card(&cards, &CardSel::parse("pci:0000:ff:00.0")).is_err());
    }

    #[test]
    fn no_connected_card_is_an_error() {
        let cards = vec![card(
            "/dev/dri/card0",
            "amdgpu",
            "0000:0e:00.0",
            false,
            &[("HDMI-A-1", false)],
        )];
        assert!(choose_card(&cards, &CardSel::Auto).is_err());
    }

    #[test]
    fn connector_selection() {
        let cards = two_gpus();
        let c = &cards[1];
        assert_eq!(choose_connector(c, "auto").unwrap().name, "HDMI-A-1");
        assert!(choose_connector(c, "DP-1").is_err(), "未接続は選べない");
        assert!(choose_connector(c, "VGA-1").is_err());
    }

    #[test]
    fn mode_parse() {
        assert_eq!(ModeSel::parse("preferred").unwrap(), ModeSel::Preferred);
        assert_eq!(
            ModeSel::parse("1920x1080").unwrap(),
            ModeSel::Size {
                w: 1920,
                h: 1080,
                refresh: None
            }
        );
        assert_eq!(
            ModeSel::parse("2560x1440@60").unwrap(),
            ModeSel::Size {
                w: 2560,
                h: 1440,
                refresh: Some(60)
            }
        );
        assert!(ModeSel::parse("big").is_err());
    }

    #[test]
    fn scan_reads_fake_sysfs() {
        let tmp = std::env::temp_dir().join(format!("miharashi-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let real = tmp.join("devices/pci0000:00/0000:01:00.0");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("vendor"), "0x10de\n").unwrap();
        std::fs::write(real.join("boot_vga"), "1\n").unwrap();
        let drivers = tmp.join("drivers/nvidia");
        std::fs::create_dir_all(&drivers).unwrap();
        std::os::unix::fs::symlink(&drivers, real.join("driver")).unwrap();
        let sys = tmp.join("class/drm");
        std::fs::create_dir_all(sys.join("card1-HDMI-A-1")).unwrap();
        std::fs::create_dir_all(sys.join("card1")).unwrap();
        std::os::unix::fs::symlink(&real, sys.join("card1/device")).unwrap();
        std::fs::write(sys.join("card1-HDMI-A-1/status"), "connected\n").unwrap();
        std::fs::write(sys.join("card1-HDMI-A-1/modes"), "1920x1080\n1280x720\n").unwrap();

        let cards = scan_at(&sys, Path::new("/dev/dri"));
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(
            (c.driver.as_str(), c.pci.as_deref(), c.vendor_id, c.boot_vga),
            ("nvidia", Some("0000:01:00.0"), Some(0x10de), true)
        );
        assert_eq!(c.connectors[0].name, "HDMI-A-1");
        assert!(c.connectors[0].connected);
        assert_eq!(c.connectors[0].modes, ["1920x1080", "1280x720"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
