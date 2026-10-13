//! libgbm の最小限のラッパー(gbm-sys を直接使う)。gbm クレートは drm 0.14 に依存していて、
//! このワークスペースの drm 0.15 と型が合わないため使わない。
use drm::buffer::{Buffer, DrmFourcc, Handle};
use gbm_sys as sys;
use std::num::NonZeroU32;
use std::os::fd::{AsRawFd, BorrowedFd};

/// XRGB8888 の fourcc('XR24')。GBM と DRM で同じ値。
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// GBM デバイス。fd は借りるだけで閉じない(fd の持ち主より先に drop すること)。
pub struct Device {
    ptr: *mut sys::gbm_device,
}

impl Device {
    pub fn new(fd: BorrowedFd<'_>) -> Result<Self, String> {
        let ptr = unsafe { sys::gbm_create_device(fd.as_raw_fd()) };
        if ptr.is_null() {
            return Err("gbm_create_device に失敗".into());
        }
        Ok(Self { ptr })
    }

    pub fn as_ptr(&self) -> *mut sys::gbm_device {
        self.ptr
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { sys::gbm_device_destroy(self.ptr) }
    }
}

/// 走査出力と描画の両方に使える XRGB8888 の GBM surface(EGL の window surface になる)。
pub struct Surface {
    ptr: *mut sys::gbm_surface,
}

impl Surface {
    pub fn new(dev: &Device, (w, h): (u32, u32)) -> Result<Self, String> {
        let flags = sys::gbm_bo_flags::GBM_BO_USE_SCANOUT | sys::gbm_bo_flags::GBM_BO_USE_RENDERING;
        let ptr = unsafe { sys::gbm_surface_create(dev.as_ptr(), w, h, XRGB8888, flags) };
        if ptr.is_null() {
            return Err(format!("gbm_surface_create({w}x{h}) に失敗"));
        }
        Ok(Self { ptr })
    }

    pub fn as_ptr(&self) -> *mut sys::gbm_surface {
        self.ptr
    }

    /// eglSwapBuffers の直後に呼び、描き終わったバッファを受け取る。
    pub fn lock_front_buffer(&self) -> Result<Bo, String> {
        let ptr = unsafe { sys::gbm_surface_lock_front_buffer(self.ptr) };
        if ptr.is_null() {
            return Err("gbm_surface_lock_front_buffer に失敗".into());
        }
        Ok(Bo { ptr })
    }

    /// 表示し終わったバッファを surface に返す。
    pub fn release(&self, bo: Bo) {
        unsafe { sys::gbm_surface_release_buffer(self.ptr, bo.ptr) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { sys::gbm_surface_destroy(self.ptr) }
    }
}

/// surface から借りているバッファ。`Surface::release` で返す。
pub struct Bo {
    ptr: *mut sys::gbm_bo,
}

impl Bo {
    /// GEM ハンドル。surface は同じバッファを使い回すので、framebuffer のキャッシュのキーにする。
    pub fn gem_handle(&self) -> u32 {
        unsafe { sys::gbm_bo_get_handle(self.ptr).u32_ }
    }
}

impl Buffer for Bo {
    fn size(&self) -> (u32, u32) {
        unsafe {
            (
                sys::gbm_bo_get_width(self.ptr),
                sys::gbm_bo_get_height(self.ptr),
            )
        }
    }
    fn format(&self) -> DrmFourcc {
        DrmFourcc::Xrgb8888
    }
    fn pitch(&self) -> u32 {
        unsafe { sys::gbm_bo_get_stride(self.ptr) }
    }
    fn handle(&self) -> Handle {
        Handle::from(NonZeroU32::new(self.gem_handle()).expect("GEM ハンドルが 0"))
    }
}
