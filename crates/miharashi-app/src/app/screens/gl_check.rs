//! GL の状態をわざと汚したまま返す試験画面(Issue #74、docs/DESIGN-screen-plugin-v2.md §5.5)。実機で、
//! 3D の画面のあとに続く画面(写真・Radar・Agent・キューブ)が前の状態に影響されないことを確かめるためのもの。
//! 汚し方を変えた 2 種類(`dirty-a`・`dirty-b`)があり、`[gl_check] enabled = true` のときだけ登録する(既定は無効)。
//! どちらも `draw` の中で背景を塗ったあと、`Compositor` が設定し直す状態(§5.1)を片っ端から変えて、元に戻さずに返す。
use super::cube::build_program;
use crate::screen::{
    Composite, GlComposite, GlScene, Region, RenderContext, Screen, ScreenId, Top,
};
use glow::HasContext;
use miharashi_glscene::{GlTarget, ObjectCount};
use miharashi_render::canvas::rgba;
use miharashi_render::notice;
use std::rc::Rc;

/// 画面の下地の色(`Region` が見える単色)。
pub const BASE_COLOR: [u8; 4] = [48, 24, 64, 255];

/// 汚し方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dirt {
    /// 描画の状態(FBO・scissor・color_mask・blend・テスト・pixel store・sampler など)を幅広く汚す
    A,
    /// 別の値で汚す(深度・面の向き・stencil・別のユニットの bind・別の pixel store など)
    B,
}

const VS: &str = "#version 300 es
void main() { gl_Position = vec4(0.0); }
";
const FS: &str = "#version 300 es
precision mediump float;
out vec4 o;
void main() { o = vec4(1.0); }
";

/// 汚れを作る場面。使うオブジェクトは最初の `draw` で作り、`release` で消す(`ObjectCount` で数える)。
pub struct DirtyScene {
    dirt: Dirt,
    count: Rc<ObjectCount>,
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    buffers: Vec<glow::Buffer>,
    textures: Vec<glow::Texture>,
    samplers: Vec<glow::Sampler>,
    fbos: Vec<glow::Framebuffer>,
    built: bool,
}

impl DirtyScene {
    pub fn new(dirt: Dirt, count: Rc<ObjectCount>) -> Self {
        Self {
            dirt,
            count,
            program: None,
            vao: None,
            buffers: Vec::new(),
            textures: Vec::new(),
            samplers: Vec::new(),
            fbos: Vec::new(),
            built: false,
        }
    }

    /// 汚れに使うオブジェクトを作る(作りかけで失敗しても、作ったものは持ったまま)。
    fn build(&mut self, gl: &glow::Context) -> Result<(), String> {
        if self.built {
            return Ok(());
        }
        self.program = Some(build_program(gl, &self.count, VS, FS)?);
        self.vao = Some(self.count.create_vertex_array(gl)?);
        for _ in 0..2 {
            self.buffers.push(self.count.create_buffer(gl)?);
        }
        for _ in 0..3 {
            self.textures.push(self.count.create_texture(gl)?);
        }
        self.samplers.push(self.count.create_sampler(gl)?);
        self.fbos.push(self.count.create_framebuffer(gl)?);
        self.built = true;
        Ok(())
    }

    fn dirty_a(&self, gl: &glow::Context) {
        let (pbo, tex, cube, sampler, fbo) = (
            self.buffers[1],
            self.textures[0],
            self.textures[1],
            self.samplers[0],
            self.fbos[0],
        );
        unsafe {
            // 層の FBO 自身の draw buffer(FBO ごとの状態)。bind を変える前にやる
            gl.draw_buffers(&[glow::NONE]);
            gl.read_buffer(glow::NONE);
            gl.clear_color(1.0, 0.0, 1.0, 1.0);
            gl.disable(glow::DITHER);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ZERO);
            gl.blend_equation(glow::FUNC_REVERSE_SUBTRACT);
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE,
                glow::ZERO,
                glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.blend_equation_separate(glow::FUNC_SUBTRACT, glow::MAX);
            gl.blend_color(1.0, 1.0, 1.0, 1.0);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::ALWAYS);
            gl.depth_range_f32(1.0, 0.0);
            gl.depth_mask(false);
            gl.clear_depth_f32(0.0);
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT);
            gl.front_face(glow::CW);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, 1, 1);
            gl.color_mask(false, true, false, true);
            gl.enable(glow::STENCIL_TEST);
            gl.stencil_func(glow::NEVER, 1, 0);
            gl.stencil_op(glow::ZERO, glow::ZERO, glow::ZERO);
            gl.stencil_mask(0);
            gl.stencil_func_separate(glow::BACK, glow::NEVER, 3, 5);
            gl.stencil_op_separate(glow::BACK, glow::ZERO, glow::ZERO, glow::ZERO);
            gl.stencil_mask_separate(glow::BACK, 0);
            gl.enable(glow::RASTERIZER_DISCARD);
            gl.enable(glow::SAMPLE_COVERAGE);
            gl.enable(glow::SAMPLE_MASK);
            gl.sample_coverage(0.0, true);
            gl.line_width(3.0);
            gl.hint(glow::GENERATE_MIPMAP_HINT, glow::NICEST);
            gl.hint(glow::FRAGMENT_SHADER_DERIVATIVE_HINT, glow::NICEST);
            gl.enable(glow::SAMPLE_ALPHA_TO_COVERAGE);
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(5.0, 5.0);
            // 別の FBO・viewport
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, 3, 3);
            // プログラム・VAO・バッファ・テクスチャ・pixel store
            gl.use_program(self.program);
            gl.bind_vertex_array(self.vao);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.buffers[0]));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.buffers[0]));
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pbo));
            gl.bind_buffer(glow::DRAW_INDIRECT_BUFFER, Some(pbo));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(cube));
            // sampler は bind_texture(None) では外れない。Compositor が使わないユニット(9)にも残す
            gl.sampler_parameter_i32(sampler, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.sampler_parameter_i32(sampler, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            for unit in [0, 1, 2, 3, 9] {
                gl.bind_sampler(unit, Some(sampler));
            }
            gl.active_texture(glow::TEXTURE3);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 7);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 2);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 1);
            gl.pixel_store_i32(glow::UNPACK_IMAGE_HEIGHT, 5);
            gl.pixel_store_i32(glow::UNPACK_SKIP_IMAGES, 1);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 9);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 2);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 1);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        }
    }

    fn dirty_b(&self, gl: &glow::Context) {
        let (vbo, ebo) = (self.buffers[0], self.buffers[1]);
        let (tex2d, tex_cube, tex_array) = (self.textures[0], self.textures[1], self.textures[2]);
        let (sampler, fbo) = (self.samplers[0], self.fbos[0]);
        unsafe {
            // draw buffer は位置と同じ番号の attachment か NONE しか選べない
            gl.draw_buffers(&[glow::COLOR_ATTACHMENT0, glow::NONE]);
            gl.read_buffer(glow::COLOR_ATTACHMENT1);
            gl.clear_color(0.0, 1.0, 1.0, 0.5);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::DST_COLOR, glow::ONE_MINUS_DST_ALPHA);
            gl.blend_equation(glow::MIN);
            gl.blend_func_separate(
                glow::CONSTANT_COLOR,
                glow::ONE_MINUS_CONSTANT_COLOR,
                glow::SRC_ALPHA_SATURATE,
                glow::DST_ALPHA,
            );
            gl.blend_equation_separate(glow::MAX, glow::FUNC_REVERSE_SUBTRACT);
            gl.blend_color(0.2, 0.4, 0.6, 0.8);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::NOTEQUAL);
            gl.depth_range_f32(0.5, 0.25);
            gl.clear_depth_f32(0.25);
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::FRONT_AND_BACK);
            gl.front_face(glow::CW);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(5, 5, 10, 10);
            gl.color_mask(true, true, false, false);
            gl.enable(glow::STENCIL_TEST);
            gl.stencil_func(glow::GREATER, 7, 0x0F);
            gl.stencil_op(glow::INCR, glow::DECR, glow::INVERT);
            gl.stencil_mask(0x0F);
            gl.stencil_func_separate(glow::FRONT, glow::LEQUAL, 2, 0x33);
            gl.stencil_op_separate(glow::FRONT, glow::REPLACE, glow::INCR_WRAP, glow::DECR_WRAP);
            gl.stencil_mask_separate(glow::FRONT, 0x0F);
            gl.clear_stencil(9);
            gl.enable(glow::RASTERIZER_DISCARD);
            gl.enable(glow::PRIMITIVE_RESTART_FIXED_INDEX);
            gl.enable(glow::SAMPLE_ALPHA_TO_COVERAGE);
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(-2.0, 3.0);
            gl.line_width(2.0);
            gl.hint(glow::GENERATE_MIPMAP_HINT, glow::FASTEST);
            gl.hint(glow::FRAGMENT_SHADER_DERIVATIVE_HINT, glow::FASTEST);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.viewport(7, 7, 5, 5);
            gl.use_program(self.program);
            gl.bind_vertex_array(self.vao);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(ebo));
            // Compositor が使わないユニット(5・15)と、2D 以外の種類のテクスチャ
            gl.active_texture(glow::TEXTURE5);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(tex_array));
            gl.bind_texture(glow::TEXTURE_2D, Some(tex2d));
            gl.active_texture(glow::TEXTURE15);
            gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(tex_cube));
            gl.sampler_parameter_i32(sampler, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.sampler_parameter_i32(sampler, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
            for unit in [1, 5, 15] {
                gl.bind_sampler(unit, Some(sampler));
            }
            gl.active_texture(glow::TEXTURE15);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 33);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 4);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 3);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 8);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 21);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 2);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 2);
        }
    }
}

impl GlScene for DirtyScene {
    fn draw(&mut self, gl: &glow::Context, _target: &GlTarget) -> Result<(), String> {
        self.build(gl)?;
        unsafe {
            // 層を単色で塗る(画面で見えるように。Compositor が渡した状態のうちは、clear が効く)
            let (r, g, b) = match self.dirt {
                Dirt::A => (0.5, 0.1, 0.2),
                Dirt::B => (0.1, 0.4, 0.2),
            };
            gl.clear_color(r, g, b, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        }
        match self.dirt {
            Dirt::A => self.dirty_a(gl),
            Dirt::B => self.dirty_b(gl),
        }
        Ok(())
    }

    fn release(&mut self, gl: &glow::Context) {
        if let Some(p) = self.program.take() {
            self.count.delete_program(gl, p);
        }
        if let Some(v) = self.vao.take() {
            self.count.delete_vertex_array(gl, v);
        }
        for b in self.buffers.drain(..) {
            self.count.delete_buffer(gl, b);
        }
        for t in self.textures.drain(..) {
            self.count.delete_texture(gl, t);
        }
        for s in self.samplers.drain(..) {
            self.count.delete_sampler(gl, s);
        }
        for f in self.fbos.drain(..) {
            self.count.delete_framebuffer(gl, f);
        }
        self.built = false;
    }
}

/// 状態を汚す試験画面。中央の縦横半分の領域を塗り、単色の下地に重ねる。3D だけの画面(GL が使えなければ案内)。
pub struct DirtyScreen {
    dirt: Dirt,
    scene: DirtyScene,
    count: Rc<ObjectCount>,
}

impl DirtyScreen {
    pub fn new(dirt: Dirt) -> Self {
        let count = Rc::new(ObjectCount::default());
        Self {
            dirt,
            scene: DirtyScene::new(dirt, count.clone()),
            count,
        }
    }

    pub fn region((w, h): (u32, u32)) -> Region {
        Region {
            x: w / 4,
            y: h / 4,
            w: w / 2,
            h: h / 2,
        }
    }
}

impl Screen for DirtyScreen {
    fn id(&self) -> ScreenId {
        match self.dirt {
            Dirt::A => "dirty-a",
            Dirt::B => "dirty-b",
        }
    }

    fn title(&self) -> &'static str {
        match self.dirt {
            Dirt::A => "DIRTY A",
            Dirt::B => "DIRTY B",
        }
    }

    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        notice::gpu_required(ctx.canvas, ctx.text, self.title());
    }

    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        ctx.canvas.clear(rgba(
            BASE_COLOR[0],
            BASE_COLOR[1],
            BASE_COLOR[2],
            BASE_COLOR[3],
        ));
        let size = (ctx.canvas.width(), ctx.canvas.height());
        Composite::Gl(GlComposite {
            base: Some(Box::new(Composite::Canvas)),
            scene: &mut self.scene,
            region: Self::region(size),
            top: Top::Off,
        })
    }

    fn needs_gl(&self) -> bool {
        true
    }

    fn gl_scene(&mut self) -> Option<&mut dyn GlScene> {
        Some(&mut self.scene)
    }

    fn gl_live_objects(&self) -> Option<i64> {
        Some(self.count.live())
    }
}
