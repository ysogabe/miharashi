//! 3D の画面(Issue #74、設計は docs/DESIGN-screen-plugin-v2.md)。画面が実装する `GlScene` を、
//! Compositor が持つ深度つきの層(FBO)へ描かせ、下地と最上段の重ね絵の間に重ねる。
//! - 描く順は 下地 → 3D の層 → 最上段の重ね絵(アラート・メニュー)。
//! - Compositor が自分の描画の前と `GlScene::draw` の前に、GL の状態を決まった値へ設定し直す
//!   (`Compositor::reset_gl_state`)。画面は GL の状態を「戻す」責任を持たない。
//! - `draw` の panic・`Err`・GL エラーは、その場面の失敗として扱う。Compositor が `release` を呼び、
//!   そのフレームは 3D 抜き(下地と重ね絵だけ)で完成させて `SceneOutcome::SceneFailed` を返す。
//!   致命的な失敗(下地の描画や表示)だけが `Err(SceneError)` になる。場面の失敗のあとに致命的な失敗が起きても、
//!   場面の失敗は `SceneError::scene_failed` で運ぶ(呼び出し側が、`release` 済みの場面を終了時に二重に解放しないため)。
use crate::compose::{Compositor, Frame, VERT};
use crate::photo::{program as link, texture as tex2d};
use glow::HasContext;
use miharashi_glscene::{GlScene, GlTarget, Region};
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// 層(3D の描画先)を画面へ重ねる。層の画素は `gl_FragCoord` から領域の原点(左下)を引いて 1 対 1 で読む
const FRAG_LAYER: &str = r"#version 300 es
precision highp float;
uniform sampler2D tex;
uniform ivec2 origin;
out vec4 color;
void main() {
    color = texelFetch(tex, ivec2(gl_FragCoord.xy) - origin, 0);
}
";

/// 3D の下地の上、最上段に重ねる絵(アラート・メニュー)の指示。
#[derive(Clone, Copy)]
pub enum SceneTop<'a> {
    /// 重ねない
    Off,
    /// 前回の絵のまま重ねる。前回が `Upload` か `Keep` のときだけ意味がある(`Off` の次は `Upload` が要る。
    /// 絵が無いときは `Off` と同じに扱う)
    Keep,
    /// premultiplied RGBA8(上から下、stride = 幅 × 4、画面と同じ大きさ、透明背景)を転送して重ねる
    Upload(&'a [u8]),
}

/// 3D の画面 1 枚。`Frame` は変えず、その隣に置く入口。
pub struct SceneFrame<'a> {
    /// 下地(2D の `Frame`)。None なら黒
    pub base: Option<Frame<'a>>,
    pub scene: &'a mut dyn GlScene,
    /// 3D を置く画面上の矩形。画面の中に収まっていること
    pub region: Region,
    pub top: SceneTop<'a>,
}

/// `draw_scene` / `present_scene` の結果。3D の失敗は表示全体の失敗にしない(`Err` は DRM・EGL などの致命的なもの)。
#[derive(Debug, PartialEq, Eq)]
pub enum SceneOutcome {
    Drawn,
    /// 3D が失敗した(panic を含む)。その場面の `release` は済んでいて、そのフレームは 3D 抜きで完成している
    SceneFailed(String),
}

/// `draw_scene` / `present_scene` の致命的な失敗(下地の描画・最上段の重ね絵・swap・page flip・DRM など)。
/// 場面の失敗(`SceneOutcome::SceneFailed`)より後に起きたときも、場面の失敗を `scene_failed` で運ぶ
/// (`Err` だけ返すと、`release` 済みの場面が呼び出し側に「まだ生きている」と見えて、終了時に二重に解放される)。
#[derive(Debug, PartialEq, Eq)]
pub struct SceneError {
    pub message: String,
    /// この致命的な失敗の前に場面が失敗していたときの理由(その場面の `release` は済んでいる)。
    /// 呼び出し側は、`Err` でもこれを必ず場面の失敗として反映すること
    pub scene_failed: Option<String>,
}

impl SceneError {
    /// 場面の失敗(`outcome`)より後に起きた致命的な失敗にする
    pub fn after(outcome: &SceneOutcome, message: String) -> Self {
        Self {
            message,
            scene_failed: match outcome {
                SceneOutcome::Drawn => None,
                SceneOutcome::SceneFailed(m) => Some(m.clone()),
            },
        }
    }
}

/// 場面に触れる前の致命的な失敗(下地の描画など)
impl From<String> for SceneError {
    fn from(message: String) -> Self {
        Self {
            message,
            scene_failed: None,
        }
    }
}

impl From<SceneError> for String {
    fn from(e: SceneError) -> Self {
        e.message
    }
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 色(RGBA8 のテクスチャ)と深度・ステンシル(DEPTH24_STENCIL8 の renderbuffer)つきの FBO。
struct Layer {
    fbo: glow::Framebuffer,
    color: glow::Texture,
    depth: glow::Renderbuffer,
    size: (u32, u32),
}

impl Layer {
    fn new(gl: &glow::Context, size: (u32, u32)) -> Result<Self, String> {
        let color = tex2d(gl, glow::RGBA8, size, glow::NEAREST)?;
        unsafe {
            let fbo = match gl.create_framebuffer() {
                Ok(f) => f,
                Err(e) => {
                    gl.delete_texture(color);
                    return Err(e);
                }
            };
            let depth = match gl.create_renderbuffer() {
                Ok(d) => d,
                Err(e) => {
                    gl.delete_framebuffer(fbo);
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
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::DEPTH24_STENCIL8,
                size.0 as i32,
                size.1 as i32,
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_STENCIL_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(depth),
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_renderbuffer(glow::RENDERBUFFER, None);
            let layer = Self {
                fbo,
                color,
                depth,
                size,
            };
            if status != glow::FRAMEBUFFER_COMPLETE {
                layer.destroy(gl);
                return Err(format!("3D の層の FBO が不完全: 0x{status:x}"));
            }
            Ok(layer)
        }
    }

    fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_renderbuffer(self.depth);
            gl.delete_texture(self.color);
        }
    }
}

/// `Compositor` が 3D のために持つもの。使うまで何も作らない。
#[derive(Default)]
pub(crate) struct SceneState {
    layer: Option<Layer>,
    /// 層を画面へ重ねるプログラム(最初の層と一緒に作る)
    layer_program: Option<glow::Program>,
    /// 最上段の重ね絵(最初の `Upload` で作る)
    top_tex: Option<glow::Texture>,
    /// 重ね絵のテクスチャに、重ねてよい絵が入っているか
    top_ready: bool,
    /// `draw` の前に読み出した GL エラー(下地の合成などで出たもの)をログに出したか
    pre_error_logged: bool,
    /// 試験用: 次の最上段の重ね絵の転送を、この理由で失敗させる(`draw_top` の `Err` は GL の資源が尽きたときだけで、
    /// 実機では起こせない)
    #[cfg(test)]
    fail_top: Option<String>,
}

impl SceneState {
    /// 層の FBO を返す(大きさが変わったら作り直す)。
    fn layer(&mut self, gl: &glow::Context, size: (u32, u32)) -> Result<&Layer, String> {
        if self.layer.as_ref().is_some_and(|l| l.size != size)
            && let Some(old) = self.layer.take()
        {
            old.destroy(gl);
        }
        if self.layer_program.is_none() {
            self.layer_program = Some(link(gl, VERT, FRAG_LAYER)?);
        }
        if self.layer.is_none() {
            self.layer = Some(Layer::new(gl, size)?);
        }
        self.layer.as_ref().ok_or_else(|| "3D の層がない".into())
    }

    pub(crate) fn destroy(&self, gl: &glow::Context) {
        unsafe {
            if let Some(l) = &self.layer {
                l.destroy(gl);
            }
            if let Some(p) = self.layer_program {
                gl.delete_program(p);
            }
            if let Some(t) = self.top_tex {
                gl.delete_texture(t);
            }
        }
    }
}

/// `reset_gl_state` がテクスチャと sampler の束縛を外すユニットの数(GLES 3.x の最低保証の 16)。
/// Compositor 自身が使うのは 0〜3 だけだが、3D の場面が使うユニットに前の場面の sampler を残さないため広く外す。
const RESET_TEXTURE_UNITS: u32 = 16;

/// 溜まっている GL エラーを読み出しきり、最初のものを返す(`GL_CONTEXT_LOST` で終わらないよう回数に上限)。
fn drain_errors(gl: &glow::Context) -> Option<u32> {
    let mut first = None;
    for _ in 0..16 {
        let e = unsafe { gl.get_error() };
        if e == glow::NO_ERROR {
            break;
        }
        first.get_or_insert(e);
    }
    first
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(メッセージなし)".into())
}

/// `scene.release` を `catch_unwind` つきで呼ぶ。panic したらそのメッセージを返す。
fn release_guarded(gl: &glow::Context, scene: &mut dyn GlScene) -> Option<String> {
    let r = catch_unwind(AssertUnwindSafe(|| scene.release(gl)));
    r.err().map(|p| panic_message(&*p))
}

impl Compositor {
    /// `Compositor` が前提にする GL の状態へ設定し直す(§5.1)。`target` は描画先の framebuffer(画面なら None)、
    /// `size` はその viewport。自分の描画の前と、`GlScene::draw` の前(3D に渡す初期状態)に呼ぶ。
    /// 前の画面や借りた側が何を残したかに頼らない。
    pub fn reset_gl_state(
        gl: &glow::Context,
        (w, h): (u32, u32),
        target: Option<glow::Framebuffer>,
    ) {
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, target);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, target);
            gl.viewport(0, 0, w as i32, h as i32);
            gl.scissor(0, 0, w as i32, h as i32);
            // 画面(既定の framebuffer)は BACK、FBO は COLOR_ATTACHMENT0。FBO ごとの状態なので target に対して設定する
            let color = if target.is_some() {
                glow::COLOR_ATTACHMENT0
            } else {
                glow::BACK
            };
            gl.draw_buffers(&[color]);
            gl.read_buffer(color);
            for cap in [
                glow::DEPTH_TEST,
                glow::CULL_FACE,
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
                glow::RASTERIZER_DISCARD,
                glow::SAMPLE_ALPHA_TO_COVERAGE,
                glow::SAMPLE_COVERAGE,
                glow::SAMPLE_MASK,
                glow::PRIMITIVE_RESTART_FIXED_INDEX,
                glow::POLYGON_OFFSET_FILL,
            ] {
                gl.disable(cap);
            }
            gl.enable(glow::DITHER);
            gl.depth_mask(true);
            gl.color_mask(true, true, true, true);
            // 無効にした機能のパラメーターも既定へ(次の画面に漏らさない)
            gl.depth_func(glow::LESS);
            gl.depth_range_f32(0.0, 1.0);
            gl.front_face(glow::CCW);
            gl.cull_face(glow::BACK);
            gl.stencil_func(glow::ALWAYS, 0, 0xFF);
            gl.stencil_op(glow::KEEP, glow::KEEP, glow::KEEP);
            gl.stencil_mask(0xFF);
            gl.clear_depth_f32(1.0);
            gl.clear_stencil(0);
            // blend_func / blend_equation は RGB とアルファの両方を設定する(separate の値も戻る)
            gl.blend_func(glow::ONE, glow::ZERO);
            gl.blend_equation(glow::FUNC_ADD);
            gl.blend_color(0.0, 0.0, 0.0, 0.0);
            gl.polygon_offset(0.0, 0.0);
            gl.line_width(1.0);
            gl.sample_coverage(1.0, false);
            gl.hint(glow::GENERATE_MIPMAP_HINT, glow::DONT_CARE);
            gl.hint(glow::FRAGMENT_SHADER_DERIVATIVE_HINT, glow::DONT_CARE);
            gl.use_program(None);
            // VAO を外してから ELEMENT_ARRAY_BUFFER を外す(VAO の中の束縛を壊さないため)
            gl.bind_vertex_array(None);
            for binding in [
                glow::ARRAY_BUFFER,
                glow::ELEMENT_ARRAY_BUFFER,
                glow::PIXEL_UNPACK_BUFFER,
                glow::PIXEL_PACK_BUFFER,
                glow::DRAW_INDIRECT_BUFFER,
            ] {
                gl.bind_buffer(binding, None);
            }
            // sampler オブジェクトは、束縛が残るとテクスチャ自身のフィルター設定より優先される
            for unit in (0..RESET_TEXTURE_UNITS).rev() {
                gl.active_texture(glow::TEXTURE0 + unit);
                for kind in [
                    glow::TEXTURE_2D,
                    glow::TEXTURE_CUBE_MAP,
                    glow::TEXTURE_3D,
                    glow::TEXTURE_2D_ARRAY,
                ] {
                    gl.bind_texture(kind, None);
                }
                gl.bind_sampler(unit, None);
            }
            for pname in [
                glow::UNPACK_ROW_LENGTH,
                glow::UNPACK_SKIP_PIXELS,
                glow::UNPACK_SKIP_ROWS,
                glow::UNPACK_IMAGE_HEIGHT,
                glow::UNPACK_SKIP_IMAGES,
                glow::PACK_ROW_LENGTH,
                glow::PACK_SKIP_PIXELS,
                glow::PACK_SKIP_ROWS,
            ] {
                gl.pixel_store_i32(pname, 0);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
        }
    }

    /// `target`(画面なら None)に、下地 → `frame.scene` を層へ描いて重ねたもの → 最上段の重ね絵、の順で描く。
    /// 3D の失敗(panic・`Err`・GL エラー・不正な領域・層を作れない)は `SceneFailed` で返し、その場面の
    /// `release` を呼んでから、3D 抜きで残りを描く。`Err` は下地の描画など致命的なものだけで、
    /// 場面の失敗のあとに起きたときは、その理由を `SceneError::scene_failed` に残す。
    pub fn draw_scene(
        &mut self,
        gl: &glow::Context,
        target: Option<glow::Framebuffer>,
        frame: &mut SceneFrame<'_>,
    ) -> Result<SceneOutcome, SceneError> {
        let size = self.size;
        let region = frame.region;
        // 1. 下地
        Self::reset_gl_state(gl, size, target);
        match &frame.base {
            Some(base) => self.draw(gl, base)?,
            None => unsafe {
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            },
        }
        // 2. 3D を層へ
        let (outcome, layer) = match self.draw_layer(gl, target, frame) {
            Ok(layer) => (SceneOutcome::Drawn, Some(layer)),
            Err(msg) => (SceneOutcome::SceneFailed(msg), None),
        };
        // 3. 層を画面へ重ねる(失敗したときは、中途半端な絵を出さない)
        Self::reset_gl_state(gl, size, target);
        if let (Some(color), Some(program)) = (layer, self.scene.layer_program) {
            let gl_y = (size.1 - region.y - region.h) as i32;
            unsafe {
                gl.viewport(region.x as i32, gl_y, region.w as i32, region.h as i32);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.use_program(Some(program));
                gl.uniform_1_i32(gl.get_uniform_location(program, "tex").as_ref(), 0);
                gl.uniform_2_i32(
                    gl.get_uniform_location(program, "origin").as_ref(),
                    region.x as i32,
                    gl_y,
                );
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(color));
                gl.bind_vertex_array(Some(self.vao));
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
                gl.disable(glow::BLEND);
                gl.viewport(0, 0, size.0 as i32, size.1 as i32);
            }
        }
        // 4. 最上段の重ね絵
        if let Err(msg) = self.draw_top(gl, frame.top) {
            return Err(SceneError::after(&outcome, msg));
        }
        Ok(outcome)
    }

    /// 層に `frame.scene` を描かせる。成功したら層の色テクスチャを返す。失敗したら(`release` まで済ませて)理由を返す。
    fn draw_layer(
        &mut self,
        gl: &glow::Context,
        target: Option<glow::Framebuffer>,
        frame: &mut SceneFrame<'_>,
    ) -> Result<glow::Texture, String> {
        let size = self.size;
        let r = frame.region;
        let inside = r.w > 0
            && r.h > 0
            && r.x.checked_add(r.w).is_some_and(|x| x <= size.0)
            && r.y.checked_add(r.h).is_some_and(|y| y <= size.1);
        let drawn = if inside {
            self.scene
                .layer(gl, (r.w, r.h))
                .map(|l| (l.fbo, l.color, l.size))
                .and_then(|(fbo, color, lsize)| {
                    self.run_scene(gl, fbo, lsize, frame.scene).map(|()| color)
                })
        } else {
            Err(format!("3D の領域が画面({}x{})の外: {r:?}", size.0, size.1))
        };
        let msg = match drawn {
            Ok(color) => return Ok(color),
            Err(msg) => msg,
        };
        // 失敗: 状態を戻し、GL エラーを読み出しきってから、その場で release する。ScreenManager は GL を
        // 持たないので後からは呼べない。作りかけのオブジェクトがあっても release は動く約束
        Self::reset_gl_state(gl, size, target);
        drain_errors(gl);
        let released = release_guarded(gl, frame.scene);
        // release の GL エラーを次のフレームの「draw の前のエラー」に混ぜない
        drain_errors(gl);
        Err(match released {
            Some(p) => format!("{msg} (release も panic: {p})"),
            None => msg,
        })
    }

    /// 層を初期状態(§5.4)にして `scene.draw` を呼ぶ。`Err`・panic・GL エラーはすべて Err にする。
    fn run_scene(
        &mut self,
        gl: &glow::Context,
        fbo: glow::Framebuffer,
        size: (u32, u32),
        scene: &mut dyn GlScene,
    ) -> Result<(), String> {
        Self::reset_gl_state(gl, size, Some(fbo));
        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear_depth_f32(1.0);
            gl.clear_stencil(0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT);
        }
        // 下地の合成などで出たエラーを 3D の失敗にしない
        if let Some(e) = drain_errors(gl)
            && !self.scene.pre_error_logged
        {
            self.scene.pre_error_logged = true;
            eprintln!("3D の前に GL エラー 0x{e:x}(下地の描画で出たもの。3D の失敗にはしない)");
        }
        let t = GlTarget { fbo, size };
        let r = catch_unwind(AssertUnwindSafe(|| scene.draw(gl, &t)));
        let gl_error = drain_errors(gl);
        match r {
            Ok(Ok(())) => match gl_error {
                None => Ok(()),
                Some(e) => Err(format!("GL エラー 0x{e:x}")),
            },
            Ok(Err(e)) => Err(e),
            Err(p) => Err(format!("panic: {}", panic_message(&*p))),
        }
    }

    /// 最上段の重ね絵を全面に重ねる(`ONE, ONE_MINUS_SRC_ALPHA`)。
    fn draw_top(&mut self, gl: &glow::Context, top: SceneTop<'_>) -> Result<(), String> {
        match top {
            SceneTop::Off => self.scene.top_ready = false,
            SceneTop::Keep => {}
            SceneTop::Upload(rgba) => {
                #[cfg(test)]
                if let Some(msg) = self.scene.fail_top.take() {
                    return Err(msg);
                }
                let tex = match self.scene.top_tex {
                    Some(t) => t,
                    None => {
                        let t = tex2d(gl, glow::RGBA8, self.size, glow::NEAREST)?;
                        self.scene.top_tex = Some(t);
                        t
                    }
                };
                self.upload(gl, tex, rgba);
                self.scene.top_ready = true;
            }
        }
        let Some(tex) = self.scene.top_tex.filter(|_| self.scene.top_ready) else {
            return Ok(());
        };
        unsafe {
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.use_program(Some(self.overlay));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.disable(glow::BLEND);
        }
        Ok(())
    }

    /// 持っている `GlScene` を解放する(終了時など。GL が current なときに呼ぶ)。`release` の panic は
    /// 握りつぶさずメッセージを返す(止めないだけ)。`size` と `target` は描画先(画面なら None)。
    pub fn release_scenes(
        gl: &glow::Context,
        size: (u32, u32),
        target: Option<glow::Framebuffer>,
        scenes: &mut [&mut dyn GlScene],
    ) -> Vec<String> {
        Self::reset_gl_state(gl, size, target);
        let mut panics = Vec::new();
        for scene in scenes.iter_mut() {
            if let Some(p) = release_guarded(gl, &mut **scene) {
                panics.push(p);
            }
        }
        drain_errors(gl);
        panics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{PhotoLayer, PhotoSource};
    use crate::offscreen::{Offscreen, diff_rgb};
    use std::cell::Cell;
    use std::path::Path;
    use std::rc::Rc;

    const SCREEN: (u32, u32) = (64, 32);
    const REGION: Region = Region {
        x: 16,
        y: 8,
        w: 32,
        h: 16,
    };
    const BASE: [u8; 4] = [200, 40, 10, 255];

    #[derive(Default, Clone)]
    struct Counters {
        created: Rc<Cell<u32>>,
        deleted: Rc<Cell<u32>>,
        draws: Rc<Cell<u32>>,
        releases: Rc<Cell<u32>>,
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Fail {
        None,
        /// プログラムを作ったあと、VBO を作る前に panic する(作りかけ)
        PanicMidInit,
        Panic,
        Err,
        GlError,
        /// `Err` を返し、`release` も panic する
        ErrAndReleasePanic,
    }

    /// 試験用の場面。深度テストつきの 3 つの四角を 1 回の draw call で描く。
    /// - A: 手前(z = -0.5)の赤。左から 0.625 の幅
    /// - B: 奥(z = +0.5)の青。右の 0.625 の幅(A のあとに描くので、深度テストが効かないと重なりが青になる)
    /// - C: 左上の小さな半透明の緑(premultiplied で (0, 0.5, 0, 0.5))
    ///
    /// `dirty` なら描いたあとに GL の状態を片っ端から汚す。`check_contract` なら draw の頭で
    /// §5.4 の初期状態を `glGet` で確かめ、違えば Err を返す。
    struct TestScene {
        c: Counters,
        fail: Fail,
        dirty: bool,
        check_contract: bool,
        program: Option<glow::Program>,
        vao: Option<glow::VertexArray>,
        vbo: Option<glow::Buffer>,
        dirt_fbo: Option<glow::Framebuffer>,
        dirt_tex: Option<glow::Texture>,
        dirt_pbo: Option<glow::Buffer>,
        dirt_cube: Option<glow::Texture>,
        dirt_sampler: Option<glow::Sampler>,
    }

    const VS: &str = "#version 300 es
layout(location = 0) in vec3 pos;
layout(location = 1) in vec4 col;
out vec4 v_col;
void main() { v_col = col; gl_Position = vec4(pos, 1.0); }
";
    const FS: &str = "#version 300 es
precision mediump float;
in vec4 v_col;
out vec4 o;
void main() { o = v_col; }
";

    impl TestScene {
        fn new(c: &Counters, fail: Fail, dirty: bool) -> Self {
            Self {
                c: c.clone(),
                fail,
                dirty,
                check_contract: true,
                program: None,
                vao: None,
                vbo: None,
                dirt_fbo: None,
                dirt_tex: None,
                dirt_pbo: None,
                dirt_cube: None,
                dirt_sampler: None,
            }
        }

        fn created(&self) {
            self.c.created.set(self.c.created.get() + 1);
        }

        fn deleted(&self) {
            self.c.deleted.set(self.c.deleted.get() + 1);
        }

        /// §5.4 の初期状態。違うものがあれば Err(理由)
        fn contract(gl: &glow::Context, t: &GlTarget) -> Result<(), String> {
            unsafe {
                let mut bad = Vec::new();
                for (name, cap) in [
                    ("DEPTH_TEST", glow::DEPTH_TEST),
                    ("CULL_FACE", glow::CULL_FACE),
                    ("BLEND", glow::BLEND),
                    ("SCISSOR_TEST", glow::SCISSOR_TEST),
                    ("STENCIL_TEST", glow::STENCIL_TEST),
                    ("RASTERIZER_DISCARD", glow::RASTERIZER_DISCARD),
                    ("SAMPLE_ALPHA_TO_COVERAGE", glow::SAMPLE_ALPHA_TO_COVERAGE),
                    ("SAMPLE_COVERAGE", glow::SAMPLE_COVERAGE),
                    ("SAMPLE_MASK", glow::SAMPLE_MASK),
                    ("POLYGON_OFFSET_FILL", glow::POLYGON_OFFSET_FILL),
                ] {
                    if gl.is_enabled(cap) {
                        bad.push(format!("{name} が有効"));
                    }
                }
                if !gl.is_enabled(glow::DITHER) {
                    bad.push("DITHER が無効".into());
                }
                for (name, pname, want) in [
                    ("DEPTH_FUNC", glow::DEPTH_FUNC, glow::LESS),
                    ("CULL_FACE_MODE", glow::CULL_FACE_MODE, glow::BACK),
                    ("FRONT_FACE", glow::FRONT_FACE, glow::CCW),
                    ("BLEND_SRC_RGB", glow::BLEND_SRC_RGB, glow::ONE),
                    ("BLEND_DST_RGB", glow::BLEND_DST_RGB, glow::ZERO),
                    ("BLEND_SRC_ALPHA", glow::BLEND_SRC_ALPHA, glow::ONE),
                    ("BLEND_DST_ALPHA", glow::BLEND_DST_ALPHA, glow::ZERO),
                    (
                        "BLEND_EQUATION_RGB",
                        glow::BLEND_EQUATION_RGB,
                        glow::FUNC_ADD,
                    ),
                    (
                        "BLEND_EQUATION_ALPHA",
                        glow::BLEND_EQUATION_ALPHA,
                        glow::FUNC_ADD,
                    ),
                    ("STENCIL_FUNC", glow::STENCIL_FUNC, glow::ALWAYS),
                    ("STENCIL_BACK_FUNC", glow::STENCIL_BACK_FUNC, glow::ALWAYS),
                    ("STENCIL_REF", glow::STENCIL_REF, 0),
                    ("STENCIL_BACK_REF", glow::STENCIL_BACK_REF, 0),
                    ("STENCIL_VALUE_MASK", glow::STENCIL_VALUE_MASK, 0xFF),
                    (
                        "STENCIL_BACK_VALUE_MASK",
                        glow::STENCIL_BACK_VALUE_MASK,
                        0xFF,
                    ),
                    ("STENCIL_FAIL", glow::STENCIL_FAIL, glow::KEEP),
                    ("STENCIL_BACK_FAIL", glow::STENCIL_BACK_FAIL, glow::KEEP),
                    (
                        "STENCIL_PASS_DEPTH_FAIL",
                        glow::STENCIL_PASS_DEPTH_FAIL,
                        glow::KEEP,
                    ),
                    (
                        "STENCIL_BACK_PASS_DEPTH_FAIL",
                        glow::STENCIL_BACK_PASS_DEPTH_FAIL,
                        glow::KEEP,
                    ),
                    (
                        "STENCIL_PASS_DEPTH_PASS",
                        glow::STENCIL_PASS_DEPTH_PASS,
                        glow::KEEP,
                    ),
                    (
                        "STENCIL_BACK_PASS_DEPTH_PASS",
                        glow::STENCIL_BACK_PASS_DEPTH_PASS,
                        glow::KEEP,
                    ),
                    ("STENCIL_WRITEMASK", glow::STENCIL_WRITEMASK, 0xFF),
                    ("STENCIL_BACK_WRITEMASK", glow::STENCIL_BACK_WRITEMASK, 0xFF),
                    ("DEPTH_WRITEMASK", glow::DEPTH_WRITEMASK, 1),
                    ("CURRENT_PROGRAM", glow::CURRENT_PROGRAM, 0),
                    ("VERTEX_ARRAY_BINDING", glow::VERTEX_ARRAY_BINDING, 0),
                    ("ARRAY_BUFFER_BINDING", glow::ARRAY_BUFFER_BINDING, 0),
                    (
                        "PIXEL_UNPACK_BUFFER_BINDING",
                        glow::PIXEL_UNPACK_BUFFER_BINDING,
                        0,
                    ),
                    (
                        "PIXEL_PACK_BUFFER_BINDING",
                        glow::PIXEL_PACK_BUFFER_BINDING,
                        0,
                    ),
                    (
                        "DRAW_INDIRECT_BUFFER_BINDING",
                        glow::DRAW_INDIRECT_BUFFER_BINDING,
                        0,
                    ),
                    ("ACTIVE_TEXTURE", glow::ACTIVE_TEXTURE, glow::TEXTURE0),
                    ("READ_BUFFER", glow::READ_BUFFER, glow::COLOR_ATTACHMENT0),
                    (
                        "GENERATE_MIPMAP_HINT",
                        glow::GENERATE_MIPMAP_HINT,
                        glow::DONT_CARE,
                    ),
                    (
                        "FRAGMENT_SHADER_DERIVATIVE_HINT",
                        glow::FRAGMENT_SHADER_DERIVATIVE_HINT,
                        glow::DONT_CARE,
                    ),
                    ("UNPACK_ROW_LENGTH", glow::UNPACK_ROW_LENGTH, 0),
                    ("UNPACK_SKIP_PIXELS", glow::UNPACK_SKIP_PIXELS, 0),
                    ("UNPACK_SKIP_ROWS", glow::UNPACK_SKIP_ROWS, 0),
                    ("UNPACK_IMAGE_HEIGHT", glow::UNPACK_IMAGE_HEIGHT, 0),
                    ("UNPACK_SKIP_IMAGES", glow::UNPACK_SKIP_IMAGES, 0),
                    ("UNPACK_ALIGNMENT", glow::UNPACK_ALIGNMENT, 4),
                    ("PACK_ROW_LENGTH", glow::PACK_ROW_LENGTH, 0),
                    ("PACK_SKIP_PIXELS", glow::PACK_SKIP_PIXELS, 0),
                    ("PACK_SKIP_ROWS", glow::PACK_SKIP_ROWS, 0),
                    ("PACK_ALIGNMENT", glow::PACK_ALIGNMENT, 4),
                ] {
                    let got = gl.get_parameter_i32(pname);
                    if got != want as i32 {
                        bad.push(format!("{name} = 0x{got:x}(期待 0x{want:x})"));
                    }
                }
                for (name, pname, want) in [
                    ("LINE_WIDTH", glow::LINE_WIDTH, 1.0),
                    ("POLYGON_OFFSET_FACTOR", glow::POLYGON_OFFSET_FACTOR, 0.0),
                    ("POLYGON_OFFSET_UNITS", glow::POLYGON_OFFSET_UNITS, 0.0),
                    ("SAMPLE_COVERAGE_VALUE", glow::SAMPLE_COVERAGE_VALUE, 1.0),
                ] {
                    let got = gl.get_parameter_f32(pname);
                    if got != want {
                        bad.push(format!("{name} = {got}(期待 {want})"));
                    }
                }
                if gl.get_parameter_bool(glow::SAMPLE_COVERAGE_INVERT) {
                    bad.push("SAMPLE_COVERAGE_INVERT".into());
                }
                let mut range = [0f32; 2];
                gl.get_parameter_f32_slice(glow::DEPTH_RANGE, &mut range);
                if range != [0.0, 1.0] {
                    bad.push(format!("DEPTH_RANGE = {range:?}"));
                }
                let mut color = [1f32; 4];
                gl.get_parameter_f32_slice(glow::BLEND_COLOR, &mut color);
                if color != [0.0; 4] {
                    bad.push(format!("BLEND_COLOR = {color:?}"));
                }
                // sampler とテクスチャの束縛は、Compositor が使うユニット(0〜3)の外も含めて全部外れている
                for unit in 0..RESET_TEXTURE_UNITS {
                    gl.active_texture(glow::TEXTURE0 + unit);
                    if gl.get_parameter_i32(glow::SAMPLER_BINDING) != 0 {
                        bad.push(format!("TEXTURE{unit} に sampler が残っている"));
                    }
                    for (name, pname) in [
                        ("2D", glow::TEXTURE_BINDING_2D),
                        ("CUBE_MAP", glow::TEXTURE_BINDING_CUBE_MAP),
                        ("3D", glow::TEXTURE_BINDING_3D),
                        ("2D_ARRAY", glow::TEXTURE_BINDING_2D_ARRAY),
                    ] {
                        if gl.get_parameter_i32(pname) != 0 {
                            bad.push(format!("TEXTURE{unit} に {name} の bind が残っている"));
                        }
                    }
                }
                gl.active_texture(glow::TEXTURE0);
                if gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK) != [true; 4] {
                    bad.push("color_mask".into());
                }
                let mut vp = [0i32; 4];
                gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
                if vp != [0, 0, t.size.0 as i32, t.size.1 as i32] {
                    bad.push(format!("viewport = {vp:?}"));
                }
                gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut vp);
                if vp != [0, 0, t.size.0 as i32, t.size.1 as i32] {
                    bad.push(format!("scissor = {vp:?}"));
                }
                if gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING) != Some(t.fbo)
                    || gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING) != Some(t.fbo)
                {
                    bad.push("FBO が target.fbo ではない".into());
                }
                let mut db = [0i32; 1];
                gl.get_parameter_i32_slice(glow::DRAW_BUFFER0, &mut db);
                if db[0] != glow::COLOR_ATTACHMENT0 as i32 {
                    bad.push(format!("DRAW_BUFFER0 = 0x{:x}", db[0]));
                }
                if bad.is_empty() {
                    Ok(())
                } else {
                    Err(format!("初期状態の約束が破れている: {}", bad.join(", ")))
                }
            }
        }

        /// 描いたあとに、GL の状態を片っ端から汚す(設計書 §5.5)。
        fn make_dirt(&mut self, gl: &glow::Context, t: &GlTarget) {
            unsafe {
                // 層の FBO 自身の draw buffer(FBO ごとの状態)。bind を変える前にやる
                gl.draw_buffers(&[glow::NONE]);
                gl.read_buffer(glow::NONE);
                gl.clear_color(1.0, 0.0, 1.0, 1.0);
                gl.disable(glow::DITHER);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ZERO);
                gl.blend_equation(glow::FUNC_REVERSE_SUBTRACT);
                // RGB とアルファで別々の値(blend_func / blend_equation のあとに設定する)
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
                // 前後関係が逆になる
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
                let fbo = gl.create_framebuffer().unwrap();
                self.created();
                self.dirt_fbo = Some(fbo);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.viewport(0, 0, 3, 3);
                // プログラム・VAO・バッファ・テクスチャ・pixel store
                gl.use_program(self.program);
                gl.bind_vertex_array(self.vao);
                gl.bind_buffer(glow::ARRAY_BUFFER, self.vbo);
                gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, self.vbo);
                let pbo = gl.create_buffer().unwrap();
                self.created();
                self.dirt_pbo = Some(pbo);
                gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(pbo));
                gl.bind_buffer(glow::DRAW_INDIRECT_BUFFER, Some(pbo));
                let tex = gl.create_texture().unwrap();
                self.created();
                self.dirt_tex = Some(tex);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                let cube = gl.create_texture().unwrap();
                self.created();
                self.dirt_cube = Some(cube);
                gl.active_texture(glow::TEXTURE2);
                gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(cube));
                // sampler は bind_texture(None) では外れない。Compositor が使わないユニット(9)にも残す
                let sampler = gl.create_sampler().unwrap();
                self.created();
                self.dirt_sampler = Some(sampler);
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
            let _ = t;
        }
    }

    impl GlScene for TestScene {
        fn draw(&mut self, gl: &glow::Context, t: &GlTarget) -> Result<(), String> {
            self.c.draws.set(self.c.draws.get() + 1);
            if self.check_contract {
                Self::contract(gl, t)?;
            }
            unsafe {
                if self.program.is_none() {
                    self.program = Some(link(gl, VS, FS)?);
                    self.created();
                }
                if self.fail == Fail::PanicMidInit {
                    panic!("初期化の途中で panic");
                }
                if self.vao.is_none() {
                    self.vao = Some(gl.create_vertex_array()?);
                    self.created();
                }
                if self.vbo.is_none() {
                    self.vbo = Some(gl.create_buffer()?);
                    self.created();
                }
                // x, y, z, r, g, b, a
                let quad = |x0: f32, x1: f32, y0: f32, y1: f32, z: f32, c: [f32; 4]| {
                    [[x0, y0], [x1, y0], [x1, y1], [x0, y0], [x1, y1], [x0, y1]]
                        .map(|[x, y]| [x, y, z, c[0], c[1], c[2], c[3]])
                };
                let verts: Vec<f32> = [
                    quad(-1.0, 0.25, -0.5, 0.5, -0.5, [1.0, 0.0, 0.0, 1.0]),
                    quad(-0.25, 1.0, -0.5, 0.5, 0.5, [0.0, 0.0, 1.0, 1.0]),
                    quad(-1.0, -0.5, 0.75, 1.0, 0.0, [0.0, 0.5, 0.0, 0.5]),
                ]
                .into_iter()
                .flatten()
                .flatten()
                .collect();
                gl.bind_vertex_array(self.vao);
                gl.bind_buffer(glow::ARRAY_BUFFER, self.vbo);
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    std::slice::from_raw_parts(verts.as_ptr().cast::<u8>(), verts.len() * 4),
                    glow::STATIC_DRAW,
                );
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 28, 0);
                gl.enable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, 28, 12);
                gl.use_program(self.program);
                gl.enable(glow::DEPTH_TEST);
                gl.draw_arrays(glow::TRIANGLES, 0, 18);
            }
            if self.dirty {
                self.make_dirt(gl, t);
            }
            match self.fail {
                Fail::None | Fail::PanicMidInit => Ok(()),
                Fail::Panic => panic!("draw の途中で panic"),
                Fail::Err | Fail::ErrAndReleasePanic => Err("draw が Err".into()),
                Fail::GlError => {
                    unsafe { gl.enable(0xFFFF) };
                    Ok(())
                }
            }
        }

        fn release(&mut self, gl: &glow::Context) {
            self.c.releases.set(self.c.releases.get() + 1);
            if self.fail == Fail::ErrAndReleasePanic {
                panic!("release が panic");
            }
            unsafe {
                if let Some(p) = self.program.take() {
                    gl.delete_program(p);
                    self.deleted();
                }
                if let Some(v) = self.vao.take() {
                    gl.delete_vertex_array(v);
                    self.deleted();
                }
                if let Some(b) = self.vbo.take() {
                    gl.delete_buffer(b);
                    self.deleted();
                }
                if let Some(b) = self.dirt_pbo.take() {
                    gl.delete_buffer(b);
                    self.deleted();
                }
                if let Some(t) = self.dirt_tex.take() {
                    gl.delete_texture(t);
                    self.deleted();
                }
                if let Some(t) = self.dirt_cube.take() {
                    gl.delete_texture(t);
                    self.deleted();
                }
                if let Some(smp) = self.dirt_sampler.take() {
                    gl.delete_sampler(smp);
                    self.deleted();
                }
                if let Some(f) = self.dirt_fbo.take() {
                    gl.delete_framebuffer(f);
                    self.deleted();
                }
            }
        }
    }

    fn offscreen() -> Option<Offscreen> {
        match Offscreen::open(Path::new("/dev/dri/renderD128")) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                None
            }
        }
    }

    fn solid(c: [u8; 4], (w, h): (u32, u32)) -> Vec<u8> {
        c.repeat((w * h) as usize)
    }

    /// 重ね絵: 右の x = 44 以降(領域の右端 48 の外も含む)を不透明な黄色にする
    fn top_image() -> Vec<u8> {
        let (w, h) = SCREEN;
        let mut v = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                v.extend_from_slice(if x >= 44 {
                    &[255, 255, 0, 255]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        v
    }

    fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SCREEN.0 + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    /// 領域内の位置(幅・高さに対する割合)の画面上の画素
    fn at(img: &[u8], r: Region, fx: f32, fy: f32) -> [u8; 4] {
        px(
            img,
            r.x + (fx * r.w as f32) as u32,
            r.y + (fy * r.h as f32) as u32,
        )
    }

    fn near(got: [u8; 4], want: [u8; 4], tol: u8) -> bool {
        got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= tol)
    }

    /// 3D の場面の中身(深度・透明・半透明)が、領域のとおりの位置に出ていること。重ね絵は別に見る
    fn assert_scene_pixels(img: &[u8], r: Region) {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        assert_eq!(at(img, r, 0.2, 0.5), red, "A だけの所");
        assert_eq!(at(img, r, 0.5, 0.5), red, "重なりは手前の A(深度テスト)");
        assert_eq!(at(img, r, 0.8, 0.5), blue, "B だけの所");
        assert_eq!(at(img, r, 0.8, 0.05), BASE, "層が透明な所は下地");
        // 半透明の緑 (0, 0.5, 0, 0.5) を下地に premultiplied で重ねる
        let a = 128.0 / 255.0;
        let want = [
            (BASE[0] as f32 * (1.0 - a)) as u8,
            (128.0 + BASE[1] as f32 * (1.0 - a)) as u8,
            (BASE[2] as f32 * (1.0 - a)) as u8,
            255,
        ];
        let got = at(img, r, 0.05, 0.03);
        assert!(near(got, want, 2), "半透明 {got:?} vs {want:?}");
    }

    fn assert_outside_is_base(img: &[u8], r: Region) {
        for (x, y) in [(0, 0), (SCREEN.0 - 1, 0), (0, SCREEN.1 - 1), (2, 16)] {
            let inside = x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
            if !inside && x < 44 {
                assert_eq!(px(img, x, y), BASE, "領域の外 ({x},{y}) は下地のまま");
            }
        }
    }

    /// 重ね絵(x >= 44)は 3D の上に出る
    fn assert_top_over(img: &[u8]) {
        let yellow = [255, 255, 0, 255];
        assert_eq!(px(img, 45, 16), yellow, "3D(青)の上");
        assert_eq!(px(img, 45, 8), yellow, "3D の透明な所(下地)の上");
        assert_eq!(px(img, 60, 16), yellow, "領域の外");
    }

    fn frame<'a>(
        scene: &'a mut dyn GlScene,
        base: &'a [u8],
        region: Region,
        top: SceneTop<'a>,
    ) -> SceneFrame<'a> {
        SceneFrame {
            base: Some(Frame::Image(base)),
            scene,
            region,
            top,
        }
    }

    #[test]
    fn scene_is_drawn_between_base_and_top_at_region() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        let c = Counters::default();
        let mut scene = TestScene::new(&c, Fail::None, false);
        let (outcome, img) = off
            .render_scene(
                &mut frame(&mut scene, &base, REGION, SceneTop::Upload(&top)),
                SCREEN,
            )
            .unwrap();
        assert_eq!(outcome, SceneOutcome::Drawn);
        assert_eq!(c.draws.get(), 1);
        assert_scene_pixels(&img, REGION);
        assert_outside_is_base(&img, REGION);
        assert_top_over(&img);

        // Keep は前回の絵のまま、Off で消え、Off の次の Keep は何も出さない
        let (outs, img) = off
            .render_with_compositor(SCREEN, |gl, comp, target| {
                let mut outs = Vec::new();
                let mut s = TestScene::new(&c, Fail::None, false);
                for (i, t) in [
                    SceneTop::Upload(&top),
                    SceneTop::Keep,
                    SceneTop::Off,
                    SceneTop::Keep,
                ]
                .into_iter()
                .enumerate()
                {
                    outs.push(
                        comp.draw_scene(gl, target, &mut frame(&mut s, &base, REGION, t))
                            .unwrap(),
                    );
                    if i == 1 {
                        // ここまでは黄色が残っている(img は最後だけ読み戻すので GL で直接見る)
                        let mut p = [0u8; 4];
                        unsafe {
                            gl.read_pixels(
                                45,
                                16,
                                1,
                                1,
                                glow::RGBA,
                                glow::UNSIGNED_BYTE,
                                glow::PixelPackData::Slice(Some(&mut p)),
                            );
                        }
                        assert_eq!(p, [255, 255, 0, 255], "Keep は前回の重ね絵のまま");
                    }
                }
                s.release(gl);
                outs
            })
            .unwrap();
        assert!(outs.iter().all(|o| *o == SceneOutcome::Drawn));
        assert_eq!(
            px(&img, 45, 16),
            [0, 0, 255, 255],
            "Off のあとの Keep は重ねない"
        );
        assert_scene_pixels(&img, REGION);
    }

    #[test]
    fn scene_follows_region_and_layer_is_recreated_when_size_changes() {
        let Some(off) = offscreen() else { return };
        let base = solid(BASE, SCREEN);
        let full = Region {
            x: 0,
            y: 0,
            w: SCREEN.0,
            h: SCREEN.1,
        };
        let right = Region {
            x: 40,
            y: 16,
            w: 24,
            h: 16,
        };
        for regions in [[REGION, full], [full, right]] {
            let c = Counters::default();
            let ((), img) = off
                .render_with_compositor(SCREEN, |gl, comp, target| {
                    let mut s = TestScene::new(&c, Fail::None, false);
                    for r in regions {
                        let o = comp
                            .draw_scene(gl, target, &mut frame(&mut s, &base, r, SceneTop::Off))
                            .unwrap();
                        assert_eq!(o, SceneOutcome::Drawn);
                    }
                    s.release(gl);
                })
                .unwrap();
            assert_scene_pixels(&img, regions[1]);
            assert_outside_is_base(&img, regions[1]);
        }
    }

    /// 本番の `GlesDisplay::present_frame` と同じ呼び方(framebuffer を bind するだけ)で、2D のフレームを描く。
    /// 0 = 写真(フェードと重ね絵つき)、1 = Radar、2 = Agent(拡大した 2x2 のカードと scrim)。
    /// どれも画面全体を不透明な下地から描くので、直前の 3D の絵は残らない。
    const FRAMES_2D: usize = 3;

    fn draw_2d(
        gl: &glow::Context,
        comp: &mut Compositor,
        target: Option<glow::Framebuffer>,
        kind: usize,
    ) {
        use crate::compose::{AgentGpuCard, Radar};
        let (old, new, top) = (
            solid([200, 40, 10, 255], SCREEN),
            solid([20, 100, 240, 255], SCREEN),
            top_image(),
        );
        let bg = solid([8, 12, 20, 255], SCREEN);
        let card = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let layer = |id, rgba| PhotoLayer {
            id,
            src: PhotoSource::Frame(rgba),
        };
        let blips = [[40.0, 12.0, 0.0], [20.0, 20.0, 0.0]];
        let cards = [AgentGpuCard {
            id: 7,
            revision: 1,
            pixels: &card,
            size: (2, 2),
            rect: (8.0, 4.0, 24.0, 16.0),
            alpha: 0.9,
        }];
        let frame = match kind {
            0 => Frame::Photo {
                photo: layer(2, &new),
                old: Some((layer(1, &old), 0.5)),
                overlay: Some(&top),
            },
            1 => Frame::Radar {
                background: Some(&bg),
                radar: Radar {
                    center: (32.0, 16.0),
                    radius: 14.0,
                    sweep: 1.0,
                    trail: 0.9,
                    scale: 1.0,
                    blips: &blips,
                },
            },
            _ => Frame::Agents {
                background: Some(&bg),
                cards: &cards,
                scrim_alpha: 0.3,
                focus_card_id: None,
            },
        };
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, target) };
        comp.draw(gl, &frame).unwrap();
    }

    /// 3D の場面がどれだけ状態を汚しても、同じフレームの層の合成と重ね絵、次の 2D の下地(本番と同じ呼び方)、
    /// 次の 3D の場面が、汚さなかったときと同じになる。
    #[test]
    fn dirty_scene_does_not_leak_into_composite_or_following_frames() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        let c = Counters::default();
        // 3D(dirty か clean)のあとに続けるもの
        #[derive(Clone, Copy, Debug)]
        enum Then {
            /// 3D の場面。`contract` なら draw の頭で §5.4 の約束も確かめる(なしなら絵だけで確かめる)
            Scene {
                contract: bool,
            },
            Frame2d(usize),
        }
        let run = |dirty: bool, then: Then| {
            let c = c.clone();
            off.render_with_compositor(SCREEN, |gl, comp, target| {
                let mut a = TestScene::new(&c, Fail::None, dirty);
                let o = comp
                    .draw_scene(
                        gl,
                        target,
                        &mut frame(&mut a, &base, REGION, SceneTop::Upload(&top)),
                    )
                    .unwrap();
                assert_eq!(o, SceneOutcome::Drawn);
                let mid = match then {
                    Then::Frame2d(kind) => {
                        draw_2d(gl, comp, target, kind);
                        None
                    }
                    Then::Scene { contract } => {
                        let mut b = TestScene::new(&c, Fail::None, false);
                        b.check_contract = contract;
                        let o = comp
                            .draw_scene(
                                gl,
                                target,
                                &mut frame(&mut b, &base, REGION, SceneTop::Keep),
                            )
                            .unwrap();
                        b.release(gl);
                        Some(o)
                    }
                };
                a.release(gl);
                mid
            })
            .unwrap()
        };
        let mut thens = vec![
            Then::Scene { contract: true },
            Then::Scene { contract: false },
        ];
        thens.extend((0..FRAMES_2D).map(Then::Frame2d));
        for then in thens {
            let (clean_mid, clean) = run(false, then);
            let (dirty_mid, dirty) = run(true, then);
            assert_eq!(dirty_mid, clean_mid, "{then:?}");
            if matches!(then, Then::Scene { .. }) {
                assert_eq!(
                    dirty_mid,
                    Some(SceneOutcome::Drawn),
                    "前の場面の状態が漏れた: {then:?}"
                );
                assert_scene_pixels(&dirty, REGION);
                assert_top_over(&dirty);
            }
            assert_eq!(diff_rgb(&clean, &dirty, 0), (0, 0), "{then:?}");
        }
        assert_eq!(c.created.get(), c.deleted.get());
    }

    /// 失敗した 3D(panic・`Err`・GL エラー。状態を汚したまま)のあとでも、次の 2D のフレーム(本番と同じ呼び方)と
    /// 次の 3D の場面(前後関係を絵で確かめる)が、成功した汚れのない 3D のあとと同じになる。
    #[test]
    fn failed_scene_does_not_leak_into_following_frames() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        let c = Counters::default();
        let run = |fail: Fail, dirty: bool, kind: Option<usize>| {
            let c = c.clone();
            off.render_with_compositor(SCREEN, |gl, comp, target| {
                let mut a = TestScene::new(&c, fail, dirty);
                let o = comp
                    .draw_scene(
                        gl,
                        target,
                        &mut frame(&mut a, &base, REGION, SceneTop::Upload(&top)),
                    )
                    .unwrap();
                assert_eq!(
                    matches!(o, SceneOutcome::SceneFailed(_)),
                    fail != Fail::None,
                    "{fail:?}"
                );
                let again = match kind {
                    Some(kind) => {
                        draw_2d(gl, comp, target, kind);
                        None
                    }
                    None => {
                        let mut b = TestScene::new(&c, Fail::None, false);
                        b.check_contract = false;
                        let o = comp
                            .draw_scene(
                                gl,
                                target,
                                &mut frame(&mut b, &base, REGION, SceneTop::Keep),
                            )
                            .unwrap();
                        b.release(gl);
                        Some(o)
                    }
                };
                a.release(gl);
                again
            })
            .unwrap()
        };
        let kinds = std::iter::once(None).chain((0..FRAMES_2D).map(Some));
        for kind in kinds {
            let (_, clean) = run(Fail::None, false, kind);
            for fail in [Fail::None, Fail::Panic, Fail::Err, Fail::GlError] {
                let (again, img) = run(fail, true, kind);
                assert_eq!(diff_rgb(&clean, &img, 0), (0, 0), "{fail:?} kind={kind:?}");
                if kind.is_none() {
                    assert_eq!(again, Some(SceneOutcome::Drawn), "{fail:?}");
                    assert_scene_pixels(&img, REGION);
                }
            }
        }
        assert_eq!(c.created.get(), c.deleted.get());
    }

    #[test]
    fn failed_scene_is_released_and_frame_is_completed_without_it() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        let cases = [
            (Fail::PanicMidInit, "初期化の途中で panic"),
            (Fail::Panic, "draw の途中で panic"),
            (Fail::Err, "draw が Err"),
            (Fail::GlError, "GL エラー"),
            (Fail::ErrAndReleasePanic, "release も panic"),
        ];
        for dirty in [false, true] {
            for (fail, want) in cases {
                let c = Counters::default();
                let ((outcome, again), img) = off
                    .render_with_compositor(SCREEN, |gl, comp, target| {
                        let mut s = TestScene::new(&c, fail, dirty);
                        let outcome = comp
                            .draw_scene(
                                gl,
                                target,
                                &mut frame(&mut s, &base, REGION, SceneTop::Upload(&top)),
                            )
                            .unwrap();
                        // 失敗直後のフレームは 3D 抜きの下地と重ね絵で完成している
                        let mut now = vec![0u8; 4];
                        unsafe {
                            gl.read_pixels(
                                20,
                                SCREEN.1 as i32 - 1 - 16,
                                1,
                                1,
                                glow::RGBA,
                                glow::UNSIGNED_BYTE,
                                glow::PixelPackData::Slice(Some(&mut now)),
                            );
                        }
                        assert_eq!(now, BASE, "{fail:?}: 3D の中途半端な絵が出ていない");
                        // 同じ Compositor で、新しい場面が次のフレームから普通に描ける
                        let mut s2 = TestScene::new(&c, Fail::None, false);
                        let again = comp
                            .draw_scene(
                                gl,
                                target,
                                &mut frame(&mut s2, &base, REGION, SceneTop::Keep),
                            )
                            .unwrap();
                        s2.release(gl);
                        (outcome, again)
                    })
                    .unwrap();
                let SceneOutcome::SceneFailed(msg) = outcome else {
                    panic!("{fail:?} dirty={dirty}: 失敗が返らない");
                };
                assert!(msg.contains(want), "{fail:?}: {msg}");
                assert_eq!(
                    c.releases.get(),
                    2,
                    "{fail:?}: 失敗した場面と次の場面の release"
                );
                if fail != Fail::ErrAndReleasePanic {
                    assert_eq!(
                        c.created.get(),
                        c.deleted.get(),
                        "{fail:?}: 作りかけも消える"
                    );
                }
                assert_eq!(again, SceneOutcome::Drawn, "{fail:?}: 次のフレーム");
                assert_scene_pixels(&img, REGION);
                assert_top_over(&img);
            }
        }
    }

    /// 場面が失敗したあとに、最上段の重ね絵が失敗して `Err` になっても、場面の失敗は `scene_failed` で運ぶ
    /// (呼び出し側が `release` 済みの場面を解放済みと分かるように)。場面が成功していたなら `scene_failed` は無い。
    #[test]
    fn scene_failure_survives_a_later_fatal_error() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        for (fail, want) in [(Fail::Err, Some("draw が Err")), (Fail::None, None)] {
            let c = Counters::default();
            let ((err, ok), _) = off
                .render_with_compositor(SCREEN, |gl, comp, target| {
                    let mut s = TestScene::new(&c, fail, false);
                    comp.scene.fail_top = Some("重ね絵の転送に失敗".into());
                    let err = comp
                        .draw_scene(
                            gl,
                            target,
                            &mut frame(&mut s, &base, REGION, SceneTop::Upload(&top)),
                        )
                        .expect_err("最上段の失敗は Err");
                    // 致命的な失敗のあとでも、同じ Compositor で次のフレームを描ける
                    let mut s2 = TestScene::new(&c, Fail::None, false);
                    let ok = comp.draw_scene(
                        gl,
                        target,
                        &mut frame(&mut s2, &base, REGION, SceneTop::Upload(&top)),
                    );
                    s2.release(gl);
                    s.release(gl);
                    (err, ok)
                })
                .unwrap();
            assert_eq!(err.message, "重ね絵の転送に失敗", "{fail:?}");
            match want {
                Some(w) => assert!(
                    err.scene_failed.as_deref().is_some_and(|m| m.contains(w)),
                    "{fail:?}: {:?}",
                    err.scene_failed
                ),
                None => assert_eq!(err.scene_failed, None, "{fail:?}"),
            }
            assert_eq!(ok, Ok(SceneOutcome::Drawn), "{fail:?}");
        }
    }

    /// 失敗した場面は、`release` のあとでも `draw` で作り直せる(契約。v1 は失敗後に draw しないが単体で確かめる)
    #[test]
    fn released_scene_can_draw_again() {
        let Some(off) = offscreen() else { return };
        let base = solid(BASE, SCREEN);
        let c = Counters::default();
        let ((), img) = off
            .render_with_compositor(SCREEN, |gl, comp, target| {
                let mut s = TestScene::new(&c, Fail::PanicMidInit, false);
                let o = comp
                    .draw_scene(gl, target, &mut frame(&mut s, &base, REGION, SceneTop::Off))
                    .unwrap();
                assert!(matches!(o, SceneOutcome::SceneFailed(_)));
                assert_eq!(
                    (c.created.get(), c.deleted.get()),
                    (1, 1),
                    "プログラムだけ作りかけ"
                );
                s.fail = Fail::None;
                let o = comp
                    .draw_scene(gl, target, &mut frame(&mut s, &base, REGION, SceneTop::Off))
                    .unwrap();
                assert_eq!(o, SceneOutcome::Drawn);
                s.release(gl);
            })
            .unwrap();
        assert_eq!(c.created.get(), c.deleted.get());
        assert_scene_pixels(&img, REGION);
    }

    /// 下地の描画で出た GL エラーは、3D の失敗にしない
    #[test]
    fn gl_error_before_draw_is_not_a_scene_failure() {
        let Some(off) = offscreen() else { return };
        let base = solid(BASE, SCREEN);
        let c = Counters::default();
        let (outcome, _) = off
            .render_with_compositor(SCREEN, |gl, comp, target| {
                unsafe { gl.enable(0xFFFF) };
                let mut s = TestScene::new(&c, Fail::None, false);
                let o = comp
                    .draw_scene(gl, target, &mut frame(&mut s, &base, REGION, SceneTop::Off))
                    .unwrap();
                s.release(gl);
                o
            })
            .unwrap();
        assert_eq!(outcome, SceneOutcome::Drawn);
    }

    #[test]
    fn invalid_region_is_a_scene_failure_not_an_error() {
        let Some(off) = offscreen() else { return };
        let (base, top) = (solid(BASE, SCREEN), top_image());
        for region in [
            Region {
                x: 40,
                y: 0,
                w: 32,
                h: 16,
            },
            Region {
                x: 0,
                y: 20,
                w: 8,
                h: 16,
            },
            Region {
                x: 0,
                y: 0,
                w: 0,
                h: 16,
            },
            Region {
                x: u32::MAX,
                y: 0,
                w: 2,
                h: 2,
            },
        ] {
            let c = Counters::default();
            let mut s = TestScene::new(&c, Fail::None, false);
            let (outcome, img) = off
                .render_scene(
                    &mut frame(&mut s, &base, region, SceneTop::Upload(&top)),
                    SCREEN,
                )
                .unwrap();
            assert!(
                matches!(outcome, SceneOutcome::SceneFailed(_)),
                "{region:?}"
            );
            assert_eq!(c.draws.get(), 0, "{region:?}: draw は呼ばない");
            assert_eq!(c.releases.get(), 1, "{region:?}");
            assert_eq!(px(&img, 5, 5), BASE);
            assert_top_over(&img);
        }
    }

    #[test]
    fn base_none_is_black() {
        let Some(off) = offscreen() else { return };
        let c = Counters::default();
        let mut s = TestScene::new(&c, Fail::None, false);
        let (outcome, img) = off
            .render_scene(
                &mut SceneFrame {
                    base: None,
                    scene: &mut s,
                    region: REGION,
                    top: SceneTop::Off,
                },
                SCREEN,
            )
            .unwrap();
        assert_eq!(outcome, SceneOutcome::Drawn);
        assert_eq!(px(&img, 2, 2), [0, 0, 0, 255]);
        assert_eq!(
            at(&img, REGION, 0.8, 0.05),
            [0, 0, 0, 255],
            "透明な所は黒の下地"
        );
        assert_eq!(at(&img, REGION, 0.8, 0.5), [0, 0, 255, 255]);
    }

    #[test]
    fn reset_gl_state_leaves_no_gl_error() {
        let Some(off) = offscreen() else { return };
        let ((), _) = off
            .render_with_compositor(SCREEN, |gl, _, target| {
                unsafe { while gl.get_error() != glow::NO_ERROR {} }
                Compositor::reset_gl_state(gl, SCREEN, target);
                assert_eq!(unsafe { gl.get_error() }, glow::NO_ERROR);
            })
            .unwrap();
    }
}
