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

/// 側面配置で、写真を縮めて余白を作るときに保つ高さの下限(画面の高さに対する割合)。
pub const MIN_SIDE_FILL: f32 = 0.85;

/// 配置 `mode` での写真の表示寸法 (幅, 高さ)。選べなければ None。
/// まず高さに合わせ、側面配置で余白が足りないときは、高さの 85% までなら縮めて上下に背景を見せる
/// (1920×1080 の 3:2 写真は 1500×1000 になる)。中央配置と Cover は縮めない。
pub fn fit_size(
    mode: Mode,
    canvas_w: u32,
    canvas_h: u32,
    photo_w: u32,
    photo_h: u32,
) -> Option<(u32, u32)> {
    let pw = fitted_width(canvas_h, photo_w, photo_h);
    if allowed(mode, canvas_w, canvas_h, pw) {
        return Some((pw, canvas_h));
    }
    if !matches!(mode, Mode::Left | Mode::Right) {
        return None;
    }
    let max_w = canvas_w as f32 - (420.0 * scale(canvas_h)).ceil();
    if max_w < 1.0 {
        return None;
    }
    let ph = (max_w as f64 * photo_h as f64 / photo_w.max(1) as f64).floor() as u32;
    (ph as f32 >= canvas_h as f32 * MIN_SIDE_FILL).then_some((max_w as u32, ph))
}

/// 配置 `mode` の矩形を作る(`fit_size` の寸法を使うこと)。写真は上下中央に置く。
pub fn layout_for(mode: Mode, canvas_w: u32, canvas_h: u32, pw: u32, ph: u32) -> PhotoLayout {
    let (w, h) = (canvas_w, canvas_h);
    let ph = ph.min(h);
    let py = (h - ph) / 2;
    let rect = |x, w| IRect { x, y: 0, w, h };
    let photo = |x, w| IRect { x, y: py, w, h: ph };
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
            photo: photo(0, pw),
            info: rect(pw, w - pw),
            info2: None,
        },
        Mode::Right => PhotoLayout {
            mode,
            photo: photo(w - pw, pw),
            info: rect(0, w - pw),
            info2: None,
        },
        Mode::Center => {
            let left = (w - pw) / 2;
            PhotoLayout {
                mode,
                photo: photo(left, pw),
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
    for k in 0..CYCLE.len() {
        let i = (*slot + k) % CYCLE.len();
        if let Some((pw, ph)) = fit_size(CYCLE[i], canvas_w, canvas_h, photo_w, photo_h) {
            *slot = (i + 1) % CYCLE.len();
            return layout_for(CYCLE[i], canvas_w, canvas_h, pw, ph);
        }
    }
    layout_for(Mode::Cover, canvas_w, canvas_h, canvas_w, canvas_h)
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
            [Left, Right],
            "3:2 は高さ 1000 に縮めて側面配置"
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
    fn side_mode_shrinks_3x2_but_not_16x9() {
        let mut s = 0;
        let l = plan(&mut s, 1920, 1080, 2560, 1707);
        assert_eq!(l.mode, Mode::Left);
        assert_eq!((l.photo.w, l.photo.h, l.photo.y), (1500, 1000, 40));
        assert_eq!((l.info.x, l.info.w, l.info.h), (1500, 420, 1080));
        // 16:9 は 1500×843(78%)になるので縮めず Cover
        assert_eq!(fit_size(Mode::Left, 1920, 1080, 16, 9), None);
        // 中央配置は縮めない
        assert_eq!(fit_size(Mode::Center, 1920, 1080, 3, 2), None);
        // 4:3 は今まで通り高さいっぱい
        assert_eq!(fit_size(Mode::Left, 1920, 1080, 4, 3), Some((1440, 1080)));
    }

    #[test]
    fn thresholds_scale_with_height() {
        // 高さ 540 の画面では 210px 以上の空きで側面配置できる。
        assert!(allowed(Mode::Left, 960, 540, 960 - 210));
        assert!(!allowed(Mode::Left, 960, 540, 960 - 209));
    }
}
