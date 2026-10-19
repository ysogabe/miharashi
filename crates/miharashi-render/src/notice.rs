//! 画面の代わりに出す案内(CPU 描画)。3D だけの画面は CPU では描けないので、GPU が使えないときと
//! 3D が失敗したときは、この案内が完成画面になる(docs/DESIGN-screen-plugin-v2.md §6)。
use crate::canvas::{Canvas, rgba};
use crate::photo_layout::scale;
use crate::text::TextEngine;

const BG: [u8; 4] = [10, 14, 22, 255];
const ACCENT: [u8; 4] = [64, 200, 255, 255];
const TEXT: [u8; 4] = [230, 236, 245, 255];
const DIM: [u8; 4] = [140, 150, 165, 255];

/// `title` の画面は GPU 描画が必要で、いまは出せない、という案内を画面いっぱいに描く。
pub fn gpu_required(c: &mut Canvas, t: &mut TextEngine, title: &str) {
    let k = scale(c.height());
    let (w, h) = (c.width() as f32, c.height() as f32);
    c.clear(rgba(BG[0], BG[1], BG[2], BG[3]));
    let line = |t: &mut TextEngine, c: &mut Canvas, text: &str, y: f32, size: f32, color| {
        let size = size * k;
        let x = (w - t.measure(text, size)) / 2.0;
        t.draw(c, text, x, y * k + h / 2.0 - 120.0 * k, size, color);
    };
    line(t, c, title, 0.0, 28.0, ACCENT);
    line(t, c, "この画面は GPU 描画が必要です", 60.0, 52.0, TEXT);
    line(
        t,
        c,
        "3D を GPU で描くため、CPU 描画(renderer = cpu)や GPU が使えないときは出せません",
        160.0,
        26.0,
        DIM,
    );
    line(
        t,
        c,
        "3D の描画に失敗したときは、再起動するまで出せません。ほかの画面に切り替えてください",
        204.0,
        26.0,
        DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_required_fills_the_screen_and_draws_text() {
        let mut c = Canvas::new(640, 360);
        let mut t = TextEngine::new("Noto Sans CJK JP");
        gpu_required(&mut c, &mut t, "CUBE");
        let px = c.pixmap.data();
        assert_eq!(&px[..4], &BG, "背景は不透明");
        assert!(
            px.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 150 && p[1] > 150),
            "文字が描かれる"
        );
    }
}
