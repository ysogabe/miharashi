//! libseat(logind または seatd)経由での DRM・入力デバイスの取得と、VT 切替の通知。
//! seat が使えない開発環境(SSH + グループ権限)では `Direct` で直接開く。
use libseat::{Seat, SeatEvent};
use std::cell::Cell;
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatMode {
    /// libseat を試し、使えなければ直接オープンにフォールバックする。
    Auto,
    /// libseat 必須。使えなければエラー。
    Libseat,
    /// デバイスを直接開く(`video`・`input` グループが必要)。
    Direct,
}

impl SeatMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "" | "auto" => Ok(Self::Auto),
            "libseat" => Ok(Self::Libseat),
            "direct" => Ok(Self::Direct),
            other => Err(format!(
                "seat: auto / libseat / direct のいずれか(指定値: {other})"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// VT が非アクティブになる。デバイスの使用を止め、`ack_disable` を呼ぶ。
    Disabled,
    /// VT がアクティブに戻った。デバイスを開き直して描画を再開する。
    Enabled,
}

#[derive(Default)]
struct Flags {
    enabled: Cell<bool>,
    disabled_event: Cell<bool>,
    enabled_event: Cell<bool>,
}

pub struct Opened {
    pub fd: OwnedFd,
    dev: Option<libseat::Device>,
}

pub struct Session {
    seat: Option<Seat>,
    flags: Rc<Flags>,
}

impl Session {
    pub fn open(mode: SeatMode) -> Result<Self, String> {
        if mode == SeatMode::Direct {
            return Ok(Self::direct());
        }
        let flags = Rc::new(Flags::default());
        let cb_flags = flags.clone();
        let opened = Seat::open(move |_seat, ev| match ev {
            SeatEvent::Enable => {
                cb_flags.enabled.set(true);
                cb_flags.enabled_event.set(true);
            }
            SeatEvent::Disable => {
                cb_flags.enabled.set(false);
                cb_flags.disabled_event.set(true);
            }
        });
        match opened {
            Ok(mut seat) => {
                // 最初の Enable を待つ(VT が非アクティブなら届かない)。
                let deadline = Instant::now() + Duration::from_secs(3);
                while !flags.enabled.get() && Instant::now() < deadline {
                    let _ = seat.dispatch(100);
                }
                if !flags.enabled.get() {
                    // この VT/セッションが seat に属していない(SSH など)か、VT が非アクティブ。
                    if mode == SeatMode::Auto {
                        eprintln!(
                            "seat: 有効にならない(seat 外のセッション?)。直接オープンにフォールバック"
                        );
                        drop(seat);
                        return Ok(Self::direct());
                    }
                    return Err("seat が有効にならない(この VT がアクティブか確認)".into());
                }
                flags.enabled_event.set(false);
                Ok(Self {
                    seat: Some(seat),
                    flags,
                })
            }
            Err(e) if mode == SeatMode::Auto => {
                eprintln!("seat: libseat を開けない({e:?})。直接オープンにフォールバック");
                Ok(Self::direct())
            }
            Err(e) => Err(format!("libseat を開けない: {e:?}")),
        }
    }

    fn direct() -> Self {
        let flags = Rc::new(Flags::default());
        flags.enabled.set(true);
        Self { seat: None, flags }
    }

    pub fn is_libseat(&self) -> bool {
        self.seat.is_some()
    }

    pub fn is_enabled(&self) -> bool {
        self.flags.enabled.get()
    }

    pub fn name(&mut self) -> String {
        self.seat
            .as_mut()
            .map_or_else(|| "direct".to_string(), |s| s.name().to_string())
    }

    pub fn open_device(&mut self, path: &Path) -> Result<Opened, String> {
        match self.seat.as_mut() {
            Some(seat) => {
                let dev = seat
                    .open_device(&path)
                    .map_err(|e| format!("{}: {e:?}", path.display()))?;
                let fd = std::os::fd::AsFd::as_fd(&dev)
                    .try_clone_to_owned()
                    .map_err(|e| e.to_string())?;
                Ok(Opened { fd, dev: Some(dev) })
            }
            None => {
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(Opened {
                    fd: f.into(),
                    dev: None,
                })
            }
        }
    }

    pub fn close_device(&mut self, opened: Opened) {
        if let (Some(seat), Some(dev)) = (self.seat.as_mut(), opened.dev) {
            // libseat は device id を解放するだけで、open_device が返した fd は閉じない
            // (閉じるのは呼び出し側。wlroots も close(fd) する)。閉じないと VT 往復のたびに fd が漏れる。
            let raw = std::os::fd::AsRawFd::as_raw_fd(&std::os::fd::AsFd::as_fd(&dev));
            let _ = seat.close_device(dev);
            // SAFETY: raw は libseat が返した fd で、複製(Opened.fd)とは別。ここでしか閉じない。
            unsafe { libc::close(raw) };
        }
    }

    /// libseat のイベントを処理する(ノンブロッキング)。
    pub fn dispatch(&mut self) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        if let Some(seat) = self.seat.as_mut() {
            let _ = seat.dispatch(0);
        }
        if self.flags.disabled_event.replace(false) {
            out.push(SessionEvent::Disabled);
        }
        if self.flags.enabled_event.replace(false) {
            out.push(SessionEvent::Enabled);
        }
        out
    }

    /// `Disabled` を受けて、デバイスの使用を止めた後に呼ぶ。
    pub fn ack_disable(&mut self) {
        if let Some(seat) = self.seat.as_mut() {
            let _ = seat.disable();
        }
    }
}
