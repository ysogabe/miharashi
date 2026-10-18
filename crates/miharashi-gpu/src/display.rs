//! GLES 3.2 で KMS に出す Presenter。GBM surface のダブルバッファを page flip で切り替える。
//! 合成は `compose::Compositor`(写真のフェードと文字の重ね絵は GPU で行う)。
use crate::compose::{Compositor, Frame};
use crate::scene::{SceneError, SceneFrame, SceneOutcome};
use crate::{egl, gbm};
use drm::Device as _;
use drm::control::{Device as ControlDevice, Event, PageFlipFlags, framebuffer};
use miharashi_glscene::GlScene;
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

    /// これまでに Agent 画面の背景とカードの面を GPU へ転送した量 [バイト](`Frame::Agents` の分だけ)。
    pub fn agent_uploaded(&self) -> u64 {
        self.comp.agent_uploaded
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

    /// 画面の電源を切り替える(DPMS)。消している間は page flip が終わらないことがあるので、
    /// 呼び出し側は `present_frame` を呼ばないこと。
    pub fn set_power(&mut self, on: bool) -> Result<(), String> {
        self.out.set_power(&self.card, on)
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
        use glow::HasContext;
        // 直前に `with_gl` / `present_with` で貸した側が FBO を残していても、キオスクの描画は画面に出す
        unsafe { self.ctx.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        self.comp.draw(&self.ctx.gl, frame)?;
        self.flip()
    }

    /// 3D の画面を GPU で合成して表示する(下地 → 3D の層 → 最上段の重ね絵。`Compositor::draw_scene`)。
    /// page flip の完了まで待つ。一時停止中は何も描かず(`GlScene::draw` も呼ばず)`Drawn` を返す。
    /// 3D の失敗(panic・`Err`・GL エラー)は `Err` にせず `SceneFailed` で返す。そのとき失敗した場面の
    /// `release` はここで済んでいて、そのフレームは下地と重ね絵だけで表示してある。
    /// `Err` は下地の描画や表示(DRM・EGL・page flip)の失敗だけ。場面の失敗のあとに `Err` になったとき
    /// (最上段の重ね絵の失敗、swap・page flip の失敗)も、場面の失敗は `SceneError::scene_failed` に残る
    /// (呼び出し側は `Err` でも必ず反映すること。反映しないと、`release` 済みの場面を終了時にもう一度 `release` する)。
    pub fn present_scene(&mut self, f: &mut SceneFrame<'_>) -> Result<SceneOutcome, SceneError> {
        if self.suspended {
            return Ok(SceneOutcome::Drawn);
        }
        let outcome = self.comp.draw_scene(&self.ctx.gl, None, f)?;
        self.flip()
            .map_err(|msg| SceneError::after(&outcome, msg))?;
        Ok(outcome)
    }

    /// `scenes` の `release` を GL が current な状態で呼ぶ(終了時。`drop` の前に呼ぶこと)。
    /// `release` が panic しても止めず、そのメッセージを返す。
    pub fn release_scenes(&mut self, scenes: &mut [&mut dyn GlScene]) -> Vec<String> {
        Compositor::release_scenes(&self.ctx.gl, self.out.size(), None, scenes)
    }

    /// `frame` を `present_frame` と同じく GPU で合成したあと、`with_gl` と同じ約束で GL を `f` に貸し
    /// (写真の上へ追加で描くため)、そのあと swap → page flip の完了待ちまで行う。一時停止中は何もせず None。
    /// `present_frame` / `present_with` の挙動は変えない。VRM を写真画面に重ねる PoC(`vrm-view`)が使う。
    pub fn present_frame_with<R>(
        &mut self,
        frame: &Frame<'_>,
        f: impl FnOnce(&glow::Context, (u32, u32)) -> R,
    ) -> Result<Option<R>, String> {
        if self.suspended {
            return Ok(None);
        }
        let size = self.out.size();
        let r = draw_frame_with(&mut self.comp, &self.ctx.gl, size, None, frame, f)?;
        self.flip()?;
        Ok(Some(r))
    }

    /// 既定の framebuffer(ウィンドウ surface)を bind した状態で GL を `f` に貸す(表示はしない)。
    /// 深度バッファは無いので、深度が要る描画は `f` の中で FBO を作って blit すること。
    /// `f` には GL と表示サイズを渡す。VRM 表示の PoC(`vrm-view`)が、初期化と後始末に使う。
    ///
    /// 貸す側と借りる側の約束(Issue #74 の土台):
    /// - 借りた側は、自分で作った GL のオブジェクト(FBO・テクスチャ・プログラムなど)を自分で消す。
    ///   `f` を呼ぶ前と返ったあとに、`Compositor::reset_gl_state` が `Compositor` の前提にする状態へ設定し直す:
    ///   framebuffer の bind(READ / DRAW とも既定)、viewport(画面全体)、
    ///   DEPTH_TEST・CULL_FACE・BLEND・SCISSOR_TEST・STENCIL_TEST などの無効化とそのパラメーターの既定化
    ///   (depth_func・front_face・cull_face・stencil・blend_func・ポリゴンオフセットなど)、
    ///   depth_mask と color_mask の全許可、プログラム・VAO・バッファの bind 解除、
    ///   TEXTURE0〜15 のテクスチャと sampler の bind 解除、PIXEL_STORE(UNPACK / PACK)の既定化、
    ///   scissor の矩形(画面全体)、深度範囲、ブレンド色、line_width など(詳しくは設計書 §5.1)。
    /// - 設定し直さないもの(借りた側が変えたら、そのまま残る): clear 色、uniform の値。
    ///   `Compositor` は使う前にそれぞれ自分で設定する。
    pub fn with_gl<R>(&mut self, f: impl FnOnce(&glow::Context, (u32, u32)) -> R) -> R {
        let size = self.out.size();
        Compositor::reset_gl_state(&self.ctx.gl, size, None);
        let r = f(&self.ctx.gl, size);
        Compositor::reset_gl_state(&self.ctx.gl, size, None);
        r
    }

    /// `with_gl` で `f` に描かせてから、swap → page flip の完了待ちまで行う。一時停止中は何もせず None。
    pub fn present_with<R>(
        &mut self,
        f: impl FnOnce(&glow::Context, (u32, u32)) -> R,
    ) -> Result<Option<R>, String> {
        if self.suspended {
            return Ok(None);
        }
        let r = self.with_gl(f);
        self.flip()?;
        Ok(Some(r))
    }

    /// 描き終えた既定の framebuffer を swap し、front buffer を page flip で出して完了を待つ。
    fn flip(&mut self) -> Result<(), String> {
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

/// `target`(画面なら None)に `frame` を合成し、`Compositor` が前提にする状態へ戻してから `f` に GL を貸し、
/// 終わったらまた戻す。`present_frame_with` の本体(表示なしで試験できるよう分けてある)。
pub fn draw_frame_with<R>(
    comp: &mut Compositor,
    gl: &glow::Context,
    size: (u32, u32),
    target: Option<glow::Framebuffer>,
    frame: &Frame<'_>,
    f: impl FnOnce(&glow::Context, (u32, u32)) -> R,
) -> Result<R, String> {
    use glow::HasContext;
    unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, target) };
    comp.draw(gl, frame)?;
    // 合成のあとはプログラムや TEXTURE1 が残っているので、貸す前に約束の状態へ戻す
    Compositor::reset_gl_state(gl, size, target);
    let r = f(gl, size);
    Compositor::reset_gl_state(gl, size, target);
    Ok(r)
}

/// 背景の合成なしで、`target`(画面なら None)を bind して `with_gl` と同じ約束で GL を `f` に貸す
/// (`present_with` の本体。表示なしで使えるよう分けてある)。
pub fn draw_with<R>(
    gl: &glow::Context,
    size: (u32, u32),
    target: Option<glow::Framebuffer>,
    f: impl FnOnce(&glow::Context, (u32, u32)) -> R,
) -> R {
    Compositor::reset_gl_state(gl, size, target);
    let r = f(gl, size);
    Compositor::reset_gl_state(gl, size, target);
    r
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
