//! VT のテキスト/グラフィックスモード切替。キオスクの間はカーネルのテキスト描画を止める。
use std::os::fd::AsRawFd;

const KDGETMODE: libc::c_ulong = 0x4B3B;
const KDSETMODE: libc::c_ulong = 0x4B3A;
const KD_TEXT: libc::c_int = 0;
const KD_GRAPHICS: libc::c_int = 1;

/// 標準入力が VT なら KD_GRAPHICS にし、Drop で元のモードへ戻す。VT でなければ何もしない。
pub struct VtGuard {
    fd: Option<i32>,
    prev: libc::c_int,
}

impl VtGuard {
    pub fn enter() -> Self {
        let fd = std::io::stdin().as_raw_fd();
        let mut mode: libc::c_int = KD_TEXT;
        // SAFETY: KDGETMODE は int を 1 つ書き込むだけ。VT でなければ -1 が返る。
        let is_vt = unsafe { libc::ioctl(fd, KDGETMODE, &mut mode as *mut libc::c_int) } == 0;
        if !is_vt {
            return Self {
                fd: None,
                prev: KD_TEXT,
            };
        }
        // SAFETY: 引数は整数値。
        unsafe { libc::ioctl(fd, KDSETMODE, KD_GRAPHICS) };
        Self {
            fd: Some(fd),
            prev: mode,
        }
    }

    pub fn is_vt(&self) -> bool {
        self.fd.is_some()
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        if let Some(fd) = self.fd {
            // SAFETY: 引数は整数値。
            unsafe { libc::ioctl(fd, KDSETMODE, self.prev) };
        }
    }
}
