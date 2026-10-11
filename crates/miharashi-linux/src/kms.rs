use crate::drm_select::ModeSel;
use drm::Device;
use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{
    Device as ControlDevice, Mode, ModeTypeFlags, connector, crtc, dumbbuffer::DumbBuffer,
    framebuffer,
};
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

pub struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

/// 1 コネクターへ dumb buffer で出力する Presenter。Drop で元の CRTC 状態を復元する。
/// デバイスの fd は呼び出し側(seat)が閉じるので、Display を先に drop すること。
pub struct Display {
    card: Card,
    crtc: crtc::Handle,
    conn: connector::Handle,
    mode: Mode,
    saved: crtc::Info,
    fb: framebuffer::Handle,
    db: DumbBuffer,
    suspended: bool,
}

fn conn_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

impl Display {
    /// `fd` は DRM デバイスの fd(複製済み)。`connector` は "auto" か "HDMI-A-1" など。
    pub fn open(fd: OwnedFd, connector: &str, mode_sel: ModeSel) -> Result<Self, String> {
        let card = Card(File::from(fd));
        let res = card
            .resource_handles()
            .map_err(|e| format!("resource_handles: {e}"))?;
        let want = connector.trim();
        let mut found = None;
        for h in res.connectors() {
            let info = card.get_connector(*h, true).map_err(|e| e.to_string())?;
            let ok = info.state() == connector::State::Connected
                && (want.is_empty()
                    || want == "auto"
                    || want.eq_ignore_ascii_case(&conn_name(&info)));
            if ok {
                found = Some(info);
                break;
            }
        }
        let info = found.ok_or_else(|| format!("接続済みのコネクター({want})がない"))?;
        let mode = pick_mode(info.modes(), mode_sel)
            .ok_or_else(|| format!("モード {mode_sel:?} が {} にない", conn_name(&info)))?;

        // 現在の encoder が使っている CRTC を優先し、なければ encoder が使える CRTC から選ぶ。
        let crtc = info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| {
                info.encoders()
                    .iter()
                    .filter_map(|e| card.get_encoder(*e).ok())
                    .find_map(|e| res.filter_crtcs(e.possible_crtcs()).first().copied())
            })
            .ok_or("使える CRTC がない")?;
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
            suspended: false,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        (w as u32, h as u32)
    }

    pub fn refresh(&self) -> u32 {
        self.mode.vrefresh()
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// VT が非アクティブになるとき。以後 `present` は何もしない。DRM master は手放す。
    pub fn suspend(&mut self) {
        self.suspended = true;
        let _ = self.card.release_master_lock();
    }

    /// VT がアクティブに戻ったとき。master を取り直して CRTC を設定し直す。
    pub fn resume(&mut self) -> Result<(), String> {
        let _ = self.card.acquire_master_lock();
        self.card
            .set_crtc(
                self.crtc,
                Some(self.fb),
                (0, 0),
                &[self.conn],
                Some(self.mode),
            )
            .map_err(|e| format!("set_crtc(復帰): {e}"))?;
        self.suspended = false;
        Ok(())
    }

    /// フレームを dumb buffer へ書く。`fill` は (バッファ, pitch) を受け取る。一時停止中は何もしない。
    pub fn present(&mut self, fill: impl FnOnce(&mut [u8], usize)) -> Result<(), String> {
        if self.suspended {
            return Ok(());
        }
        let pitch = self.db.pitch() as usize;
        let mut map = self
            .card
            .map_dumb_buffer(&mut self.db)
            .map_err(|e| format!("map_dumb_buffer: {e}"))?;
        fill(map.as_mut(), pitch);
        Ok(())
    }
}

fn pick_mode(modes: &[Mode], sel: ModeSel) -> Option<Mode> {
    match sel {
        ModeSel::Preferred => modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(modes.first())
            .copied(),
        ModeSel::Size { w, h, refresh } => modes
            .iter()
            .find(|m| m.size() == (w, h) && refresh.is_none_or(|r| m.vrefresh() == r))
            .copied(),
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if !self.suspended {
            let _ = self.card.set_crtc(
                self.crtc,
                self.saved.framebuffer(),
                self.saved.position(),
                &[self.conn],
                self.saved.mode(),
            );
        }
        let _ = self.card.destroy_framebuffer(self.fb);
        let _ = self.card.destroy_dumb_buffer(self.db);
    }
}
