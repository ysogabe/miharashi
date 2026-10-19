//! キャラクターを `GlScene` として画面に出す場面(Issue #71 の B、docs/DESIGN-screen-plugin-v2.md §2・§3.5)。
//! 層(透明な FBO)に描くだけで、背景は塗らず、層を画面へ重ねるのも Compositor の仕事。
//!
//! CPU 側(モデルの解析結果 `Rc<Model>`)と GL 側(`Renderer` が持つバッファ・テクスチャ・プログラム)を分ける。
//! `release` は GL 側だけを消し、解析結果は残す。次の `draw` が同じ `Rc<Model>` から転送し直す。
//! 読み込み(`Model::from_path`)は別スレッドで走らせ、`draw` は終わるまで何も描かずに `Ok` を返す。
//! モデルがない・壊れている・読み込みスレッドが落ちた・GPU に転送できない、のいずれも 3D なしで続ける
//! (`draw` は `Err` にしない。`Err` は ScreenManager に 3D の失敗として扱われ、場面ごと捨てられる)。
//! その理由は 1 回だけログに出し、`state` / `error` で外から問い合わせられる。
use crate::gesture::Aim;
use crate::model::Model;
use crate::render::{Framing, Level, Renderer};
use miharashi_glscene::{GlScene, GlTarget, ObjectCount};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// 次の `draw` が使う入力(時刻と演出)。表情・口パク・身振りの組み立ては呼び出し側の仕事で、
/// この場面は渡されたものを `Renderer` に流して描くだけ。
#[derive(Clone, Debug)]
pub struct SceneInput {
    /// 待機モーション・身振りの時刻 [秒]
    pub t: f32,
    /// 前の `draw` からの経過 [秒](SpringBone の積分に使う)。0 なら揺れは進まない
    pub dt: f32,
    /// 構図
    pub framing: Framing,
    /// カメラをモデルのまわりで回す角度 [rad]
    pub yaw: f32,
    /// expression の weight。ここに挙げないものは 0 に戻る(このモデルにない名前は外し、ログに 1 回出す。
    /// ほかの名前は適用する)
    pub expressions: Vec<(String, f32)>,
    /// 身振りを付けるか(humanoid の腕の骨がないモデルでは付かない)
    pub gesture: bool,
    /// 身振りに重ねる視線と指さし
    pub aim: Option<Aim>,
}

impl Default for SceneInput {
    fn default() -> Self {
        Self {
            t: 0.0,
            dt: 0.0,
            framing: Framing::Full,
            yaw: 0.0,
            expressions: Vec::new(),
            gesture: false,
            aim: None,
        }
    }
}

/// モデルの読み込みの状況。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// 別スレッドで読んでいる最中。`draw` は何も描かない
    Loading,
    /// 解析が済んでいる(GPU への転送は最初の `draw`、`release` のあとは次の `draw`)
    Ready,
    /// 読めない・転送できないなどで、3D なしで続けている。理由は `VrmScene::error`
    Failed,
}

enum Load {
    Loading(Receiver<Result<Model, String>>),
    Ready(Rc<Model>),
    Failed(String),
}

pub struct VrmScene {
    count: Rc<ObjectCount>,
    load: Load,
    level: Level,
    /// GL 側。最初の `draw` で作り、`release` で消す
    renderer: Option<Renderer>,
    /// 次の `draw` が使う入力
    pub input: SceneInput,
    /// `renderer` に反映済みの身振りの有無(`renderer` を作り直すと `None` に戻る)
    gesture_applied: Option<bool>,
    /// 入力の不具合(未知の expression・身振りを付けられないモデル)をログに出したか
    input_warned: bool,
}

/// `catch_unwind` の payload から文言を取り出す。
fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

impl VrmScene {
    fn with_load(count: Rc<ObjectCount>, load: Load) -> Self {
        Self {
            count,
            load,
            level: Level::Spring,
            renderer: None,
            input: SceneInput::default(),
            gesture_applied: None,
            input_warned: false,
        }
    }

    /// `path` の glb を別スレッドで読む。読み終わるまでの `draw` は何も描かない。
    pub fn from_path(path: PathBuf, count: Rc<ObjectCount>) -> Self {
        Self::from_loader(count, move || Model::from_path(&path))
    }

    /// `load` を別スレッドで走らせる(試験で、遅い読み込みや panic を作るための入口でもある)。
    /// `load` の panic は `Failed` として受け、スレッドを作れなかったときも `Failed` にする。
    pub fn from_loader(
        count: Rc<ObjectCount>,
        load: impl FnOnce() -> Result<Model, String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("vrm-load".into())
            .spawn(move || {
                let r = catch_unwind(AssertUnwindSafe(load))
                    .unwrap_or_else(|p| Err(format!("読み込みが panic: {}", panic_message(&*p))));
                // 受け手(場面)が先に捨てられていたら送れないが、それでよい
                let _ = tx.send(r);
            });
        match spawned {
            Ok(_) => Self::with_load(count, Load::Loading(rx)),
            Err(e) => {
                let mut s = Self::with_load(count, Load::Failed(String::new()));
                s.fail(format!("読み込みスレッドを作れない: {e}"));
                s
            }
        }
    }

    /// 解析済みのモデルから始める(読み込みを待たない)。
    pub fn from_model(model: Model, count: Rc<ObjectCount>) -> Self {
        Self::with_load(count, Load::Ready(Rc::new(model)))
    }

    /// 機能段階を選ぶ(既定は最大の `Spring`)。最初の `draw` の前に呼ぶ。
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// 読み込みの状況。`Loading` から `Ready`・`Failed` への切り替えは `poll` か `draw` で起きる。
    pub fn state(&self) -> LoadState {
        match self.load {
            Load::Loading(_) => LoadState::Loading,
            Load::Ready(_) => LoadState::Ready,
            Load::Failed(_) => LoadState::Failed,
        }
    }

    /// `Failed` の理由。
    pub fn error(&self) -> Option<&str> {
        match &self.load {
            Load::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// 場面が作って、まだ消していない GL オブジェクトの数(`ObjectCount::live`)。
    pub fn gl_live_objects(&self) -> i64 {
        self.count.live()
    }

    /// 読み込みが終わっていれば結果を取り込む(`draw` も呼ぶ)。呼び出し側の `tick` から呼べば、
    /// `draw` が来る前に `Loading` が `Ready`・`Failed` に変わる。
    pub fn poll(&mut self) {
        let received = match &self.load {
            Load::Loading(rx) => rx.try_recv(),
            _ => return,
        };
        match received {
            Ok(Ok(model)) => self.load = Load::Ready(Rc::new(model)),
            Ok(Err(e)) => self.fail(format!("モデルを読めない: {e}")),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.fail("読み込みスレッドが結果を返さずに終わった".into())
            }
        }
    }

    /// 3D なしで続ける状態にして、理由を 1 回だけログに出す(`Failed` からは出ない)。
    fn fail(&mut self, reason: String) {
        eprintln!("vrm: {reason}(3D なしで続ける)");
        self.load = Load::Failed(reason);
    }

    /// 入力を `Renderer` に流す。身振りを付けられない・未知の expression は、ログに 1 回出して続ける。
    fn apply_input(&mut self, r: &mut Renderer) {
        let mut problem = None;
        if self.gesture_applied != Some(self.input.gesture) {
            // 反映済みとして扱う(付けられないモデルで毎フレーム試さない)
            self.gesture_applied = Some(self.input.gesture);
            if let Err(e) = r.set_gesture(self.input.gesture) {
                problem = Some(e);
            }
        }
        r.set_aim(self.input.aim);
        // 未知の名前はこのモデルにないものとして外す(`set_expressions` は最初の未知の名前で止まり、
        // 残りの有効な表情まで適用されなくなる)
        let (known, unknown): (Vec<_>, Vec<_>) = self
            .input
            .expressions
            .iter()
            .map(|(n, w)| (n.as_str(), *w))
            .partition(|(n, _)| r.model().vrm.expression(n).is_some());
        if let Some((n, _)) = unknown.first() {
            problem = Some(format!("expression {n} がない(ほかの表情は適用する)"));
        }
        if let Err(e) = r.set_expressions(&known) {
            problem = Some(e);
        }
        if let Some(e) = problem
            && !self.input_warned
        {
            self.input_warned = true;
            eprintln!("vrm: {e}");
        }
    }
}

impl GlScene for VrmScene {
    fn draw(&mut self, gl: &glow::Context, target: &GlTarget) -> Result<(), String> {
        self.poll();
        let model = match &self.load {
            Load::Ready(m) => m.clone(),
            // 読み込み中・失敗: 透明のまま
            _ => return Ok(()),
        };
        if target.size.0 == 0 || target.size.1 == 0 {
            return Ok(());
        }
        if self.renderer.is_none() {
            match Renderer::with_shared(gl, model, self.level, self.count.clone()) {
                Ok(mut r) => {
                    // 層は Compositor が (0,0,0,0) で塗ってある。不透明な既定色で塗り直して潰さない
                    r.clear_color = [0.0; 4];
                    self.gesture_applied = None;
                    self.renderer = Some(r);
                }
                Err(e) => {
                    // 転送できないモデル(関節が多すぎるなど)。作りかけは `with_shared` が消している
                    self.fail(format!("GPU に転送できない: {e}"));
                    return Ok(());
                }
            }
        }
        // 入力の反映のために `renderer` を一時的に外す(`apply_input` が `self` も使うので)
        let mut r = self.renderer.take().expect("直前に作った");
        self.apply_input(&mut r);
        r.animate(self.input.t, self.input.dt);
        r.update(gl);
        let (vp, eye) = r.framing_camera(self.input.framing, target.size, self.input.yaw);
        r.render_camera(gl, target.size, &vp, eye);
        self.renderer = Some(r);
        Ok(())
    }

    fn release(&mut self, gl: &glow::Context) {
        if let Some(r) = self.renderer.take() {
            r.destroy(gl);
        }
        self.gesture_applied = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;
    use glow::HasContext;
    use miharashi_gpu::{Offscreen, render_node_for_pci};
    use serde_json::json;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn synth_model() -> Model {
        Model::from_glb(&synth::build_vrm_glb(&synth::Opts {
            segments: 12,
            texture_size: 64,
        }))
        .unwrap()
    }

    fn offscreen() -> Option<Offscreen> {
        match Offscreen::open(&render_node_for_pci("0000:10:00.0")) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("GLES が使えないので skip: {e}");
                None
            }
        }
    }

    /// Compositor が作る層と同じ形(色 RGBA8 のテクスチャ + 深度ステンシル)の FBO を、(0,0,0,0) で塗って返す。
    unsafe fn make_layer(
        gl: &glow::Context,
        (w, h): (u32, u32),
    ) -> (glow::Framebuffer, glow::Texture, glow::Renderbuffer) {
        unsafe {
            let color = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(color));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, w as i32, h as i32);
            let depth = gl.create_renderbuffer().unwrap();
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::DEPTH24_STENCIL8,
                w as i32,
                h as i32,
            );
            let fbo = gl.create_framebuffer().unwrap();
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(color),
                0,
            );
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_STENCIL_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(depth),
            );
            assert_eq!(
                gl.check_framebuffer_status(glow::FRAMEBUFFER),
                glow::FRAMEBUFFER_COMPLETE
            );
            (fbo, color, depth)
        }
    }

    /// 層を (0,0,0,0) で塗り、Compositor の `draw` と同じ条件で `scene.draw` を呼び、層の RGBA を読む。
    fn draw_layer(gl: &glow::Context, scene: &mut VrmScene, size: (u32, u32)) -> Vec<u8> {
        unsafe {
            let (fbo, color, depth) = make_layer(gl, size);
            gl.viewport(0, 0, size.0 as i32, size.1 as i32);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear_depth_f32(1.0);
            gl.clear_stencil(0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT);
            scene.draw(gl, &GlTarget { fbo, size }).unwrap();
            assert_eq!(gl.get_error(), glow::NO_ERROR, "draw のあとの GL エラー");
            let mut out = vec![0u8; size.0 as usize * size.1 as usize * 4];
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.read_pixels(
                0,
                0,
                size.0 as i32,
                size.1 as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut out)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.delete_framebuffer(fbo);
            gl.delete_texture(color);
            gl.delete_renderbuffer(depth);
            out
        }
    }

    fn no_gl_error(gl: &glow::Context, what: &str) {
        assert_eq!(unsafe { gl.get_error() }, glow::NO_ERROR, "{what}");
    }

    /// 透明な背景に描かれ、モデルのある所だけアルファがある(不透明な既定色で層を潰さない)。
    #[test]
    fn draws_on_transparent_background() {
        let Some(off) = offscreen() else { return };
        let size = (200u32, 300u32);
        let count = Rc::new(ObjectCount::default());
        let (img, _) = off
            .with_fbo(size, |gl| {
                let mut scene = VrmScene::from_model(synth_model(), count.clone());
                let img = draw_layer(gl, &mut scene, size);
                scene.release(gl);
                img
            })
            .unwrap();
        let px = img.as_chunks::<4>().0;
        let covered = px.iter().filter(|p| p[3] > 0).count();
        let opaque = px.iter().filter(|p| p[3] == 255).count();
        assert!(covered > 1000, "モデルの画素が少ない: {covered}");
        assert!(covered < px.len() * 9 / 10, "層の全面にアルファがある");
        assert!(opaque > 500, "不透明な画素が少ない: {opaque}");
        // 4 隅は何も描かれない(モデルは中央に収まる)。透明な画素は色も 0
        for &(x, y) in &[(0, 0), (199, 0), (0, 299), (199, 299)] {
            assert_eq!(px[y * 200 + x], [0, 0, 0, 0], "隅 ({x},{y}) が透明でない");
        }
        // premultiplied alpha: 色はアルファを超えない
        assert!(
            px.iter().all(|p| p[..3].iter().all(|&c| c <= p[3])),
            "色がアルファを超える(premultiplied でない)"
        );
        assert_eq!(count.live(), 0);
    }

    /// release → draw の作り直しで同じ絵になる。release のあとは生きている GL オブジェクトが 0。
    /// release は 2 回呼んでも安全。
    #[test]
    fn release_then_redraw_matches_and_is_idempotent() {
        let Some(off) = offscreen() else { return };
        let size = (160u32, 240u32);
        let count = Rc::new(ObjectCount::default());
        let (res, _) = off
            .with_fbo(size, |gl| {
                let mut scene = VrmScene::from_model(synth_model(), count.clone());
                scene.input.gesture = true;
                scene.input.t = 1.5;
                let first = draw_layer(gl, &mut scene, size);
                assert!(
                    count.live() > 0,
                    "描いたのに GL オブジェクトが数えられていない"
                );
                let created = count.created();
                scene.release(gl);
                assert_eq!(count.live(), 0, "release のあとにオブジェクトが残る");
                no_gl_error(gl, "release のあと");
                // 2 回目の release は何もしない
                scene.release(gl);
                assert_eq!(count.live(), 0);
                assert_eq!(count.created(), created, "2 回目の release が何か作った");
                no_gl_error(gl, "2 回目の release のあと");
                // 解析結果は残っていて、次の draw で作り直せる
                assert_eq!(scene.state(), LoadState::Ready);
                let second = draw_layer(gl, &mut scene, size);
                assert!(count.live() > 0);
                assert_eq!(count.created(), created * 2, "作り直しの数が違う");
                scene.release(gl);
                scene.release(gl);
                assert_eq!(count.live(), 0);
                no_gl_error(gl, "最後");
                (first, second)
            })
            .unwrap();
        let (first, second) = res;
        assert!(first.iter().any(|&b| b != 0), "何も描かれていない");
        assert_eq!(first, second, "作り直したら絵が変わった");
    }

    /// 実際の経路(`Compositor::draw_scene`)で、GL エラーや panic なしに `Drawn` で描かれ、領域の外は下地のまま、
    /// 領域の中にモデルが重なる。release のあとの描き直しも同じ絵になる。
    #[test]
    fn draws_through_compositor_and_survives_release() {
        use miharashi_gpu::{Region, SceneFrame, SceneOutcome, SceneTop, compose::Compositor};
        let Some(off) = offscreen() else { return };
        let size = (300u32, 200u32);
        let region = Region {
            x: 20,
            y: 10,
            w: 100,
            h: 180,
        };
        let count = Rc::new(ObjectCount::default());
        let (shots, img) = off
            .render_with_compositor(size, |gl, comp: &mut Compositor, target| {
                let mut scene = VrmScene::from_model(synth_model(), count.clone());
                let mut shots = Vec::new();
                for _ in 0..2 {
                    let mut frame = SceneFrame {
                        base: None,
                        scene: &mut scene,
                        region,
                        top: SceneTop::Off,
                    };
                    let outcome = comp.draw_scene(gl, target, &mut frame).unwrap();
                    assert_eq!(outcome, SceneOutcome::Drawn);
                    let mut out = vec![0u8; size.0 as usize * size.1 as usize * 4];
                    let mut scratch = Vec::new();
                    miharashi_gpu::read_rgba_top_down(
                        gl,
                        target.unwrap(),
                        size,
                        &mut out,
                        &mut scratch,
                    );
                    shots.push(out);
                    let panics = Compositor::release_scenes(gl, size, target, &mut [&mut scene]);
                    assert!(panics.is_empty(), "{panics:?}");
                    assert_eq!(count.live(), 0);
                }
                shots
            })
            .unwrap();
        drop(img);
        assert_eq!(shots[0], shots[1], "release のあとの描き直しで絵が変わった");
        let px = |x: u32, y: u32| &shots[0][((y * size.0 + x) * 4) as usize..][..4];
        // 下地は黒(不透明)。領域の外はそのまま
        for (x, y) in [(0, 0), (299, 199), (150, 100), (130, 5)] {
            assert_eq!(px(x, y), [0, 0, 0, 255], "領域の外 ({x},{y}) が変わった");
        }
        let inside = (region.y..region.y + region.h)
            .flat_map(|y| (region.x..region.x + region.w).map(move |x| (x, y)))
            .filter(|&(x, y)| px(x, y)[..3] != [0, 0, 0])
            .count();
        assert!(inside > 500, "領域の中にモデルが重なっていない: {inside}");
    }

    /// 表情の入力: このモデルにない名前が混ざっても、ほかの表情は適用される。
    #[test]
    fn unknown_expression_does_not_block_the_others() {
        let Some(off) = offscreen() else { return };
        let count = Rc::new(ObjectCount::default());
        off.with_fbo((64, 64), |gl| {
            let mut scene = VrmScene::from_model(synth_model(), count.clone());
            let _ = draw_layer(gl, &mut scene, (64, 64));
            let base = scene.renderer.as_ref().unwrap().morphed_vertices(0);
            scene.input.expressions = vec![("nothing".into(), 1.0), ("blink".into(), 1.0)];
            let _ = draw_layer(gl, &mut scene, (64, 64));
            let blinked = scene.renderer.as_ref().unwrap().morphed_vertices(0);
            assert_ne!(base, blinked, "未知の名前のせいで blink が適用されない");
            assert!(scene.input_warned, "未知の名前がログに出ていない");
            // 空にすると 0 に戻る
            scene.input.expressions.clear();
            let _ = draw_layer(gl, &mut scene, (64, 64));
            assert_eq!(base, scene.renderer.as_ref().unwrap().morphed_vertices(0));
            scene.release(gl);
        })
        .unwrap();
    }

    /// 描かずに release しても安全(作っていないものは消さない)で、数えもずれない。
    #[test]
    fn release_before_draw_is_safe() {
        let Some(off) = offscreen() else { return };
        let count = Rc::new(ObjectCount::default());
        off.with_fbo((8, 8), |gl| {
            let mut scene = VrmScene::from_model(synth_model(), count.clone());
            scene.release(gl);
            scene.release(gl);
            assert_eq!((count.created(), count.deleted()), (0, 0));
            no_gl_error(gl, "draw の前の release");
        })
        .unwrap();
    }

    /// 転送できないモデル(関節が多すぎる)は、Err にせず 3D なしで続け、作りかけは消える。
    #[test]
    fn untransferable_model_fails_softly_without_leaks() {
        let Some(off) = offscreen() else { return };
        let count = Rc::new(ObjectCount::default());
        off.with_fbo((64, 64), |gl| {
            let mut model = synth_model();
            // 上限(256)を超える関節を持つことにして、転送の検査で弾かせる
            let joint = model.joint_nodes[0];
            model.joint_nodes = vec![joint; crate::render::MAX_JOINTS + 1];
            let mut scene = VrmScene::from_model(model, count.clone());
            let img = draw_layer(gl, &mut scene, (64, 64));
            assert!(img.iter().all(|&b| b == 0), "失敗したのに描かれている");
            assert_eq!(scene.state(), LoadState::Failed);
            assert!(scene.error().unwrap().contains("関節が多すぎる"));
            assert_eq!(count.live(), 0, "作りかけが残る");
            // 失敗したあとの draw / release も安全
            let _ = draw_layer(gl, &mut scene, (64, 64));
            scene.release(gl);
            assert_eq!(count.live(), 0);
        })
        .unwrap();
    }

    /// 読み込みが終わるまで何も描かず Ok を返し、終わったあとの draw で描かれる。
    #[test]
    fn loads_on_another_thread_then_draws() {
        let Some(off) = offscreen() else { return };
        let size = (160u32, 240u32);
        let count = Rc::new(ObjectCount::default());
        let (gate_tx, gate_rx) = channel::<()>();
        let glb = synth::build_vrm_glb(&synth::Opts {
            segments: 12,
            texture_size: 64,
        });
        off.with_fbo(size, |gl| {
            let mut scene = VrmScene::from_loader(count.clone(), move || {
                // 合図が来るまで読み込みを終えない(draw が待たされないことの確認)
                gate_rx.recv().map_err(|e| e.to_string())?;
                Model::from_glb(&glb)
            });
            assert_eq!(scene.state(), LoadState::Loading);
            let t0 = Instant::now();
            let loading = draw_layer(gl, &mut scene, size);
            assert!(
                t0.elapsed() < Duration::from_secs(2),
                "draw が読み込みを待った"
            );
            assert!(loading.iter().all(|&b| b == 0), "読み込み中に描かれている");
            assert_eq!(scene.state(), LoadState::Loading);
            assert_eq!(count.created(), 0);
            gate_tx.send(()).unwrap();
            // 読み込みが終わるのを待つ(poll は tick から呼ぶ想定)
            let deadline = Instant::now() + Duration::from_secs(20);
            while scene.state() == LoadState::Loading {
                assert!(Instant::now() < deadline, "読み込みが終わらない");
                std::thread::sleep(Duration::from_millis(5));
                scene.poll();
            }
            assert_eq!(scene.state(), LoadState::Ready);
            let img = draw_layer(gl, &mut scene, size);
            assert!(
                img.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count() > 1000,
                "読み込み後に描かれていない"
            );
            scene.release(gl);
            assert_eq!(count.live(), 0);
        })
        .unwrap();
    }

    /// 存在しないパス・壊れた glb・panic する読み込みでも、draw は Ok を返し続け、描かれず、GL を作らない。
    #[test]
    fn broken_models_never_fail_draw() {
        let Some(off) = offscreen() else { return };
        let dir = std::env::temp_dir().join(format!("vrm-scene-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = synth::build_vrm_glb(&synth::Opts {
            segments: 12,
            texture_size: 64,
        });
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        // sparse の values の byteStride が要素(VEC3 の float = 12 バイト)より小さい glb
        let (mut doc, bin) = synth::build_vrm_doc(&synth::Opts {
            segments: 12,
            texture_size: 64,
        });
        let t = doc["meshes"][0]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let views = doc["bufferViews"].as_array_mut().unwrap();
        views.push(json!({"buffer": 0, "byteOffset": 0, "byteLength": 4}));
        views.push(json!({"buffer": 0, "byteOffset": 0, "byteLength": 24, "byteStride": 4}));
        let (iv, vv) = (views.len() - 2, views.len() - 1);
        doc["accessors"][t]["sparse"] = json!({
            "count": 2,
            "indices": {"bufferView": iv, "componentType": 5123},
            "values": {"bufferView": vv},
        });
        let sparse = write("sparse.glb", &synth::pack_glb(&doc, &bin));
        let cases: Vec<(&str, VrmScene)> = vec![
            (
                "存在しないパス",
                VrmScene::from_path(dir.join("nothing.glb"), Rc::new(ObjectCount::default())),
            ),
            (
                "切り詰めた glb",
                VrmScene::from_path(
                    write("truncated.glb", &good[..good.len() / 2]),
                    Rc::new(ObjectCount::default()),
                ),
            ),
            (
                "sparse の stride が要素より小さい",
                VrmScene::from_path(sparse, Rc::new(ObjectCount::default())),
            ),
            (
                "glb ではない",
                VrmScene::from_path(
                    write("text.glb", b"not a glb at all"),
                    Rc::new(ObjectCount::default()),
                ),
            ),
            (
                "読み込みが panic",
                VrmScene::from_loader(Rc::new(ObjectCount::default()), || {
                    panic!("読み込みの途中で落ちた")
                }),
            ),
        ];
        for (what, mut scene) in cases {
            off.with_fbo((32, 32), |gl| {
                // 読み込みが終わるまで draw を回す。どの draw も Ok(draw_layer が unwrap する)
                let deadline = Instant::now() + Duration::from_secs(20);
                while scene.state() == LoadState::Loading {
                    assert!(Instant::now() < deadline, "{what}: 読み込みが終わらない");
                    let img = draw_layer(gl, &mut scene, (32, 32));
                    assert!(img.iter().all(|&b| b == 0), "{what}: 描かれている");
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert_eq!(scene.state(), LoadState::Failed, "{what}");
                assert!(scene.error().is_some(), "{what}");
                for _ in 0..3 {
                    let img = draw_layer(gl, &mut scene, (32, 32));
                    assert!(img.iter().all(|&b| b == 0), "{what}: 失敗後に描かれている");
                }
                assert_eq!(scene.gl_live_objects(), 0, "{what}");
                scene.release(gl);
                scene.release(gl);
            })
            .unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// GPU が無くても動く確認: 失敗の理由は 1 つに定まり、スレッドの panic は文言に残る。
    #[test]
    fn failure_reasons_are_recorded() {
        let mut scene = VrmScene::from_loader(Rc::new(ObjectCount::default()), || {
            panic!("読み込みの途中で落ちた")
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while scene.state() == LoadState::Loading {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
            scene.poll();
        }
        assert_eq!(scene.state(), LoadState::Failed);
        let e = scene.error().unwrap();
        assert!(
            e.contains("panic") && e.contains("読み込みの途中で落ちた"),
            "{e}"
        );

        let mut scene = VrmScene::from_path(
            "/nonexistent/vrm/model.glb".into(),
            Rc::new(ObjectCount::default()),
        );
        while scene.state() == LoadState::Loading {
            std::thread::sleep(Duration::from_millis(2));
            scene.poll();
        }
        assert!(scene.error().unwrap().contains("model.glb"));
    }
}
