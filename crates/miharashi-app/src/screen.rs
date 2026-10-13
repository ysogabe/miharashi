//! 実行ループ(runtime)と画面ロジックの境界。runtime は `App` だけを知る。
use miharashi_render::Canvas;
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
    /// 状態ログ用。runtime は中身を解釈せず、`k=v` を並べて出す。
    fn stats(&self) -> Vec<(&'static str, String)>;
}
