use crate::Canvas;
use crate::photo_layout::{self, Mode, PhotoLayout};
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};
use tiny_skia::Pixmap;

/// ぼかし背景の明るさ(元の写真に対する倍率)。
const BG_BRIGHTNESS: f32 = 0.42;

/// straight RGBA8 を premultiplied の Pixmap にする。
fn to_pixmap(img: &RgbaImage) -> Result<Pixmap, String> {
    let mut pix = Pixmap::new(img.width(), img.height()).ok_or("invalid size")?;
    for (s, d) in img
        .as_raw()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(pix.data_mut().as_chunks_mut::<4>().0.iter_mut())
    {
        let a = s[3] as u32;
        d[0] = ((s[0] as u32 * a + 127) / 255) as u8;
        d[1] = ((s[1] as u32 * a + 127) / 255) as u8;
        d[2] = ((s[2] as u32 * a + 127) / 255) as u8;
        d[3] = s[3];
    }
    Ok(pix)
}

/// `w`×`h` を埋めるよう中央切り抜きで拡大縮小する(cover)。
fn cover(img: &DynamicImage, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let scale = (w as f32 / iw).max(h as f32 / ih);
    let (rw, rh) = ((iw * scale).ceil() as u32, (ih * scale).ceil() as u32);
    let resized = img.resize_exact(rw.max(w), rh.max(h), FilterType::Lanczos3);
    resized
        .crop_imm((resized.width() - w) / 2, (resized.height() - h) / 2, w, h)
        .to_rgba8()
}

/// JPEG/PNG をデコードし、`w`×`h` を埋めるよう中央切り抜きで縮小する(cover)。
pub fn load_cover(bytes: &[u8], w: u32, h: u32) -> Result<Pixmap, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    to_pixmap(&cover(&img, w, h))
}

/// 箱型ぼかし(横→縦、端は端の画素で延長)を `passes` 回かける。`buf` は RGBA8。
pub fn box_blur(buf: &mut [u8], w: usize, h: usize, radius: usize, passes: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..passes {
        blur_axis(buf, &mut tmp, w, h, radius, true);
        blur_axis(&tmp, buf, w, h, radius, false);
    }
}

/// 1 方向の箱型ぼかし。`horizontal` なら行方向、そうでなければ列方向。
fn blur_axis(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines, step, line_step) = if horizontal {
        (w, h, 4, w * 4)
    } else {
        (h, w, w * 4, 4)
    };
    let win = (2 * r + 1) as u32;
    for line in 0..lines {
        let base = line * line_step;
        for ch in 0..4 {
            let at =
                |i: isize| src[base + i.clamp(0, len as isize - 1) as usize * step + ch] as u32;
            let mut sum: u32 = (-(r as isize)..=r as isize).map(at).sum();
            for i in 0..len {
                dst[base + i * step + ch] = ((sum + win / 2) / win) as u8;
                sum = sum + at((i + r + 1) as isize) - at(i as isize - r as isize);
            }
        }
    }
}

/// 写真を `w`×`h` に拡大し、ぼかして暗くした背景(不透明 RGBA8)を作る。
/// 1/16 に縮小 → ぼかし → 双一次で拡大するので、大きな画面でも軽い。
pub fn blurred_background(img: &DynamicImage, w: u32, h: u32) -> RgbaImage {
    let (sw, sh) = ((w / 16).max(2), (h / 16).max(2));
    let mut small = cover(&img.thumbnail(sw * 4, sh * 4), sw, sh);
    box_blur(small.as_mut(), sw as usize, sh as usize, 3, 3);
    let mut bg = image::imageops::resize(&small, w, h, FilterType::Triangle);
    for p in bg.pixels_mut() {
        for c in &mut p.0[..3] {
            *c = (*c as f32 * BG_BRIGHTNESS) as u8;
        }
        p.0[3] = 255;
    }
    bg
}

/// 写真の周りに薄い影を落とす(背景との境界をなじませる)。
fn drop_shadow(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, s: f32) {
    for i in (1..=10).rev() {
        let d = i as f32 * 3.0 * s;
        c.fill_rect(
            x - d,
            y - d,
            w + 2.0 * d,
            h + 2.0 * d,
            crate::canvas::rgba(0, 0, 0, 9),
        );
    }
}

/// 配置が決まった写真から、画面いっぱいの完成フレーム(背景 + 写真)を作る。
pub fn compose_image(
    img: &DynamicImage,
    w: u32,
    h: u32,
    layout: &PhotoLayout,
) -> Result<Pixmap, String> {
    if layout.mode == Mode::Cover {
        return to_pixmap(&cover(img, w, h));
    }
    let p = layout.photo;
    let mut c = Canvas::new(w, h);
    c.pixmap = to_pixmap(&blurred_background(img, w, h))?;
    let k = photo_layout::scale(h);
    drop_shadow(&mut c, p.x as f32, p.y as f32, p.w as f32, p.h as f32, k);
    let fitted = img.resize_exact(p.w, p.h, FilterType::Lanczos3).to_rgba8();
    c.draw_pixmap(&to_pixmap(&fitted)?, p.x as i32, p.y as i32);
    Ok(c.pixmap)
}

/// バイト列をデコードし、巡回の `slot` に従って配置を決めて完成フレームを作る。
pub fn compose(
    bytes: &[u8],
    w: u32,
    h: u32,
    slot: &mut usize,
) -> Result<(Pixmap, PhotoLayout), String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let layout = photo_layout::plan(slot, w, h, img.width(), img.height());
    let frame = compose_image(&img, w, h, &layout)?;
    Ok((frame, layout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn px(p: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * p.width() + x) * 4) as usize;
        p.data()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn cover_fills_target_size_for_4x3_source() {
        let png = solid_png(160, 120, [200, 100, 50]);
        let pix = load_cover(&png, 192, 108).unwrap();
        assert_eq!((pix.width(), pix.height()), (192, 108));
        assert_eq!(&pix.data()[0..4], &[200, 100, 50, 255]);
    }

    #[test]
    fn blur_keeps_flat_color_and_spreads_edges() {
        let (w, h) = (16usize, 4usize);
        let mut buf = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0 } else { 200 };
                buf[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        box_blur(&mut buf, w, h, 2, 2);
        assert_eq!(buf[3], 255);
        assert_eq!(buf[0], 0, "遠い端は変わらない");
        assert_eq!(buf[(w - 1) * 4], 200);
        let mid = buf[(w / 2) * 4];
        assert!(mid > 0 && mid < 200, "境界はなだらかになる: {mid}");
    }

    #[test]
    fn background_has_canvas_size_and_is_darkened() {
        let img = image::load_from_memory(&solid_png(160, 120, [200, 100, 50])).unwrap();
        let bg = blurred_background(&img, 192, 108);
        assert_eq!((bg.width(), bg.height()), (192, 108));
        let p = bg.get_pixel(96, 54).0;
        assert!(
            (p[0] as i32 - (200.0 * BG_BRIGHTNESS) as i32).abs() <= 2,
            "{p:?}"
        );
        assert_eq!(p[3], 255);
    }

    #[test]
    fn compose_places_photo_at_rect_and_darkens_outside() {
        let png = solid_png(160, 120, [200, 100, 50]);
        let mut slot = 0;
        let (pix, l) = compose(&png, 1920, 1080, &mut slot).unwrap();
        assert_eq!((pix.width(), pix.height()), (1920, 1080));
        assert_eq!((l.mode, l.photo.x, l.photo.w), (Mode::Left, 0, 1440));
        assert_eq!(px(&pix, 700, 500), [200, 100, 50, 255], "写真の中");
        let out = px(&pix, 1800, 500);
        assert!(out[0] < 120 && out[0] > 40, "余白は暗いぼかし背景: {out:?}");
        // 次は右側。写真は x=480 から。
        let (pix, l) = compose(&png, 1920, 1080, &mut slot).unwrap();
        assert_eq!((l.mode, l.photo.x), (Mode::Right, 480));
        assert_eq!(px(&pix, 1000, 500), [200, 100, 50, 255]);
        assert!(px(&pix, 100, 500)[0] < 120);
    }

    #[test]
    fn compose_cover_for_16x9_on_1080p() {
        let png = solid_png(160, 90, [10, 200, 10]);
        let mut slot = 0;
        let (pix, l) = compose(&png, 1920, 1080, &mut slot).unwrap();
        assert_eq!(l.mode, Mode::Cover);
        assert_eq!(px(&pix, 5, 5), [10, 200, 10, 255]);
        assert_eq!(slot, 0, "Cover では巡回を進めない");
    }
}
