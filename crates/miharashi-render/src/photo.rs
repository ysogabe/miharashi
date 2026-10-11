use image::imageops::FilterType;
use tiny_skia::Pixmap;

/// JPEG/PNG をデコードし、`w`×`h` を埋めるよう中央切り抜きで縮小する(cover)。
pub fn load_cover(bytes: &[u8], w: u32, h: u32) -> Result<Pixmap, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let scale = (w as f32 / iw).max(h as f32 / ih);
    let (rw, rh) = ((iw * scale).ceil() as u32, (ih * scale).ceil() as u32);
    let resized = img.resize_exact(rw.max(w), rh.max(h), FilterType::Lanczos3);
    let cropped = resized.crop_imm((resized.width() - w) / 2, (resized.height() - h) / 2, w, h).to_rgba8();
    let mut pix = Pixmap::new(w, h).ok_or("invalid size")?;
    for (s, d) in cropped.chunks_exact(4).zip(pix.data_mut().chunks_exact_mut(4)) {
        let a = s[3] as u32;
        d[0] = ((s[0] as u32 * a + 127) / 255) as u8;
        d[1] = ((s[1] as u32 * a + 127) / 255) as u8;
        d[2] = ((s[2] as u32 * a + 127) / 255) as u8;
        d[3] = s[3];
    }
    Ok(pix)
}
