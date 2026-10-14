//! Linux 専用: 内蔵 GPU で描く GLES 3.2 の表示経路(PoC-005 #16)。GBM + EGL で KMS に直接出す。
//! 描画は表示カードだけで行い、別の GPU(NVIDIA)は使わない。
pub mod compose;
pub mod display;
mod egl;
mod gbm;
pub mod offscreen;
pub mod photo;

pub use compose::{Frame, PhotoLayer, PhotoSource, RADAR_BLIPS, Radar};
pub use photo::{Placement, SourceImage};
pub use display::GlesDisplay;
pub use offscreen::{Offscreen, diff_rgb, render_node_for_pci};
