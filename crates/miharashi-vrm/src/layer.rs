//! キャラクターを透明な背景で描いて、画面の一部(領域)に重ねる層。
//! 深度のない既定の framebuffer や、写真が描かれた画面の上に VRM を載せるために、領域と同じ大きさの
//! FBO(色はテクスチャ + 深度 renderbuffer)へ描き、あとでアルファ付きで画面の領域へ合成する。
//!
//! アルファの扱い: FBO は `(0, 0, 0, 0)` で塗ってから描く。不透明な面は `(rgb, 1)`、半透明の面(BLEND)は
//! 色を `src.rgb * a + dst.rgb * (1 - a)`、アルファを `a + dst.a * (1 - a)` として貯める(`render.rs` の
//! `blend_func_separate`)ので、FBO の中身は **premultiplied alpha** になる。合成は `ONE, ONE_MINUS_SRC_ALPHA`。
use crate::render::attach_depth;
use glow::HasContext;

const VS: &str = "#version 300 es
void main() {
    // 頂点 3 つで覆う三角形。ビューポートを領域にして描くので、領域の全面だけが塗られる
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

const FS: &str = "#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform ivec2 u_origin;
out vec4 o_color;
void main() {
    o_color = texelFetch(u_tex, ivec2(gl_FragCoord.xy) - u_origin, 0);
}
";

pub struct VrmLayer {
    fbo: glow::Framebuffer,
    color: glow::Texture,
    depth: glow::Renderbuffer,
    size: (u32, u32),
    program: glow::Program,
    vao: glow::VertexArray,
    u_tex: Option<glow::UniformLocation>,
    u_origin: Option<glow::UniformLocation>,
}

impl VrmLayer {
    /// `size` は領域の大きさ(画素)。呼び出し前の framebuffer の bind は変わる(終わりは既定に戻す)。
    pub fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self, String> {
        unsafe {
            let color = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(color));
            gl.tex_storage_2d(
                glow::TEXTURE_2D,
                1,
                glow::RGBA8,
                size.0 as i32,
                size.1 as i32,
            );
            for (k, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            let fbo = match gl.create_framebuffer() {
                Ok(f) => f,
                Err(e) => {
                    gl.delete_texture(color);
                    return Err(e);
                }
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(color),
                0,
            );
            let depth = match attach_depth(gl, size) {
                Ok(d) => d,
                Err(e) => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.delete_framebuffer(fbo);
                    gl.delete_texture(color);
                    return Err(e);
                }
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            let built = (|| -> Result<_, String> {
                let program = crate::render::compile_program(gl, VS, FS)?;
                let vao = match gl.create_vertex_array() {
                    Ok(v) => v,
                    Err(e) => {
                        gl.delete_program(program);
                        return Err(e);
                    }
                };
                Ok((program, vao))
            })();
            let (program, vao) = match built {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_renderbuffer(depth);
                    gl.delete_framebuffer(fbo);
                    gl.delete_texture(color);
                    return Err(e);
                }
            };
            Ok(Self {
                fbo,
                color,
                depth,
                size,
                u_tex: gl.get_uniform_location(program, "u_tex"),
                u_origin: gl.get_uniform_location(program, "u_origin"),
                program,
                vao,
            })
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// この層の FBO を描画先にする(`Renderer::render_camera` の前)。
    pub fn bind(&self, gl: &glow::Context) {
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo)) };
    }

    /// 描いた内容を、`target` の framebuffer(画面なら None)の `origin`(左下が原点)から領域の大きさだけ、
    /// 下の絵にアルファで重ねる。`ONE, ONE_MINUS_SRC_ALPHA`(中身は premultiplied)。
    pub fn composite(
        &self,
        gl: &glow::Context,
        target: Option<glow::Framebuffer>,
        origin: (i32, i32),
    ) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            gl.viewport(origin.0, origin.1, self.size.0 as i32, self.size.1 as i32);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.color));
            gl.uniform_1_i32(self.u_tex.as_ref(), 0);
            gl.uniform_2_i32(self.u_origin.as_ref(), origin.0, origin.1);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
            gl.disable(glow::BLEND);
        }
    }

    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_vertex_array(self.vao);
            gl.delete_program(self.program);
            gl.delete_renderbuffer(self.depth);
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.color);
        }
    }
}
