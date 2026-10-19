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

/// 場面が作った GL オブジェクトの数え方(docs/DESIGN-screen-plugin-v2.md §3.3)。作成と削除を `create_*` / `delete_*` の
/// 経由で数え、切り替えを重ねても「作成 − 削除」(`live`)が増えないこと、`release` のあとに 0 へ戻ることを確かめる。
/// `glIs*` を名前の範囲で総当たりして数えない(ドライバーが小さい整数から割り当てる前提に頼らない)。
#[derive(Debug, Default)]
pub struct ObjectCount {
    created: std::cell::Cell<u64>,
    deleted: std::cell::Cell<u64>,
}

macro_rules! counted {
    ($($create:ident, $delete:ident, $ty:ident);* $(;)?) => {
        impl ObjectCount {
            $(
                /// 作って数える。
                pub fn $create(&self, gl: &glow::Context) -> Result<glow::$ty, String> {
                    use glow::HasContext;
                    let object = unsafe { gl.$create() }?;
                    self.created.set(self.created.get() + 1);
                    Ok(object)
                }

                /// 消して数える。
                pub fn $delete(&self, gl: &glow::Context, object: glow::$ty) {
                    use glow::HasContext;
                    unsafe { gl.$delete(object) };
                    self.deleted.set(self.deleted.get() + 1);
                }
            )*
        }
    };
}

counted! {
    create_buffer, delete_buffer, Buffer;
    create_texture, delete_texture, Texture;
    create_vertex_array, delete_vertex_array, VertexArray;
    create_program, delete_program, Program;
    create_framebuffer, delete_framebuffer, Framebuffer;
    create_renderbuffer, delete_renderbuffer, Renderbuffer;
    create_sampler, delete_sampler, Sampler;
}

impl ObjectCount {
    /// シェーダーを作って数える(`kind` は `VERTEX_SHADER` / `FRAGMENT_SHADER`)。
    pub fn create_shader(&self, gl: &glow::Context, kind: u32) -> Result<glow::Shader, String> {
        use glow::HasContext;
        let object = unsafe { gl.create_shader(kind) }?;
        self.created.set(self.created.get() + 1);
        Ok(object)
    }

    /// シェーダーを消して数える。
    pub fn delete_shader(&self, gl: &glow::Context, object: glow::Shader) {
        use glow::HasContext;
        unsafe { gl.delete_shader(object) };
        self.deleted.set(self.deleted.get() + 1);
    }

    pub fn created(&self) -> u64 {
        self.created.get()
    }

    pub fn deleted(&self) -> u64 {
        self.deleted.get()
    }

    /// 作成 − 削除。生きているオブジェクトの数。
    pub fn live(&self) -> i64 {
        self.created.get() as i64 - self.deleted.get() as i64
    }
}
