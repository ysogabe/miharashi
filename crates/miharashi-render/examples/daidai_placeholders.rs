//! だいだい画面のアバターのプレースホルダー素材(480x480、48x48 のドットを 10 倍にしたもの)を書き出す。
//! 実ファイルができたら、同じ名前(daidai-<pose>.png)で assets/daidai/ に置き換える。
//!
//! 使い方: `cargo run --release -p miharashi-render --example daidai_placeholders -- crates/miharashi-render/assets/daidai`
use miharashi_render::canvas::rgba;
use miharashi_render::{Canvas, TextEngine};

const CELL: f32 = 10.0;

fn cell(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, col: [u8; 4]) {
    c.fill_rect(
        x as f32 * CELL,
        y as f32 * CELL,
        w as f32 * CELL,
        h as f32 * CELL,
        rgba(col[0], col[1], col[2], col[3]),
    );
}

fn main() -> Result<(), String> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("出力先のディレクトリが要る")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut text = TextEngine::new("Noto Sans CJK JP");
    let body = [255, 146, 48, 255];
    let edge = [196, 92, 20, 255];
    let ink = [40, 24, 16, 255];
    for pose in ["idle", "working", "milestone", "making"] {
        let mut c = Canvas::new(480, 480);
        c.clear(rgba(0, 0, 0, 0));
        // 丸いからだ
        for y in 0..48 {
            for x in 0..48 {
                let (dx, dy) = (
                    (x as f32 + 0.5 - 24.0) / 17.0,
                    (y as f32 + 0.5 - 27.0) / 16.0,
                );
                let r = dx * dx + dy * dy;
                if r <= 1.0 {
                    cell(&mut c, x, y, 1, 1, if r > 0.80 { edge } else { body });
                }
            }
        }
        // ヘタ
        cell(&mut c, 23, 8, 2, 3, [70, 140, 60, 255]);
        cell(&mut c, 25, 9, 4, 2, [70, 140, 60, 255]);
        // 目と口
        cell(&mut c, 17, 23, 3, 4, ink);
        cell(&mut c, 29, 23, 3, 4, ink);
        cell(&mut c, 21, 31, 7, 1, ink);
        cell(&mut c, 20, 30, 1, 1, ink);
        cell(&mut c, 28, 30, 1, 1, ink);
        // ポーズごとの目印
        match pose {
            "working" => {
                cell(&mut c, 15, 20, 6, 1, ink);
                cell(&mut c, 28, 20, 6, 1, ink);
                cell(&mut c, 12, 38, 24, 5, [90, 100, 120, 255]);
            }
            "milestone" => {
                for (x, y) in [(6, 8), (40, 6), (4, 24), (43, 22)] {
                    cell(&mut c, x, y - 2, 1, 5, [255, 230, 90, 255]);
                    cell(&mut c, x - 2, y, 5, 1, [255, 230, 90, 255]);
                }
            }
            "making" => {
                cell(&mut c, 36, 34, 8, 3, [150, 160, 175, 255]);
                cell(&mut c, 34, 32, 3, 7, [150, 160, 175, 255]);
            }
            _ => {}
        }
        let label = format!("PLACEHOLDER {}", pose.to_uppercase());
        let w = text.measure(&label, 26.0);
        text.draw(
            &mut c,
            &label,
            (480.0 - w) / 2.0,
            440.0,
            26.0,
            [255, 255, 255, 255],
        );
        c.save_png(&std::path::Path::new(&dir).join(format!("daidai-{pose}.png")))?;
    }
    Ok(())
}
