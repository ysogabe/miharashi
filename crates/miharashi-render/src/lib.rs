//! デバイスを開かない共通レンダラー。出力は premultiplied RGBA8 の `Canvas`。
pub mod canvas;
pub mod chart;
pub mod photo;
pub mod screens;
pub mod status;
pub mod text;

pub use canvas::Canvas;
pub use text::TextEngine;
pub use tiny_skia::Pixmap;
