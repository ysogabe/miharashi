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

    /// dmabuf(動画のデコード結果など)をテクスチャに取り込む道具。
    pub fn dmabuf(&self) -> Result<crate::video::Dmabuf<'_>, String> {
        crate::video::Dmabuf::new(&self.ctx)
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
        let (r, img) = self.with_fbo((w, h), |gl| {
            let mut comp = Compositor::new(gl, (w, h))?;
            let r = frames.iter().try_for_each(|f| comp.draw(gl, f));
            comp.destroy(gl);
            r
        })?;
        r.map(|()| img)
    }

    /// 画面と同じ大きさの FBO を bind して `f` を呼び、終わったあとの FBO を上から下の RGBA8 で
    /// 読み戻す(ベンチマークの場面もこの上で描く)。
    pub fn with_fbo<R>(
        &self,
        (w, h): (u32, u32),
        f: impl FnOnce(&glow::Context) -> R,
    ) -> Result<(R, Vec<u8>), String> {
        let gl = &self.ctx.gl;
        let mut out = vec![0u8; w as usize * h as usize * 4];
        let r;
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
            r = f(gl);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
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
        // glReadPixels は下の行から返すので、上から下に並べ直す
        let stride = w as usize * 4;
        let flipped = out.chunks_exact(stride).rev().flatten().copied().collect();
        Ok((r, flipped))
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

    /// 借りた側が FBO や GL の状態を汚したまま返しても、`restore_gl_state` のあとの
    /// `Compositor` の描画が、きれいな状態の描画と一致すること(`GlesDisplay::with_gl` の約束)。
    #[test]
    fn offscreen_draw_after_dirty_borrow_matches_clean_draw() {
        let node = Path::new("/dev/dri/renderD128");
        let off = match Offscreen::open(node) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                return;
            }
        };
        let (w, h) = (64u32, 32u32);
        let mut img = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                img.extend_from_slice(&[(x * 4) as u8, (y * 8) as u8, ((x + y) * 3) as u8, 255]);
            }
        }
        let clean = off.render(&img, (w, h)).unwrap();

        // 別の FBO の bind、各種テスト・マスク・bind を残す
        let dirty = |gl: &glow::Context| unsafe {
            let rb = gl.create_renderbuffer().unwrap();
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, 8, 8);
            let fbo = gl.create_framebuffer().unwrap();
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(rb),
            );
            gl.viewport(0, 0, 3, 3);
            for cap in [
                glow::DEPTH_TEST,
                glow::CULL_FACE,
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
            ] {
                gl.enable(cap);
            }
            gl.scissor(0, 0, 1, 1);
            gl.depth_mask(false);
            gl.color_mask(false, true, false, true);
            gl.bind_vertex_array(Some(gl.create_vertex_array().unwrap()));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(gl.create_buffer().unwrap()));
            gl.active_texture(glow::TEXTURE3);
            gl.bind_texture(glow::TEXTURE_2D, Some(gl.create_texture().unwrap()));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        };

        // 汚したままだと描画先がずれて一致しない(この試験が効いている確認)
        let (r, unrestored) = off
            .with_fbo((w, h), |gl| {
                let mut comp = Compositor::new(gl, (w, h))?;
                dirty(gl);
                let r = comp.draw(gl, &Frame::Image(&img));
                comp.destroy(gl);
                r
            })
            .unwrap();
        r.unwrap();
        assert_ne!(diff_rgb(&clean, &unrestored, 0).0, 0);

        let (r, restored) = off
            .with_fbo((w, h), |gl| {
                let target = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
                let mut comp = Compositor::new(gl, (w, h))?;
                dirty(gl);
                crate::display::restore_gl_state(gl, (w, h), target);
                let r = comp.draw(gl, &Frame::Image(&img));
                comp.destroy(gl);
                r
            })
            .unwrap();
        r.unwrap();
        assert_eq!(diff_rgb(&clean, &restored, 0), (0, 0));
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

    /// `present_frame_with` の本体(`draw_frame_with`): 写真の上に追加の描画が重なり、貸した側が
    /// 状態(scissor・ブレンド・プログラムなど)を散らかしても、そのあとの `Compositor` の描画は
    /// 追加の描画なしのときと同じになること。
    #[test]
    fn offscreen_draw_frame_with_overlays_and_leaves_next_frame_intact() {
        use crate::display::draw_frame_with;
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let (w, h) = (64u32, 16u32);
        let n = (w * h) as usize;
        let old = [200u8, 40, 10, 255].repeat(n);
        let new = [20u8, 100, 240, 255].repeat(n);
        let p = |id, rgba| PhotoLayer {
            id,
            src: PhotoSource::Frame(rgba),
        };
        let f1 = Frame::Photo {
            photo: p(1, &old),
            old: None,
            overlay: None,
        };
        let f2 = Frame::Photo {
            photo: p(2, &new),
            old: Some((p(1, &old), 0.5)),
            overlay: None,
        };
        // 貸された側が、半透明の赤を左下 16x8 に ブレンドで重ね、scissor・ブレンドを有効のままにする
        let dirty = |gl: &glow::Context, _: (u32, u32)| unsafe {
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, 16, 8);
            gl.clear_color(0.5, 0.0, 0.0, 0.5);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ZERO);
            gl.active_texture(glow::TEXTURE1);
        };
        // 1 枚目の上に追加の描画が載る
        let ((), img) = off
            .with_fbo((w, h), |gl| {
                let fbo = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
                let mut comp = Compositor::new(gl, (w, h)).unwrap();
                draw_frame_with(&mut comp, gl, (w, h), fbo, &f1, dirty).unwrap();
                comp.destroy(gl);
            })
            .unwrap();
        // 画像は上から下。GL の左下 = 画像の最終行の左端
        let px = |x: usize, y: usize| &img[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4];
        assert_eq!(px(0, h as usize - 1), [128, 0, 0, 128], "追加の描画");
        assert_eq!(
            px(40, h as usize - 1)[..3],
            [200, 40, 10],
            "領域の外は写真のまま"
        );
        assert_eq!(px(0, 0)[..3], [200, 40, 10], "上の行は写真のまま");
        // 貸したあとの次の描画は、貸さなかったときと同じ
        let seq = |lend: bool| {
            off.with_fbo((w, h), |gl| {
                let fbo = unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) };
                let mut comp = Compositor::new(gl, (w, h)).unwrap();
                if lend {
                    draw_frame_with(&mut comp, gl, (w, h), fbo, &f1, dirty).unwrap();
                } else {
                    comp.draw(gl, &f1).unwrap();
                }
                // present_frame と同じ前処理(FBO を戻して描く)
                comp.draw(gl, &f2).unwrap();
                comp.destroy(gl);
            })
            .unwrap()
            .1
        };
        let (a, b) = (seq(true), seq(false));
        assert_eq!(diff_rgb(&a, &b, 0), (0, 0), "次のフレームが影響を受けない");
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
        for (img, mode) in [
            (photo(1600, 1200), Mode::Left),
            (photo(2000, 1125), Mode::Cover),
        ] {
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

    /// レーダー: 線の直後は緑に光り、線の直前と円の外は背景のまま。点は線が通った直後ほど明るい。
    #[test]
    fn offscreen_radar_sweep_and_blips() {
        use crate::compose::{Frame, Radar};
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let size = (400u32, 300u32);
        let bg: Vec<u8> = [8u8, 8, 8, 255].repeat((size.0 * size.1) as usize);
        // 中心 (200, 150)、半径 100。線は東(π/2)。点は北東(さっき通った)と南東(これから通る)
        let blips = [[250.0, 100.0, 0.0], [250.0, 200.0, 0.0]];
        let frame = Frame::Radar {
            background: Some(&bg),
            radar: Radar {
                center: (200.0, 150.0),
                radius: 100.0,
                sweep: std::f32::consts::FRAC_PI_2,
                trail: 0.9,
                scale: 1.0,
                blips: &blips,
            },
        };
        let got = off.render_frames(&[frame], size).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * size.0 + x) * 4) as usize;
            [got[i], got[i + 1], got[i + 2]]
        };
        let after = px(260, 140); // 線の少し後ろ(北東寄り)
        let before = px(260, 165); // 線の少し手前(南東寄り)
        assert!(after[1] > after[0] + 30, "線の後ろは緑: {after:?}");
        assert!(before[1] < 20, "線の手前は暗い: {before:?}");
        assert_eq!(px(10, 10), [8, 8, 8], "円の外は背景のまま");
        let (swept, coming) = (px(250, 100), px(250, 200));
        assert!(
            swept[1] > coming[1] + 40,
            "通った直後の点が明るい: {swept:?} {coming:?}"
        );
    }

    #[test]
    fn offscreen_agent_card_orientation_pose_cache_and_revision() {
        use crate::compose::{AgentGpuCard, Compositor, Frame};
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let size = (12u32, 8u32);
        let bg = [8u8, 12, 20, 255].repeat((size.0 * size.1) as usize);
        // Upper row: red/green. Lower row: blue/white. This catches vertical and horizontal flips.
        let card_a = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let card_b = [
            0u8, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255,
        ];
        let ((uploaded, moved_uploaded, changed_uploaded), got) = off
            .with_fbo(size, |gl| {
                let mut comp = Compositor::new(gl, size).unwrap();
                let layer = |revision, pixels, rect| AgentGpuCard {
                    id: 55,
                    revision,
                    pixels,
                    size: (2, 2),
                    rect,
                    alpha: 1.0,
                };
                // Keep this orientation check at 1:1 so LINEAR filtering is
                // sampled at texel centers; scaling is covered by the CPU/GPU test below.
                let cards = [layer(1, &card_a, (1.0, 1.0, 2.0, 2.0))];
                comp.draw(
                    gl,
                    &Frame::Agents {
                        background: Some(&bg),
                        cards: &cards,
                        scrim_alpha: 0.0,
                        focus_card_id: None,
                    },
                )
                .unwrap();
                let first = comp.agent_uploaded;
                // Movement changes only uniforms; neither full background nor card is transferred.
                let moved = [layer(1, &card_a, (3.0, 1.0, 2.0, 2.0))];
                comp.draw(
                    gl,
                    &Frame::Agents {
                        background: None,
                        cards: &moved,
                        scrim_alpha: 0.0,
                        focus_card_id: None,
                    },
                )
                .unwrap();
                let second = comp.agent_uploaded;
                // A new content revision replaces only the 2x2 cached surface.
                let revised = [layer(2, &card_b, (3.0, 1.0, 2.0, 2.0))];
                comp.draw(
                    gl,
                    &Frame::Agents {
                        background: None,
                        cards: &revised,
                        scrim_alpha: 0.0,
                        focus_card_id: None,
                    },
                )
                .unwrap();
                let third = comp.agent_uploaded;
                comp.destroy(gl);
                (first, second, third)
            })
            .unwrap();
        assert_eq!(uploaded, bg.len() as u64 + card_a.len() as u64);
        assert_eq!(
            moved_uploaded, uploaded,
            "pose-only frames transfer no pixels"
        );
        assert_eq!(
            changed_uploaded,
            uploaded + card_b.len() as u64,
            "a changed revision transfers just its card surface"
        );
        let px = |x: usize, y: usize| {
            let at = (y * size.0 as usize + x) * 4;
            &got[at..at + 4]
        };
        assert_eq!(
            px(3, 1),
            &[0, 0, 255, 255],
            "top-left source pixel remains at top-left"
        );
        assert_eq!(
            px(4, 1),
            &[255, 255, 255, 255],
            "top-right source pixel remains at top-right"
        );
        assert_eq!(
            px(3, 2),
            &[255, 0, 0, 255],
            "bottom-left source pixel remains at bottom-left"
        );
        assert_eq!(
            px(4, 2),
            &[0, 255, 0, 255],
            "bottom-right source pixel remains at bottom-right"
        );
    }

    #[test]
    fn offscreen_agent_alpha_scale_and_scrim_match_cpu_scene() {
        use crate::compose::{AgentGpuCard, Frame};
        use miharashi_render::agents::{AgentCardSurface, AgentPose, AgentScene, render_scene_cpu};
        use std::sync::Arc;
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES が使えないので skip");
            return;
        };
        let size = (12u32, 8u32);
        let bg = [40u8, 80, 120, 255].repeat((size.0 * size.1) as usize);
        let mut card = miharashi_render::Pixmap::new(2, 2).unwrap();
        // Four distinct premultiplied half-opacity colors exercise bilinear
        // sampling while the card is enlarged to twice its surface size.
        card.data_mut().copy_from_slice(&[
            80, 20, 0, 128, 0, 40, 80, 128, 20, 60, 0, 128, 100, 0, 30, 128,
        ]);
        let card = Arc::new(card);
        let pose = AgentPose {
            x: 2.0,
            y: 2.0,
            width: 4.0,
            height: 4.0,
            alpha: 0.5,
        };
        let layer = AgentCardSurface {
            id: 1,
            revision: 1,
            surface: card.clone(),
            pose,
        };
        let scene = AgentScene {
            size,
            background: Some(&bg),
            cards: &[layer],
            scrim_alpha: 0.25,
            focus_card_id: Some(2),
        };
        let cpu = render_scene_cpu(&scene).unwrap();
        let gpu_card = [AgentGpuCard {
            id: 1,
            revision: 1,
            pixels: card.data(),
            size: (2, 2),
            rect: (pose.x, pose.y, pose.width, pose.height),
            alpha: pose.alpha,
        }];
        let gpu = off
            .render_frames(
                &[Frame::Agents {
                    background: Some(&bg),
                    cards: &gpu_card,
                    scrim_alpha: 0.25,
                    focus_card_id: Some(2),
                }],
                size,
            )
            .unwrap();
        let (different, max) = diff_rgb(cpu.data(), &gpu, 2);
        assert_eq!(
            different, 0,
            "CPU/GPU card scale, alpha, and scrim differ; max channel delta={max}"
        );
    }

    /// Manual GPU-side submission sample. This measures CPU time spent issuing
    /// compositor commands; it does not measure GPU completion or app CPU use.
    #[test]
    #[ignore = "manual render-node sample: six cached 600x300 cards over 300 pose-only frames"]
    fn bench_agent_pose_only_submission_300_frames() {
        use crate::compose::{AgentGpuCard, Compositor, Frame};
        use std::time::{Duration, Instant};
        let Ok(off) = Offscreen::open(Path::new("/dev/dri/renderD128")) else {
            eprintln!("GLES render node is unavailable; skipping manual sample");
            return;
        };
        let size = (1920u32, 1080u32);
        let background = [8u8, 12, 20, 255].repeat(size.0 as usize * size.1 as usize);
        let card_size = (600u32, 300u32);
        let colors = [
            [64u8, 200, 255, 255],
            [255, 188, 102, 255],
            [196, 160, 255, 255],
            [255, 127, 140, 255],
            [103, 217, 173, 255],
            [143, 163, 183, 255],
        ];
        let surfaces: Vec<Vec<u8>> = colors
            .iter()
            .map(|color| color.repeat(card_size.0 as usize * card_size.1 as usize))
            .collect();
        let initial: Vec<AgentGpuCard<'_>> = surfaces
            .iter()
            .enumerate()
            .map(|(i, pixels)| AgentGpuCard {
                id: i as u64 + 1,
                revision: 1,
                pixels,
                size: card_size,
                rect: (
                    60.0 + (i % 3) as f32 * 630.0,
                    120.0 + (i / 3) as f32 * 430.0,
                    600.0,
                    300.0,
                ),
                alpha: 1.0,
            })
            .collect();
        let ((initial_upload, final_upload, total_submission), _) = off
            .with_fbo(size, |gl| {
                let mut compositor = Compositor::new(gl, size).unwrap();
                compositor
                    .draw(
                        gl,
                        &Frame::Agents {
                            background: Some(&background),
                            cards: &initial,
                            scrim_alpha: 0.0,
                            focus_card_id: None,
                        },
                    )
                    .unwrap();
                let first_upload = compositor.agent_uploaded;
                let expected_upload =
                    background.len() as u64 + surfaces.iter().map(|s| s.len() as u64).sum::<u64>();
                assert_eq!(
                    first_upload, expected_upload,
                    "only the background and six card surfaces upload initially"
                );

                let mut submission = Duration::ZERO;
                for frame_index in 0..300 {
                    let t = frame_index as f32 / 300.0;
                    let cards: Vec<AgentGpuCard<'_>> = surfaces
                        .iter()
                        .enumerate()
                        .map(|(i, pixels)| {
                            let phase = t * std::f32::consts::TAU + i as f32 * 0.7;
                            let scale = 0.94 + 0.06 * (phase.sin() * 0.5 + 0.5);
                            let width = card_size.0 as f32 * scale;
                            let height = card_size.1 as f32 * scale;
                            let center_x = 360.0 + (i % 3) as f32 * 630.0;
                            let center_y = 270.0 + (i / 3) as f32 * 430.0;
                            AgentGpuCard {
                                id: i as u64 + 1,
                                revision: 1,
                                pixels,
                                size: card_size,
                                rect: (
                                    center_x - width * 0.5,
                                    center_y - height * 0.5,
                                    width,
                                    height,
                                ),
                                alpha: 0.85 + 0.15 * (phase.cos() * 0.5 + 0.5),
                            }
                        })
                        .collect();
                    let start = Instant::now();
                    compositor
                        .draw(
                            gl,
                            &Frame::Agents {
                                background: None,
                                cards: &cards,
                                scrim_alpha: 0.0,
                                focus_card_id: None,
                            },
                        )
                        .unwrap();
                    submission += start.elapsed();
                }
                let last_upload = compositor.agent_uploaded;
                let gl_error = unsafe { gl.get_error() };
                compositor.destroy(gl);
                assert_eq!(
                    gl_error,
                    glow::NO_ERROR,
                    "GLES error during cached card frames: 0x{gl_error:x}"
                );
                (first_upload, last_upload, submission)
            })
            .unwrap();
        assert_eq!(
            final_upload, initial_upload,
            "pose/scale/alpha-only frames must not transfer pixels"
        );
        let average_ms = total_submission.as_secs_f64() * 1000.0 / 300.0;
        eprintln!(
            "agent cached pose-only: 300 frames, 6 cards at 1920x1080; initial_upload={initial_upload} B; final_upload={final_upload} B; mean CPU submission={average_ms:.4} ms/frame"
        );
    }
}
