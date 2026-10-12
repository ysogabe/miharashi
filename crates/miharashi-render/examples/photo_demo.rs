//! 合成した写真で、比率・画面サイズ・配置ごとのフォトフレームを /tmp/claude-photo-demo/ に出力する。
//! 実行: cargo run --release -p miharashi-render --example photo_demo
use image::{DynamicImage, Rgba, RgbaImage};
use miharashi_render::photo::compose_image;
use miharashi_render::photo_layout::{Mode, fit_size, layout_for};
use miharashi_render::screens::{PhotoInfo, Snapshot, photo_frame};
use miharashi_render::{Canvas, TextEngine};
use std::time::Instant;

/// 空から地面へのグラデーションに、太陽・山・格子を描いた見分けやすい写真。
fn synthetic(w: u32, h: u32) -> DynamicImage {
    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let t = y as f32 / h as f32;
            let (r, g, b) = if t < 0.6 {
                (60.0 + 120.0 * t, 120.0 + 100.0 * t, 230.0 - 80.0 * t)
            } else {
                (50.0, 140.0 - 60.0 * (t - 0.6), 60.0)
            };
            img.put_pixel(x, y, Rgba([r as u8, g as u8, b as u8, 255]));
        }
    }
    let (cx, cy, rr) = (w as f32 * 0.75, h as f32 * 0.25, h as f32 * 0.1);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            if dx * dx + dy * dy < rr * rr {
                img.put_pixel(x, y, Rgba([255, 220, 80, 255]));
            }
            let ridge = h as f32 * (0.55 - 0.15 * ((x as f32 / w as f32 * 6.0).sin().abs()));
            if y as f32 > ridge && y as f32 <= h as f32 * 0.62 {
                img.put_pixel(x, y, Rgba([70, 80, 110, 255]));
            }
            // 四隅と中央に目印、全体に薄い格子
            let edge = x < 6 || y < 6 || x + 6 >= w || y + 6 >= h;
            let grid = (x % (w / 8).max(1) == 0) || (y % (h / 6).max(1) == 0);
            if edge {
                img.put_pixel(x, y, Rgba([255, 0, 80, 255]));
            } else if grid {
                img.put_pixel(x, y, Rgba([255, 255, 255, 60]));
            }
        }
    }
    DynamicImage::ImageRgba8(img)
}

fn main() {
    let font = std::path::Path::new("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc");
    let mut t = TextEngine::from_font_file("Noto Sans CJK JP", font).expect("font");
    let out = std::path::Path::new("/tmp/claude-photo-demo");
    std::fs::create_dir_all(out).unwrap();
    let snap = Snapshot {
        date: "2026/10/12 (月)".into(),
        time: "14:07:33".into(),
        ..Default::default()
    };
    let info = PhotoInfo {
        place: "徳島県 美馬市 木屋平".into(),
        taken: "撮影 2026/03/08 13:22".into(),
        age: "7か月前".into(),
        position: (12, 187),
    };
    let ratios: [(&str, u32, u32); 5] = [
        ("4x3", 2000, 1500),
        ("3x2", 2100, 1400),
        ("16x9", 2240, 1260),
        ("3x4", 1500, 2000),
        ("21x9", 2520, 1080),
    ];
    for (cw, ch) in [(1920u32, 1080u32), (2560, 1080)] {
        for (name, pw0, ph0) in ratios {
            let img = synthetic(pw0, ph0);
            let modes: Vec<(Mode, (u32, u32))> = [Mode::Left, Mode::Right, Mode::Center]
                .into_iter()
                .filter_map(|m| fit_size(m, cw, ch, pw0, ph0).map(|sz| (m, sz)))
                .collect();
            let modes = if modes.is_empty() {
                vec![(Mode::Cover, (cw, ch))]
            } else {
                modes
            };
            for (m, (pw, ph)) in modes {
                let layout = layout_for(m, cw, ch, pw, ph);
                let t0 = Instant::now();
                let frame = compose_image(&img, cw, ch, &layout).unwrap();
                let compose_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let mut c = Canvas::new(cw, ch);
                let t1 = Instant::now();
                photo_frame(
                    &mut c,
                    &mut t,
                    &snap,
                    Some((&frame, &layout)),
                    Some(&info),
                    None,
                    None,
                );
                let draw_ms = t1.elapsed().as_secs_f64() * 1000.0;
                let file = out.join(format!("{cw}x{ch}_{name}_{m:?}.png"));
                c.save_png(&file).unwrap();
                println!(
                    "{} compose {compose_ms:.0} ms, overlay {draw_ms:.1} ms",
                    file.display()
                );
            }
        }
    }
    // 通知バッジ(再接続中)つき
    let img = synthetic(2000, 1500);
    let layout = layout_for(Mode::Left, 1920, 1080, 1440, 1080);
    let frame = compose_image(&img, 1920, 1080, &layout).unwrap();
    let mut c = Canvas::new(1920, 1080);
    photo_frame(
        &mut c,
        &mut t,
        &snap,
        Some((&frame, &layout)),
        Some(&info),
        None,
        Some("GEOFRAME に接続できません(8秒後に再試行)"),
    );
    c.save_png(&out.join("1920x1080_notice.png")).unwrap();
    photo_frame(&mut c, &mut t, &snap, None, None, None, None);
    c.save_png(&out.join("1920x1080_loading.png")).unwrap();
}
