//! CPU で描いた RGBA 画像をテクスチャに転送し、全画面に 1 枚描く。PR2 の GLES 経路は見た目を
//! 変えないので、描画はこれだけ。
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

// 2560 px 幅でも画素がずれないよう highp、拡大縮小しないので NEAREST
const FRAG: &str = r"#version 300 es
precision highp float;
uniform sampler2D tex;
in highp vec2 uv;
out vec4 color;
void main() {
    color = vec4(texture(tex, uv).rgb, 1.0);
}
";

pub struct Blitter {
    program: glow::Program,
    vao: glow::VertexArray,
    tex: glow::Texture,
    size: (u32, u32),
}

impl Blitter {
    pub fn new(gl: &glow::Context, (w, h): (u32, u32)) -> Result<Self, String> {
        unsafe {
            let program = gl.create_program()?;
            for (kind, src) in [(glow::VERTEX_SHADER, VERT), (glow::FRAGMENT_SHADER, FRAG)] {
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
            let vao = gl.create_vertex_array()?;
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
            Ok(Self {
                program,
                vao,
                tex,
                size: (w, h),
            })
        }
    }

    /// `rgba` は上から下へ並んだ RGBA8(stride = 幅 × 4)。今バインドされている framebuffer に描く。
    pub fn draw(&self, gl: &glow::Context, rgba: &[u8]) {
        let (w, h) = self.size;
        assert_eq!(
            rgba.len(),
            w as usize * h as usize * 4,
            "画像の大きさが違う"
        );
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
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
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_texture(self.tex);
            gl.delete_vertex_array(self.vao);
            gl.delete_program(self.program);
        }
    }
}
