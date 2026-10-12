//! 写真の配置を決める純粋関数。キャンバス寸法に依存せず、解像度が変わっても同じ規則で動く。

/// 写真の表示方式。`Left`/`Right`/`Center` は高さに合わせた全体表示、`Cover` は全面+下部の帯。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Cover,
    Left,
    Right,
    Center,
}

/// 巡回の順序(焼き付き防止)。
pub const CYCLE: [Mode; 3] = [Mode::Left, Mode::Right, Mode::Center];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 完成フレームの内訳。毎フレームの描画はこれを見て文字だけ重ねる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhotoLayout {
    pub mode: Mode,
    /// 写真そのものの矩形
    pub photo: IRect,
    /// 情報の領域(Cover は下部の帯、Center は時刻・日付を出す左の余白)
    pub info: IRect,
    /// Center のみ: 写真の情報を出す右の余白
    pub info2: Option<IRect>,
}

/// 高さ 1080 を基準にした寸法の倍率。
pub fn scale(canvas_h: u32) -> f32 {
    canvas_h as f32 / 1080.0
}

/// 高さに合わせたときの写真の幅。
pub fn fitted_width(canvas_h: u32, photo_w: u32, photo_h: u32) -> u32 {
    ((photo_w as f64 * canvas_h as f64 / photo_h.max(1) as f64).round() as u32).max(1)
}

/// 写真幅 `pw` のとき、その配置を選べるか。
pub fn allowed(mode: Mode, canvas_w: u32, canvas_h: u32, pw: u32) -> bool {
    if pw >= canvas_w {
        return mode == Mode::Cover;
    }
    let free = (canvas_w - pw) as f32;
    let s = scale(canvas_h);
    match mode {
        Mode::Cover => true,
        Mode::Left | Mode::Right => free >= 420.0 * s,
        Mode::Center => free / 2.0 >= 360.0 * s,
    }
}

/// 配置 `mode` の矩形を作る(`allowed` を満たすこと)。
pub fn layout_for(mode: Mode, canvas_w: u32, canvas_h: u32, pw: u32) -> PhotoLayout {
    let (w, h) = (canvas_w, canvas_h);
    let rect = |x, w| IRect { x, y: 0, w, h };
    match mode {
        Mode::Cover => {
            let band = ((130.0 * scale(h)).round() as u32).min(h);
            PhotoLayout {
                mode,
                photo: rect(0, w),
                info: IRect {
                    x: 0,
                    y: h - band,
                    w,
                    h: band,
                },
                info2: None,
            }
        }
        Mode::Left => PhotoLayout {
            mode,
            photo: rect(0, pw),
            info: rect(pw, w - pw),
            info2: None,
        },
        Mode::Right => PhotoLayout {
            mode,
            photo: rect(w - pw, pw),
            info: rect(0, w - pw),
            info2: None,
        },
        Mode::Center => {
            let left = (w - pw) / 2;
            PhotoLayout {
                mode,
                photo: rect(left, pw),
                info: rect(0, left),
                info2: Some(rect(left + pw, w - pw - left)),
            }
        }
    }
}

/// 新しい写真ごとに呼ぶ。`slot` は巡回の次の候補(0=左,1=右,2=中央)。
/// 候補が選べない写真は飛ばして次へ進み、どれも選べなければ Cover にする(`slot` は動かさない)。
pub fn plan(
    slot: &mut usize,
    canvas_w: u32,
    canvas_h: u32,
    photo_w: u32,
    photo_h: u32,
) -> PhotoLayout {
    let pw = fitted_width(canvas_h, photo_w, photo_h);
    for k in 0..CYCLE.len() {
        let i = (*slot + k) % CYCLE.len();
        if allowed(CYCLE[i], canvas_w, canvas_h, pw) {
            *slot = (i + 1) % CYCLE.len();
            return layout_for(CYCLE[i], canvas_w, canvas_h, pw);
        }
    }
    layout_for(Mode::Cover, canvas_w, canvas_h, pw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(w: u32, h: u32, pw: u32, ph: u32, n: usize) -> Vec<Mode> {
        let mut slot = 0;
        (0..n).map(|_| plan(&mut slot, w, h, pw, ph).mode).collect()
    }

    #[test]
    fn aspect_ratios_at_1080p() {
        use Mode::*;
        assert_eq!(modes(1920, 1080, 4, 3, 4), [Left, Right, Left, Right]);
        assert_eq!(
            modes(1920, 1080, 3, 2, 2),
            [Cover, Cover],
            "3:2 は空き 300px"
        );
        assert_eq!(modes(1920, 1080, 16, 9, 2), [Cover, Cover]);
        assert_eq!(modes(1920, 1080, 21, 9, 1), [Cover], "幅が画面を超える");
        assert_eq!(
            modes(1920, 1080, 3, 4, 6),
            [Left, Right, Center, Left, Right, Center]
        );
    }

    #[test]
    fn aspect_ratios_at_ultrawide() {
        use Mode::*;
        assert_eq!(modes(2560, 1080, 4, 3, 3), [Left, Right, Center]);
        assert_eq!(modes(2560, 1080, 16, 9, 4), [Left, Right, Left, Right]);
        assert_eq!(modes(2560, 1080, 21, 9, 1), [Cover]);
    }

    #[test]
    fn photo_rects() {
        let mut s = 0;
        let l = plan(&mut s, 1920, 1080, 4, 3);
        assert_eq!(
            (l.photo.x, l.photo.w, l.info.x, l.info.w),
            (0, 1440, 1440, 480)
        );
        let r = plan(&mut s, 1920, 1080, 4, 3);
        assert_eq!((r.photo.x, r.info.x, r.info.w), (480, 0, 480));
        let mut s = 2;
        let c = plan(&mut s, 2560, 1080, 4, 3);
        assert_eq!(c.mode, Mode::Center);
        assert_eq!((c.photo.x, c.photo.w), (560, 1440));
        assert_eq!(c.info.w + c.info2.unwrap().w + c.photo.w, 2560);
    }

    #[test]
    fn skipped_slot_advances_past_center() {
        // 16:9 を 2560 で表示: Right のあと Center を飛ばして Left に戻る。
        let mut slot = 1;
        let a = plan(&mut slot, 2560, 1080, 16, 9);
        assert_eq!((a.mode, slot), (Mode::Right, 2));
        let b = plan(&mut slot, 2560, 1080, 16, 9);
        assert_eq!((b.mode, slot), (Mode::Left, 1));
    }

    #[test]
    fn thresholds_scale_with_height() {
        // 高さ 540 の画面では 210px 以上の空きで側面配置できる。
        assert!(allowed(Mode::Left, 960, 540, 960 - 210));
        assert!(!allowed(Mode::Left, 960, 540, 960 - 209));
    }
}
