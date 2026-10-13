//! 表示せずに GLES 経路を検証する。render node で surfaceless のコンテキストを作り、表示と同じ
//! `Blitter` で FBO に描いて読み戻す。CPU 経路の画像と画素が一致するかを確かめる(`png --renderer gles`)。
use crate::blit::Blitter;
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
    pub fn render(&self, rgba: &[u8], (w, h): (u32, u32)) -> Result<Vec<u8>, String> {
        let gl = &self.ctx.gl;
        let blit = Blitter::new(gl, (w, h))?;
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
            blit.draw(gl, rgba);
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
        blit.destroy(gl);
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
}
