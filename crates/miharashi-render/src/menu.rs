//! 画面の一覧(メニュー)。テンキーの 0 で開き、8/2 で選び、5 で移る。表示中の画面の上に重ねる。
use crate::canvas::{Canvas, rgba};
use crate::photo_layout::scale;
use crate::text::TextEngine;

const ACCENT: [u8; 4] = [64, 200, 255, 255];
const TEXT: [u8; 4] = [230, 236, 245, 255];
const DIM: [u8; 4] = [140, 150, 165, 255];

/// `titles` は登録順の画面の題名。`current` は表示中、`selected` は選んでいる画面の添字。
pub fn menu(c: &mut Canvas, t: &mut TextEngine, titles: &[&str], current: usize, selected: usize) {
    let k = scale(c.height());
    let (w, h) = (c.width() as f32, c.height() as f32);
    c.fill_rect(0.0, 0.0, w, h, rgba(0, 0, 0, 150));
    let row = 84.0 * k;
    let (pw, ph) = (720.0 * k, 150.0 * k + row * titles.len() as f32);
    let (x, y) = ((w - pw) / 2.0, (h - ph) / 2.0);
    c.fill_rect(x, y, pw, ph, rgba(16, 24, 36, 240));
    c.stroke_rect(x, y, pw, ph, 2.0 * k, rgba(64, 200, 255, 160));
    t.draw(c, "MENU", x + 40.0 * k, y + 28.0 * k, 30.0 * k, ACCENT);
    t.draw_right(
        c,
        "8/2 選ぶ  5 移る  0 閉じる",
        x + pw - 40.0 * k,
        y + 36.0 * k,
        22.0 * k,
        DIM,
    );
    for (i, title) in titles.iter().enumerate() {
        let ry = y + 100.0 * k + row * i as f32;
        if i == selected {
            c.fill_rect(
                x + 24.0 * k,
                ry,
                pw - 48.0 * k,
                row - 12.0 * k,
                rgba(64, 200, 255, 60),
            );
            c.fill_rect(
                x + 24.0 * k,
                ry,
                6.0 * k,
                row - 12.0 * k,
                rgba(64, 200, 255, 255),
            );
        }
        let color = if i == selected { TEXT } else { DIM };
        t.draw(
            c,
            &format!("{}  {title}", i + 1),
            x + 56.0 * k,
            ry + 14.0 * k,
            36.0 * k,
            color,
        );
        if i == current {
            t.draw_right(c, "表示中", x + pw - 56.0 * k, ry + 22.0 * k, 24.0 * k, DIM);
        }
    }
}
