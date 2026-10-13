//! GLES 3.2 で KMS に出す Presenter。GBM surface のダブルバッファを page flip で切り替える。
//! 合成は `compose::Compositor`(写真のフェードと文字の重ね絵は GPU で行う)。
use crate::compose::{Compositor, Frame};
use crate::{egl, gbm};
use drm::Device as _;
use drm::control::{Device as ControlDevice, Event, PageFlipFlags, framebuffer};
use miharashi_linux::drm_select::ModeSel;
use miharashi_linux::kms::{Card, Output};
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};

/// page flip の完了を待つ上限。60Hz なら 17ms で終わる。
const FLIP_TIMEOUT_MS: i32 = 1000;

/// Drop で元の CRTC 状態を復元する。デバイスの fd は呼び出し側(seat)が閉じるので、先に drop すること。
pub struct GlesDisplay {
    // フィールドは宣言順に drop される: EGL → GBM surface → GBM device → カード
    ctx: egl::Context,
    surface: gbm::Surface,
    _dev: gbm::Device,
    card: Card,
    comp: Compositor,
    out: Output,
    /// 表示中のバッファ(最初の表示の前は None)
    current: Option<gbm::Bo>,
    /// GEM ハンドル → framebuffer。surface は 2〜3 枚のバッファを使い回す
    fbs: HashMap<u32, framebuffer::Handle>,
    suspended: bool,
}

impl GlesDisplay {
    /// `fd` は DRM デバイス(表示カード)の fd(複製済み)。`connector` は "auto" か "HDMI-A-1" など。
    pub fn open(fd: OwnedFd, connector: &str, mode_sel: ModeSel) -> Result<Self, String> {
        let card = Card::new(fd);
        let out = Output::find(&card, connector, mode_sel)?;
        let dev = gbm::Device::new(card.as_fd())?;
        let surface = gbm::Surface::new(&dev, out.size())?;
        let ctx = egl::Context::new(&dev, Some(&surface))?;
        let comp = Compositor::new(&ctx.gl, out.size())?;
        Ok(Self {
            ctx,
            surface,
            _dev: dev,
            card,
            comp,
            out,
            current: None,
            fbs: HashMap::new(),
            suspended: false,
        })
    }

    /// "GLES 3.2 Mesa ... / AMD Radeon ..." のような説明(ログ用)。
    pub fn describe(&self) -> String {
        self.ctx.describe()
    }

    pub fn size(&self) -> (u32, u32) {
        self.out.size()
    }

    pub fn refresh(&self) -> u32 {
        self.out.mode.vrefresh()
    }

    /// GPU で扱える写真の最大の幅と高さ。
    pub fn max_texture(&self) -> u32 {
        self.comp.max_texture()
    }

    /// 前回呼んでから今までに、写真から完成フレームを作った CPU 時間 [ms](1 枚ごと)。
    pub fn take_build_ms(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.comp.build_ms)
    }

    /// VT が非アクティブになるとき。以後 `present` は何もしない。DRM master は手放す。
    pub fn suspend(&mut self) {
        self.suspended = true;
        let _ = self.card.release_master_lock();
    }

    /// VT がアクティブに戻ったとき。master を取り直して、表示中のバッファで CRTC を設定し直す。
    pub fn resume(&mut self) -> Result<(), String> {
        let _ = self.card.acquire_master_lock();
        if let Some(bo) = &self.current {
            let fb = fb_for(&self.card, &mut self.fbs, bo)?;
            self.out
                .set(&self.card, fb)
                .map_err(|e| format!("set_crtc(復帰): {e}"))?;
        }
        self.suspended = false;
        Ok(())
    }

    /// RGBA8(上から下、stride = 幅 × 4)の完成画面を表示する。
    pub fn present(&mut self, rgba: &[u8]) -> Result<(), String> {
        self.present_frame(&Frame::Image(rgba))
    }

    /// `frame` を GPU で合成して表示する。page flip の完了まで待つ。一時停止中は何もしない。
    pub fn present_frame(&mut self, frame: &Frame<'_>) -> Result<(), String> {
        if self.suspended {
            return Ok(());
        }
        self.comp.draw(&self.ctx.gl, frame)?;
        self.ctx.swap_buffers()?;
        let bo = self.surface.lock_front_buffer()?;
        let fb = match fb_for(&self.card, &mut self.fbs, &bo) {
            Ok(fb) => fb,
            Err(e) => {
                self.surface.release(bo);
                return Err(e);
            }
        };
        let shown = match &self.current {
            // 最初の 1 枚は set_crtc で出す
            None => self.out.set(&self.card, fb).map_err(|e| {
                format!("set_crtc: {e} (DRM master を他のプロセスが持っていないか確認)")
            }),
            Some(_) => self
                .card
                .page_flip(self.out.crtc, fb, PageFlipFlags::EVENT, None)
                .map_err(|e| format!("page_flip: {e}"))
                .and_then(|()| self.wait_flip()),
        };
        if let Err(e) = shown {
            self.surface.release(bo);
            return Err(e);
        }
        if let Some(prev) = self.current.replace(bo) {
            self.surface.release(prev);
        }
        Ok(())
    }

    /// page flip の完了イベントを待つ。
    fn wait_flip(&self) -> Result<(), String> {
        loop {
            let mut pfd = libc::pollfd {
                fd: self.card.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, FLIP_TIMEOUT_MS) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("poll: {e}"));
            }
            if n == 0 {
                return Err(format!("page flip が {FLIP_TIMEOUT_MS}ms 以内に終わらない"));
            }
            let events = self
                .card
                .receive_events()
                .map_err(|e| format!("DRM イベントの読み取り: {e}"))?;
            if events.into_iter().any(|e| matches!(e, Event::PageFlip(_))) {
                return Ok(());
            }
        }
    }
}

/// `bo` の framebuffer を返す(初めてのバッファなら作る)。
fn fb_for(
    card: &Card,
    fbs: &mut HashMap<u32, framebuffer::Handle>,
    bo: &gbm::Bo,
) -> Result<framebuffer::Handle, String> {
    if let Some(fb) = fbs.get(&bo.gem_handle()) {
        return Ok(*fb);
    }
    let fb = card
        .add_framebuffer(bo, 24, 32)
        .map_err(|e| format!("add_framebuffer(GBM): {e}"))?;
    fbs.insert(bo.gem_handle(), fb);
    Ok(fb)
}

impl Drop for GlesDisplay {
    fn drop(&mut self) {
        // 走査中の framebuffer を消す前に元の CRTC に戻す
        if !self.suspended {
            self.out.restore(&self.card);
        }
        if let Some(bo) = self.current.take() {
            self.surface.release(bo);
        }
        for fb in self.fbs.values() {
            let _ = self.card.destroy_framebuffer(*fb);
        }
        self.comp.destroy(&self.ctx.gl);
    }
}
