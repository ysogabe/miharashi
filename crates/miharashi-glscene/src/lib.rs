//! 画面に GL を貸す口(Issue #72 / #74)。画面が自前のメッシュ・シェーダーで 3D を描くための窓口だけを置く。
//! 依存は glow だけ(Linux 固有の DRM・EGL や、画面・アプリの型には依存しない)なので、
//! 描く側の `miharashi-gpu`、画面を作る側の `miharashi-app`・`miharashi-vrm` のどれからも使える。

/// 画面上の矩形。画素。原点は左上(Canvas と同じ)。GL の左下原点への換算は Compositor がする。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Compositor が `draw` に渡す描画先。`fbo` は bind 済みで、色(RGBA8)と深度・ステンシル(DEPTH24_STENCIL8)つき。
#[derive(Clone, Copy, Debug)]
pub struct GlTarget {
    /// 層の FBO。画面が自前の深度パスなどで bind を変えたら、これに戻す
    pub fbo: glow::Framebuffer,
    /// 層の大きさ(= region の大きさ)。viewport は (0, 0, w, h) に設定済み
    pub size: (u32, u32),
}

/// 画面が実装する 3D 描画。呼ぶのは Compositor だけ(GL が current なスレッド = 実行ループ)。
///
/// 呼ばれた時点の GL の状態は Compositor が保証する(`miharashi-gpu` の `Compositor::reset_gl_state`):
/// `target.fbo` が bind 済みで、色 `(0,0,0,0)`・深度 1.0・ステンシル 0 で塗ってあり、テスト・マスク・
/// bind は既定値、プログラム・VAO・バッファ・テクスチャは bind されていない。画面が「戻す」責任は持たない。
/// 開始した query など、Compositor が設定し直せないものだけは `draw` の中で閉じること。
///
/// sampler の束縛を `draw` の前に解除するのは、テクスチャユニット 0〜15 まで(GLES 3.x の最低保証の数)。
/// 16 以降のユニットを使う場面は、前の場面が残した sampler に頼らず、自分で sampler を明示的に設定すること。
pub trait GlScene {
    /// `target` に今の状態を描く。初回は GL オブジェクトを作る(遅延・冪等)。
    /// 出力は premultiplied alpha。失敗は Err(文字列。コードベースの他と同じ)。
    fn draw(&mut self, gl: &glow::Context, target: &GlTarget) -> Result<(), String>;
    /// 自分が作った GL オブジェクトをすべて消す。呼んだあとの `draw` で作り直せること。
    /// `draw` の途中(作りかけ)で失敗したあとにも呼ばれるので、作っていないものは消さない。
    ///
    /// Compositor / ScreenManager は、1 つの場面に対して `release` を高々 1 回だけ呼ぶ(`draw` が失敗して
    /// その場で呼んだ場面は、終了時には呼ばない)。ただし場面の側も 2 回目の呼び出しに耐える書き方にしておくこと
    /// (作ったものを `Option::take()` で持ち、消したら `None` に戻すなど)。
    fn release(&mut self, gl: &glow::Context);
}
