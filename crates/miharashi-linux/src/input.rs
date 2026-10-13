use crate::seat::{Opened, Session};
use evdev::{Device, EventType, KeyCode};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Prev,
    Switch,
    Next,
    Quit,
}

struct Dev {
    device: Device,
    opened: Opened,
}

pub struct Keys {
    devices: Vec<Dev>,
    name_filter: Vec<String>,
}

fn is_target(d: &Device) -> bool {
    d.supported_keys()
        .is_some_and(|k| k.contains(KeyCode::KEY_KP4) || k.contains(KeyCode::KEY_LEFT))
}

impl Keys {
    /// 4/6(テンキー、矢印、メイン列)を持つ evdev デバイスを seat 経由で開く(非ブロッキング)。
    /// `names` が空でなければ、デバイス名にそのいずれかを含むものだけを使う。
    pub fn open(session: &mut Session, names: &[String]) -> Result<Self, String> {
        let mut keys = Self {
            devices: Vec::new(),
            name_filter: names.to_vec(),
        };
        keys.reopen(session);
        if keys.devices.is_empty() {
            return Err("対象のキーボードが見つからない(input グループ/seat の権限を確認)".into());
        }
        Ok(keys)
    }

    /// 開いているデバイスを閉じる(VT が非アクティブになるとき)。
    pub fn close(&mut self, session: &mut Session) {
        for d in self.devices.drain(..) {
            drop(d.device);
            session.close_device(d.opened);
        }
    }

    /// デバイスを開き直す(VT が戻ったとき)。開けたデバイス数を返す。
    pub fn reopen(&mut self, session: &mut Session) -> usize {
        self.close(session);
        let mut paths: Vec<PathBuf> = std::fs::read_dir("/dev/input")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"))
            })
            .collect();
        paths.sort();
        for path in paths {
            let Ok(opened) = session.open_device(&path) else {
                continue;
            };
            let Ok(fd) = opened.fd.try_clone() else {
                session.close_device(opened);
                continue;
            };
            let device = match Device::from_fd(fd) {
                Ok(d) if is_target(&d) => d,
                _ => {
                    session.close_device(opened);
                    continue;
                }
            };
            let name = device.name().unwrap_or("").to_string();
            if !self.name_filter.is_empty()
                && !self.name_filter.iter().any(|f| name.contains(f.as_str()))
            {
                session.close_device(opened);
                continue;
            }
            if device.set_nonblocking(true).is_err() {
                session.close_device(opened);
                continue;
            }
            self.devices.push(Dev { device, opened });
        }
        self.devices.len()
    }

    pub fn device_names(&self) -> Vec<String> {
        self.devices
            .iter()
            .map(|d| d.device.name().unwrap_or("?").to_string())
            .collect()
    }

    /// 溜まっているキー押下(value=1 のみ、リピート除外)を取り出す。
    pub fn poll(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        for d in &mut self.devices {
            let Ok(events) = d.device.fetch_events() else {
                continue;
            };
            for ev in events {
                if ev.event_type() != EventType::KEY || ev.value() != 1 {
                    continue;
                }
                if let Some(a) = map_key(KeyCode::new(ev.code())) {
                    out.push(a);
                }
            }
        }
        out
    }
}

pub fn map_key(k: KeyCode) -> Option<Action> {
    match k {
        KeyCode::KEY_KP4 | KeyCode::KEY_LEFT | KeyCode::KEY_4 => Some(Action::Prev),
        KeyCode::KEY_KP5 | KeyCode::KEY_5 => Some(Action::Switch),
        KeyCode::KEY_KP6 | KeyCode::KEY_RIGHT | KeyCode::KEY_6 => Some(Action::Next),
        KeyCode::KEY_ESC | KeyCode::KEY_Q => Some(Action::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_keys_map_regardless_of_numlock() {
        // NumLock の状態はカーネルの evdev には影響せず、テンキーは常に KEY_KP* として届く。
        assert_eq!(map_key(KeyCode::KEY_KP4), Some(Action::Prev));
        assert_eq!(map_key(KeyCode::KEY_KP5), Some(Action::Switch));
        assert_eq!(map_key(KeyCode::KEY_KP6), Some(Action::Next));
    }

    #[test]
    fn arrows_and_main_row_keys_also_work() {
        assert_eq!(map_key(KeyCode::KEY_LEFT), Some(Action::Prev));
        assert_eq!(map_key(KeyCode::KEY_RIGHT), Some(Action::Next));
        assert_eq!(map_key(KeyCode::KEY_4), Some(Action::Prev));
        assert_eq!(map_key(KeyCode::KEY_6), Some(Action::Next));
        assert_eq!(map_key(KeyCode::KEY_ESC), Some(Action::Quit));
        assert_eq!(map_key(KeyCode::KEY_A), None);
    }
}
