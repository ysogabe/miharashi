//! 表示せずに GLES 経路を検証する。render node で surfaceless のコンテキストを作り、表示と同じ
//! `Compositor` で FBO に描いて読み戻す。CPU 経路の画像と画素が一致するかを確かめる(`png --renderer gles`)。
use crate::compose::{Compositor, Frame};
use crate::{egl, gbm};
use glow::HasContext;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

/// PCI アドレスから render node(`/dev/dri/by-path/pci-…-render`)を返す。
pub fn render_node_for_pci(pci: &str) -> PathBuf {
    PathBuf::from(format!("/dev/dri/by-path/pci-{pci}-render"))
}

/// 検証用のオフスクリーン描画。コンテキストを使い回して複数の画像を描ける。
pub struct Offscreen {
    // 宣言順に drop: EGL → GBM device → render node
    ctx: egl::Context,
    _dev: gbm::Device,
    _node: std::fs::File,
}

impl Offscreen {
    pub fn open(node: &Path) -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(|e| format!("{}: {e}", node.display()))?;
        let dev = gbm::Device::new(file.as_fd())?;
        let ctx = egl::Context::new(&dev, None)?;
        Ok(Self {
            ctx,
            _dev: dev,
            _node: file,
        })
    }

    pub fn describe(&self) -> String {
        self.ctx.describe()
    }

    /// `rgba`(上から下、stride = 幅 × 4)を GLES で描き、上から下の RGBA8 で読み戻す。
    pub fn render(&self, rgba: &[u8], size: (u32, u32)) -> Result<Vec<u8>, String> {
        self.render_frames(&[Frame::Image(rgba)], size)
    }

    /// `frames` を順に同じ合成器で描き、最後の結果を読み戻す(写真のキャッシュや重ね絵の
    /// 使い回しも表示と同じように動く)。
    pub fn render_frames(
        &self,
        frames: &[Frame<'_>],
        (w, h): (u32, u32),
    ) -> Result<Vec<u8>, String> {
        let gl = &self.ctx.gl;
        let mut comp = Compositor::new(gl, (w, h))?;
        let mut out = vec![0u8; w as usize * h as usize * 4];
        unsafe {
            let rb = gl.create_renderbuffer()?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, w as i32, h as i32);
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(rb),
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("FBO が不完全: 0x{status:x}"));
            }
            for f in frames {
                comp.draw(gl, f)?;
            }
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut out)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(fbo);
            gl.delete_renderbuffer(rb);
        }
        comp.destroy(gl);
        // glReadPixels は下の行から返すので、上から下に並べ直す
        let stride = w as usize * 4;
        let flipped = out.chunks_exact(stride).rev().flatten().copied().collect();
        Ok(flipped)
    }
}

/// 2 枚の RGBA8 画像で RGB の差が `tol` を超える画素の数と、最大の差を返す(アルファは見ない)。
pub fn diff_rgb(a: &[u8], b: &[u8], tol: u8) -> (usize, u8) {
    let mut over = 0;
    let mut max = 0u8;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        let d = (0..3).map(|i| pa[i].abs_diff(pb[i])).max().unwrap_or(0);
        max = max.max(d);
        if d > tol {
            over += 1;
        }
    }
    (over, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{Frame, PhotoLayer, PhotoSource};
    use crate::photo::{Placement, SourceImage};

    #[test]
    fn diff_counts_pixels_over_tolerance() {
        let a = [10, 20, 30, 255, 0, 0, 0, 255];
        let b = [11, 20, 30, 0, 0, 0, 9, 255];
        assert_eq!(diff_rgb(&a, &b, 0), (2, 9));
        assert_eq!(diff_rgb(&a, &b, 1), (1, 9), "アルファの違いは数えない");
    }

    /// render node と Mesa がある環境だけで動く。ない環境(CI など)では skip する。
    #[test]
    fn offscreen_blit_matches_input_pixels() {
        let node = Path::new("/dev/dri/renderD128");
        let off = match Offscreen::open(node) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                return;
            }
        };
        eprintln!("{}", off.describe());
        // 行と列で値が変わる模様。上下・左右の反転やずれがあれば一致しない
        let (w, h) = (257u32, 63u32);
        let mut img = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                img.extend_from_slice(&[
                    (x % 256) as u8,
                    (y * 4) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        let got = off.render(&img, (w, h)).unwrap();
        assert_eq!(diff_rgb(&img, &got, 0), (0, 0));
    }

    /// フェード(旧 → 新を不透明度 t で混ぜる)と重ね絵(premultiplied の source-over)が、
    /// CPU で計算した値と ±1 以内で一致すること。重ね絵を渡さない回は前回のものを使うこと。
    #[test]
    fn offscreen_fade_and_overlay_match_cpu_math() {
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let (w, h) = (64u32, 16u32);
        let n = (w * h) as usize;
        let solid = |c: [u8; 4]| c.repeat(n);
        let old = solid([200, 40, 10, 255]);
        let new = solid([20, 100, 240, 255]);
        // 左半分は半透明の白(premultiplied で 128,128,128,128)、右半分は透明
        let mut overlay = Vec::with_capacity(n * 4);
        for _ in 0..h {
            for x in 0..w {
                overlay.extend_from_slice(if x < w / 2 {
                    &[128, 128, 128, 128]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        let p = |id, rgba| PhotoLayer {
            id,
            src: PhotoSource::Frame(rgba),
        };
        let t = 0.25f32;
        let got = off
            .render_frames(
                &[
                    Frame::Photo {
                        photo: p(1, &old),
                        old: None,
                        overlay: Some(&overlay),
                    },
                    Frame::Photo {
                        photo: p(2, &new),
                        old: Some((p(1, &old), t)),
                        overlay: None,
                    },
                ],
                (w, h),
            )
            .unwrap();
        let mix = |a: u8, b: u8| a as f32 + (b as f32 - a as f32) * t;
        let over = |base: f32, src: u8, sa: u8| src as f32 + base * (1.0 - sa as f32 / 255.0);
        for (x, want) in [(0usize, true), (w as usize - 1, false)] {
            let px = &got[x * 4..x * 4 + 3];
            for i in 0..3 {
                let base = mix(old[i], new[i]);
                let exp = if want { over(base, 128, 128) } else { base };
                assert!(
                    (px[i] as f32 - exp).abs() <= 1.0,
                    "x={x} ch={i}: {} vs {exp}",
                    px[i]
                );
            }
        }
    }

    /// 写真から GPU で作った完成フレームが、CPU 経路の `compose_image` とほぼ同じになること。
    /// 左寄せ(ぼかし背景と影あり)と、画面いっぱい(cover)の両方を比べる。
    #[test]
    fn offscreen_photo_build_matches_cpu_compose() {
        use miharashi_render::photo::compose_image;
        use miharashi_render::photo_layout::{self, Mode};
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        // 細かい模様の入ったグラデーション(縮小の重みの違いが差に出るように)
        let photo = |w: u32, h: u32| {
            let mut seed = 0x1234_5678u32;
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let n = seed & 0x1f;
                image::Rgb([
                    ((x * 255 / w + n) % 256) as u8,
                    ((y * 255 / h + n) % 256) as u8,
                    (((x / 7 + y / 5) % 2) * 160 + n) as u8,
                ])
            }))
        };
        let size = (1920u32, 1080u32);
        for (img, mode) in [(photo(1600, 1200), Mode::Left), (photo(2000, 1125), Mode::Cover)] {
            let l = photo_layout::plan(&mut 0, size.0, size.1, img.width(), img.height());
            assert_eq!(l.mode, mode);
            let cpu = compose_image(&img, size.0, size.1, &l).unwrap();
            let src = SourceImage {
                pixels: img.as_bytes(),
                size: (img.width(), img.height()),
                channels: 3,
                place: Placement {
                    cover: l.mode == Mode::Cover,
                    rect: (l.photo.x, l.photo.y, l.photo.w, l.photo.h),
                    scale: photo_layout::scale(size.1),
                },
            };
            let got = off
                .render_frames(
                    &[Frame::Photo {
                        photo: PhotoLayer {
                            id: 1,
                            src: PhotoSource::Image(src),
                        },
                        old: None,
                        overlay: None,
                    }],
                    size,
                )
                .unwrap();
            // 写真の部分と背景の部分を分けて比べる
            let (pw, rect) = (size.0 as usize, l.photo);
            let (mut in_photo, mut in_bg) = ((0usize, 0u8), (0usize, 0u8));
            for (i, (a, b)) in cpu
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(got.as_chunks::<4>().0)
                .enumerate()
            {
                let (x, y) = ((i % pw) as u32, (i / pw) as u32);
                let inside = l.mode == Mode::Cover
                    || (x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h);
                let d = (0..3).map(|k| a[k].abs_diff(b[k])).max().unwrap();
                let s = if inside { &mut in_photo } else { &mut in_bg };
                s.0 += (d > 1) as usize;
                s.1 = s.1.max(d);
            }
            eprintln!("{mode:?}: 写真 {in_photo:?} / 背景 {in_bg:?}(差 >1 の画素数, 最大差)");
            assert!(in_photo.1 <= 2, "{mode:?} 写真の部分: {in_photo:?}");
            assert!(in_bg.1 <= 2, "{mode:?} 背景の部分: {in_bg:?}");
        }
    }
}
