//! EGL(platform GBM)と GLES 3.2 のコンテキスト。libEGL は実行時に読み込む。
use crate::gbm;
use khronos_egl as egl;
use std::ffi::c_void;

/// `EGL_PLATFORM_GBM_KHR`
const PLATFORM_GBM_KHR: egl::Enum = 0x31D7;

pub type Instance = egl::DynamicInstance<egl::EGL1_5>;

/// EGL display とコンテキスト。window surface を持つ場合と持たない場合(surfaceless)がある。
pub struct Context {
    pub egl: Instance,
    pub display: egl::Display,
    pub context: egl::Context,
    pub surface: Option<egl::Surface>,
    pub gl: glow::Context,
}

impl Context {
    /// `window` が Some ならその GBM surface に描く。None なら surfaceless(FBO に描く検証用)。
    pub fn new(dev: &gbm::Device, window: Option<&gbm::Surface>) -> Result<Self, String> {
        let egl = unsafe { Instance::load_required() }
            .map_err(|e| format!("libEGL を読み込めない: {e}"))?;
        let display = unsafe {
            egl.get_platform_display(
                PLATFORM_GBM_KHR,
                dev.as_ptr() as *mut c_void,
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(|e| format!("eglGetPlatformDisplay(GBM): {e}"))?;
        egl.initialize(display)
            .map_err(|e| format!("eglInitialize: {e}"))?;
        egl.bind_api(egl::OPENGL_ES_API)
            .map_err(|e| format!("eglBindAPI: {e}"))?;

        let surface_bit = if window.is_some() { egl::WINDOW_BIT } else { 0 };
        let attrs = [
            egl::SURFACE_TYPE,
            surface_bit,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::NONE,
        ];
        let mut configs = Vec::with_capacity(64);
        egl.choose_config(display, &attrs, &mut configs)
            .map_err(|e| format!("eglChooseConfig: {e}"))?;
        // window に描くときは、GBM surface と同じ XRGB8888 の config を選ぶ
        let config = configs
            .iter()
            .copied()
            .find(|c| {
                window.is_none()
                    || egl
                        .get_config_attrib(display, *c, egl::NATIVE_VISUAL_ID)
                        .is_ok_and(|v| v as u32 == gbm::XRGB8888)
            })
            .ok_or("XRGB8888 で GLES 3 が使える EGL config がない")?;

        let ctx_attrs = [
            egl::CONTEXT_MAJOR_VERSION,
            3,
            egl::CONTEXT_MINOR_VERSION,
            2,
            egl::NONE,
        ];
        let context = egl
            .create_context(display, config, None, &ctx_attrs)
            .map_err(|e| format!("eglCreateContext(GLES 3.2): {e}"))?;
        let surface = match window {
            Some(w) => Some(
                unsafe {
                    egl.create_platform_window_surface(
                        display,
                        config,
                        w.as_ptr() as *mut c_void,
                        &[egl::ATTRIB_NONE],
                    )
                }
                .map_err(|e| format!("eglCreatePlatformWindowSurface: {e}"))?,
            ),
            None => None,
        };
        egl.make_current(display, surface, surface, Some(context))
            .map_err(|e| format!("eglMakeCurrent: {e}"))?;
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            })
        };
        Ok(Self {
            egl,
            display,
            context,
            surface,
            gl,
        })
    }

    /// "GLES 3.2 Mesa 25.0.7 / AMD Radeon Graphics (...)" のような説明(ログ用)。
    pub fn describe(&self) -> String {
        use glow::HasContext;
        unsafe {
            format!(
                "{} / {}",
                self.gl.get_parameter_string(glow::VERSION),
                self.gl.get_parameter_string(glow::RENDERER)
            )
        }
    }

    pub fn swap_buffers(&self) -> Result<(), String> {
        let s = self.surface.ok_or("surface がない")?;
        self.egl
            .swap_buffers(self.display, s)
            .map_err(|e| format!("eglSwapBuffers: {e}"))
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        if let Some(s) = self.surface {
            let _ = self.egl.destroy_surface(self.display, s);
        }
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}
