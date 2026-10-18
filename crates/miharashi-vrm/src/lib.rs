//! VRM 1.0 モデルを GLES(glow)で描く PoC(Issue #71 の A. 単体スパイク)。
//! 読み込み(`model`)は GL に触れず、描画(`render`)は呼び出し側が持つ GL コンテキストを使う。
//! 画面には組み込まない。計測は `vrm-bench`(feature `bench`、既定で有効。`miharashi-gpu` はこの bin と試験だけが使う)。
#[cfg(feature = "view")]
pub mod agent_layout;
#[cfg(feature = "view")]
pub mod agent_stage;
pub mod anim;
pub mod demo;
pub mod gesture;
pub mod layer;
pub mod math;
pub mod model;
#[cfg(feature = "view")]
pub mod photo_stage;
pub mod render;
pub mod spring;
pub mod synth;
pub mod vrm;

pub use layer::VrmLayer;
pub use model::{AlphaMode, Model, Stats};
pub use render::{
    Framing, Level, Region, Renderer, UpdateTimes, attach_depth, detach_depth, view_proj,
};
