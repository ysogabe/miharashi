//! 焼き付き対策の「暗転して描き直す」演出。グリッチ(横の帯のずれと色の分離)→ 暗転 → グリッチが
//! 弱まりながら再表示、の順に `REFRESH_SECS` 秒で終わる。動かない線(レーダーの輪や枠)を出し続ける
//! 画面で、数分に一度だけ使う。CPU で 1 フレームずつ作る(短い演出なので GPU には載せない)。
use crate::canvas::{Canvas, rgba};
use crate::photo_layout::scale;

/// 演出の長さ [秒]。
pub const REFRESH_SECS: f32 = 1.6;
/// グリッチが強まる区間の終わり、暗転の終わり [秒]。
const GLITCH_IN: f32 = 0.6;
const BLACK_END: f32 = 1.0;

/// 演出の開始から `t` 秒のフレームを `dst` に作る。`clean` は元の画面、`seed` はフレームごとに変える。
pub fn refresh_frame(dst: &mut Canvas, clean: &Canvas, t: f32, seed: u32) {
    if t < GLITCH_IN {
        glitch(dst, clean, (t / GLITCH_IN).clamp(0.05, 1.0), seed);
    } else if t < BLACK_END {
        dst.clear(rgba(0, 0, 0, 255));
    } else if t < REFRESH_SECS {
        let s = 1.0 - (t - BLACK_END) / (REFRESH_SECS - BLACK_END);
        glitch(dst, clean, s.clamp(0.0, 1.0), seed);
    } else {
        dst.pixmap.data_mut().copy_from_slice(clean.pixmap.data());
    }
}

/// xorshift32。演出の乱れ方を決めるだけなので、暗号の強さは要らない。
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// [lo, hi) の整数。
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo).max(1) as u32) as i32
    }
}

/// 強さ `s`(0〜1)のグリッチ。帯を横にずらし、強いときは色(R と B)を左右に分け、全体を少し暗くする。
fn glitch(dst: &mut Canvas, clean: &Canvas, s: f32, seed: u32) {
    let (w, h) = (dst.width() as usize, dst.height() as usize);
    let k = scale(dst.height());
    let src = clean.pixmap.data();
    let out = dst.pixmap.data_mut();
    out.copy_from_slice(src);
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761).max(1));
    let row = w * 4;
    // 横の帯のずれ
    let bands = 3 + (s * 12.0) as i32;
    let max_dx = ((4.0 + 160.0 * s) * k) as i32;
    let max_bh = ((8.0 + 70.0 * s) * k) as i32;
    let split = ((2.0 + 16.0 * s) * k) as usize;
    for _ in 0..bands {
        let y0 = rng.range(0, h as i32) as usize;
        let bh = rng.range(2, max_bh.max(3)) as usize;
        let dx = rng.range(-max_dx, max_dx + 1);
        for y in y0..(y0 + bh).min(h) {
            let line = &src[y * row..(y + 1) * row];
            let o = &mut out[y * row..(y + 1) * row];
            shift_row(o, line, dx);
            if s > 0.4 {
                split_channels(o, split);
            }
        }
    }
    // 強いときは全体の色を分け、走査線のように 1 行おきに暗くする
    let dim = 1.0 - 0.35 * s;
    for y in 0..h {
        let o = &mut out[y * row..(y + 1) * row];
        if s > 0.7 && y % 3 == 0 {
            split_channels(o, split / 2);
        }
        let f = if y % 2 == 0 {
            dim
        } else {
            dim * (1.0 - 0.25 * s)
        };
        if f < 0.999 {
            let f = (f * 256.0) as u32;
            for px in o.as_chunks_mut::<4>().0 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * f) >> 8) as u8;
                }
            }
        }
    }
    // 細い色の線(シアンとマゼンタ)をいくつか
    let lines = (s * 4.0) as i32;
    for i in 0..lines {
        let y = rng.range(0, h as i32) as f32;
        let x = rng.range(0, w as i32 / 2) as f32;
        let len = rng.range(w as i32 / 8, w as i32 / 2) as f32;
        let color = if i % 2 == 0 {
            rgba(60, 240, 255, 150)
        } else {
            rgba(255, 60, 220, 150)
        };
        dst.fill_rect(x, y, len, (2.0 * k).max(1.0), color);
    }
}

/// 1 行を `dx` px ずらして写す。はみ出した側は黒で埋める。
fn shift_row(out: &mut [u8], line: &[u8], dx: i32) {
    let n = line.len();
    let d = (dx.unsigned_abs() as usize * 4).min(n);
    if dx >= 0 {
        out[d..].copy_from_slice(&line[..n - d]);
        fill_black(&mut out[..d]);
    } else {
        out[..n - d].copy_from_slice(&line[d..]);
        fill_black(&mut out[n - d..]);
    }
}

fn fill_black(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        *p = [0, 0, 0, 255];
    }
}

/// 赤を左へ、青を右へ `split` px ずらす(色ずれ)。
fn split_channels(o: &mut [u8], split: usize) {
    let w = o.len() / 4;
    if split == 0 || split >= w {
        return;
    }
    for x in 0..w - split {
        o[x * 4] = o[(x + split) * 4];
    }
    for x in (split..w).rev() {
        o[x * 4 + 2] = o[(x - split) * 4 + 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Canvas {
        let mut c = Canvas::new(320, 135);
        c.clear(rgba(10, 60, 30, 255));
        c.fill_rect(100.0, 40.0, 120.0, 50.0, rgba(170, 245, 190, 255));
        c
    }

    #[test]
    fn glitch_then_black_then_back_to_the_screen() {
        let src = clean();
        let mut dst = Canvas::new(320, 135);
        refresh_frame(&mut dst, &src, 0.3, 7);
        assert!(dst.pixmap.data() != src.pixmap.data(), "乱れる");
        refresh_frame(&mut dst, &src, 0.8, 8);
        assert!(
            dst.pixmap
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 0, 255]),
            "暗転"
        );
        refresh_frame(&mut dst, &src, 1.3, 9);
        assert!(dst.pixmap.data() != src.pixmap.data(), "乱れながら戻る");
        refresh_frame(&mut dst, &src, REFRESH_SECS, 10);
        assert!(dst.pixmap.data() == src.pixmap.data(), "元の画面");
    }

    #[test]
    fn shift_fills_the_gap_with_black() {
        let line: Vec<u8> = (0..4u8).flat_map(|i| [i, i, i, 255]).collect();
        let mut out = vec![0u8; 16];
        shift_row(&mut out, &line, 1);
        assert_eq!(&out[..8], &[0, 0, 0, 255, 0, 0, 0, 255]);
        shift_row(&mut out, &line, -1);
        assert_eq!(&out[..4], &[1, 1, 1, 255]);
        assert_eq!(&out[12..], &[0, 0, 0, 255]);
        shift_row(&mut out, &line, 99);
        assert!(out.as_chunks::<4>().0.iter().all(|p| *p == [0, 0, 0, 255]));
    }
}
