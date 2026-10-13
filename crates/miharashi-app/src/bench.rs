//! `miharashi bench`: CPU 描画のベースライン(PoC-005 #16)。デバイスを開かず、場面ごとに
//! N 回描いて CPU の ms/回を出す。GPU 経路(PR2 以降)と比べる基準にする。
use crate::data;
use image::{DynamicImage, RgbImage};
use miharashi_render::photo::{self, blurred_background, compose_image};
use miharashi_render::photo_layout;
use miharashi_render::screens::{self, PhotoInfo, StatusPage};
use miharashi_render::{Canvas, TextEngine};
use std::time::Instant;

/// 1 場面の結果。
pub struct Row {
    pub scene: &'static str,
    pub size: (u32, u32),
    pub runs: usize,
    pub avg_ms: f64,
    pub p95_ms: f64,
}

/// `f` を `runs` 回実行し、1 回あたりの時間を集計する(最初の 1 回は捨てる)。
fn measure(scene: &'static str, size: (u32, u32), runs: usize, mut f: impl FnMut()) -> Row {
    f();
    let mut ms: Vec<f64> = (0..runs.max(1))
        .map(|_| {
            let t0 = Instant::now();
            f();
            t0.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    let p95 = ms[((ms.len() as f64 * 0.95).ceil() as usize).clamp(1, ms.len()) - 1];
    Row {
        scene,
        size,
        runs: ms.len(),
        avg_ms: ms.iter().sum::<f64>() / ms.len() as f64,
        p95_ms: p95,
    }
}

/// 写真の代わりに使う 4032×3024(4:3、スマートフォン相当)のノイズ入りグラデーション。
/// 平坦な画像だと JPEG のデコードが軽すぎるので、細かい模様を入れる。
pub fn synthetic_photo() -> DynamicImage {
    let (w, h) = (4032u32, 3024u32);
    let mut seed = 0x9e37_79b9_u32;
    let img = RgbImage::from_fn(w, h, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let n = seed & 0x3f;
        image::Rgb([
            ((x * 255 / w + n) % 256) as u8,
            ((y * 255 / h + n) % 256) as u8,
            (((x + y) * 128 / (w + h) + n * 2) % 256) as u8,
        ])
    });
    DynamicImage::ImageRgb8(img)
}

fn jpeg_bytes(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut out),
        image::ImageFormat::Jpeg,
    )
    .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 全場面を `sizes` ごとに測る。`photo` は JPEG/PNG のバイト列(None なら合成画像)。
pub fn run(
    text: &mut TextEngine,
    sizes: &[(u32, u32)],
    frames: usize,
    photo_bytes: Option<Vec<u8>>,
) -> Result<Vec<Row>, String> {
    let bytes = match photo_bytes {
        Some(b) => b,
        None => jpeg_bytes(&synthetic_photo())?,
    };
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let heavy = frames.clamp(1, 5);
    let snap = data::dummy_snapshot(0.0);
    let info = PhotoInfo {
        place: "東京都 渋谷区".into(),
        taken: "撮影 2026/03/08 13:22".into(),
        age: "7か月前".into(),
        position: (3, 120),
    };
    let mut rows = Vec::new();
    for &(w, h) in sizes {
        let size = (w, h);
        let mut c = Canvas::new(w, h);
        // 写真の完成フレーム(取得スレッドが 1 枚ごとに 1 回作るもの)と、フェード元の古いフレーム
        let layout = photo_layout::plan(&mut 0, w, h, img.width(), img.height());
        let frame = compose_image(&img, w, h, &layout)?;
        let old = compose_image(&img.fliph(), w, h, &layout)?;

        rows.push(measure("status-overview", size, frames, || {
            screens::status(&mut c, text, &snap, StatusPage::Overview)
        }));
        rows.push(measure("status-details", size, frames, || {
            screens::status(&mut c, text, &snap, StatusPage::Details)
        }));
        rows.push(measure("photo-static", size, frames, || {
            screens::photo_frame(
                &mut c,
                text,
                &snap,
                Some((&frame, &layout)),
                Some(&info),
                None,
                None,
            )
        }));
        rows.push(measure("photo-fade", size, frames, || {
            screens::photo_frame(
                &mut c,
                text,
                &snap,
                Some((&frame, &layout)),
                Some(&info),
                Some((&old, 0.5)),
                None,
            )
        }));
        // GPU 経路(PoC-005 PR3)で CPU に残る仕事: 透明な Canvas に重ね絵だけを描く。
        // 写真の貼り付けとフェードの合成は GPU が行う
        rows.push(measure("photo-overlay", size, frames, || {
            c.clear(miharashi_render::canvas::rgba(0, 0, 0, 0));
            screens::photo_overlay(&mut c, text, &snap, &layout, Some(&info), None)
        }));
        // dumb buffer への変換コピー(runtime の present と同じ処理)
        let pitch = w as usize * 4;
        let mut buf = vec![0u8; pitch * h as usize];
        rows.push(measure("present-xrgb8888", size, frames, || {
            c.write_xrgb8888(&mut buf, pitch)
        }));
        // 写真 1 枚ごとの処理(取得スレッド)
        rows.push(measure("photo-decode", size, heavy, || {
            let _ = image::load_from_memory(&bytes);
        }));
        rows.push(measure("photo-blur-bg", size, heavy, || {
            let _ = blurred_background(&img, w, h);
        }));
        rows.push(measure("photo-compose", size, heavy, || {
            let _ = photo::compose(&bytes, w, h, &mut 0);
        }));
    }
    Ok(rows)
}

/// 結果の表。CPU 使用率への換算は場面の頻度(状況画面は 1Hz、フェードは 20ms ごと、
/// 写真の処理は 1 枚ごと)で変わるので、ここでは時間だけを出す。
pub fn format(rows: &[Row]) -> String {
    let mut s = format!(
        "{:<18} {:>9} {:>5} {:>9} {:>9}\n",
        "scene", "size", "runs", "avg_ms", "p95_ms"
    );
    for r in rows {
        s.push_str(&format!(
            "{:<18} {:>9} {:>5} {:>9.2} {:>9.2}\n",
            r.scene,
            format!("{}x{}", r.size.0, r.size.1),
            r.runs,
            r.avg_ms,
            r.p95_ms,
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_runs_all_scenes_on_small_canvas() {
        let mut t = crate::app::text_engine("");
        // 小さい画像と画面で全場面が動くことだけ確かめる
        let small = jpeg_bytes(&DynamicImage::ImageRgb8(RgbImage::from_pixel(
            64,
            48,
            image::Rgb([120, 80, 40]),
        )))
        .unwrap();
        let rows = run(&mut t, &[(320, 135)], 2, Some(small)).unwrap();
        let scenes: Vec<&str> = rows.iter().map(|r| r.scene).collect();
        assert_eq!(
            scenes,
            [
                "status-overview",
                "status-details",
                "photo-static",
                "photo-fade",
                "photo-overlay",
                "present-xrgb8888",
                "photo-decode",
                "photo-blur-bg",
                "photo-compose"
            ]
        );
        assert!(rows.iter().all(|r| r.avg_ms >= 0.0 && r.p95_ms >= 0.0));
        let table = format(&rows);
        assert!(table.contains("photo-fade") && table.contains("320x135"));
    }
}
