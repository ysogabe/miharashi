//! 3D の仕組み(Issue #74、docs/DESIGN-screen-plugin-v2.md)を確かめるための試験用の場面と画面。
//! `png` の fixture と単体テストが使う。本番の画面一覧(`app::kiosk`)にも設定にも足さない。
//! 本物の 3D の画面(キューブ)は別に作る。
use crate::screen::{
    Composite, GlComposite, GlScene, PhotoData, PhotoRef, Region, RenderContext, Screen, ScreenId,
    Top,
};
use glow::HasContext;
use miharashi_glscene::GlTarget;
use miharashi_render::canvas::rgba;
use miharashi_render::notice;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

/// 場面の下の画面(`CheckScreen` の装飾)の色。
pub const BASE_COLOR: [u8; 4] = [30, 60, 120, 255];
/// 手前の四角(z = -0.5)の色。
#[cfg(test)]
pub const FRONT_COLOR: [u8; 4] = [255, 0, 0, 255];
/// 奥の四角(z = +0.5)の色。
#[cfg(test)]
pub const BACK_COLOR: [u8; 4] = [0, 0, 255, 255];

/// 場面と画面を外から見るための印と、外から動かす指示。
#[derive(Default)]
pub struct Probe {
    /// `GlScene::draw` を呼ばれた回数
    pub draws: Cell<u32>,
    /// `GlScene::release` を呼ばれた回数
    pub releases: Cell<u32>,
    /// true の間、`draw` は panic する
    pub fail_draw: Cell<bool>,
    /// `Screen::render` を呼ばれた回数(CPU の描画)
    pub renders: Cell<u32>,
    /// `Screen::render_composite` を呼ばれた回数
    pub composites: Cell<u32>,
    /// `Screen::invalidate` を呼ばれた回数
    pub invalidates: Cell<u32>,
    /// `Screen::set_presentation` で最後に渡された点灯状態。まだ渡されていなければ None
    pub display_on: Cell<Option<bool>>,
    /// `Screen::scene_animating` の答え
    pub scene_animating: Cell<bool>,
}

/// 深度テストつきの四角 2 枚。手前(赤、左)を先に、奥(青、右)を後に描くので、深度テストが効いていれば
/// 重なりは赤になる(効いていなければ青)。四角は領域の中央の縦半分。
pub struct CheckScene {
    probe: Rc<Probe>,
    program: Option<glow::Program>,
}

const VS: &str = "#version 300 es
const vec4 RECT[2] = vec4[2](vec4(-1.0, -0.5, 0.25, 0.5), vec4(-0.25, -0.5, 1.0, 0.5));
const float Z[2] = float[2](-0.5, 0.5);
const vec3 COL[2] = vec3[2](vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0));
const vec2 P[6] = vec2[6](vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0));
out vec3 v_col;
void main() {
    int q = gl_VertexID / 6;
    vec2 p = P[gl_VertexID % 6];
    vec4 r = RECT[q];
    v_col = COL[q];
    gl_Position = vec4(mix(r.xy, r.zw, p), Z[q], 1.0);
}
";
const FS: &str = "#version 300 es
precision mediump float;
in vec3 v_col;
out vec4 o;
void main() { o = vec4(v_col, 1.0); }
";

impl CheckScene {
    pub fn new(probe: Rc<Probe>) -> Self {
        Self {
            probe,
            program: None,
        }
    }

    fn build(gl: &glow::Context) -> Result<glow::Program, String> {
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
                let sh = gl.create_shader(kind)?;
                gl.shader_source(sh, src);
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    let log = gl.get_shader_info_log(sh);
                    gl.delete_shader(sh);
                    shaders.into_iter().for_each(|s| gl.delete_shader(s));
                    gl.delete_program(program);
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
                gl.delete_shader(sh);
            }
            if !linked {
                gl.delete_program(program);
                return Err(format!("リンク: {log}"));
            }
            Ok(program)
        }
    }
}

impl GlScene for CheckScene {
    fn draw(&mut self, gl: &glow::Context, _target: &GlTarget) -> Result<(), String> {
        self.probe.draws.set(self.probe.draws.get() + 1);
        if self.probe.fail_draw.get() {
            panic!("試験用の失敗");
        }
        let program = match self.program {
            Some(p) => p,
            None => *self.program.insert(Self::build(gl)?),
        };
        unsafe {
            gl.use_program(Some(program));
            gl.enable(glow::DEPTH_TEST);
            gl.draw_arrays(glow::TRIANGLES, 0, 12);
        }
        Ok(())
    }

    fn release(&mut self, gl: &glow::Context) {
        self.probe.releases.set(self.probe.releases.get() + 1);
        if let Some(p) = self.program.take() {
            unsafe { gl.delete_program(p) };
        }
    }
}

/// 試験画面の下地。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Base {
    /// 下地なし(黒)
    None,
    /// 画面の Canvas を `BASE_COLOR` で塗った完成画面
    Canvas,
    /// 写真(重ね絵つき)。重ね絵は `overlay_changed` のときだけ描き直す(写真画面と同じ)
    #[cfg_attr(not(test), allow(dead_code))]
    Photo,
}

/// 領域の中に `CheckScene` を出す試験画面。`gl_only` なら 3D だけの画面(`needs_gl`)で、GL が使えないときは案内を出す。
/// そうでなければ下地(`Base`)に 3D を足す装飾で、GL が使えないときは下地だけを出す。
pub struct CheckScreen {
    id: ScreenId,
    gl_only: bool,
    base: Base,
    scene: CheckScene,
    probe: Rc<Probe>,
    /// 写真の重ね絵を描き直す(`invalidate` で立つ。`Base::Photo`)
    overlay_dirty: bool,
}

impl CheckScreen {
    pub fn new(id: ScreenId, gl_only: bool, base: Base, probe: Rc<Probe>) -> Self {
        Self {
            id,
            gl_only,
            base,
            scene: CheckScene::new(probe.clone()),
            probe,
            overlay_dirty: true,
        }
    }

    /// 3D を置く領域(画面の中央の縦横半分)。
    pub fn region(size: (u32, u32)) -> Region {
        Region {
            x: size.0 / 4,
            y: size.1 / 4,
            w: size.0 / 2,
            h: size.1 / 2,
        }
    }
}

impl Screen for CheckScreen {
    fn id(&self) -> ScreenId {
        self.id
    }

    fn title(&self) -> &'static str {
        "CHECK"
    }

    fn set_presentation(&mut self, _visible: bool, _covered: bool, display_on: bool, _: Instant) {
        self.probe.display_on.set(Some(display_on));
    }

    /// CPU の完成画面。3D だけの画面は案内、装飾は下地。
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        self.probe.renders.set(self.probe.renders.get() + 1);
        if self.gl_only {
            notice::gpu_required(ctx.canvas, ctx.text, self.title());
        } else {
            ctx.canvas
                .clear(rgba(BASE_COLOR[0], BASE_COLOR[1], BASE_COLOR[2], 255));
        }
    }

    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        self.probe.composites.set(self.probe.composites.get() + 1);
        let size = (ctx.canvas.width(), ctx.canvas.height());
        let base = match self.base {
            Base::None => None,
            Base::Canvas => {
                self.render(ctx);
                Some(Box::new(Composite::Canvas))
            }
            Base::Photo => {
                // 写真の重ね絵(左上の白い四角が「写真の文字」)
                let overlay_changed = std::mem::take(&mut self.overlay_dirty) || ctx.force_overlay;
                if overlay_changed {
                    ctx.canvas.clear(rgba(0, 0, 0, 0));
                    ctx.canvas
                        .fill_rect(4.0, 4.0, 24.0, 12.0, rgba(255, 255, 255, 255));
                }
                Some(Box::new(Composite::Photo {
                    photo: PhotoRef {
                        id: 1,
                        data: PhotoData::Resident,
                    },
                    old: None,
                    overlay_changed,
                }))
            }
        };
        Composite::Gl(GlComposite {
            base,
            scene: &mut self.scene,
            region: Self::region(size),
            top: Top::Off,
        })
    }

    fn invalidate(&mut self) {
        self.probe.invalidates.set(self.probe.invalidates.get() + 1);
        self.overlay_dirty = true;
    }

    fn needs_gl(&self) -> bool {
        self.gl_only
    }

    fn gl_scene(&mut self) -> Option<&mut dyn GlScene> {
        Some(&mut self.scene)
    }

    fn scene_animating(&self) -> bool {
        self.probe.scene_animating.get()
    }
}
