//! GLES 3.x(glow)での描画。Lambert の平行光源 + baseColor テクスチャ + 深度テスト。
//! 表情は morph target を CPU で合成して頂点バッファを更新する(`set_expressions` → `update`)。
use crate::math;
use crate::model::{AlphaMode, FLOATS_PER_VERTEX, Model};
use glow::HasContext;
use std::collections::BTreeMap;

const VS: &str = "#version 300 es
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_nrm;
layout(location = 2) in vec2 a_uv;
uniform mat4 u_vp;
out vec3 v_nrm;
out vec2 v_uv;
void main() {
    v_nrm = a_nrm;
    v_uv = a_uv;
    gl_Position = u_vp * vec4(a_pos, 1.0);
}
";

// テクスチャは SRGB8_ALPHA8 で読むので、ここで線形になっている。最後にガンマをかけて出力する
const FS: &str = "#version 300 es
precision highp float;
in vec3 v_nrm;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
uniform float u_has_tex;
uniform float u_cutoff;
uniform float u_opaque;
uniform vec3 u_light;
out vec4 o_color;
void main() {
    vec4 c = u_color;
    if (u_has_tex > 0.5) c *= texture(u_tex, v_uv);
    if (u_cutoff >= 0.0 && c.a < u_cutoff) discard;
    vec3 n = normalize(gl_FrontFacing ? v_nrm : -v_nrm);
    float d = max(dot(n, normalize(u_light)), 0.0);
    vec3 lit = c.rgb * (0.35 + 0.75 * d);
    o_color = vec4(pow(lit, vec3(1.0 / 2.2)), u_opaque > 0.5 ? 1.0 : c.a);
}
";

struct GpuPrimitive {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    index_count: i32,
    /// morph で書き換えたあとなら true(ゼロに戻るときにベース形状へ戻す)
    morphed: bool,
}

/// シェーダの uniform の場所(一度だけ引いて持つ)。
struct Uniforms {
    vp: Option<glow::UniformLocation>,
    light: Option<glow::UniformLocation>,
    tex: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
    has_tex: Option<glow::UniformLocation>,
    cutoff: Option<glow::UniformLocation>,
    opaque: Option<glow::UniformLocation>,
}

/// `Renderer::new` の途中で作った GL オブジェクトの控え。失敗したときにまとめて消す。
#[derive(Default)]
struct Created {
    program: Option<glow::Program>,
    vaos: Vec<glow::VertexArray>,
    buffers: Vec<glow::Buffer>,
    textures: Vec<glow::Texture>,
}

impl Created {
    unsafe fn delete(self, gl: &glow::Context) {
        unsafe {
            for v in self.vaos {
                gl.delete_vertex_array(v);
            }
            for b in self.buffers {
                gl.delete_buffer(b);
            }
            for t in self.textures {
                gl.delete_texture(t);
            }
            if let Some(p) = self.program {
                gl.delete_program(p);
            }
        }
    }
}

pub struct Renderer {
    model: Model,
    program: glow::Program,
    uniforms: Uniforms,
    prims: Vec<GpuPrimitive>,
    textures: Vec<glow::Texture>,
    white: glow::Texture,
    /// 描く順(OPAQUE/MASK → BLEND)のプリミティブ番号
    order: Vec<usize>,
    /// ノード番号 → morph target ごとの weight
    weights: BTreeMap<usize, Vec<f32>>,
    dirty: bool,
    scratch: Vec<f32>,
    /// GPU に転送したバイト数(頂点・インデックス・テクスチャ)
    pub uploaded_bytes: u64,
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    // SAFETY: f32 / u32 / u8 の並びを読むだけ(パディングなし)
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

unsafe fn compile(gl: &glow::Context) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
            let s = match gl.create_shader(kind) {
                Ok(s) => s,
                Err(e) => {
                    for s in shaders {
                        gl.delete_shader(s);
                    }
                    gl.delete_program(program);
                    return Err(e);
                }
            };
            gl.shader_source(s, src);
            gl.compile_shader(s);
            shaders.push(s);
            if !gl.get_shader_compile_status(s) {
                let log = gl.get_shader_info_log(s);
                for s in shaders {
                    gl.delete_shader(s);
                }
                gl.delete_program(program);
                return Err(format!("シェーダ: {log}"));
            }
            gl.attach_shader(program, s);
        }
        gl.link_program(program);
        let ok = gl.get_program_link_status(program);
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        if !ok {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(format!("リンク: {log}"));
        }
        Ok(program)
    }
}

impl Renderer {
    /// モデルを GPU に転送する。失敗したときは、それまでに作った GL オブジェクトを消して返る。
    pub fn new(gl: &glow::Context, model: Model) -> Result<Self, String> {
        let mut created = Created::default();
        unsafe {
            match Self::build(gl, model, &mut created) {
                Ok(r) => Ok(r),
                Err(e) => {
                    created.delete(gl);
                    // 途中で立った GL エラーを呼び出し側に残さない
                    for _ in 0..32 {
                        if gl.get_error() == glow::NO_ERROR {
                            break;
                        }
                    }
                    Err(e)
                }
            }
        }
    }

    unsafe fn build(
        gl: &glow::Context,
        model: Model,
        created: &mut Created,
    ) -> Result<Self, String> {
        let mut uploaded = 0u64;
        unsafe {
            let program = compile(gl)?;
            created.program = Some(program);
            let loc = |n: &str| gl.get_uniform_location(program, n);
            let uniforms = Uniforms {
                vp: loc("u_vp"),
                light: loc("u_light"),
                tex: loc("u_tex"),
                color: loc("u_color"),
                has_tex: loc("u_has_tex"),
                cutoff: loc("u_cutoff"),
                opaque: loc("u_opaque"),
            };
            let mut prims = Vec::new();
            for p in &model.primitives {
                let vao = gl.create_vertex_array()?;
                created.vaos.push(vao);
                gl.bind_vertex_array(Some(vao));
                let vbo = gl.create_buffer()?;
                created.buffers.push(vbo);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                let usage = if p.targets.is_empty() {
                    glow::STATIC_DRAW
                } else {
                    glow::DYNAMIC_DRAW
                };
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&p.vertices), usage);
                let stride = (FLOATS_PER_VERTEX * 4) as i32;
                for (loc, comps, offset) in [(0, 3, 0), (1, 3, 12), (2, 2, 24)] {
                    gl.enable_vertex_attrib_array(loc);
                    gl.vertex_attrib_pointer_f32(loc, comps, glow::FLOAT, false, stride, offset);
                }
                let ebo = gl.create_buffer()?;
                created.buffers.push(ebo);
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
                gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    as_bytes(&p.indices),
                    glow::STATIC_DRAW,
                );
                uploaded += (p.vertices.len() * 4 + p.indices.len() * 4) as u64;
                prims.push(GpuPrimitive {
                    vao,
                    vbo,
                    ebo,
                    index_count: p.indices.len() as i32,
                    morphed: false,
                });
            }
            gl.bind_vertex_array(None);

            let mut textures = Vec::new();
            for t in &model.textures {
                let img = &model.images[t.image];
                let tex =
                    upload_texture(gl, img.width, img.height, &img.rgba, (t.wrap_s, t.wrap_t))?;
                created.textures.push(tex);
                // ミップマップ込みで約 4/3 倍
                uploaded += img.rgba.len() as u64 * 4 / 3;
                textures.push(tex);
            }
            let white = upload_texture(gl, 1, 1, &[255; 4], (glow::REPEAT, glow::REPEAT))?;
            created.textures.push(white);

            let mut order: Vec<usize> = (0..model.primitives.len()).collect();
            order.sort_by_key(|&i| {
                model.materials[model.primitives[i].material].alpha_mode == AlphaMode::Blend
            });
            let mut weights = BTreeMap::new();
            for p in &model.primitives {
                if !p.targets.is_empty() {
                    weights.insert(p.node, vec![0.0; p.targets.len()]);
                }
            }
            let e = gl.get_error();
            if e != glow::NO_ERROR {
                return Err(format!("GL エラー 0x{e:x}(モデルの転送)"));
            }
            Ok(Self {
                model,
                program,
                uniforms,
                prims,
                textures,
                white,
                order,
                weights,
                dirty: false,
                scratch: Vec::new(),
                uploaded_bytes: uploaded,
            })
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// expression の weight(0..1)を与える。ここに挙げないものは 0 に戻る。
    /// 実際の頂点の更新は `update` で行う。
    pub fn set_expressions(&mut self, list: &[(&str, f32)]) -> Result<(), String> {
        for w in self.weights.values_mut() {
            w.fill(0.0);
        }
        for &(name, value) in list {
            let e = self
                .model
                .vrm
                .expression(name)
                .ok_or_else(|| format!("expression {name} がない"))?;
            for b in &e.binds {
                let slot = self
                    .weights
                    .get_mut(&b.node)
                    .and_then(|w| w.get_mut(b.index))
                    .ok_or_else(|| {
                        format!(
                            "expression {name} の bind 先(node {} / {})がない",
                            b.node, b.index
                        )
                    })?;
                *slot = (*slot + b.weight * value.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn set_expression(&mut self, name: &str, weight: f32) -> Result<(), String> {
        self.set_expressions(&[(name, weight)])
    }

    /// morph 合成後の頂点(`[px,py,pz,nx,ny,nz,u,v]`)。テスト用に CPU 側で同じ計算をする。
    pub fn morphed_vertices(&self, prim: usize) -> Vec<f32> {
        let p = &self.model.primitives[prim];
        let mut out = p.vertices.clone();
        let Some(w) = self.weights.get(&p.node) else {
            return out;
        };
        for (t, &wt) in p.targets.iter().zip(w) {
            if wt == 0.0 {
                continue;
            }
            for (i, v) in out
                .as_chunks_mut::<FLOATS_PER_VERTEX>()
                .0
                .iter_mut()
                .enumerate()
            {
                for k in 0..3 {
                    v[k] += wt * t.positions[i][k];
                    v[3 + k] += wt * t.normals[i][k];
                }
            }
        }
        out
    }

    /// weight が変わっていれば morph を CPU で合成し、頂点バッファを更新する。
    pub fn update(&mut self, gl: &glow::Context) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        for i in 0..self.prims.len() {
            if self.model.primitives[i].targets.is_empty() {
                continue;
            }
            let active = self.weights[&self.model.primitives[i].node]
                .iter()
                .any(|&w| w != 0.0);
            if !active && !self.prims[i].morphed {
                continue;
            }
            self.scratch = self.morphed_vertices(i);
            self.prims[i].morphed = active;
            unsafe {
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.prims[i].vbo));
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&self.scratch));
            }
        }
    }

    /// 今 bind されている FBO(深度付き)の `(w, h)` に描く。カメラは外接箱から自動で決める。
    pub fn render(&self, gl: &glow::Context, (w, h): (u32, u32)) {
        let (min, max) = self.model.bounds;
        let c = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let aspect = w as f32 / h as f32;
        let fovy = 30f32.to_radians();
        let t = (fovy * 0.5).tan();
        // 全身(縦)も横幅も入る距離。+Z 側から -Z を向く(VRM 1.0 は +Z が前)
        let dist = ((size[1] * 0.5 / t).max(size[0] * 0.5 / (t * aspect)) * 1.15) + size[2] * 0.5;
        let proj = math::perspective(fovy, aspect, dist * 0.1, dist * 10.0);
        let view = math::translate(-c[0], -c[1], -(c[2] + dist));
        let vp = math::mul(&proj, &view);
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.clear_color(0.08, 0.09, 0.12, 1.0);
            gl.clear_depth_f32(1.0);
            gl.depth_mask(true);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.use_program(Some(self.program));
            let u = &self.uniforms;
            gl.uniform_matrix_4_f32_slice(u.vp.as_ref(), false, as_bytes_f32(&vp));
            gl.uniform_3_f32(u.light.as_ref(), 0.4, 0.6, 0.7);
            gl.uniform_1_i32(u.tex.as_ref(), 0);
            gl.active_texture(glow::TEXTURE0);
            for &i in &self.order {
                let m = &self.model.materials[self.model.primitives[i].material];
                let blend = m.alpha_mode == AlphaMode::Blend;
                if m.double_sided {
                    gl.disable(glow::CULL_FACE);
                } else {
                    gl.enable(glow::CULL_FACE);
                    gl.cull_face(glow::BACK);
                }
                if blend {
                    gl.enable(glow::BLEND);
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                    gl.depth_mask(false);
                } else {
                    gl.disable(glow::BLEND);
                    gl.depth_mask(true);
                }
                let tex = m.texture.map(|t| self.textures[t]);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex.unwrap_or(self.white)));
                gl.uniform_4_f32_slice(u.color.as_ref(), &m.base_color);
                gl.uniform_1_f32(u.has_tex.as_ref(), tex.is_some() as u32 as f32);
                let cutoff = if m.alpha_mode == AlphaMode::Mask {
                    m.alpha_cutoff
                } else {
                    -1.0
                };
                gl.uniform_1_f32(u.cutoff.as_ref(), cutoff);
                gl.uniform_1_f32(
                    u.opaque.as_ref(),
                    (m.alpha_mode != AlphaMode::Blend) as u32 as f32,
                );
                gl.bind_vertex_array(Some(self.prims[i].vao));
                gl.draw_elements(
                    glow::TRIANGLES,
                    self.prims[i].index_count,
                    glow::UNSIGNED_INT,
                    0,
                );
            }
            gl.bind_vertex_array(None);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::DEPTH_TEST);
        }
    }

    /// GPU リソースをすべて解放する。
    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            for p in &self.prims {
                gl.delete_vertex_array(p.vao);
                gl.delete_buffer(p.vbo);
                gl.delete_buffer(p.ebo);
            }
            for &t in &self.textures {
                gl.delete_texture(t);
            }
            gl.delete_texture(self.white);
            gl.delete_program(self.program);
        }
    }
}

fn as_bytes_f32(m: &math::Mat4) -> &[f32] {
    m.as_flattened()
}

unsafe fn upload_texture(
    gl: &glow::Context,
    w: u32,
    h: u32,
    rgba: &[u8],
    (wrap_s, wrap_t): (u32, u32),
) -> Result<glow::Texture, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::SRGB8_ALPHA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(rgba)),
        );
        gl.generate_mipmap(glow::TEXTURE_2D);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, wrap_s as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, wrap_t as i32);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR_MIPMAP_LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        Ok(tex)
    }
}

/// 今 bind されている FBO に深度 renderbuffer を付ける(`Offscreen::with_fbo` の FBO には深度がない)。
/// 返した renderbuffer は描き終わったら `detach_depth` で外して消す。
pub fn attach_depth(gl: &glow::Context, (w, h): (u32, u32)) -> Result<glow::Renderbuffer, String> {
    unsafe {
        let rb = gl.create_renderbuffer()?;
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
        gl.renderbuffer_storage(
            glow::RENDERBUFFER,
            glow::DEPTH24_STENCIL8,
            w as i32,
            h as i32,
        );
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(rb),
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_renderbuffer(rb);
            return Err(format!("深度付き FBO が不完全: 0x{status:x}"));
        }
        Ok(rb)
    }
}

pub fn detach_depth(gl: &glow::Context, rb: glow::Renderbuffer) {
    unsafe {
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            None,
        );
        gl.delete_renderbuffer(rb);
    }
}
