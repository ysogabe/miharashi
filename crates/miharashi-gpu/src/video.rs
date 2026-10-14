//! PoC-005 PR4 B2: 動画と認識結果の重ね合わせのベンチマーク。
//! - デコードした画面(VA-API のサーフェス)は dmabuf のまま EGLImage にしてテクスチャに結びつける。
//!   CPU で画素をコピーしない。NV12 を Y(R8)と UV(GR88)の 2 枚で受け取り、シェーダーで RGB にする。
//! - 検出枠とラベルはインスタンス描画で重ねる。ラベルの文字は CPU で 1 回だけ描いたアトラスを使う。
use crate::egl;
use crate::photo::program as link;
use glow::HasContext;
use khronos_egl as k;
use std::ffi::c_void;
use std::os::fd::RawFd;

/// `EGL_LINUX_DMA_BUF_EXT` ほか(EGL_EXT_image_dma_buf_import / _modifiers)
const LINUX_DMA_BUF: k::Enum = 0x3270;
const LINUX_DRM_FOURCC: k::Attrib = 0x3271;
const PLANE0_FD: k::Attrib = 0x3272;
const PLANE0_OFFSET: k::Attrib = 0x3273;
const PLANE0_PITCH: k::Attrib = 0x3274;
const PLANE0_MODIFIER_LO: k::Attrib = 0x3443;
const PLANE0_MODIFIER_HI: k::Attrib = 0x3444;
/// 修飾子なし(`DRM_FORMAT_MOD_INVALID`)
pub const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// dmabuf の 1 枚(1 プレーン)。NV12 なら Y は R8、UV は GR88 の 1 枚ずつで渡す。
#[derive(Clone, Copy, Debug)]
pub struct Layer {
    pub fd: RawFd,
    /// DRM の fourcc(`DRM_FORMAT_R8` など)
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub offset: u32,
    pub pitch: u32,
    pub modifier: u64,
}

/// テクスチャに結びつけた EGLImage。描き終えたら `Dmabuf::release` で捨てる。
pub struct Image(k::Image);

type TargetTexture = unsafe extern "system" fn(u32, *mut c_void);

/// dmabuf を EGLImage にしてテクスチャに結びつける。
pub struct Dmabuf<'a> {
    ctx: &'a egl::Context,
    target: TargetTexture,
}

impl<'a> Dmabuf<'a> {
    pub(crate) fn new(ctx: &'a egl::Context) -> Result<Self, String> {
        let exts = ctx
            .egl
            .query_string(Some(ctx.display), k::EXTENSIONS)
            .map_err(|e| format!("eglQueryString: {e}"))?
            .to_string_lossy()
            .into_owned();
        if !exts
            .split_whitespace()
            .any(|e| e == "EGL_EXT_image_dma_buf_import")
        {
            return Err("EGL_EXT_image_dma_buf_import が無い".into());
        }
        let f = ctx
            .egl
            .get_proc_address("glEGLImageTargetTexture2DOES")
            .ok_or("glEGLImageTargetTexture2DOES が無い")?;
        // SAFETY: GL_OES_EGL_image の関数(target, image)
        let target = unsafe { std::mem::transmute::<extern "system" fn(), TargetTexture>(f) };
        Ok(Self { ctx, target })
    }

    /// `layer` を EGLImage にして `tex`(GL_TEXTURE_2D)に結びつける。
    pub fn bind(
        &self,
        gl: &glow::Context,
        tex: glow::Texture,
        layer: &Layer,
    ) -> Result<Image, String> {
        let mut attrs: Vec<k::Attrib> = vec![
            k::WIDTH as k::Attrib,
            layer.width as k::Attrib,
            k::HEIGHT as k::Attrib,
            layer.height as k::Attrib,
            LINUX_DRM_FOURCC,
            layer.fourcc as k::Attrib,
            PLANE0_FD,
            layer.fd as k::Attrib,
            PLANE0_OFFSET,
            layer.offset as k::Attrib,
            PLANE0_PITCH,
            layer.pitch as k::Attrib,
        ];
        if layer.modifier != MOD_INVALID {
            attrs.extend([
                PLANE0_MODIFIER_LO,
                (layer.modifier & 0xffff_ffff) as k::Attrib,
                PLANE0_MODIFIER_HI,
                (layer.modifier >> 32) as k::Attrib,
            ]);
        }
        attrs.push(k::ATTRIB_NONE);
        // SAFETY: dmabuf の取り込みはコンテキストもクライアントバッファも使わない(NULL を渡す)
        let image = unsafe {
            self.ctx.egl.create_image(
                self.ctx.display,
                k::Context::from_ptr(k::NO_CONTEXT),
                LINUX_DMA_BUF,
                k::ClientBuffer::from_ptr(std::ptr::null_mut()),
                &attrs,
            )
        }
        .map_err(|e| format!("eglCreateImage(dmabuf {:08x}): {e}", layer.fourcc))?;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            (self.target)(glow::TEXTURE_2D, image.as_ptr());
        }
        Ok(Image(image))
    }

    pub fn release(&self, image: Image) {
        let _ = self.ctx.egl.destroy_image(self.ctx.display, image.0);
    }
}

/// 検出枠 1 個(動画の画素座標)。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 種類(色とラベルを決める)
    pub class: usize,
}

/// 種類ごとの色(sRGB 0〜1)
pub const CLASS_COLORS: [[f32; 3]; 6] = [
    [0.95, 0.30, 0.25],
    [0.25, 0.75, 0.95],
    [0.40, 0.85, 0.35],
    [0.98, 0.75, 0.20],
    [0.75, 0.45, 0.95],
    [0.95, 0.55, 0.75],
];

/// ラベルのアトラス: 1 行に 1 個、上から順に並べた RGBA(乗算済み、文字だけ)。
pub struct Atlas<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    /// 1 個の高さ
    pub cell: u32,
    /// 各ラベルの文字の幅 [px]
    pub widths: &'a [f32],
}

// 動画は rect(画面の画素、左上原点、x, y, w, h)に置く。頂点 6 個で四角(三角形 2 枚)
const VERT_QUAD: &str = r"#version 300 es
uniform vec4 rect;
uniform vec2 screen;
out highp vec2 uv;
const vec2 corner[6] = vec2[6](vec2(0,0), vec2(1,0), vec2(0,1), vec2(0,1), vec2(1,0), vec2(1,1));
void main() {
    vec2 p = corner[gl_VertexID];
    uv = p;
    vec2 px = rect.xy + p * rect.zw;
    gl_Position = vec4(px.x / screen.x * 2.0 - 1.0, 1.0 - px.y / screen.y * 2.0, 0.0, 1.0);
}
";

// NV12(BT.709、限定範囲)を RGB にする
const FRAG_NV12: &str = r"#version 300 es
precision highp float;
uniform sampler2D ty;
uniform sampler2D tuv;
in highp vec2 uv;
out vec4 o;
void main() {
    float y = (texture(ty, uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    vec2 c = (texture(tuv, uv).rg - 128.0 / 255.0) * (255.0 / 224.0);
    vec3 rgb = vec3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    o = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
";

// 検出枠: 1 個につき辺 4 本 × 三角形 2 枚 = 頂点 24 個。インスタンスは (x, y, w, h)、(r, g, b, ラベルの幅)
const VERT_BOX: &str = r"#version 300 es
layout(location = 0) in vec4 box;
layout(location = 1) in vec4 col;
uniform vec4 rect;
uniform vec2 video;
uniform vec2 screen;
uniform float thick;
out vec3 c;
const vec2 corner[6] = vec2[6](vec2(0,0), vec2(1,0), vec2(0,1), vec2(0,1), vec2(1,0), vec2(1,1));
void main() {
    int edge = gl_VertexID / 6;
    vec2 p = corner[gl_VertexID % 6];
    vec2 s = rect.zw / video;
    vec2 o = rect.xy + box.xy * s;
    vec2 size = box.zw * s;
    vec2 px;
    if (edge == 0) px = o + p * vec2(size.x, thick);
    else if (edge == 1) px = o + vec2(0.0, size.y - thick) + p * vec2(size.x, thick);
    else if (edge == 2) px = o + p * vec2(thick, size.y);
    else px = o + vec2(size.x - thick, 0.0) + p * vec2(thick, size.y);
    c = col.rgb;
    gl_Position = vec4(px.x / screen.x * 2.0 - 1.0, 1.0 - px.y / screen.y * 2.0, 0.0, 1.0);
}
";

const FRAG_BOX: &str = r"#version 300 es
precision mediump float;
in vec3 c;
out vec4 o;
void main() { o = vec4(c, 1.0); }
";

// ラベル: 枠の左上の上に、種類の色の帯と白い文字。アトラスの gl_InstanceID 行目を使う
const VERT_LABEL: &str = r"#version 300 es
layout(location = 0) in vec4 box;
layout(location = 1) in vec4 col;
uniform vec4 rect;
uniform vec2 video;
uniform vec2 screen;
uniform vec2 atlas;
uniform float cell;
out vec3 c;
out highp vec2 uv;
const vec2 corner[6] = vec2[6](vec2(0,0), vec2(1,0), vec2(0,1), vec2(0,1), vec2(1,0), vec2(1,1));
void main() {
    vec2 p = corner[gl_VertexID];
    vec2 s = rect.zw / video;
    vec2 o = rect.xy + box.xy * s - vec2(0.0, cell);
    vec2 size = vec2(col.a + 12.0, cell);
    vec2 px = o + p * size;
    c = col.rgb;
    uv = vec2((p.x * size.x - 6.0) / atlas.x, (float(gl_InstanceID) + p.y) * cell / atlas.y);
    gl_Position = vec4(px.x / screen.x * 2.0 - 1.0, 1.0 - px.y / screen.y * 2.0, 0.0, 1.0);
}
";

const FRAG_LABEL: &str = r"#version 300 es
precision mediump float;
uniform sampler2D tex;
in vec3 c;
in highp vec2 uv;
out vec4 o;
void main() {
    float a = (uv.x < 0.0 || uv.x > 1.0) ? 0.0 : texture(tex, uv).a;
    o = vec4(mix(c * 0.55, vec3(1.0), a), 0.9);
}
";

/// 動画と検出枠を描く。
pub struct VideoScene {
    nv12: glow::Program,
    boxes: glow::Program,
    labels: glow::Program,
    tex_y: glow::Texture,
    tex_uv: glow::Texture,
    atlas: glow::Texture,
    atlas_size: (u32, u32),
    cell: u32,
    widths: Vec<f32>,
    vao: glow::VertexArray,
    inst: glow::Buffer,
    /// 枠の太さ [画面の px]
    pub thick: f32,
}

/// 一度に描ける検出枠の数の上限
pub const MAX_DETECTIONS: usize = 256;

impl VideoScene {
    pub fn new(gl: &glow::Context, atlas: &Atlas) -> Result<Self, String> {
        let nv12 = link(gl, VERT_QUAD, FRAG_NV12)?;
        let boxes = link(gl, VERT_BOX, FRAG_BOX)?;
        let labels = link(gl, VERT_LABEL, FRAG_LABEL)?;
        unsafe {
            let tex = || -> Result<glow::Texture, String> {
                let t = gl.create_texture()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (k, v) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
                }
                Ok(t)
            };
            let (tex_y, tex_uv, atlas_tex) = (tex()?, tex()?, tex()?);
            let rows = (atlas.rgba.len() / 4) as u32 / atlas.width;
            gl.bind_texture(glow::TEXTURE_2D, Some(atlas_tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                atlas.width as i32,
                rows as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(atlas.rgba)),
            );
            let vao = gl.create_vertex_array()?;
            gl.bind_vertex_array(Some(vao));
            let inst = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(inst));
            gl.buffer_data_size(
                glow::ARRAY_BUFFER,
                (MAX_DETECTIONS * 32) as i32,
                glow::STREAM_DRAW,
            );
            for loc in 0..2 {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(loc, 4, glow::FLOAT, false, 32, loc as i32 * 16);
                gl.vertex_attrib_divisor(loc, 1);
            }
            gl.bind_vertex_array(None);
            for (p, names) in [(nv12, &["ty", "tuv"][..]), (labels, &["tex"][..])] {
                gl.use_program(Some(p));
                for (i, n) in names.iter().enumerate() {
                    gl.uniform_1_i32(gl.get_uniform_location(p, n).as_ref(), i as i32);
                }
            }
            Ok(Self {
                nv12,
                boxes,
                labels,
                tex_y,
                tex_uv,
                atlas: atlas_tex,
                atlas_size: (atlas.width, rows),
                cell: atlas.cell,
                widths: atlas.widths.to_vec(),
                vao,
                inst,
                thick: 3.0,
            })
        }
    }

    /// 動画の 1 枚(Y と UV)を取り込む。戻り値の Image は、この枚を描き終えたら捨てる。
    pub fn import(
        &self,
        gl: &glow::Context,
        dmabuf: &Dmabuf,
        y: &Layer,
        uv: &Layer,
    ) -> Result<[Image; 2], String> {
        let a = dmabuf.bind(gl, self.tex_y, y)?;
        match dmabuf.bind(gl, self.tex_uv, uv) {
            Ok(b) => Ok([a, b]),
            Err(e) => {
                dmabuf.release(a);
                Err(e)
            }
        }
    }

    /// 画面を消し、いま取り込んである動画を `rect`(画面の画素、x, y, w, h)に描く。
    pub fn draw_video(&self, gl: &glow::Context, screen: (u32, u32), rect: [f32; 4]) {
        unsafe {
            gl.viewport(0, 0, screen.0 as i32, screen.1 as i32);
            gl.clear_color(0.03, 0.035, 0.045, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(self.nv12));
            let u = |n| gl.get_uniform_location(self.nv12, n);
            gl.uniform_4_f32_slice(u("rect").as_ref(), &rect);
            gl.uniform_2_f32_slice(u("screen").as_ref(), &[screen.0 as f32, screen.1 as f32]);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex_y));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex_uv));
            gl.active_texture(glow::TEXTURE0);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }

    /// 動画の上に検出枠とラベルを重ねる。`video` は動画の大きさ(検出枠の座標の基準)。
    pub fn draw_overlay(
        &self,
        gl: &glow::Context,
        screen: (u32, u32),
        rect: [f32; 4],
        video: (u32, u32),
        dets: &[Detection],
    ) {
        let dets = &dets[..dets.len().min(MAX_DETECTIONS).min(self.widths.len())];
        if dets.is_empty() {
            return;
        }
        let mut inst = Vec::with_capacity(dets.len() * 8);
        for (i, d) in dets.iter().enumerate() {
            let c = CLASS_COLORS[d.class % CLASS_COLORS.len()];
            inst.extend([d.x, d.y, d.w, d.h, c[0], c[1], c[2], self.widths[i]]);
        }
        let scr = [screen.0 as f32, screen.1 as f32];
        let vid = [video.0 as f32, video.1 as f32];
        unsafe {
            let set = |p: glow::Program| {
                gl.use_program(Some(p));
                let u = move |n| gl.get_uniform_location(p, n);
                gl.uniform_4_f32_slice(u("rect").as_ref(), &rect);
                gl.uniform_2_f32_slice(u("screen").as_ref(), &scr);
                gl.uniform_2_f32_slice(u("video").as_ref(), &vid);
                u
            };
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.inst));
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes(&inst));
            let u = set(self.boxes);
            gl.uniform_1_f32(u("thick").as_ref(), self.thick);
            gl.draw_arrays_instanced(glow::TRIANGLES, 0, 24, dets.len() as i32);

            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            let u = set(self.labels);
            gl.uniform_2_f32_slice(
                u("atlas").as_ref(),
                &[self.atlas_size.0 as f32, self.atlas_size.1 as f32],
            );
            gl.uniform_1_f32(u("cell").as_ref(), self.cell as f32);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas));
            gl.draw_arrays_instanced(glow::TRIANGLES, 0, 6, dets.len() as i32);
            gl.disable(glow::BLEND);
            gl.bind_vertex_array(None);
        }
    }

    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            for p in [self.nv12, self.boxes, self.labels] {
                gl.delete_program(p);
            }
            for t in [self.tex_y, self.tex_uv, self.atlas] {
                gl.delete_texture(t);
            }
            gl.delete_buffer(self.inst);
            gl.delete_vertex_array(self.vao);
        }
    }
}

fn bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 の配列(詰め物なし)
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// 認識結果の代わりに、画面の中を動き回る検出枠を作る(再現できる乱数)。
pub struct Tracks {
    rng: crate::connectome::Rng,
    video: (f32, f32),
    objs: Vec<(Detection, [f32; 2])>,
}

impl Tracks {
    pub fn new(n: usize, video: (u32, u32), seed: u64) -> Self {
        let mut rng = crate::connectome::Rng::new(seed);
        let (vw, vh) = (video.0 as f32, video.1 as f32);
        let objs = (0..n)
            .map(|i| {
                let w = 60.0 + rng.f32() * 240.0;
                let h = w * (0.6 + rng.f32() * 1.2);
                let d = Detection {
                    x: rng.f32() * (vw - w),
                    y: rng.f32() * (vh - h),
                    w,
                    h,
                    class: i % CLASS_COLORS.len(),
                };
                let v = [(rng.f32() - 0.5) * 240.0, (rng.f32() - 0.5) * 160.0];
                (d, v)
            })
            .collect();
        Self {
            rng,
            video: (vw, vh),
            objs,
        }
    }

    /// `dt` 秒進める。端に当たったら跳ね返り、大きさは少し揺らす。
    pub fn step(&mut self, dt: f32) -> Vec<Detection> {
        let (vw, vh) = self.video;
        for (d, v) in &mut self.objs {
            d.x += v[0] * dt;
            d.y += v[1] * dt;
            if d.x < 0.0 || d.x + d.w > vw {
                v[0] = -v[0];
                d.x = d.x.clamp(0.0, vw - d.w);
            }
            if d.y < 0.0 || d.y + d.h > vh {
                v[1] = -v[1];
                d.y = d.y.clamp(0.0, vh - d.h);
            }
            let jitter = (self.rng.f32() - 0.5) * 2.0;
            d.w = (d.w + jitter).clamp(40.0, vw / 3.0);
            d.x = d.x.min(vw - d.w);
        }
        self.objs.iter().map(|(d, _)| *d).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_stay_inside_video() {
        let mut t = Tracks::new(50, (1920, 1080), 9);
        for _ in 0..300 {
            for d in t.step(1.0 / 30.0) {
                assert!(d.x >= 0.0 && d.y >= 0.0, "{d:?}");
                assert!(d.x + d.w <= 1920.5 && d.y + d.h <= 1080.5, "{d:?}");
            }
        }
        assert_eq!(t.step(0.0).len(), 50);
    }
}
