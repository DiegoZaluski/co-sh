pub mod config;
pub mod daemon;
pub mod fallback;
pub mod harness;
pub mod mcp;
pub mod notification;
pub mod session;
pub mod setup;
pub mod telemetry;
pub mod types;
pub mod uninstall;
pub mod update;
pub mod usage;
pub mod util;
/// The weights installer's application side (config → install → events).
/// Only meaningful with the ONNX engine: without the feature the decision
/// model cannot load, so the module compiles empty.
#[cfg(feature = "onnx")]
pub mod weights;
pub use cosh_tools::{bash, fs, plan, skills, web};
pub use harness::events::ModelEntry;
