use evdev::{Device, EventType, KeyCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Prev,
    Switch,
    Next,
    Quit,
}

pub struct Keys {
    devices: Vec<Device>,
}

impl Keys {
    /// 4/6(テンキー、矢印、メイン列)を持つ evdev デバイスをすべて開く(非ブロッキング)。
    pub fn open() -> Result<Self, String> {
        let devices: Vec<Device> = evdev::enumerate()
            .map(|(_, d)| d)
            .filter(|d| d.supported_keys().is_some_and(|k| k.contains(KeyCode::KEY_KP4) || k.contains(KeyCode::KEY_LEFT)))
            .collect();
        if devices.is_empty() {
            return Err("テンキー付きキーボードが見つからない(input グループの権限を確認)".into());
        }
        for d in &devices {
            d.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        Ok(Self { devices })
    }

    pub fn device_names(&self) -> Vec<String> {
        self.devices.iter().map(|d| d.name().unwrap_or("?").to_string()).collect()
    }

    /// 溜まっているキー押下(value=1 のみ、リピート除外)を取り出す。
    pub fn poll(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        for d in &mut self.devices {
            let Ok(events) = d.fetch_events() else { continue };
            for ev in events {
                if ev.event_type() != EventType::KEY || ev.value() != 1 {
                    continue;
                }
                match KeyCode::new(ev.code()) {
                    KeyCode::KEY_KP4 | KeyCode::KEY_LEFT | KeyCode::KEY_4 => out.push(Action::Prev),
                    KeyCode::KEY_KP5 | KeyCode::KEY_5 => out.push(Action::Switch),
                    KeyCode::KEY_KP6 | KeyCode::KEY_RIGHT | KeyCode::KEY_6 => out.push(Action::Next),
                    KeyCode::KEY_ESC | KeyCode::KEY_Q => out.push(Action::Quit),
                    _ => {}
                }
            }
        }
        out
    }
}
