//! デバイスを開かない共通レンダラー。出力は premultiplied RGBA8 の `Canvas`。
pub mod aircraft;
pub mod alert;
pub mod canvas;
pub mod chart;
pub mod geo;
pub mod glitch;
pub mod menu;
pub mod photo;
pub mod photo_layout;
pub mod photo_screen;
pub mod screens;
pub mod status;
pub mod text;
pub mod trip_card;

pub use canvas::Canvas;
pub use text::TextEngine;
pub use tiny_skia::Pixmap;
