//! 実行ループ(runtime)と画面ロジックの境界。runtime は `App` だけを知る。
use image::DynamicImage;
use miharashi_render::aircraft::RadarGeom;
use miharashi_render::photo_layout::PhotoLayout;
use miharashi_render::{Canvas, Pixmap};
use std::time::Instant;

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
