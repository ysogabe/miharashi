//! GPU での画面の合成(PoC-005 PR3)。
//! - `Frame::Image`: CPU で描いた完成画面を 1 枚転送して描く(状況画面など。PR2 と同じ)。
//! - `Frame::Photo`: 写真のフレームは 1 枚ごとに 1 回だけテクスチャにし、クロスフェードは
//!   シェーダーで混ぜる。文字などの重ね絵(透明背景の RGBA)は変わったときだけ転送して上に重ねる。
use glow::HasContext;

const VERT: &str = r"#version 300 es
out highp vec2 uv;
void main() {
    // 頂点 3 つで画面全体を覆う三角形。画像の 1 行目(uv.y = 0)が画面の上に来る
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = vec2(p.x, 1.0 - p.y);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

// 下地: 旧フレーム a と新フレーム b を t で混ぜる(フェードなしは a = b、t = 1)。
// CPU 経路の「旧を描いてから新を不透明度 t で重ねる」と同じ式。
// 2560 px 幅でも画素がずれないよう highp、拡大縮小しないので NEAREST
const FRAG_BASE: &str = r"#version 300 es
precision highp float;
uniform sampler2D a;
uniform sampler2D b;
uniform float t;
in highp vec2 uv;
out vec4 color;
void main() {
    color = vec4(mix(texture(a, uv).rgb, texture(b, uv).rgb, t), 1.0);
}
";

// 重ね絵: premultiplied の RGBA をそのまま出し、ブレンド(ONE, ONE_MINUS_SRC_ALPHA)で重ねる
const FRAG_OVERLAY: &str = r"#version 300 es
precision highp float;
uniform sampler2D tex;
in highp vec2 uv;
out vec4 color;
void main() {
    color = texture(tex, uv);
}
";

/// 写真のテクスチャを残しておく枚数(表示中とフェード元、と先読みの余裕)。
const PHOTO_CACHE: usize = 3;

/// 写真のフレーム。`id` が同じなら同じ画像とみなし、転送し直さない。
#[derive(Clone, Copy)]
pub struct PhotoLayer<'a> {
    pub id: u64,
    /// premultiplied RGBA8、上から下、stride = 幅 × 4(画面と同じ大きさ、不透明)
    pub rgba: &'a [u8],
}

/// 1 回の表示の内容。
pub enum Frame<'a> {
    /// CPU で描いた完成画面(不透明)
    Image(&'a [u8]),
    Photo {
        photo: PhotoLayer<'a>,
        /// フェード中の (旧フレーム, 新フレームの不透明度 0〜1)
        old: Option<(PhotoLayer<'a>, f32)>,
        /// 重ね絵が変わったときだけ Some(premultiplied RGBA8、透明背景)。None なら前回のものを使う
        overlay: Option<&'a [u8]>,
    },
}

pub struct Compositor {
    base: glow::Program,
    overlay: glow::Program,
    vao: glow::VertexArray,
    /// `Frame::Image` 用
    image_tex: glow::Texture,
    overlay_tex: glow::Texture,
    /// (写真の id, テクスチャ)。古いものから捨てる
    photos: Vec<(u64, glow::Texture)>,
    /// 重ね絵を 1 度でも転送したか(まだなら重ねない)
    overlay_ready: bool,
    size: (u32, u32),
    /// 今の時点までの転送量 [バイト](ログ用)
    pub uploaded: u64,
}

fn program(gl: &glow::Context, frag: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        for (kind, src) in [(glow::VERTEX_SHADER, VERT), (glow::FRAGMENT_SHADER, frag)] {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(format!("shader: {}", gl.get_shader_info_log(s)));
            }
            gl.attach_shader(program, s);
            gl.delete_shader(s);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("link: {}", gl.get_program_info_log(program)));
        }
        Ok(program)
    }
}

fn texture(gl: &glow::Context, (w, h): (u32, u32)) -> Result<glow::Texture, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32);
        for (k, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
            (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
        }
        Ok(tex)
    }
}

impl Compositor {
    pub fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self, String> {
        let base = program(gl, FRAG_BASE)?;
        let overlay = program(gl, FRAG_OVERLAY)?;
        unsafe {
            gl.use_program(Some(base));
            gl.uniform_1_i32(gl.get_uniform_location(base, "a").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(base, "b").as_ref(), 1);
            gl.use_program(Some(overlay));
            gl.uniform_1_i32(gl.get_uniform_location(overlay, "tex").as_ref(), 0);
            Ok(Self {
                base,
                overlay,
                vao: gl.create_vertex_array()?,
                image_tex: texture(gl, size)?,
                overlay_tex: texture(gl, size)?,
                photos: Vec::new(),
                overlay_ready: false,
                size,
                uploaded: 0,
            })
        }
    }

    fn upload(&mut self, gl: &glow::Context, tex: glow::Texture, rgba: &[u8]) {
        let (w, h) = self.size;
        assert_eq!(
            rgba.len(),
            w as usize * h as usize * 4,
            "画像の大きさが違う"
        );
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
        }
        self.uploaded += rgba.len() as u64;
    }

    /// 写真のテクスチャを返す(初めての id なら転送する)。
    fn photo_tex(
        &mut self,
        gl: &glow::Context,
        layer: PhotoLayer<'_>,
    ) -> Result<glow::Texture, String> {
        if let Some(i) = self.photos.iter().position(|(id, _)| *id == layer.id) {
            // 最近使ったものを後ろへ
            let e = self.photos.remove(i);
            self.photos.push(e);
            return Ok(e.1);
        }
        let tex = if self.photos.len() >= PHOTO_CACHE {
            self.photos.remove(0).1
        } else {
            texture(gl, self.size)?
        };
        self.upload(gl, tex, layer.rgba);
        self.photos.push((layer.id, tex));
        Ok(tex)
    }

    fn draw_base(&self, gl: &glow::Context, a: glow::Texture, b: glow::Texture, t: f32) {
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.base));
            gl.uniform_1_f32(gl.get_uniform_location(self.base, "t").as_ref(), t);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(a));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(b));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    /// 今バインドされている framebuffer に `frame` を描く。
    pub fn draw(&mut self, gl: &glow::Context, frame: &Frame<'_>) -> Result<(), String> {
        let (w, h) = self.size;
        unsafe { gl.viewport(0, 0, w as i32, h as i32) };
        match frame {
            Frame::Image(rgba) => {
                self.upload(gl, self.image_tex, rgba);
                self.draw_base(gl, self.image_tex, self.image_tex, 1.0);
            }
            Frame::Photo {
                photo,
                old,
                overlay,
            } => {
                let new = self.photo_tex(gl, *photo)?;
                match old {
                    Some((o, t)) => {
                        let old = self.photo_tex(gl, *o)?;
                        self.draw_base(gl, old, new, t.clamp(0.0, 1.0));
                    }
                    None => self.draw_base(gl, new, new, 1.0),
                }
                if let Some(rgba) = overlay {
                    self.upload(gl, self.overlay_tex, rgba);
                    self.overlay_ready = true;
                }
                if !self.overlay_ready {
                    return Ok(());
                }
                unsafe {
                    gl.enable(glow::BLEND);
                    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                    gl.use_program(Some(self.overlay));
                    gl.bind_texture(glow::TEXTURE_2D, Some(self.overlay_tex));
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                    gl.disable(glow::BLEND);
                }
            }
        }
        Ok(())
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            for (_, t) in &self.photos {
                gl.delete_texture(*t);
            }
            gl.delete_texture(self.image_tex);
            gl.delete_texture(self.overlay_tex);
            gl.delete_vertex_array(self.vao);
            gl.delete_program(self.base);
            gl.delete_program(self.overlay);
        }
    }
}
