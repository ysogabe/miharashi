//! PoC-005 PR4 B1: MaleCNS 全体級の合成コネクトームを GPU で描くベンチマーク。
//! - ニューロン(既定 16 万)は点、結合(既定 300 万本)は線で、どちらも加算合成で描く。
//! - 発火の減衰と伝播は compute shader で毎フレーム更新する。CPU は刺激するニューロンの番号を渡すだけ。
//! - 実データの読み込みはしない。乱数の種を固定した合成データで、規模だけを合わせる。
use crate::photo::program as link;
use glow::HasContext;
use std::time::{Duration, Instant};

/// `GL_TIME_ELAPSED_EXT`(EXT_disjoint_timer_query)
const TIME_ELAPSED: u32 = 0x88BF;
/// `GL_GPU_DISJOINT_EXT`: 計測の途中で GPU の時計が飛んだら 1(その回の値は捨てる)
const GPU_DISJOINT: u32 = 0x8FBB;
/// compute shader の 1 グループのスレッド数
const GROUP: u32 = 256;
/// 1 フレームに刺激できるニューロンの数の上限(シェーダーの配列の大きさ)
pub const MAX_STIM: usize = 16;

/// 合成したグラフ。結合は受け手(dst)の順に並べ、`offsets[i]..offsets[i + 1]` が i への結合。
pub struct Graph {
    pub pos: Vec<[f32; 3]>,
    pub offsets: Vec<u32>,
    /// (送り手, 受け手)。そのまま線の頂点番号の列としても使う
    pub edges: Vec<[u32; 2]>,
}

/// 再現できる乱数(xorshift64*)。
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// [0, 1)
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// [0, n)
    pub fn below(&mut self, n: u32) -> u32 {
        (((self.next_u64() >> 32) * n as u64) >> 32) as u32
    }
    /// 標準正規分布(Box–Muller)
    fn gauss(&mut self) -> f32 {
        let u = self.f32().max(1e-7);
        let v = self.f32();
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

/// 脳らしい形の合成グラフを作る。左右対称に置いた神経の塊(ニューロピル)にニューロンを散らし、
/// 結合は 85% を同じ塊の中、10% を反対側の対になる塊、5% をどこかのニューロンから引く。
pub fn synth(neurons: u32, synapses: u32, seed: u64) -> Graph {
    const PAIRS: usize = 24;
    let mut rng = Rng::new(seed);
    // 塊の中心(右半球に置き、左は x を反転)と広がり、大きさの重み
    let mut centers = Vec::with_capacity(PAIRS * 2);
    let mut sigma = Vec::with_capacity(PAIRS * 2);
    let mut weight = Vec::with_capacity(PAIRS * 2);
    for _ in 0..PAIRS {
        let c = [
            0.15 + rng.f32() * 1.25,
            (rng.f32() - 0.5) * 1.2,
            (rng.f32() - 0.5) * 0.8,
        ];
        let s = 0.06 + rng.f32() * 0.14;
        let w = 0.3 + rng.f32();
        for sign in [1.0, -1.0] {
            centers.push([c[0] * sign, c[1], c[2]]);
            sigma.push(s);
            weight.push(w);
        }
    }
    // 塊ごとにニューロンを連続した番号で割り当てる(同じ塊から送り手を選ぶため)
    let total: f32 = weight.iter().sum();
    let mut start = Vec::with_capacity(centers.len() + 1);
    let mut acc = 0.0;
    for w in &weight {
        start.push((acc / total * neurons as f32) as u32);
        acc += w;
    }
    start.push(neurons);
    let mut pos = Vec::with_capacity(neurons as usize);
    let mut cluster = Vec::with_capacity(neurons as usize);
    for c in 0..centers.len() {
        for _ in start[c]..start[c + 1] {
            let s = sigma[c];
            pos.push([
                centers[c][0] + rng.gauss() * s,
                centers[c][1] + rng.gauss() * s,
                centers[c][2] + rng.gauss() * s,
            ]);
            cluster.push(c as u32);
        }
    }
    // 受け手ごとの本数は平均の 0.25〜1.75 倍でばらつかせ、合計を synapses に合わせる
    let w: Vec<f32> = (0..neurons).map(|_| 0.25 + rng.f32() * 1.5).collect();
    let wsum: f64 = w.iter().map(|&x| x as f64).sum();
    let mut deg: Vec<u32> = w
        .iter()
        .map(|&x| (synapses as f64 * x as f64 / wsum) as u32)
        .collect();
    let mut short = synapses - deg.iter().sum::<u32>();
    while short > 0 {
        deg[rng.below(neurons) as usize] += 1;
        short -= 1;
    }
    let mut offsets = Vec::with_capacity(neurons as usize + 1);
    let mut edges = Vec::with_capacity(synapses as usize);
    for dst in 0..neurons {
        offsets.push(edges.len() as u32);
        let c = cluster[dst as usize] as usize;
        // 対になる塊は隣の番号(右、左の順に並べた)
        let mirror = c ^ 1;
        for _ in 0..deg[dst as usize] {
            let r = rng.f32();
            let src = if r < 0.85 {
                start[c] + rng.below(start[c + 1] - start[c])
            } else if r < 0.95 {
                start[mirror] + rng.below(start[mirror + 1] - start[mirror])
            } else {
                rng.below(neurons)
            };
            edges.push([src, dst]);
        }
    }
    offsets.push(edges.len() as u32);
    Graph {
        pos,
        offsets,
        edges,
    }
}

// 1 フレーム分の発火の更新。状態は (膜の値 v, 不応期 r)。
// 入力 = gain × Σ 送り手の v × (1 − r) を確率として発火する(v = 1、r = 1)。発火しなければ v と r は減衰する。
// 確率にすると、1 回の発火が次に何個を発火させるか(分岐比)を gain で調整できる。
// collect = 1 のときは、送り手が光っている(v > thr)結合を act に書き出す(描く線の一覧)。
const COMP_STEP: &str = r"#version 310 es
layout(local_size_x = 256) in;
layout(std430, binding = 0) readonly buffer Off { uint off[]; };
layout(std430, binding = 1) readonly buffer Src { uint src[]; };
layout(std430, binding = 2) readonly buffer In { vec2 s_in[]; };
layout(std430, binding = 3) writeonly buffer Out { vec2 s_out[]; };
layout(std430, binding = 4) buffer Count { uint n_act; };
layout(std430, binding = 5) writeonly buffer Act { uvec2 act[]; };
uniform uint n;
uniform uint stim[16];
uniform uint nstim;
uniform float gain;
uniform uint frame;
uniform uint collect;
uniform uint cap;
uniform float thr;
// 番号とフレームから作る [0, 1) の乱数(PCG のハッシュ)
float rand01(uint x) {
    x = x * 747796405u + 2891336453u;
    x = ((x >> ((x >> 28u) + 4u)) ^ x) * 277803737u;
    x = (x >> 22u) ^ x;
    return float(x >> 8u) / 16777216.0;
}
void main() {
    uint i = gl_GlobalInvocationID.x;
    if (i >= n) return;
    vec2 s = s_in[i];
    float sum = 0.0;
    uint b = off[i + 1u];
    for (uint e = off[i]; e < b; e++) {
        uint j = src[e];
        float v = s_in[j].x;
        sum += v;
        if (collect != 0u && v > thr) {
            uint k = atomicAdd(n_act, 1u);
            if (k < cap) act[k] = uvec2(j, i);
        }
    }
    bool fire = rand01(i ^ (frame * 2654435761u)) < gain * sum * (1.0 - s.y);
    for (uint k = 0u; k < nstim; k++) fire = fire || stim[k] == i;
    s_out[i] = fire ? vec2(1.0, 1.0) : vec2(s.x * 0.80, s.y * 0.95);
}
";

// 集めた線の数を、間接描画の命令(DrawElementsIndirectCommand)に書き、数を 0 に戻す。
const COMP_FINISH: &str = r"#version 310 es
layout(local_size_x = 1) in;
layout(std430, binding = 4) buffer Count { uint n_act; };
layout(std430, binding = 6) writeonly buffer Cmd { uint cmd[5]; };
uniform uint cap;
void main() {
    cmd[0] = min(n_act, cap) * 2u;
    cmd[1] = 1u;
    cmd[2] = 0u;
    cmd[3] = 0u;
    cmd[4] = 0u;
    n_act = 0u;
}
";

// 線: 頂点はニューロンの番号(結合の配列をそのまま添字の列に使う)。発火した側が明るい。
const VERT_LINE: &str = r"#version 320 es
layout(location = 0) in vec3 pos;
layout(std430, binding = 2) readonly buffer S { vec2 s[]; };
uniform mat4 mvp;
uniform vec3 base;
uniform vec3 hot;
out vec3 col;
void main() {
    gl_Position = mvp * vec4(pos, 1.0);
    col = base + hot * s[gl_VertexID].x;
}
";

const VERT_POINT: &str = r"#version 320 es
layout(location = 0) in vec3 pos;
layout(std430, binding = 2) readonly buffer S { vec2 s[]; };
uniform mat4 mvp;
out vec3 col;
void main() {
    gl_Position = mvp * vec4(pos, 1.0);
    float a = s[gl_VertexID].x;
    gl_PointSize = 1.0 + 3.0 * a;
    col = vec3(0.006, 0.014, 0.026) + vec3(0.9, 0.6, 0.2) * a;
}
";

const FRAG: &str = r"#version 320 es
precision mediump float;
in vec3 col;
out vec4 o;
void main() { o = vec4(col, 1.0); }
";

/// 結合の線の描き方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lines {
    /// 線を描かない(点と伝播だけ)
    None,
    /// 全部の結合を毎フレーム描く(力ずく。上限を測る)
    All,
    /// 配線の 1/SAMPLE を薄く描き、送り手が光っている結合だけを明るく描く
    Active,
}

impl Lines {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "all" => Ok(Self::All),
            "active" => Ok(Self::Active),
            _ => Err(format!("--lines は none / all / active: {s}")),
        }
    }
}

/// `Lines::Active` で薄く描く配線の間引き(16 本に 1 本)
const SAMPLE: usize = 16;
/// `Lines::Active` で 1 フレームに描く光った結合の上限
const ACTIVE_CAP: u32 = 1 << 20;
/// 光っているとみなす v(発火から約 10 フレーム)
const ACTIVE_THR: f32 = 0.1;

/// GPU 上のコネクトーム。`advance` で 1 段進め、`draw` でいまの framebuffer に描く。
pub struct Scene {
    step: glow::Program,
    finish: glow::Program,
    line: glow::Program,
    point: glow::Program,
    /// 頂点(ニューロンの位置)は共通で、添字の列だけが違う: 全結合 / 間引いた配線 / 光った結合
    vao_all: glow::VertexArray,
    vao_sample: glow::VertexArray,
    vao_active: glow::VertexArray,
    vbo: glow::Buffer,
    off: glow::Buffer,
    src: glow::Buffer,
    edges: glow::Buffer,
    sample: glow::Buffer,
    active: glow::Buffer,
    count: glow::Buffer,
    cmd: glow::Buffer,
    state: [glow::Buffer; 2],
    /// いまの状態が入っている方(state[cur])
    cur: usize,
    n: u32,
    e: u32,
    n_sample: u32,
    /// 進めた段数(乱数の種)
    steps: u32,
    /// 発火の確率の係数。1 回の発火から次に発火する数の期待値は、おおよそ
    /// 入ってくる結合の数(平均 19)× 発火後の v の合計(約 5)× gain
    pub gain: f32,
    pub lines: Lines,
}

/// 既定の gain。ニューロンの約 1.6% が発火している状態で落ち着く(合成データ、16 万 / 300 万本)
pub const GAIN: f32 = 0.012;

impl Scene {
    pub fn new(gl: &glow::Context, g: &Graph) -> Result<Self, String> {
        unsafe {
            if gl.get_parameter_i32(glow::MAX_VERTEX_SHADER_STORAGE_BLOCKS) < 1 {
                return Err("頂点シェーダーで SSBO が使えない".into());
            }
            let step = compute(gl, COMP_STEP)?;
            let finish = compute(gl, COMP_FINISH)?;
            let line = link(gl, VERT_LINE, FRAG)?;
            let point = link(gl, VERT_POINT, FRAG)?;
            let buf = |target: u32, data: &[u8], usage: u32| -> Result<glow::Buffer, String> {
                let b = gl.create_buffer()?;
                gl.bind_buffer(target, Some(b));
                gl.buffer_data_u8_slice(target, data, usage);
                Ok(b)
            };
            let n = g.pos.len() as u32;
            let vbo = buf(glow::ARRAY_BUFFER, bytes(&g.pos), glow::STATIC_DRAW)?;
            let sampled: Vec<[u32; 2]> = g.edges.iter().step_by(SAMPLE).copied().collect();
            let vao_for = |data: &[u8], usage| -> Result<_, String> {
                let vao = gl.create_vertex_array()?;
                gl.bind_vertex_array(Some(vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);
                let ebo = buf(glow::ELEMENT_ARRAY_BUFFER, data, usage)?;
                gl.bind_vertex_array(None);
                Ok((vao, ebo))
            };
            let (vao_all, edges) = vao_for(bytes(&g.edges), glow::STATIC_DRAW)?;
            let (vao_sample, sample) = vao_for(bytes(&sampled), glow::STATIC_DRAW)?;
            // 光った結合の一覧は compute shader が書き、そのまま添字の列として描く
            let (vao_active, active) =
                vao_for(&vec![0u8; ACTIVE_CAP as usize * 8], glow::DYNAMIC_COPY)?;
            let ssbo = |data: &[u8]| buf(glow::SHADER_STORAGE_BUFFER, data, glow::DYNAMIC_COPY);
            let src: Vec<u32> = g.edges.iter().map(|e| e[0]).collect();
            let zero = vec![[0f32; 2]; n as usize];
            Ok(Self {
                step,
                finish,
                line,
                point,
                vao_all,
                vao_sample,
                vao_active,
                vbo,
                off: ssbo(bytes(&g.offsets))?,
                src: ssbo(bytes(&src))?,
                edges,
                sample,
                active,
                count: ssbo(&[0u8; 4])?,
                cmd: ssbo(&[0u8; 20])?,
                state: [ssbo(bytes(&zero))?, ssbo(bytes(&zero))?],
                cur: 0,
                n,
                e: g.edges.len() as u32,
                n_sample: sampled.len() as u32,
                steps: 0,
                gain: GAIN,
                lines: Lines::Active,
            })
        }
    }

    /// 発火を 1 段進める(`stim` のニューロンを発火させる)。
    pub fn advance(&mut self, gl: &glow::Context, stim: &[u32]) {
        let stim = &stim[..stim.len().min(MAX_STIM)];
        let mut padded = [0u32; MAX_STIM];
        padded[..stim.len()].copy_from_slice(stim);
        let (from, to) = (self.state[self.cur], self.state[1 - self.cur]);
        let collect = self.lines == Lines::Active;
        unsafe {
            gl.use_program(Some(self.step));
            let u = |name| gl.get_uniform_location(self.step, name);
            gl.uniform_1_u32(u("n").as_ref(), self.n);
            gl.uniform_1_u32_slice(u("stim").as_ref(), &padded);
            gl.uniform_1_u32(u("nstim").as_ref(), stim.len() as u32);
            gl.uniform_1_f32(u("gain").as_ref(), self.gain);
            gl.uniform_1_u32(u("frame").as_ref(), self.steps);
            gl.uniform_1_u32(u("collect").as_ref(), collect as u32);
            gl.uniform_1_u32(u("cap").as_ref(), ACTIVE_CAP);
            gl.uniform_1_f32(u("thr").as_ref(), ACTIVE_THR);
            for (i, b) in [self.off, self.src, from, to, self.count, self.active]
                .into_iter()
                .enumerate()
            {
                gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, i as u32, Some(b));
            }
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 6, Some(self.cmd));
            gl.dispatch_compute(self.n.div_ceil(GROUP), 1, 1);
            // 次の段の compute と、このあとの描画の頂点シェーダーが読む
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            if collect {
                gl.use_program(Some(self.finish));
                gl.uniform_1_u32(
                    gl.get_uniform_location(self.finish, "cap").as_ref(),
                    ACTIVE_CAP,
                );
                gl.dispatch_compute(1, 1, 1);
                gl.memory_barrier(
                    glow::SHADER_STORAGE_BARRIER_BIT
                        | glow::COMMAND_BARRIER_BIT
                        | glow::ELEMENT_ARRAY_BARRIER_BIT,
                );
            }
        }
        self.cur = 1 - self.cur;
        self.steps = self.steps.wrapping_add(1);
    }

    /// いまの状態を、いま bind している framebuffer に描く。`t` [秒] でカメラを回す。
    pub fn draw(&self, gl: &glow::Context, (w, h): (u32, u32), t: f32) {
        let mvp = camera(t, w as f32 / h as f32);
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(self.state[self.cur]));
            gl.use_program(Some(self.line));
            let u = |name| gl.get_uniform_location(self.line, name);
            gl.uniform_matrix_4_f32_slice(u("mvp").as_ref(), false, &mvp);
            let colors = |base: [f32; 3], hot: [f32; 3]| {
                gl.uniform_3_f32_slice(u("base").as_ref(), &base);
                gl.uniform_3_f32_slice(u("hot").as_ref(), &hot);
            };
            match self.lines {
                Lines::None => {}
                Lines::All => {
                    colors([0.0015, 0.004, 0.008], [0.10, 0.07, 0.02]);
                    gl.bind_vertex_array(Some(self.vao_all));
                    gl.draw_elements(glow::LINES, (self.e * 2) as i32, glow::UNSIGNED_INT, 0);
                }
                Lines::Active => {
                    colors([0.0015, 0.004, 0.008], [0.0; 3]);
                    gl.bind_vertex_array(Some(self.vao_sample));
                    gl.draw_elements(
                        glow::LINES,
                        (self.n_sample * 2) as i32,
                        glow::UNSIGNED_INT,
                        0,
                    );
                    colors([0.0; 3], [0.12, 0.08, 0.025]);
                    gl.bind_vertex_array(Some(self.vao_active));
                    gl.bind_buffer(glow::DRAW_INDIRECT_BUFFER, Some(self.cmd));
                    gl.draw_elements_indirect_offset(glow::LINES, glow::UNSIGNED_INT, 0);
                    gl.bind_buffer(glow::DRAW_INDIRECT_BUFFER, None);
                }
            }
            gl.use_program(Some(self.point));
            gl.uniform_matrix_4_f32_slice(
                gl.get_uniform_location(self.point, "mvp").as_ref(),
                false,
                &mvp,
            );
            gl.bind_vertex_array(Some(self.vao_all));
            gl.draw_arrays(glow::POINTS, 0, self.n as i32);
            gl.bind_vertex_array(None);
            gl.disable(glow::BLEND);
        }
    }

    /// 発火中(v > 0.5)のニューロンの割合。GPU から読み戻すので計測の外で使う。
    pub fn active_fraction(&self, gl: &glow::Context) -> f32 {
        let len = self.n as i32 * 8;
        unsafe {
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(self.state[self.cur]));
            let p = gl.map_buffer_range(glow::SHADER_STORAGE_BUFFER, 0, len, glow::MAP_READ_BIT);
            if p.is_null() {
                return f32::NAN;
            }
            let s = std::slice::from_raw_parts(p as *const [f32; 2], self.n as usize);
            let on = s.iter().filter(|v| v[0] > 0.5).count();
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            on as f32 / self.n as f32
        }
    }

    /// 直前の段で集めた光った結合の数(`Lines::Active` のとき)。GPU から読み戻すので計測の外で使う。
    pub fn active_lines(&self, gl: &glow::Context) -> u32 {
        unsafe {
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(self.cmd));
            let p = gl.map_buffer_range(glow::SHADER_STORAGE_BUFFER, 0, 4, glow::MAP_READ_BIT);
            if p.is_null() {
                return 0;
            }
            let v = *(p as *const u32) / 2;
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            v
        }
    }

    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            for p in [self.step, self.finish, self.line, self.point] {
                gl.delete_program(p);
            }
            for b in [
                self.vbo,
                self.off,
                self.src,
                self.edges,
                self.sample,
                self.active,
                self.count,
                self.cmd,
                self.state[0],
                self.state[1],
            ] {
                gl.delete_buffer(b);
            }
            for v in [self.vao_all, self.vao_sample, self.vao_active] {
                gl.delete_vertex_array(v);
            }
        }
    }
}

fn compute(gl: &glow::Context, src: &str) -> Result<glow::Program, String> {
    unsafe {
        let p = gl.create_program()?;
        let s = gl.create_shader(glow::COMPUTE_SHADER)?;
        gl.shader_source(s, src);
        gl.compile_shader(s);
        if !gl.get_shader_compile_status(s) {
            return Err(format!("compute shader: {}", gl.get_shader_info_log(s)));
        }
        gl.attach_shader(p, s);
        gl.link_program(p);
        gl.delete_shader(s);
        if !gl.get_program_link_status(p) {
            return Err(format!("link: {}", gl.get_program_info_log(p)));
        }
        Ok(p)
    }
}

fn bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: f32 と u32 の配列だけを渡す(詰め物のない型)
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// ゆっくり回るカメラ(列優先の 4×4)。y 軸の回りに 0.15 rad/秒、少し上から見下ろす。
pub fn camera(t: f32, aspect: f32) -> [f32; 16] {
    let (sy, cy) = (t * 0.15).sin_cos();
    let (sx, cx) = 0.35f32.sin_cos();
    // view = 平行移動(0, 0, -dist) × 回転 x × 回転 y
    let dist = 3.0;
    let r = [
        [cy, sx * sy, -cx * sy],
        [0.0, cx, sx],
        [sy, -sx * cy, cx * cy],
    ];
    // 列優先: m[col * 4 + row]
    let mut view = [0f32; 16];
    for (col, rc) in r.iter().enumerate() {
        view[col * 4..col * 4 + 3].copy_from_slice(rc);
    }
    view[14] = -dist;
    view[15] = 1.0;
    let f = 1.0 / (0.45f32).tan();
    let (near, far) = (0.1, 20.0);
    let mut proj = [0f32; 16];
    proj[0] = f / aspect;
    proj[5] = f;
    proj[10] = (far + near) / (near - far);
    proj[11] = -1.0;
    proj[14] = 2.0 * far * near / (near - far);
    mul(&proj, &view)
}

fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut m = [0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            m[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    m
}

/// 1 フレームの計測値。
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTimes {
    /// GPU の時間 [ms](伝播、描画)。時計が飛んだ回や結果が出ていない回は None
    pub gpu_step: Option<f32>,
    pub gpu_draw: Option<f32>,
    /// 命令を出すのにかかった CPU 側の時間 [ms](待ちを除く)
    pub submit: f32,
    /// 2 フレーム前の完了を待った時間 [ms]
    pub wait: f32,
}

/// GPU の時間を 2 区間(伝播、描画)ずつ測る。結果は 2 フレーム後に読む。
pub struct Timer {
    queries: Vec<[glow::Query; 2]>,
    fences: Vec<Option<glow::Fence>>,
    frame: usize,
}

const RING: usize = 4;
/// 何フレーム前の完了を待つか
const LAG: usize = 2;

impl Timer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        if !gl
            .supported_extensions()
            .contains("GL_EXT_disjoint_timer_query")
        {
            return Err("GL_EXT_disjoint_timer_query が無い".into());
        }
        let mut queries = Vec::with_capacity(RING);
        for _ in 0..RING {
            unsafe { queries.push([gl.create_query()?, gl.create_query()?]) };
        }
        Ok(Self {
            queries,
            fences: vec![None; RING],
            frame: 0,
        })
    }

    pub fn begin(&self, gl: &glow::Context, pass: usize) {
        unsafe { gl.begin_query(TIME_ELAPSED, self.queries[self.frame % RING][pass]) };
    }

    pub fn end(&self, gl: &glow::Context) {
        unsafe { gl.end_query(TIME_ELAPSED) };
    }

    /// このフレームの命令を出し終えたら呼ぶ。LAG フレーム前の完了を待ち、その GPU 時間を返す。
    /// 戻り値: (伝播, 描画, 待った時間)
    pub fn finish(&mut self, gl: &glow::Context) -> (Option<f32>, Option<f32>, Duration) {
        let slot = self.frame % RING;
        unsafe {
            if let Ok(f) = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                self.fences[slot] = Some(f);
            }
            gl.flush();
        }
        self.frame += 1;
        if self.frame <= LAG {
            return (None, None, Duration::ZERO);
        }
        let old = (self.frame - 1 - LAG) % RING;
        let t0 = Instant::now();
        if let Some(f) = self.fences[old].take() {
            unsafe {
                gl.client_wait_sync(f, glow::SYNC_FLUSH_COMMANDS_BIT, 1_000_000_000);
                gl.delete_sync(f);
            }
        }
        let waited = t0.elapsed();
        let read = |q: glow::Query| unsafe {
            (gl.get_query_parameter_u32(q, glow::QUERY_RESULT_AVAILABLE) != 0)
                .then(|| gl.get_query_parameter_u64(q, glow::QUERY_RESULT) as f32 / 1e6)
        };
        let [qs, qd] = self.queries[old];
        let (s, d) = (read(qs), read(qd));
        // 時計が飛んでいたら、読んだ区間の値は信用しない
        if unsafe { gl.get_parameter_i32(GPU_DISJOINT) } != 0 {
            return (None, None, waited);
        }
        (s, d, waited)
    }

    pub fn destroy(self, gl: &glow::Context) {
        unsafe {
            for q in self.queries.into_iter().flatten() {
                gl.delete_query(q);
            }
            for f in self.fences.into_iter().flatten() {
                gl.delete_sync(f);
            }
        }
    }
}

/// ベンチマークの条件。
pub struct RunOpts {
    pub size: (u32, u32),
    pub duration: Duration,
    /// 1 秒あたりのフレーム数。0 なら待たずに描き続ける(上限を測る)
    pub fps: u32,
    /// 1 フレームに刺激するニューロンの数(最大 MAX_STIM)
    pub stim: usize,
    pub seed: u64,
}

/// `scene` を `opts` の条件で動かし、フレームごとに `each(番号, 開始からの時間, 計測値)` を呼ぶ。
/// GPU の時間は LAG フレーム遅れて届く。いま bind している framebuffer に描く。
pub fn run(
    gl: &glow::Context,
    scene: &mut Scene,
    opts: &RunOpts,
    mut each: impl FnMut(u64, Duration, &FrameTimes),
) -> Result<(), String> {
    let mut timer = Timer::new(gl)?;
    let mut rng = Rng::new(opts.seed ^ 0x5157);
    let period = (opts.fps > 0).then(|| Duration::from_secs_f64(1.0 / opts.fps as f64));
    let start = Instant::now();
    let mut next = start;
    let mut i = 0u64;
    let mut stim = Vec::with_capacity(MAX_STIM);
    while start.elapsed() < opts.duration {
        let t0 = Instant::now();
        stim.clear();
        stim.extend((0..opts.stim.min(MAX_STIM)).map(|_| rng.below(scene.n)));
        timer.begin(gl, 0);
        scene.advance(gl, &stim);
        timer.end(gl);
        timer.begin(gl, 1);
        let t = (t0 - start).as_secs_f32();
        scene.draw(gl, opts.size, t);
        timer.end(gl);
        let (gpu_step, gpu_draw, wait) = timer.finish(gl);
        let times = FrameTimes {
            gpu_step,
            gpu_draw,
            submit: (t0.elapsed() - wait).as_secs_f32() * 1000.0,
            wait: wait.as_secs_f32() * 1000.0,
        };
        each(i, start.elapsed(), &times);
        i += 1;
        if let Some(p) = period {
            next += p;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                // 間に合わなかったら、遅れを取り戻そうとせずにそこから数え直す
                next = now;
            }
        }
    }
    unsafe { gl.finish() };
    timer.destroy(gl);
    Ok(())
}

/// GPU の名前と、ベンチマークに使う機能が使えるか(ログ用)。
pub fn caps(gl: &glow::Context) -> String {
    unsafe {
        format!(
            "{} / {} / timer_query={} / vertex_ssbo={} / max_ssbo={}MB",
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_string(glow::RENDERER),
            gl.supported_extensions()
                .contains("GL_EXT_disjoint_timer_query"),
            gl.get_parameter_i32(glow::MAX_VERTEX_SHADER_STORAGE_BLOCKS),
            gl.get_parameter_i64(glow::MAX_SHADER_STORAGE_BLOCK_SIZE) >> 20,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_has_requested_size_and_sorted_edges() {
        let g = synth(5_000, 90_000, 7);
        assert_eq!(g.pos.len(), 5_000);
        assert_eq!(g.edges.len(), 90_000);
        assert_eq!(g.offsets.len(), 5_001);
        assert_eq!(*g.offsets.last().unwrap(), 90_000);
        for i in 0..5_000usize {
            for e in g.offsets[i]..g.offsets[i + 1] {
                let [src, dst] = g.edges[e as usize];
                assert_eq!(dst as usize, i, "受け手の順に並ぶ");
                assert!(src < 5_000);
            }
        }
        // 同じ種なら同じグラフ
        let h = synth(5_000, 90_000, 7);
        assert_eq!(g.edges, h.edges);
    }

    /// render node と Mesa がある環境だけで動く。ない環境(CI など)では skip する。
    #[test]
    fn offscreen_stimulus_spreads_and_draws() {
        let off = match crate::Offscreen::open(std::path::Path::new("/dev/dri/renderD128")) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                return;
            }
        };
        let g = synth(20_000, 400_000, 3);
        let size = (320, 160);
        let ((first, later), img) = off
            .with_fbo(size, |gl| {
                let mut s = Scene::new(gl, &g).unwrap();
                // 分岐比を 1 より大きくして、広がる仕組みだけを確かめる
                s.gain = 0.05;
                s.advance(
                    gl,
                    &(0..MAX_STIM as u32).map(|k| k * 997).collect::<Vec<_>>(),
                );
                let first = s.active_fraction(gl);
                for _ in 0..6 {
                    s.advance(gl, &[]);
                }
                let later = s.active_fraction(gl);
                s.draw(gl, size, 0.0);
                unsafe { gl.finish() };
                s.destroy(gl);
                (first, later)
            })
            .unwrap();
        assert_eq!(
            first,
            MAX_STIM as f32 / 20_000.0,
            "刺激したニューロンだけが発火"
        );
        assert!(later > first * 4.0, "発火が広がる: {first} → {later}");
        let lit = img.as_chunks::<4>().0.iter().filter(|p| p[0] > 20).count();
        assert!(lit > 100, "発火したところが明るく描ける: {lit}");
        assert!(
            lit < (size.0 * size.1) as usize / 2,
            "全面が光ってはいない: {lit}"
        );
    }

    #[test]
    fn camera_keeps_brain_in_view() {
        let m = camera(0.0, 2560.0 / 1080.0);
        // 原点は画面の中央、奥行きは手前と奥の間
        let clip = |p: [f32; 3]| {
            let v = [p[0], p[1], p[2], 1.0];
            let o: Vec<f32> = (0..4)
                .map(|r| (0..4).map(|c| m[c * 4 + r] * v[c]).sum())
                .collect();
            [o[0] / o[3], o[1] / o[3], o[2] / o[3]]
        };
        let c = clip([0.0, 0.0, 0.0]);
        assert!(
            c[0].abs() < 1e-4 && c[1].abs() < 0.2 && c[2].abs() < 1.0,
            "{c:?}"
        );
        // 塊を置く範囲の端(x = ±1.6)も画面に入る
        for x in [-1.6, 1.6] {
            let p = clip([x, 0.0, 0.0]);
            assert!(p[0].abs() < 1.0, "{p:?}");
        }
    }
}
