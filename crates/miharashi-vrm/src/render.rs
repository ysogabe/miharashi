//! GLES 3.x(glow)での描画。スキニングは頂点シェーダ(関節行列は UBO)、表情は morph target を
//! CPU で合成して頂点バッファを更新する(`set_expressions` → `update`)。
//! 機能段階(`Level`)で、Lambert + 焼き込み頂点(以前の描き方)から MToon・SpringBone まで切り替えられる。
use crate::anim::Rig;
use crate::math;
use crate::model::{AlphaMode, FLOATS_PER_VERTEX, Model};
use crate::vrm::OutlineMode;
use glow::HasContext;
use std::collections::BTreeMap;
use std::time::Instant;

/// 描き方の機能段階(後ろのものは前のものを含む)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 初期姿勢を頂点に焼き込んで Lambert(以前の描き方。morph なし)
    Bake,
    /// 実行時の GPU スキニング(毎フレーム階層を評価。ポーズは初期姿勢)+ Lambert
    Skin,
    /// + 自然なポーズと待機モーション
    Pose,
    /// + MToon(トゥーン 2 階調・リム・emissive・輪郭線)
    Mtoon,
    /// + SpringBone(髪・服の揺れ)
    Spring,
}

impl Level {
    pub const ALL: [Level; 5] = [
        Level::Bake,
        Level::Skin,
        Level::Pose,
        Level::Mtoon,
        Level::Spring,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Level::Bake => "bake",
            Level::Skin => "skin",
            Level::Pose => "pose",
            Level::Mtoon => "mtoon",
            Level::Spring => "spring",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|l| l.name() == s)
            .ok_or_else(|| format!("--feature-level は bake / skin / pose / mtoon / spring: {s}"))
    }
}

/// 関節行列の UBO に入る関節の上限(GLES 3.0 は UBO 16KB 以上を保証 = mat4 が 256 個)
pub const MAX_JOINTS: usize = 256;

/// 1 回の `update` の CPU 時間 [ms]。
#[derive(Clone, Copy, Debug, Default)]
pub struct UpdateTimes {
    /// ノード階層の評価 + 関節行列 + UBO 転送
    pub pose_ms: f64,
    /// SpringBone の計算(`pose_ms` には含めない)
    pub spring_ms: f64,
    /// morph の合成と頂点バッファの転送
    pub morph_ms: f64,
}

const VS_HEAD: &str = "#version 300 es
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_nrm;
layout(location = 2) in vec2 a_uv;
layout(location = 3) in uvec4 a_joints;
layout(location = 4) in vec4 a_weights;
layout(std140) uniform Joints { mat4 u_joint[256]; };
uniform mat4 u_vp;
uniform int u_skin;
uniform int u_joint_base;
mat4 skin_matrix() {
    if (u_skin == 0) return mat4(1.0);
    int b = u_joint_base;
    mat4 m = u_joint[b + int(a_joints.x)] * a_weights.x
           + u_joint[b + int(a_joints.y)] * a_weights.y
           + u_joint[b + int(a_joints.z)] * a_weights.z
           + u_joint[b + int(a_joints.w)] * a_weights.w;
    float s = a_weights.x + a_weights.y + a_weights.z + a_weights.w;
    return s > 0.0 ? m / s : mat4(1.0);
}
";

const VS_BODY: &str = "
out vec3 v_nrm;
out vec2 v_uv;
out vec3 v_world;
void main() {
    mat4 sm = skin_matrix();
    vec4 wp = sm * vec4(a_pos, 1.0);
    v_nrm = normalize(mat3(sm) * a_nrm);
    v_uv = a_uv;
    v_world = wp.xyz;
    gl_Position = u_vp * wp;
}
";

// 輪郭線(inverted hull): スキンした法線の方向へ押し出し、裏面だけを描く
const VS_OUTLINE: &str = "
uniform sampler2D u_width_tex;
uniform float u_has_width_tex;
uniform float u_width;
uniform float u_mode; // 0 = world(メートル)、1 = screen(画面の高さに対する割合)
uniform float u_aspect;
out vec2 v_uv;
void main() {
    mat4 sm = skin_matrix();
    vec4 wp = sm * vec4(a_pos, 1.0);
    vec3 n = normalize(mat3(sm) * a_nrm);
    float w = u_width;
    if (u_has_width_tex > 0.5) w *= textureLod(u_width_tex, a_uv, 0.0).r;
    v_uv = a_uv;
    if (u_mode < 0.5) {
        gl_Position = u_vp * vec4(wp.xyz + n * w, 1.0);
    } else {
        gl_Position = u_vp * wp;
        vec2 cn = (u_vp * vec4(n, 0.0)).xy;
        float l = length(cn);
        if (l > 1e-6) gl_Position.xy += cn / l * vec2(2.0 * w / u_aspect, 2.0 * w) * gl_Position.w;
    }
}
";

// テクスチャは SRGB8_ALPHA8 で読むので、ここで線形になっている。最後にガンマをかけて出力する
const FS_LAMBERT: &str = "#version 300 es
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

// MToon(VRMC_materials_mtoon)の近似。光は平行光源 1 つ + 環境光。
// shading = linearstep(-1 + toony, 1 - toony, dot(N, L) + shift)、色 = mix(shade, lit, shading)
const FS_MTOON: &str = "#version 300 es
precision highp float;
in vec3 v_nrm;
in vec2 v_uv;
in vec3 v_world;
uniform sampler2D u_tex;
uniform sampler2D u_shade_tex;
uniform vec4 u_color;
uniform float u_has_tex;
uniform float u_has_shade_tex;
uniform float u_cutoff;
uniform float u_opaque;
uniform vec3 u_light;
uniform float u_light_intensity;
uniform float u_ambient;
uniform vec3 u_eye;
uniform vec3 u_shade_color;
uniform float u_toony;
uniform float u_shift;
uniform vec3 u_rim_color;
uniform float u_rim_power;
uniform float u_rim_lift;
uniform float u_rim_mix;
uniform vec3 u_emissive;
out vec4 o_color;
void main() {
    vec4 c = u_color;
    if (u_has_tex > 0.5) c *= texture(u_tex, v_uv);
    if (u_cutoff >= 0.0 && c.a < u_cutoff) discard;
    vec3 n = normalize(gl_FrontFacing ? v_nrm : -v_nrm);
    vec3 lit = c.rgb;
    vec3 shade = u_shade_color;
    if (u_has_shade_tex > 0.5) shade *= texture(u_shade_tex, v_uv).rgb;
    float lo = -1.0 + u_toony;
    float hi = 1.0 - u_toony;
    float s = dot(n, normalize(u_light)) + u_shift;
    s = hi > lo ? clamp((s - lo) / (hi - lo), 0.0, 1.0) : step(lo, s);
    vec3 col = mix(shade, lit, s) * u_light_intensity + lit * u_ambient;
    // パラメトリックリム
    vec3 v = normalize(u_eye - v_world);
    float rim = pow(clamp(1.0 - dot(n, v) + u_rim_lift, 0.0, 1.0), max(u_rim_power, 1e-4));
    col += u_rim_color * rim * mix(1.0, clamp(s * u_light_intensity + u_ambient, 0.0, 1.0), u_rim_mix);
    col += u_emissive;
    o_color = vec4(pow(max(col, 0.0), vec3(1.0 / 2.2)), u_opaque > 0.5 ? 1.0 : c.a);
}
";

const FS_OUTLINE: &str = "#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
uniform float u_has_tex;
uniform float u_cutoff;
uniform float u_opaque;
uniform vec3 u_outline_color;
uniform float u_outline_mix;
uniform float u_lum;
out vec4 o_color;
void main() {
    vec4 c = u_color;
    if (u_has_tex > 0.5) c *= texture(u_tex, v_uv);
    if (u_cutoff >= 0.0 && c.a < u_cutoff) discard;
    vec3 col = u_outline_color * mix(1.0, u_lum, u_outline_mix);
    o_color = vec4(pow(max(col, 0.0), vec3(1.0 / 2.2)), u_opaque > 0.5 ? 1.0 : c.a);
}
";

/// 平行光源(モデル空間。正面上から)の向きと強さ、環境光
const LIGHT_DIR: [f32; 3] = [0.3, 0.6, 0.75];
const LIGHT_INTENSITY: f32 = 0.9;
const AMBIENT: f32 = 0.1;

struct GpuPrimitive {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    skin_vbo: glow::Buffer,
    ebo: glow::Buffer,
    index_count: i32,
    /// morph で書き換えたあとなら true(ゼロに戻るときにベース形状へ戻す)
    morphed: bool,
}

/// プログラムの uniform の場所(一度だけ引いて持つ)。ないものは `None`(設定は無視される)。
struct Locs(BTreeMap<&'static str, Option<glow::UniformLocation>>);

const UNIFORM_NAMES: &[&str] = &[
    "u_vp",
    "u_skin",
    "u_joint_base",
    "u_light",
    "u_tex",
    "u_color",
    "u_has_tex",
    "u_cutoff",
    "u_opaque",
    "u_shade_tex",
    "u_has_shade_tex",
    "u_light_intensity",
    "u_ambient",
    "u_eye",
    "u_shade_color",
    "u_toony",
    "u_shift",
    "u_rim_color",
    "u_rim_power",
    "u_rim_lift",
    "u_rim_mix",
    "u_emissive",
    "u_width_tex",
    "u_has_width_tex",
    "u_width",
    "u_mode",
    "u_aspect",
    "u_outline_color",
    "u_outline_mix",
    "u_lum",
];

impl Locs {
    unsafe fn new(gl: &glow::Context, program: glow::Program) -> Self {
        unsafe {
            Self(
                UNIFORM_NAMES
                    .iter()
                    .map(|n| (*n, gl.get_uniform_location(program, n)))
                    .collect(),
            )
        }
    }

    fn get(&self, name: &str) -> Option<&glow::UniformLocation> {
        self.0.get(name).and_then(|l| l.as_ref())
    }
}

struct Prog {
    program: glow::Program,
    locs: Locs,
}

/// `Renderer::new` の途中で作った GL オブジェクトの控え。失敗したときにまとめて消す。
#[derive(Default)]
struct Created {
    programs: Vec<glow::Program>,
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
            for p in self.programs {
                gl.delete_program(p);
            }
        }
    }
}

pub struct Renderer {
    model: Model,
    level: Level,
    lambert: Prog,
    mtoon: Prog,
    outline: Prog,
    joint_ubo: glow::Buffer,
    rig: Rig,
    spring: Option<crate::spring::SpringSim>,
    prims: Vec<GpuPrimitive>,
    textures: Vec<glow::Texture>,
    /// データとして読むテクスチャ(輪郭線の幅)。sRGB 変換をかけない
    linear_textures: BTreeMap<usize, glow::Texture>,
    white: glow::Texture,
    /// 描く順(OPAQUE/MASK、BLEND)のプリミティブ番号
    order_solid: Vec<usize>,
    order_blend: Vec<usize>,
    /// 輪郭線を描くプリミティブ(OPAQUE/MASK で、輪郭線が有効なもの)
    order_outline: Vec<usize>,
    /// ノード番号 → morph target ごとの weight
    weights: BTreeMap<usize, Vec<f32>>,
    dirty: bool,
    scratch: Vec<f32>,
    /// 輪郭線の太さの倍率(確認用。1 で VRM の指定どおり)
    pub outline_scale: f32,
    /// 描く前に塗る色(RGBA)。透明な FBO に描いて重ねるときは `[0.0; 4]`
    pub clear_color: [f32; 4],
    anchors: Anchors,
    pose_ms: f64,
    spring_ms: f64,
    /// GPU に転送したバイト数(頂点・インデックス・テクスチャ)
    pub uploaded_bytes: u64,
}

fn as_bytes<T>(v: &[T]) -> &[u8] {
    // SAFETY: f32 / u32 / u8 の並びを読むだけ(パディングなし)
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

unsafe fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
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

/// `layer` など、このクレートの別の場所からシェーダを作る。
pub(crate) fn compile_program(
    gl: &glow::Context,
    vs: &str,
    fs: &str,
) -> Result<glow::Program, String> {
    unsafe { compile(gl, vs, fs) }
}

/// 3 つのプログラムを作り、関節行列の UBO(binding 0)につなぐ。
unsafe fn make_prog(
    gl: &glow::Context,
    created: &mut Created,
    vs: &str,
    fs: &str,
) -> Result<Prog, String> {
    unsafe {
        let program = compile(gl, vs, fs)?;
        created.programs.push(program);
        if let Some(i) = gl.get_uniform_block_index(program, "Joints") {
            gl.uniform_block_binding(program, i, 0);
        }
        Ok(Prog {
            program,
            locs: Locs::new(gl, program),
        })
    }
}

impl Renderer {
    /// モデルを GPU に転送する(機能段階は最大の `Spring`)。
    pub fn new(gl: &glow::Context, model: Model) -> Result<Self, String> {
        Self::with_level(gl, model, Level::Spring)
    }

    /// 機能段階を指定して転送する。失敗したときは、それまでに作った GL オブジェクトを消して返る。
    pub fn with_level(gl: &glow::Context, model: Model, level: Level) -> Result<Self, String> {
        let mut created = Created::default();
        unsafe {
            match Self::build(gl, model, level, &mut created) {
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
        level: Level,
        created: &mut Created,
    ) -> Result<Self, String> {
        let mut uploaded = 0u64;
        unsafe {
            let n_joints = model.joint_nodes.len();
            let max_block = gl.get_parameter_i32(glow::MAX_UNIFORM_BLOCK_SIZE) as usize;
            if level != Level::Bake && (n_joints > MAX_JOINTS || n_joints * 64 > max_block) {
                return Err(format!(
                    "関節が多すぎる({n_joints} 個、上限 {} 個。UBO は {max_block} バイト)",
                    MAX_JOINTS.min(max_block / 64)
                ));
            }
            let vs_body = format!("{VS_HEAD}{VS_BODY}");
            let vs_outline = format!("{VS_HEAD}{VS_OUTLINE}");
            let lambert = make_prog(gl, created, &vs_body, FS_LAMBERT)?;
            let mtoon = make_prog(gl, created, &vs_body, FS_MTOON)?;
            let outline = make_prog(gl, created, &vs_outline, FS_OUTLINE)?;

            let joint_ubo = gl.create_buffer()?;
            created.buffers.push(joint_ubo);
            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(joint_ubo));
            gl.buffer_data_size(
                glow::UNIFORM_BUFFER,
                (MAX_JOINTS * 64) as i32,
                glow::DYNAMIC_DRAW,
            );
            gl.bind_buffer_base(glow::UNIFORM_BUFFER, 0, Some(joint_ubo));

            let mut prims = Vec::new();
            for (pi, p) in model.primitives.iter().enumerate() {
                let vao = gl.create_vertex_array()?;
                created.vaos.push(vao);
                gl.bind_vertex_array(Some(vao));
                let vbo = gl.create_buffer()?;
                created.buffers.push(vbo);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                let usage = if p.targets.is_empty() || level == Level::Bake {
                    glow::STATIC_DRAW
                } else {
                    glow::DYNAMIC_DRAW
                };
                let verts = if level == Level::Bake {
                    model.baked_vertices(pi)
                } else {
                    p.vertices.clone()
                };
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&verts), usage);
                let stride = (FLOATS_PER_VERTEX * 4) as i32;
                for (loc, comps, offset) in [(0, 3, 0), (1, 3, 12), (2, 2, 24)] {
                    gl.enable_vertex_attrib_array(loc);
                    gl.vertex_attrib_pointer_f32(loc, comps, glow::FLOAT, false, stride, offset);
                }
                // 関節番号(u16 × 4)と重み(f32 × 4)。morph で書き換える頂点とは別のバッファ
                let skin_vbo = gl.create_buffer()?;
                created.buffers.push(skin_vbo);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(skin_vbo));
                let mut skin_data = Vec::with_capacity(p.joints.len() * 24);
                for (j, w) in p.joints.iter().zip(&p.weights) {
                    for v in j {
                        skin_data.extend_from_slice(&v.to_le_bytes());
                    }
                    for v in w {
                        skin_data.extend_from_slice(&v.to_le_bytes());
                    }
                }
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &skin_data, glow::STATIC_DRAW);
                gl.enable_vertex_attrib_array(3);
                gl.vertex_attrib_pointer_i32(3, 4, glow::UNSIGNED_SHORT, 24, 0);
                gl.enable_vertex_attrib_array(4);
                gl.vertex_attrib_pointer_f32(4, 4, glow::FLOAT, false, 24, 8);
                let ebo = gl.create_buffer()?;
                created.buffers.push(ebo);
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
                gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    as_bytes(&p.indices),
                    glow::STATIC_DRAW,
                );
                uploaded += (verts.len() * 4 + skin_data.len() + p.indices.len() * 4) as u64;
                prims.push(GpuPrimitive {
                    vao,
                    vbo,
                    skin_vbo,
                    ebo,
                    index_count: p.indices.len() as i32,
                    morphed: false,
                });
            }
            gl.bind_vertex_array(None);

            let mut textures = Vec::new();
            for t in &model.textures {
                let img = &model.images[t.image];
                let tex = upload_texture(
                    gl,
                    img.width,
                    img.height,
                    &img.rgba,
                    (t.wrap_s, t.wrap_t),
                    true,
                )?;
                created.textures.push(tex);
                // ミップマップ込みで約 4/3 倍
                uploaded += img.rgba.len() as u64 * 4 / 3;
                textures.push(tex);
            }
            // 輪郭線の幅のテクスチャは、色ではなくデータ(sRGB 変換しない)として別に載せる
            let mut linear_textures = BTreeMap::new();
            if level >= Level::Mtoon {
                for m in &model.materials {
                    if let Some(ti) = m.mtoon.as_ref().and_then(|m| m.outline_width_texture)
                        && !linear_textures.contains_key(&ti)
                    {
                        let t = &model.textures[ti];
                        let img = &model.images[t.image];
                        let tex = upload_texture(
                            gl,
                            img.width,
                            img.height,
                            &img.rgba,
                            (t.wrap_s, t.wrap_t),
                            false,
                        )?;
                        created.textures.push(tex);
                        uploaded += img.rgba.len() as u64 * 4 / 3;
                        linear_textures.insert(ti, tex);
                    }
                }
            }
            let white = upload_texture(gl, 1, 1, &[255; 4], (glow::REPEAT, glow::REPEAT), true)?;
            created.textures.push(white);

            let blend = |i: usize| {
                model.materials[model.primitives[i].material].alpha_mode == AlphaMode::Blend
            };
            let order_solid: Vec<usize> = (0..prims.len()).filter(|&i| !blend(i)).collect();
            // 透明は renderQueueOffsetNumber の小さい順(同じなら元の順)
            let mut order_blend: Vec<usize> = (0..prims.len()).filter(|&i| blend(i)).collect();
            order_blend.sort_by_key(|&i| {
                model.materials[model.primitives[i].material]
                    .mtoon
                    .as_ref()
                    .map_or(0, |m| m.render_queue_offset)
            });
            let order_outline: Vec<usize> = if level >= Level::Mtoon {
                order_solid
                    .iter()
                    .copied()
                    .filter(|&i| {
                        model.materials[model.primitives[i].material]
                            .mtoon
                            .as_ref()
                            .is_some_and(|m| {
                                m.outline_mode != OutlineMode::None && m.outline_width > 0.0
                            })
                    })
                    .collect()
            } else {
                Vec::new()
            };
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
            let rig = Rig::new(&model, level >= Level::Pose);
            let anchors = Anchors::new(&model, &rig.world);
            let spring =
                (level >= Level::Spring).then(|| crate::spring::SpringSim::new(&model, &rig));
            Ok(Self {
                model,
                level,
                lambert,
                mtoon,
                outline,
                joint_ubo,
                rig,
                spring,
                prims,
                textures,
                linear_textures,
                white,
                order_solid,
                order_blend,
                order_outline,
                weights,
                dirty: false,
                scratch: Vec::new(),
                outline_scale: 1.0,
                clear_color: [0.08, 0.09, 0.12, 1.0],
                anchors,
                pose_ms: 0.0,
                spring_ms: 0.0,
                uploaded_bytes: uploaded,
            })
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// 時刻 `t` [秒] のポーズを CPU で評価する(`dt` は前フレームからの経過、SpringBone 用)。
    /// 関節行列の GPU への転送は `update` で行う。`Bake` では何もしない。
    pub fn animate(&mut self, t: f32, dt: f32) {
        if self.level == Level::Bake {
            return;
        }
        let t0 = Instant::now();
        let mut spring_time = std::time::Duration::ZERO;
        match self.spring.as_mut() {
            Some(sim) => {
                let mut f = |rig: &mut Rig, model: &Model| {
                    let s0 = Instant::now();
                    sim.step(model, rig, dt);
                    spring_time = s0.elapsed();
                };
                self.rig.update(&self.model, t, Some(&mut f));
            }
            None => self.rig.update(&self.model, t, None),
        }
        self.spring_ms = spring_time.as_secs_f64() * 1e3;
        self.pose_ms = (t0.elapsed() - spring_time).as_secs_f64() * 1e3;
    }

    /// 身振り(`gesture`)を重ねるか。`Pose` 未満の段階や、humanoid の腕の骨がないモデルではエラー。
    pub fn set_gesture(&mut self, on: bool) -> Result<(), String> {
        if on && self.level < Level::Pose {
            return Err("身振りには --feature-level pose 以上が要る".into());
        }
        if self.rig.set_gesture(on) {
            Ok(())
        } else {
            Err("humanoid の上腕・前腕・手の骨がないモデルでは身振りを付けられない".into())
        }
    }

    /// 最後に評価した関節行列(`animate` 後)。
    pub fn joint_matrices(&self) -> &[math::Mat4] {
        &self.rig.joints
    }

    pub fn rig(&self) -> &Rig {
        &self.rig
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

    /// 関節行列の UBO を更新し、weight が変わっていれば morph を CPU で合成して頂点バッファを更新する。
    pub fn update(&mut self, gl: &glow::Context) -> UpdateTimes {
        let mut times = UpdateTimes {
            pose_ms: self.pose_ms,
            spring_ms: self.spring_ms,
            morph_ms: 0.0,
        };
        let t0 = Instant::now();
        unsafe {
            gl.bind_buffer(glow::UNIFORM_BUFFER, Some(self.joint_ubo));
            if self.level == Level::Bake {
                // 変形しないので、一度だけ単位行列を入れておけばよい(u_skin = 0 で使わない)
            } else {
                gl.buffer_sub_data_u8_slice(glow::UNIFORM_BUFFER, 0, as_bytes(&self.rig.joints));
            }
        }
        times.pose_ms += t0.elapsed().as_secs_f64() * 1e3;
        if self.level == Level::Bake || !self.dirty {
            return times;
        }
        let t1 = Instant::now();
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
        times.morph_ms = t1.elapsed().as_secs_f64() * 1e3;
        times
    }

    /// 今 bind されている FBO(深度付き)の `(w, h)` に描く。カメラは外接箱から自動で決める。
    pub fn render(&self, gl: &glow::Context, size: (u32, u32)) {
        self.render_yaw(gl, size, 0.0);
    }

    fn material_state(&self, gl: &glow::Context, m: &crate::model::Material) {
        unsafe {
            if m.double_sided {
                gl.disable(glow::CULL_FACE);
            } else {
                gl.enable(glow::CULL_FACE);
                gl.cull_face(glow::BACK);
            }
            if m.alpha_mode == AlphaMode::Blend {
                gl.enable(glow::BLEND);
                // 色は SRC_ALPHA で重ね、アルファは ONE / ONE_MINUS_SRC_ALPHA で貯める。透明な FBO に描くと、
                // 色が premultiplied、アルファが本来の値(a + dst_a * (1 - a))になる(不透明な下地なら従来と同じ)
                gl.blend_func_separate(
                    glow::SRC_ALPHA,
                    glow::ONE_MINUS_SRC_ALPHA,
                    glow::ONE,
                    glow::ONE_MINUS_SRC_ALPHA,
                );
                gl.depth_mask(false);
            } else {
                gl.disable(glow::BLEND);
                gl.depth_mask(true);
            }
        }
    }

    /// 共通の uniform(カメラ・スキニング・ベースカラー)を設定する。
    unsafe fn set_common(
        &self,
        gl: &glow::Context,
        pr: &Prog,
        i: usize,
        m: &crate::model::Material,
    ) {
        unsafe {
            let l = &pr.locs;
            gl.uniform_1_i32(l.get("u_skin"), (self.level != Level::Bake) as i32);
            gl.uniform_1_i32(
                l.get("u_joint_base"),
                self.model.primitives[i].joint_base as i32,
            );
            let tex = m.texture.map(|t| self.textures[t]);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex.unwrap_or(self.white)));
            gl.uniform_1_i32(l.get("u_tex"), 0);
            gl.uniform_4_f32_slice(l.get("u_color"), &m.base_color);
            gl.uniform_1_f32(l.get("u_has_tex"), tex.is_some() as u32 as f32);
            let cutoff = if m.alpha_mode == AlphaMode::Mask {
                m.alpha_cutoff
            } else {
                -1.0
            };
            gl.uniform_1_f32(l.get("u_cutoff"), cutoff);
            gl.uniform_1_f32(
                l.get("u_opaque"),
                (m.alpha_mode != AlphaMode::Blend) as u32 as f32,
            );
        }
    }

    /// `render` に加えて、カメラをモデルの中心のまわりで Y 軸方向に `yaw`(ラジアン)だけ回す。
    pub fn render_yaw(&self, gl: &glow::Context, size: (u32, u32), yaw: f32) {
        let (vp, eye) = camera(self.model.bounds, size, yaw);
        self.render_camera(gl, size, &vp, eye);
    }

    /// 構図 `framing` のカメラ(`size` は描く先の大きさ)。`Full` は `camera` と同じ。
    pub fn framing_camera(
        &self,
        framing: Framing,
        size: (u32, u32),
        yaw: f32,
    ) -> (math::Mat4, [f32; 3]) {
        match framing {
            Framing::Full => camera(self.model.bounds, size, yaw),
            _ => self.anchors.camera(framing, size, yaw),
        }
    }

    /// 指定のカメラ(`framing_camera` の戻り値など)で、今 bind されている FBO の `(w, h)` に描く。
    pub fn render_camera(
        &self,
        gl: &glow::Context,
        (w, h): (u32, u32),
        vp: &math::Mat4,
        eye: [f32; 3],
    ) {
        let vp = *vp;
        let aspect = w as f32 / h as f32;
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            let c = self.clear_color;
            gl.clear_color(c[0], c[1], c[2], c[3]);
            gl.clear_depth_f32(1.0);
            gl.depth_mask(true);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.bind_buffer_base(glow::UNIFORM_BUFFER, 0, Some(self.joint_ubo));
            let use_mtoon = self.level >= Level::Mtoon;
            let mut current: Option<glow::Program> = None;
            // 不透明・MASK → 輪郭線 → 透明の順
            for pass in 0..3 {
                let list = match pass {
                    0 => &self.order_solid,
                    1 => &self.order_outline,
                    _ => &self.order_blend,
                };
                for &i in list {
                    let m = &self.model.materials[self.model.primitives[i].material];
                    let mt = m.mtoon.as_ref().filter(|_| use_mtoon);
                    let pr = if pass == 1 {
                        &self.outline
                    } else if mt.is_some() {
                        &self.mtoon
                    } else {
                        &self.lambert
                    };
                    if current != Some(pr.program) {
                        current = Some(pr.program);
                        gl.use_program(Some(pr.program));
                        let l = &pr.locs;
                        gl.uniform_matrix_4_f32_slice(l.get("u_vp"), false, as_bytes_f32(&vp));
                        gl.uniform_3_f32(
                            l.get("u_light"),
                            LIGHT_DIR[0],
                            LIGHT_DIR[1],
                            LIGHT_DIR[2],
                        );
                        gl.uniform_3_f32(l.get("u_eye"), eye[0], eye[1], eye[2]);
                        gl.uniform_1_f32(l.get("u_light_intensity"), LIGHT_INTENSITY);
                        gl.uniform_1_f32(l.get("u_ambient"), AMBIENT);
                        gl.uniform_1_f32(l.get("u_aspect"), aspect);
                        gl.uniform_1_f32(l.get("u_lum"), LIGHT_INTENSITY * 0.5 + AMBIENT);
                    }
                    self.set_common(gl, pr, i, m);
                    let l = &pr.locs;
                    if pass == 1 {
                        let mt = m.mtoon.as_ref().expect("輪郭線は MToon のみ");
                        gl.enable(glow::CULL_FACE);
                        gl.cull_face(glow::FRONT);
                        gl.disable(glow::BLEND);
                        gl.depth_mask(true);
                        gl.uniform_1_f32(l.get("u_width"), mt.outline_width * self.outline_scale);
                        gl.uniform_1_f32(
                            l.get("u_mode"),
                            (mt.outline_mode == OutlineMode::Screen) as u32 as f32,
                        );
                        gl.uniform_3_f32_slice(l.get("u_outline_color"), &mt.outline_color);
                        gl.uniform_1_f32(l.get("u_outline_mix"), mt.outline_lighting_mix);
                        let wt = mt
                            .outline_width_texture
                            .and_then(|t| self.linear_textures.get(&t).copied());
                        gl.active_texture(glow::TEXTURE2);
                        gl.bind_texture(glow::TEXTURE_2D, wt);
                        gl.uniform_1_i32(l.get("u_width_tex"), 2);
                        gl.uniform_1_f32(l.get("u_has_width_tex"), wt.is_some() as u32 as f32);
                        gl.active_texture(glow::TEXTURE0);
                    } else {
                        self.material_state(gl, m);
                        if let Some(mt) = mt {
                            gl.uniform_3_f32_slice(l.get("u_shade_color"), &mt.shade_color);
                            let st = mt.shade_texture.map(|t| self.textures[t]);
                            gl.active_texture(glow::TEXTURE1);
                            gl.bind_texture(glow::TEXTURE_2D, st);
                            gl.uniform_1_i32(l.get("u_shade_tex"), 1);
                            gl.uniform_1_f32(l.get("u_has_shade_tex"), st.is_some() as u32 as f32);
                            gl.active_texture(glow::TEXTURE0);
                            gl.uniform_1_f32(l.get("u_toony"), mt.shading_toony);
                            gl.uniform_1_f32(l.get("u_shift"), mt.shading_shift);
                            gl.uniform_3_f32_slice(l.get("u_rim_color"), &mt.rim_color);
                            gl.uniform_1_f32(l.get("u_rim_power"), mt.rim_fresnel_power);
                            gl.uniform_1_f32(l.get("u_rim_lift"), mt.rim_lift);
                            gl.uniform_1_f32(l.get("u_rim_mix"), mt.rim_lighting_mix);
                            gl.uniform_3_f32_slice(l.get("u_emissive"), &mt.emissive);
                        }
                    }
                    gl.bind_vertex_array(Some(self.prims[i].vao));
                    gl.draw_elements(
                        glow::TRIANGLES,
                        self.prims[i].index_count,
                        glow::UNSIGNED_INT,
                        0,
                    );
                }
            }
            gl.bind_vertex_array(None);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
            gl.cull_face(glow::BACK);
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
                gl.delete_buffer(p.skin_vbo);
                gl.delete_buffer(p.ebo);
            }
            for &t in &self.textures {
                gl.delete_texture(t);
            }
            for &t in self.linear_textures.values() {
                gl.delete_texture(t);
            }
            gl.delete_texture(self.white);
            gl.delete_buffer(self.joint_ubo);
            for p in [&self.lambert, &self.mtoon, &self.outline] {
                gl.delete_program(p.program);
            }
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
    srgb: bool,
) -> Result<glow::Texture, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            (if srgb {
                glow::SRGB8_ALPHA8
            } else {
                glow::RGBA8
            }) as i32,
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

/// 構図。`Full` は全身、`Waist` は腰から上、`Bust` は胸から上が描く先に収まる。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Full,
    Waist,
    Bust,
}

impl Framing {
    pub fn name(self) -> &'static str {
        match self {
            Framing::Full => "full",
            Framing::Waist => "waist",
            Framing::Bust => "bust",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        [Framing::Full, Framing::Waist, Framing::Bust]
            .into_iter()
            .find(|f| f.name() == s)
            .ok_or_else(|| format!("--frame は full / waist / bust: {s}"))
    }
}

/// キャラクターを描く画面上の領域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// 画面の左 1/3
    Left,
    /// 画面の中央 1/3
    Center,
    /// 画面全体
    Full,
}

impl Region {
    pub fn name(self) -> &'static str {
        match self {
            Region::Left => "left",
            Region::Center => "center",
            Region::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        [Region::Left, Region::Center, Region::Full]
            .into_iter()
            .find(|r| r.name() == s)
            .ok_or_else(|| format!("--region は left / center / full: {s}"))
    }

    /// 画面 `(w, h)` の中の領域 `(x, y, 幅, 高さ)`。GL の座標(左下が原点)。高さは画面いっぱい。
    pub fn rect(self, (w, h): (u32, u32)) -> (u32, u32, u32, u32) {
        let third = (w / 3).max(1);
        match self {
            Region::Left => (0, 0, third, h),
            Region::Center => ((w - third) / 2, 0, third, h),
            Region::Full => (0, 0, w, h),
        }
    }
}

/// 構図を決めるための、初期姿勢の体の位置(humanoid の骨から。なければ外接箱の割合で代用)。
#[derive(Clone, Copy, Debug)]
struct Anchors {
    /// 頭のてっぺん(外接箱の上端)
    top: f32,
    hips_y: f32,
    chest_y: f32,
    /// 体の中心(x, z)
    center_x: f32,
    center_z: f32,
    /// 奥行き(外接箱)
    depth: f32,
    /// 肩幅の半分
    shoulder_half: f32,
    /// 腕の長さ(上腕の付け根から手まで)
    arm_reach: f32,
}

/// 構図の余白など(目で見て決めた値)
const FRAMING_FOVY_DEG: f32 = 30.0;
/// 頭のてっぺんの上に空ける余白(収めたい高さに対する割合)
const FRAMING_TOP_MARGIN: f32 = 0.07;
/// バストアップの下端を胸の骨から下げる量(胸から頭のてっぺんまでの高さに対する割合)
const BUST_BELOW_CHEST: f32 = 0.10;
/// 腰から上の下端を腰の骨から下げる量(腰から頭のてっぺんまでの高さに対する割合)
const WAIST_BELOW_HIPS: f32 = 0.0;
/// 領域の半幅に最低限入れたい量 = 肩幅の半分 × この値 + 腕の長さ × `REACH_FRAC`
const SHOULDER_FIT: f32 = 0.9;
const REACH_FRAC: f32 = 0.0;

impl Anchors {
    fn new(model: &Model, world: &[math::Mat4]) -> Self {
        let (min, max) = model.bounds;
        let hb = &model.vrm.human_bones;
        let pos = |names: &[&str]| -> Option<[f32; 3]> {
            names.iter().find_map(|n| hb.get(*n)).and_then(|&i| {
                let m = world.get(i)?;
                Some([m[3][0], m[3][1], m[3][2]])
            })
        };
        let height = max[1] - min[1];
        let hips = pos(&["hips"]);
        let chest = pos(&["upperChest", "chest"]);
        let hips_y = hips.map_or(min[1] + height * 0.52, |p| p[1]);
        let chest_y = chest.map_or(min[1] + height * 0.72, |p| p[1]);
        let side = |s: &str| {
            Some((
                pos(&[&format!("{s}UpperArm")])?,
                pos(&[&format!("{s}Hand")])?,
            ))
        };
        let (shoulder_half, arm_reach) = match (side("left"), side("right")) {
            (Some((lu, lh)), Some((ru, _))) => (
                (lu[0] - ru[0]).abs() * 0.5,
                math::vec_len(math::vec_sub(lh, lu)),
            ),
            (Some((u, h)), None) | (None, Some((u, h))) => {
                (height * 0.12, math::vec_len(math::vec_sub(h, u)))
            }
            _ => (height * 0.12, height * 0.38),
        };
        Self {
            top: max[1],
            hips_y,
            chest_y,
            center_x: hips.map_or((min[0] + max[0]) * 0.5, |p| p[0]),
            center_z: (min[2] + max[2]) * 0.5,
            depth: max[2] - min[2],
            shoulder_half,
            arm_reach,
        }
    }

    /// 腰から上 / 胸から上が `size` の領域に収まるカメラ。幅が足りないときは、上端を保ったまま下へ広げる。
    fn camera(&self, framing: Framing, (w, h): (u32, u32), yaw: f32) -> (math::Mat4, [f32; 3]) {
        let (base, below) = match framing {
            Framing::Bust => (self.chest_y, BUST_BELOW_CHEST),
            _ => (self.hips_y, WAIST_BELOW_HIPS),
        };
        let span = (self.top - base).max(1e-3);
        let bottom = base - span * below;
        let visible_top = self.top + (self.top - bottom) * FRAMING_TOP_MARGIN;
        let aspect = w as f32 / h as f32;
        let need_half_w = self.shoulder_half * SHOULDER_FIT + self.arm_reach * REACH_FRAC;
        let half_h = ((visible_top - bottom) * 0.5)
            .max(need_half_w / aspect)
            .max(1e-3);
        let cy = visible_top - half_h;
        let t = (FRAMING_FOVY_DEG.to_radians() * 0.5).tan();
        let dist = half_h / t + self.depth * 0.5;
        look_camera(
            [self.center_x, cy, self.center_z],
            dist,
            aspect,
            yaw,
            FRAMING_FOVY_DEG.to_radians(),
        )
    }
}

/// `c` を見るカメラ(+Z 側から -Z を向く)。`yaw` は `c` のまわりの Y 軸回転。
fn look_camera(c: [f32; 3], dist: f32, aspect: f32, yaw: f32, fovy: f32) -> (math::Mat4, [f32; 3]) {
    let proj = math::perspective(fovy, aspect, dist * 0.1, dist * 10.0);
    let view = math::mul(
        &math::translate(0.0, 0.0, -dist),
        &math::mul(&math::rotate_y(yaw), &math::translate(-c[0], -c[1], -c[2])),
    );
    let back = math::rotate_y(-yaw);
    let eye = [
        c[0] + back[2][0] * dist,
        c[1] + back[2][1] * dist,
        c[2] + back[2][2] * dist,
    ];
    (math::mul(&proj, &view), eye)
}

/// 外接箱 `(min, max)` が収まるカメラの view-projection。縦横どちらにも収まる距離に置く
/// (横長の画面では高さで決まる)。`yaw` はモデルの中心のまわりの回転(0 で +Z 側から正面)。
pub fn view_proj(bounds: ([f32; 3], [f32; 3]), size: (u32, u32), yaw: f32) -> math::Mat4 {
    camera(bounds, size, yaw).0
}

/// `view_proj` と、カメラの位置(モデル空間)。
pub fn camera(
    (min, max): ([f32; 3], [f32; 3]),
    (w, h): (u32, u32),
    yaw: f32,
) -> (math::Mat4, [f32; 3]) {
    let c = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let aspect = w as f32 / h as f32;
    let fovy = 30f32.to_radians();
    let t = (fovy * 0.5).tan();
    // 全身(縦)も横幅も入る距離。+Z 側から -Z を向く(VRM 1.0 は +Z が前)。
    // 回すと横方向の見かけの幅が奥行きぶん増えるので、幅は水平の対角で見積もる
    let half_w = (size[0] * size[0] + size[2] * size[2]).sqrt() * 0.5;
    let dist = ((size[1] * 0.5 / t).max(half_w / (t * aspect)) * 1.15) + size[2] * 0.5;
    look_camera(c, dist, aspect, yaw, fovy)
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

#[cfg(test)]
mod camera_tests {
    use super::*;

    /// 外接箱の 8 隅を投影して、NDC の x/y の範囲(-1..1)に収まるかを返す。
    fn fits(bounds: ([f32; 3], [f32; 3]), size: (u32, u32), yaw: f32) -> bool {
        let vp = view_proj(bounds, size, yaw);
        let (min, max) = bounds;
        (0..8).all(|i| {
            let p = [
                [min[0], max[0]][i & 1],
                [min[1], max[1]][(i >> 1) & 1],
                [min[2], max[2]][(i >> 2) & 1],
            ];
            let v = [p[0], p[1], p[2], 1.0];
            let c: Vec<f32> = (0..4)
                .map(|r| (0..4).map(|k| vp[k][r] * v[k]).sum())
                .collect();
            c[3] > 0.0 && (c[0] / c[3]).abs() <= 1.0 && (c[1] / c[3]).abs() <= 1.0
        })
    }

    #[test]
    fn bust_and_waist_frame_head_and_chest_in_left_region() {
        // 身長 1.5m、頭のてっぺん 1.5m、胸 1.15m、腰 0.8m、肩幅 0.3m
        let a = Anchors {
            top: 1.5,
            hips_y: 0.8,
            chest_y: 1.15,
            center_x: 0.0,
            center_z: 0.0,
            depth: 0.3,
            shoulder_half: 0.15,
            arm_reach: 0.55,
        };
        let size = Region::Left.rect((2560, 1080));
        assert_eq!(size, (0, 0, 853, 1080));
        let ndc = |f: Framing, p: [f32; 3]| {
            let vp = a.camera(f, (size.2, size.3), 0.0).0;
            let c: Vec<f32> = (0..4)
                .map(|r| (0..4).map(|k| vp[k][r] * [p[0], p[1], p[2], 1.0][k]).sum())
                .collect();
            [c[0] / c[3], c[1] / c[3]]
        };
        // 頭のてっぺん(上に余白)、肩、バストの下端が領域に入る。腰より下は入らない
        let top = ndc(Framing::Bust, [0.0, 1.5, 0.0]);
        assert!(top[1] < 1.0 && top[1] > 0.6, "{top:?}");
        for x in [-0.15, 0.15] {
            assert!(ndc(Framing::Bust, [x, 1.3, 0.0])[0].abs() < 1.0);
        }
        assert!(ndc(Framing::Bust, [0.0, 1.15, 0.0])[1] > -1.0);
        assert!(ndc(Framing::Bust, [0.0, 0.8, 0.0])[1] < -1.0);
        assert!(ndc(Framing::Waist, [0.0, 0.85, 0.0])[1] > -1.0);
        assert!(ndc(Framing::Waist, [0.0, 0.5, 0.0])[1] < -1.0);
    }

    #[test]
    fn standing_model_fits_ultrawide_screen_by_height() {
        // 身長 1.6m、肩幅 0.5m、奥行き 0.3m の全身
        let b = ([-0.25, 0.0, -0.15], [0.25, 1.6, 0.15]);
        for yaw in [-20f32, 0.0, 20.0] {
            assert!(fits(b, (2560, 1080), yaw.to_radians()), "yaw {yaw}");
            assert!(fits(b, (1280, 720), yaw.to_radians()), "yaw {yaw}");
        }
    }
}
