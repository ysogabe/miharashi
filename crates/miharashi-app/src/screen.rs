//! 実行ループ(runtime)と画面ロジックの境界。runtime は `App` だけを知る。
use image::DynamicImage;
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
    /// 状態ログ用。runtime は中身を解釈せず、`k=v` を並べて出す。
    fn stats(&self) -> Vec<(&'static str, String)>;
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
}
