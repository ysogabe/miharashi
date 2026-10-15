//! 実行ループ(runtime)と画面ロジックの境界。runtime は `App` だけを知る。
//! `App` を実装する `ScreenManager`(app/manager.rs)と、個々の画面(`Screen`)の契約もここに置く
//! (docs/DESIGN-screen-interface.md §3)。
use image::DynamicImage;
use miharashi_render::aircraft::RadarGeom;
use miharashi_render::photo_layout::PhotoLayout;
use miharashi_render::screens::Snapshot;
use miharashi_render::{Canvas, Pixmap, TextEngine};
use std::time::Instant;

/// 画面の識別子。操作パネルとの約束なので、決めたら変えない(docs/DESIGN-screens.md §2)。
pub type ScreenId = &'static str;

/// 画面に渡す操作(ScreenManager が共通操作を取り除いたあとの意味)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAction {
    /// 5: 決定・詳細(状況画面のページ切替)
    Select,
    /// 前の項目(写真画面の前の写真)
    Up,
    /// 次の項目(写真画面の次の写真)
    Down,
}

/// 画面が操作を受けた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 画面が処理した
    Consumed,
    /// 画面では使わない(ScreenManager が共通操作として扱う)
    Ignored,
}

/// 写真フィードの状態。写真画面が書き、状況画面の HEALTH と操作パネルが読む。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedState {
    /// 状況画面の HEALTH に出す状態("online" / "再接続中")。まだ何も届いていなければ None
    pub status: Option<&'static str>,
    /// 写真の (何枚目, 全体の枚数)。まだ写真が無ければ None
    pub position: Option<(usize, usize)>,
    /// 取得元に接続できているか
    pub online: bool,
    /// 接続できないときの知らせ(状態ログの feed=)
    pub notice: Option<String>,
}

/// 画面が読む共有データ。書くのは ScreenManager と Provider だけで、画面は読むだけ。
#[derive(Default)]
pub struct World {
    /// 実測値と時刻(日付・時刻は 1 秒ごとに ScreenManager が設定する)
    pub system: Snapshot,
    pub photo_feed: FeedState,
}

/// 取得した値を `World` に写す。毎周期呼ぶので、ブロックしない。
pub trait Provider {
    fn update(&mut self, world: &mut World);
}

/// 描画に必要なものをまとめて画面に渡す。
pub struct RenderContext<'a> {
    pub canvas: &'a mut Canvas,
    pub text: &'a mut TextEngine,
    pub world: &'a World,
    /// アラートを重ねるか。重ねるときは ScreenManager が画面の上に描く
    pub alert: bool,
    /// GPU 経路の重ね絵を描き直させる(アラートが開いた・閉じた、キーが押された)
    pub force_overlay: bool,
}

/// 登録する画面 1 つ。画面の中の状態(ページ、写真)と、画面の中の操作、描画を受け持つ。
pub trait Screen {
    fn id(&self) -> ScreenId;
    fn title(&self) -> &'static str;
    /// 表示される直前。
    fn on_enter(&mut self, _now: Instant) {}
    /// 別の画面へ移る直前。
    fn on_leave(&mut self, _now: Instant) {}
    /// 表示中かどうかに関係なく、毎周期呼ぶ(写真の受信など)。
    /// 戻り値: 再描画が必要か(表示中の画面のものだけ使う)。
    fn tick(&mut self, _world: &World, _now: Instant) -> bool {
        false
    }
    /// 画面の中の操作。
    fn handle_action(&mut self, _action: ScreenAction, _now: Instant) -> Outcome {
        Outcome::Ignored
    }
    /// CPU で完成画面を描く。
    fn render(&mut self, ctx: &mut RenderContext<'_>);
    /// GPU 経路(`App::render_composite`)の描画。既定は `render` と同じで、Canvas が完成画面になる。
    fn render_composite(&mut self, ctx: &mut RenderContext<'_>) -> Composite<'_> {
        self.render(ctx);
        Composite::Canvas
    }
    /// 毎フレーム動かし続けているか(`App::animating`)。
    fn animating(&self) -> bool {
        false
    }
    /// 操作パネルに出す画面の中のページ名(状況画面の OVERVIEW / DETAILS)。
    fn page(&self) -> Option<&'static str> {
        None
    }
    /// 状態ログに足す `k=v`(写真画面の photo=12)。表示中かどうかに関係なく出す。
    fn stats(&self, _out: &mut Vec<(&'static str, String)>) {}
}

/// 画面ロジックに渡すキー操作。今の動作に必要な最小限。#5 で設計メモ(DESIGN-screen-interface)に沿って拡張する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPress {
    Prev,
    Switch,
    Next,
}

/// runtime から見た画面ロジック。
pub trait App {
    /// キー入力を処理する。戻り値: 再描画が必要か。
    fn on_key(&mut self, key: KeyPress) -> bool;
    /// 実行ループから毎周期呼ぶ。戻り値: 再描画が必要か。
    fn tick(&mut self, now: Instant) -> bool;
    fn render(&mut self, canvas: &mut Canvas);
    /// GPU 経路(renderer = gles)の描画。写真の合成を GPU に任せられる画面は `Composite::Photo` を返し、
    /// `canvas` には透明背景の重ね絵だけを描く。既定は `render` と同じで、Canvas が完成画面になる。
    /// PoC-005 PR3 の内部用。#5 で画面の API が決まったら合わせる。
    fn render_composite(&mut self, canvas: &mut Canvas) -> Composite<'_> {
        self.render(canvas);
        Composite::Canvas
    }
    /// 毎フレーム動かし続けている画面か(航空機の画面の回る線)。true の間、実行ループは待ちを入れず、
    /// 表示の完了待ち(vsync)だけで間隔をとる。
    fn animating(&self) -> bool {
        false
    }
    /// 画面を消している間でも点けて見せたいもの(アラート)があるか。
    fn wants_screen(&self) -> bool {
        false
    }
    /// 状態ログ用。runtime は中身を解釈せず、`k=v` を並べて出す。
    fn stats(&self) -> Vec<(&'static str, String)>;
    /// 操作パネルの `/api/v1/status` に出す画面の状態。出さない画面ロジックは None。
    fn panel_view(&self) -> Option<PanelView> {
        None
    }
}

/// 登録されている画面 1 つ。`id` はパネルとの約束なので変えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenInfo {
    pub id: &'static str,
    pub title: &'static str,
}

/// 操作パネルに見せる画面の状態(docs/panel/DESIGN.md §5.2)。変わったときだけパネルへ送る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelView {
    /// 画面の一覧(切り替わる順)
    pub screens: Vec<ScreenInfo>,
    /// 表示中の画面(`screens` の添字)
    pub current: usize,
    /// 画面の中のページ名(状況画面の OVERVIEW / DETAILS)
    pub page: Option<&'static str>,
    /// 写真の (何枚目, 全体の枚数)。まだ写真が無ければ None
    pub photo: Option<(usize, usize)>,
    /// 写真の取得元に接続できているか
    pub feed_online: bool,
}

/// 写真のフレームの中身。
#[derive(Clone, Copy)]
pub enum PhotoData<'a> {
    /// CPU で合成した完成フレーム
    Frame(&'a Pixmap),
    /// デコードしただけの写真と配置。GPU で完成フレームを作る(PoC-005 PR3b)
    Source {
        image: &'a DynamicImage,
        layout: &'a PhotoLayout,
    },
    /// 前回までに GPU に渡した
    Resident,
}

/// 写真のフレーム。`id` が同じなら同じ画像(GPU は転送し直さない)。
#[derive(Clone, Copy)]
pub struct PhotoRef<'a> {
    pub id: u64,
    pub data: PhotoData<'a>,
}

/// `App::render_composite` の結果。
pub enum Composite<'a> {
    /// `canvas` が完成画面
    Canvas,
    /// 写真(フェード中は旧フレームと混ぜる)の上に `canvas`(透明背景の重ね絵)を重ねる
    Photo {
        photo: PhotoRef<'a>,
        /// (旧フレーム, 新フレームの不透明度 0〜1)
        old: Option<(PhotoRef<'a>, f32)>,
        /// 今回 `canvas` を描き直したか(false なら前回の重ね絵を使う)
        overlay_changed: bool,
    },
    /// 航空機の画面。`canvas`(不透明の背景)の上に、回る線と点を GPU で描く(PoC-005 PR4a)
    Radar {
        /// 今回 `canvas` を描き直したか(false なら前回の背景を使う)
        background_changed: bool,
        geom: RadarGeom,
        /// 回る線の角度 [rad]。北 0、時計回り
        sweep: f32,
    },
}
