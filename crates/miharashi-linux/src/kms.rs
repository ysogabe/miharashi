use drm::Device;
use drm::control::{Device as ControlDevice, ModeTypeFlags, connector};
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
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(Card)
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
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("card"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| probe_card(&p).map_err(|e| format!("{}: {e}", p.display())))
        .collect()
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
    Ok(CardInfo {
        path: path.to_path_buf(),
        driver,
        connectors,
    })
}

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{Mode, crtc, framebuffer};

/// 1 コネクターへ dumb buffer で出力する Presenter。Drop で元の CRTC 状態を復元する。
pub struct Display {
    card: Card,
    crtc: crtc::Handle,
    conn: connector::Handle,
    mode: Mode,
    saved: crtc::Info,
    fb: framebuffer::Handle,
    db: drm::control::dumbbuffer::DumbBuffer,
}

impl Display {
    /// `connector` が None なら最初の接続済みコネクターを使う。
    pub fn open(path: &Path, connector: Option<&str>) -> Result<Self, String> {
        let card = Card::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let res = card.resource_handles().map_err(|e| e.to_string())?;
        let mut found = None;
        for h in res.connectors() {
            let info = card.get_connector(*h, true).map_err(|e| e.to_string())?;
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            if info.state() == connector::State::Connected && connector.is_none_or(|c| c == name) {
                found = Some(info);
                break;
            }
        }
        let info = found.ok_or("no connected connector")?;
        let mode = *info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(info.modes().first())
            .ok_or("no modes")?;
        // 現在の encoder が使っている CRTC を優先し、なければ空きを探す。
        let crtc = info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| res.crtcs().first().copied())
            .ok_or("no crtc")?;
        let saved = card.get_crtc(crtc).map_err(|e| e.to_string())?;
        let (w, h) = mode.size();
        let db = card
            .create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)
            .map_err(|e| format!("create_dumb_buffer: {e}"))?;
        let fb = card
            .add_framebuffer(&db, 24, 32)
            .map_err(|e| format!("add_framebuffer: {e}"))?;
        card.set_crtc(crtc, Some(fb), (0, 0), &[info.handle()], Some(mode))
            .map_err(|e| {
                format!("set_crtc: {e} (DRM master を他のプロセスが持っていないか確認)")
            })?;
        Ok(Self {
            card,
            crtc,
            conn: info.handle(),
            mode,
            saved,
            fb,
            db,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        (w as u32, h as u32)
    }

    /// フレームを dumb buffer へ書く。`fill` は (バッファ, pitch) を受け取る。
    pub fn present(&mut self, fill: impl FnOnce(&mut [u8], usize)) -> Result<(), String> {
        let pitch = self.db.pitch() as usize;
        let mut map = self
            .card
            .map_dumb_buffer(&mut self.db)
            .map_err(|e| format!("map_dumb_buffer: {e}"))?;
        fill(map.as_mut(), pitch);
        Ok(())
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        let _ = self.card.set_crtc(
            self.crtc,
            self.saved.framebuffer(),
            self.saved.position(),
            &[self.conn],
            self.saved.mode(),
        );
        let _ = self.card.destroy_framebuffer(self.fb);
        let _ = self.card.destroy_dumb_buffer(self.db);
    }
}
