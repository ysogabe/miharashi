//! Linux 専用: 内蔵 GPU で描く GLES 3.2 の表示経路(PoC-005 #16)。GBM + EGL で KMS に直接出す。
//! 描画は表示カードだけで行い、別の GPU(NVIDIA)は使わない。
pub mod compose;
pub mod connectome;
pub mod display;
mod egl;
mod gbm;
pub mod offscreen;
pub mod photo;
pub mod scene;
pub mod video;

pub use compose::{AgentGpuCard, Frame, PhotoLayer, PhotoSource, RADAR_BLIPS, Radar};
pub use display::{GlesDisplay, draw_frame_with, draw_with};
pub use miharashi_glscene::{GlScene, GlTarget, Region};
pub use offscreen::{Offscreen, diff_rgb, read_rgba_top_down, render_node_for_pci};
pub use photo::{Placement, SourceImage};
pub use scene::{SceneError, SceneFrame, SceneOutcome, SceneTop};
