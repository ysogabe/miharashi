//! 既存の画面(写真・Agent)に VRM のキャラクターを載せる装飾(Issue #71 B、docs/DESIGN-screen-plugin-v2.md §11.2)。
//! 内側の画面のメソッドはすべて委譲し、`render_composite` の結果を `Composite::Gl { base: 内側, .. }` で包む。
//! `needs_gl()` は false のままで、`id`・操作・巡回・パネルへの見せ方は内側の画面のまま。
//!
//! GL の可否は知らない。GL が使えない・3D が失敗したときに内側の画面だけになるのは ScreenManager の仕事で、
//! `renderer = cpu` や GL なしの起動では `kiosk` がそもそも包まない(モデルを RAM に読まないため)。
//!
//! 演出は PoC-006 と同じ: バストアップ、待機 → 説明 → 両手を広げる → 手を振る → 指さしの身振り、
//! 口パク(母音・まばたき・笑顔)。Agent 画面では、拡大中のカードの方へ頭を向ける
//! (T3 Agent の状態との連動はここまで。指さしはしない)。
//! 時間(`active`)は、表示中かつ点灯中に経った分だけ進める。非表示・消灯の間は止まり、復帰でまとめて進めない。
use crate::screen::{
    Composite, GlComposite, GlScene, KeyLabels, Outcome, Region, RenderContext, Screen,
    ScreenAction, ScreenId, Top, World,
};
use miharashi_vrm::demo::{self, FaceSet};
use miharashi_vrm::gesture::{AimTarget, AimTracker};
use miharashi_vrm::{Framing, LoadState, StageRegion, VrmScene};
use std::time::{Duration, Instant};

/// 前の描画からの経過の上限 [秒]。SpringBone の積分が飛ばないようにする。
const MAX_DT: f32 = 0.1;
/// 頭の画面上の位置(領域の上端からの割合。PoC-006 のバストアップの値)。視線の向きの起点にする。
const HEAD_FRACTION: f32 = 0.22;

pub struct WithScene<S: Screen> {
    inner: S,
    scene: VrmScene,
    region: StageRegion,
    interval: Option<Duration>,
    /// 画面が見えているか(`on_enter` / `on_leave` / `set_presentation`)
    visible: bool,
    display_on: bool,
    /// 表示中かつ点灯中に経った時間。身振り・口パクの時刻はこれで決まる
    active: Duration,
    /// `active` を最後に更新した時刻
    last: Option<Instant>,
    /// 前の描画の時点の `active`(SpringBone の dt を、tick の回数ではなく描画の間隔にする)
    drawn: Duration,
    /// 読み込みが終わった(`Ready` を見た)か。表情の名前はそのとき決める
    ready_seen: bool,
    faces: Option<FaceSet>,
    /// `demo::face` の出力の置き場(毎フレームの確保を避ける)
    face_buf: Vec<(&'static str, f32)>,
    aim: AimTracker,
}

impl<S: Screen> WithScene<S> {
    /// `fps` は動いている間の描画の上限(0 なら制限せず vsync だけ)。
    pub fn new(inner: S, scene: VrmScene, region: StageRegion, fps: u32) -> Self {
        let mut s = Self {
            inner,
            scene,
            region,
            interval: (fps > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(fps))),
            visible: false,
            display_on: true,
            active: Duration::ZERO,
            last: None,
            drawn: Duration::ZERO,
            ready_seen: false,
            faces: None,
            face_buf: Vec::new(),
            aim: AimTracker::default(),
        };
        s.scene.input.framing = Framing::Bust;
        s.scene.input.gesture = true;
        s
    }

    /// 3D を置く領域(画素。原点は左上)。キャラクターの領域は高さが画面いっぱいなので、GL の座標と同じ。
    pub fn region(region: StageRegion, size: (u32, u32)) -> Region {
        let (x, y, w, h) = region.rect(size);
        Region { x, y, w, h }
    }

    /// 場面(試験用)。
    #[cfg(test)]
    pub fn scene(&self) -> &VrmScene {
        &self.scene
    }

    /// 場面の入力(試験用)。
    #[cfg(test)]
    pub fn input(&self) -> &miharashi_vrm::SceneInput {
        &self.scene.input
    }

    /// 動かす時間(試験用)。
    #[cfg(test)]
    pub fn active(&self) -> Duration {
        self.active
    }

    fn running(&self) -> bool {
        self.visible && self.display_on
    }

    /// 動いているあいだだけ `now` まで時間を進め、どちらにしても基準の時刻を `now` にする
    /// (止まっていた間の分を、復帰のときにまとめて進めない)。
    fn advance(&mut self, now: Instant) {
        if self.running()
            && let Some(last) = self.last
        {
            self.active += now.saturating_duration_since(last);
        }
        self.last = Some(now);
    }

    /// 読み込みが終わった最初の 1 回、このモデルが持つ表情の名前で入力を用意する
    /// (ない名前を毎フレーム渡して、ログに警告を出さない)。
    fn on_ready(&mut self) {
        let Some(model) = self.scene.model() else {
            return;
        };
        let faces = FaceSet::of(&model.vrm);
        let mut names: Vec<String> = demo::VOWELS
            .iter()
            .zip(faces.vowels)
            .filter(|(_, has)| *has)
            .map(|(n, _)| n.to_string())
            .collect();
        if faces.blink {
            names.push("blink".into());
        }
        if faces.happy {
            names.push("happy".into());
        }
        self.scene.input.expressions = names.into_iter().map(|n| (n, 0.0)).collect();
        self.faces = Some(faces);
    }

    /// 時刻に応じた表情(口パク・まばたき・笑顔)を入力に反映する。
    fn set_faces(&mut self) {
        let Some(faces) = &self.faces else { return };
        demo::face(self.scene.input.t, faces, &mut self.face_buf);
        for (_, w) in &mut self.scene.input.expressions {
            *w = 0.0;
        }
        for (name, w) in &self.face_buf {
            if let Some(e) = self
                .scene
                .input
                .expressions
                .iter_mut()
                .find(|(n, _)| n == name)
            {
                e.1 = *w;
            }
        }
    }
}

/// Agent 画面の下地から、拡大中(フォーカス中)のカードの中心を取り出す。それ以外の下地は目標なし。
fn focus_target(base: &Composite<'_>) -> Option<AimTarget> {
    let Composite::Agents {
        cards,
        focus_card_id: Some(id),
        ..
    } = base
    else {
        return None;
    };
    let p = cards.iter().find(|c| c.id == *id)?.pose;
    Some(AimTarget {
        center: (p.x + p.width / 2.0, p.y + p.height / 2.0),
        pointing: false,
    })
}

impl<S: Screen> Screen for WithScene<S> {
    fn id(&self) -> ScreenId {
        self.inner.id()
    }

    fn title(&self) -> &'static str {
        self.inner.title()
    }

    fn on_enter(&mut self, now: Instant) {
        self.advance(now);
        self.visible = true;
        // 最初に出るまで読まない(出ない画面のモデルを RAM に置かない)。読み込みは別スレッド
        self.scene.start();
        self.inner.on_enter(now);
    }

    fn on_leave(&mut self, now: Instant) {
        self.advance(now);
        self.visible = false;
        self.inner.on_leave(now);
    }

    fn set_presentation(&mut self, visible: bool, covered: bool, display_on: bool, now: Instant) {
        // 状態が変わる前の分までを数えてから切り替える
        self.advance(now);
        self.visible = visible;
        self.display_on = display_on;
        if visible {
            self.scene.start();
        }
        self.inner
            .set_presentation(visible, covered, display_on, now);
    }

    /// 内側の画面の tick のあと、時間を進め、読み込みの結果を取り込み、身振り・口パクの入力を用意する。
    /// 戻り値は内側の画面のもの。ただし読み込みが終わった周期は true(VRM を出すため描き直す)。
    /// 動き続ける分の再描画は、ここでなく `scene_animating`(ScreenManager が GL ありのときだけ足す)で求める。
    fn tick(&mut self, world: &World, now: Instant) -> bool {
        self.advance(now);
        let redraw = self.inner.tick(world, now);
        self.scene.poll();
        let became_ready = !self.ready_seen && self.scene.state() == LoadState::Ready;
        if became_ready {
            self.ready_seen = true;
            self.on_ready();
        }
        if self.running() {
            self.scene.input.t = self.active.as_secs_f32();
            self.set_faces();
        }
        redraw || became_ready
    }

    fn handle_action(&mut self, action: ScreenAction, now: Instant) -> Outcome {
        self.inner.handle_action(action, now)
    }

    fn key_labels(&self) -> KeyLabels {
        self.inner.key_labels()
    }

    /// CPU の完成画面は内側の画面のまま(3D の CPU 描画はない)。
    fn render(&mut self, ctx: &mut RenderContext<'_>) {
        self.inner.render(ctx);
    }

    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        let size = (ctx.canvas.width(), ctx.canvas.height());
        let Self {
            inner,
            scene,
            region,
            active,
            drawn,
            last,
            aim,
            ..
        } = self;
        let base = inner.render_composite(ctx);
        // SpringBone の時間は、tick の回数でなく描画の間隔で進める
        scene.input.dt = active.saturating_sub(*drawn).as_secs_f32().min(MAX_DT);
        *drawn = *active;
        // 拡大中のカードの方へ頭を向ける。向きは「頭の位置 → カードの中心」で、重みは滑らかに出し入れする
        let (rx, ry, rw, rh) = region.rect(size);
        let head = (
            rx as f32 + rw as f32 / 2.0,
            ry as f32 + rh as f32 * HEAD_FRACTION,
        );
        scene.input.aim = aim.step(
            (*last).unwrap_or_else(Instant::now),
            focus_target(&base),
            head,
            size.1 as f32 * 0.5,
        );
        Composite::Gl(GlComposite {
            base: Some(Box::new(base)),
            scene,
            region: Self::region(*region, size),
            top: Top::Off,
        })
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }

    fn animating(&self) -> bool {
        self.inner.animating()
    }

    /// 装飾は GL が無くても元の画面として動く(GL が要る画面ではない)。
    fn needs_gl(&self) -> bool {
        false
    }

    fn gl_scene(&mut self) -> Option<&mut dyn GlScene> {
        Some(&mut self.scene)
    }

    /// VRM が動いているか: 読み込みが終わっていて、表示中かつ点灯中。読み込み中・失敗・非表示・消灯では動かさない。
    fn scene_animating(&self) -> bool {
        self.running() && self.scene.state() == LoadState::Ready
    }

    fn frame_interval(&self) -> Option<Duration> {
        if self.scene_animating() {
            self.interval
        } else {
            self.inner.frame_interval()
        }
    }

    fn gl_live_objects(&self) -> Option<i64> {
        Some(self.scene.gl_live_objects())
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn page(&self) -> Option<&'static str> {
        self.inner.page()
    }

    fn stats(&self, out: &mut Vec<(&'static str, String)>) {
        self.inner.stats(out);
    }
}
