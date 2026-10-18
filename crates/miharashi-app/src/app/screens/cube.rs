//! キューブ画面(Issue #74、docs/DESIGN-screen-plugin-v2.md §11.1)。回る立方体 1 個だけを描く 3D の画面で、
//! `Composite::Gl` と `GlScene` の口を通して画面に出す。VRM に依存せず、面ごとに色をつけた立方体を深度テストつきで描く。
//! 全面(`cube`)と、画面中央の 1/3 幅の `Region` を単色の下地に重ねる形(`cube-third`)の 2 つ。既定は無効(`[cube]`)。
//! 回転は「表示中かつ点灯中に経った時間」だけで進める。非表示・消灯の間は止め、復帰でまとめて進めない。
use crate::screen::{
    Composite, GlComposite, GlScene, Region, RenderContext, Screen, ScreenId, Top,
};
use glow::HasContext;
use miharashi_glscene::{GlTarget, ObjectCount};
use miharashi_render::canvas::rgba;
use miharashi_render::notice;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 1/3 幅の画面の下地の色(`Region` が効いていることが見て分かる単色)。
pub const THIRD_BASE: [u8; 4] = [24, 48, 64, 255];

/// 1/3 幅の領域に敷く下地の色(領域の外の `THIRD_BASE` より明るく、`Region` の範囲が見て分かる)。
pub const THIRD_BAND: [u8; 4] = [44, 84, 108, 255];

/// 面の色。並びは +X, -X, +Y, -Y, +Z, -Z(描く順でもある。深度テストが無ければ最後の -Z が前に出てしまう)。
pub const FACE_COLORS: [[u8; 3]; 6] = [
    [230, 60, 60],
    [60, 200, 200],
    [60, 200, 60],
    [200, 60, 200],
    [60, 100, 230],
    [230, 200, 60],
];

/// 立方体の半分の辺の長さ。
const HALF: f32 = 0.8;
/// カメラから立方体の中心までの距離と縦の視野角。1/3 幅の領域(縦長)でも角が切れない大きさ。
const DISTANCE: f32 = 5.0;
const FOVY: f32 = 40.0 * std::f32::consts::PI / 180.0;

const VS: &str = "#version 300 es
layout(location = 0) in vec3 pos;
layout(location = 1) in vec3 col;
uniform mat4 mvp;
out vec3 v_col;
void main() {
    v_col = col;
    gl_Position = mvp * vec4(pos, 1.0);
}
";
const FS: &str = "#version 300 es
precision mediump float;
in vec3 v_col;
out vec4 o;
void main() { o = vec4(v_col, 1.0); }
";

type Mat4 = [f32; 16];

/// 列優先の行列の積 `a * b`。
fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

/// 縦長の領域(`aspect` < 1)では、視野角を幅に合わせて、立方体の角が左右で切れないようにする。
fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = aspect.min(1.0) / (fovy / 2.0).tan();
    let mut m = [0.0; 16];
    m[0] = f / aspect;
    m[5] = f;
    m[10] = (far + near) / (near - far);
    m[11] = -1.0;
    m[14] = 2.0 * far * near / (near - far);
    m
}

fn translate_z(z: f32) -> Mat4 {
    let mut m = IDENTITY;
    m[14] = z;
    m
}

fn rotate_x(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    let mut m = IDENTITY;
    (m[5], m[6], m[9], m[10]) = (c, s, -s, c);
    m
}

fn rotate_y(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    let mut m = IDENTITY;
    (m[0], m[2], m[8], m[10]) = (c, -s, s, c);
    m
}

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// `secs`(表示中かつ点灯中に経った時間)のときの変換行列。同じ `secs` と `aspect` なら同じ行列(決定的)。
fn mvp(secs: f32, aspect: f32) -> Mat4 {
    let yaw = secs * 0.8;
    let pitch = 0.5 + 0.25 * (secs * 0.5).sin();
    let model = mul(&rotate_x(pitch), &rotate_y(yaw));
    let view = mul(&translate_z(-DISTANCE), &model);
    mul(&perspective(FOVY, aspect, 1.0, 20.0), &view)
}

/// 頂点(位置 3 + 色 3)を 6 面 × 2 三角形 × 3 頂点で並べる。面の向きは外から見て反時計回り。
fn vertices() -> Vec<f32> {
    // (法線, u, v)。u × v = 法線
    const FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut out = Vec::with_capacity(36 * 6);
    for (i, (n, u, v)) in FACES.iter().enumerate() {
        let col = FACE_COLORS[i].map(|c| c as f32 / 255.0);
        let corner = |su: f32, sv: f32| -> [f32; 3] {
            std::array::from_fn(|k| (n[k] + su * u[k] + sv * v[k]) * HALF)
        };
        let q = [
            corner(-1.0, -1.0),
            corner(1.0, -1.0),
            corner(1.0, 1.0),
            corner(-1.0, 1.0),
        ];
        for idx in [0, 1, 2, 0, 2, 3] {
            out.extend_from_slice(&q[idx]);
            out.extend_from_slice(&col);
        }
    }
    out
}

/// シェーダー 2 つからプログラムを作る。作ったシェーダーはリンクのあとに消す。失敗したら作りかけを消して理由を返す。
/// 作成・削除は `count` で数える。
pub(crate) fn build_program(
    gl: &glow::Context,
    count: &ObjectCount,
    vs: &str,
    fs: &str,
) -> Result<glow::Program, String> {
    unsafe {
        let program = count.create_program(gl)?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let sh = match count.create_shader(gl, kind) {
                Ok(sh) => sh,
                Err(e) => {
                    shaders.into_iter().for_each(|s| count.delete_shader(gl, s));
                    count.delete_program(gl, program);
                    return Err(e);
                }
            };
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                let log = gl.get_shader_info_log(sh);
                count.delete_shader(gl, sh);
                shaders.into_iter().for_each(|s| count.delete_shader(gl, s));
                count.delete_program(gl, program);
                return Err(format!("シェーダー: {log}"));
            }
            gl.attach_shader(program, sh);
            shaders.push(sh);
        }
        gl.link_program(program);
        let linked = gl.get_program_link_status(program);
        let log = gl.get_program_info_log(program);
        for sh in shaders {
            gl.detach_shader(program, sh);
            count.delete_shader(gl, sh);
        }
        if !linked {
            count.delete_program(gl, program);
            return Err(format!("リンク: {log}"));
        }
        Ok(program)
    }
}

/// 回る立方体。GL オブジェクト(プログラム・VAO・頂点バッファ)は最初の `draw` で作り、`release` で消す。
pub struct CubeScene {
    count: Rc<ObjectCount>,
    program: Option<glow::Program>,
    mvp_loc: Option<glow::UniformLocation>,
    vao: Option<glow::VertexArray>,
    vbo: Option<glow::Buffer>,
    /// 頂点バッファへの転送と属性の設定が済んでいるか
    ready: bool,
    /// 回転を決める時間 [秒]
    secs: f32,
    /// 深度テストを使うか。false は試験用(深度が効いていることを確かめるための比較)
    depth: bool,
}

impl CubeScene {
    pub fn new(count: Rc<ObjectCount>) -> Self {
        Self {
            count,
            program: None,
            mvp_loc: None,
            vao: None,
            vbo: None,
            ready: false,
            secs: 0.0,
            depth: true,
        }
    }

    /// 次の `draw` の回転を決める時間 [秒]。
    pub fn set_secs(&mut self, secs: f32) {
        self.secs = secs;
    }

    /// 試験用: 深度テストを使わない立方体。
    #[cfg(test)]
    pub fn without_depth(count: Rc<ObjectCount>) -> Self {
        Self {
            depth: false,
            ..Self::new(count)
        }
    }

    /// 足りないオブジェクトを作る。作りかけで失敗しても、作ったものは持ったまま(`release` が消す)。
    fn ensure(&mut self, gl: &glow::Context) -> Result<(glow::Program, glow::VertexArray), String> {
        let program = match self.program {
            Some(p) => p,
            None => {
                let p = build_program(gl, &self.count, VS, FS)?;
                self.program = Some(p);
                self.mvp_loc = unsafe { gl.get_uniform_location(p, "mvp") };
                p
            }
        };
        let vao = match self.vao {
            Some(v) => v,
            None => *self.vao.insert(self.count.create_vertex_array(gl)?),
        };
        let vbo = match self.vbo {
            Some(b) => b,
            None => *self.vbo.insert(self.count.create_buffer(gl)?),
        };
        if !self.ready {
            let data = vertices();
            unsafe {
                gl.bind_vertex_array(Some(vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    data.iter()
                        .flat_map(|f| f.to_ne_bytes())
                        .collect::<Vec<u8>>()
                        .as_slice(),
                    glow::STATIC_DRAW,
                );
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 24, 0);
                gl.enable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, 24, 12);
                gl.bind_vertex_array(None);
                gl.bind_buffer(glow::ARRAY_BUFFER, None);
            }
            self.ready = true;
        }
        Ok((program, vao))
    }
}

impl GlScene for CubeScene {
    fn draw(&mut self, gl: &glow::Context, target: &GlTarget) -> Result<(), String> {
        let (program, vao) = self.ensure(gl)?;
        let (w, h) = target.size;
        let m = mvp(self.secs, w as f32 / h.max(1) as f32);
        unsafe {
            gl.use_program(Some(program));
            gl.uniform_matrix_4_f32_slice(self.mvp_loc.as_ref(), false, &m);
            if self.depth {
                gl.enable(glow::DEPTH_TEST);
            }
            gl.bind_vertex_array(Some(vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 36);
            gl.bind_vertex_array(None);
            gl.use_program(None);
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
        if let Some(b) = self.vbo.take() {
            self.count.delete_buffer(gl, b);
        }
        self.mvp_loc = None;
        self.ready = false;
    }
}

/// キューブの画面。`third` なら画面中央の 1/3 幅に、単色の下地を敷いて重ねる。そうでなければ画面全体(下地は黒)。
/// 3D だけの画面なので、GL が使えないときは案内(CPU)を出し、自動巡回では飛ばす(`needs_gl`)。
pub struct CubeScreen {
    third: bool,
    interval: Option<Duration>,
    scene: CubeScene,
    count: Rc<ObjectCount>,
    /// 表示中かつ点灯中に経った時間。回転はこれで決まる
    active: Duration,
    /// `active` を最後に更新した時刻
    last: Option<Instant>,
    /// 表示中かつ点灯中か(= `active` が進むか)
    running: bool,
}

impl CubeScreen {
    /// `fps` は動いている間の描画の上限(0 なら制限せず vsync だけ)。
    pub fn new(third: bool, fps: u32) -> Self {
        let count = Rc::new(ObjectCount::default());
        Self {
            third,
            interval: (fps > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(fps))),
            scene: CubeScene::new(count.clone()),
            count,
            active: Duration::ZERO,
            last: None,
            running: false,
        }
    }

    /// 3D を置く領域。`third` なら画面の中央の 1/3 幅(縦は全体)。
    pub fn region(third: bool, (w, h): (u32, u32)) -> Region {
        if third {
            let rw = (w / 3).max(1);
            Region {
                x: (w - rw) / 2,
                y: 0,
                w: rw,
                h,
            }
        } else {
            Region { x: 0, y: 0, w, h }
        }
    }

    /// 試験用: 深度テストを使わない立方体にする。
    #[cfg(test)]
    pub fn without_depth(mut self) -> Self {
        self.scene = CubeScene::without_depth(self.count.clone());
        self
    }

    /// 回転を決める時間(試験用)。
    #[cfg(test)]
    pub fn active(&self) -> Duration {
        self.active
    }

    /// 動いている間だけ `now` まで時間を進め、どちらにしても基準の時刻を `now` にする
    /// (止まっていた間の分を、復帰のときにまとめて進めない)。
    fn advance(&mut self, now: Instant) {
        if self.running
            && let Some(last) = self.last
        {
            self.active += now.saturating_duration_since(last);
        }
        self.last = Some(now);
    }
}

impl Screen for CubeScreen {
    fn id(&self) -> ScreenId {
        if self.third { "cube-third" } else { "cube" }
    }

    fn title(&self) -> &'static str {
        if self.third { "CUBE 1/3" } else { "CUBE" }
    }

    fn set_presentation(&mut self, visible: bool, _covered: bool, display_on: bool, now: Instant) {
        // 状態が変わる前の分までを数えてから切り替える
        self.advance(now);
        self.running = visible && display_on;
    }

    fn tick(&mut self, _world: &crate::screen::World, now: Instant) -> bool {
        self.advance(now);
        false
    }

    /// CPU の完成画面は案内だけ(3D は GL が要る)。
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        notice::gpu_required(ctx.canvas, ctx.text, self.title());
    }

    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        let size = (ctx.canvas.width(), ctx.canvas.height());
        let region = Self::region(self.third, size);
        let base = self.third.then(|| {
            ctx.canvas.clear(rgba(
                THIRD_BASE[0],
                THIRD_BASE[1],
                THIRD_BASE[2],
                THIRD_BASE[3],
            ));
            ctx.canvas.fill_rect(
                region.x as f32,
                region.y as f32,
                region.w as f32,
                region.h as f32,
                rgba(THIRD_BAND[0], THIRD_BAND[1], THIRD_BAND[2], THIRD_BAND[3]),
            );
            Box::new(Composite::Canvas)
        });
        self.scene.set_secs(self.active.as_secs_f32());
        Composite::Gl(GlComposite {
            base,
            scene: &mut self.scene,
            region,
            top: Top::Off,
        })
    }

    fn needs_gl(&self) -> bool {
        true
    }

    fn gl_scene(&mut self) -> Option<&mut dyn GlScene> {
        Some(&mut self.scene)
    }

    fn scene_animating(&self) -> bool {
        self.running
    }

    fn frame_interval(&self) -> Option<Duration> {
        self.interval
    }

    fn gl_live_objects(&self) -> Option<i64> {
        Some(self.count.live())
    }
}
