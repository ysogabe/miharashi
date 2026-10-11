use drm::control::{connector, Device as ControlDevice, ModeTypeFlags};
use drm::Device;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

pub struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        OpenOptions::new().read(true).write(true).open(path).map(Card)
    }
}

pub struct ModeInfo {
    pub name: String,
    pub refresh: u32,
    pub preferred: bool,
}

pub struct ConnectorInfo {
    pub name: String,
    pub connected: bool,
    pub modes: Vec<ModeInfo>,
}

pub struct CardInfo {
    pub path: PathBuf,
    pub driver: String,
    pub connectors: Vec<ConnectorInfo>,
}

/// `/dev/dri/card*` を列挙し、ドライバー・コネクター・モードを返す(DRM master 不要)。
pub fn probe() -> Vec<Result<CardInfo, String>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")))
        .collect();
    paths.sort();
    paths.into_iter().map(|p| probe_card(&p).map_err(|e| format!("{}: {e}", p.display()))).collect()
}

fn probe_card(path: &Path) -> std::io::Result<CardInfo> {
    let card = Card::open(path)?;
    let driver = card.get_driver()?.name().to_string_lossy().into_owned();
    let res = card.resource_handles()?;
    let mut connectors = Vec::new();
    for h in res.connectors() {
        let info = card.get_connector(*h, true)?;
        connectors.push(ConnectorInfo {
            name: format!("{}-{}", info.interface().as_str(), info.interface_id()),
            connected: info.state() == connector::State::Connected,
            modes: info
                .modes()
                .iter()
                .map(|m| ModeInfo {
                    name: m.name().to_string_lossy().into_owned(),
                    refresh: m.vrefresh(),
                    preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED),
                })
                .collect(),
        });
    }
    Ok(CardInfo { path: path.to_path_buf(), driver, connectors })
}
