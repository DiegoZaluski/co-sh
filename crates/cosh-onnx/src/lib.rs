//! cosh-onnx: Rust port of the laya System 1 decision engine core.
//!
//! The crate is layered so the model-specific core stays inside `laya/`
//! while everything a future model can reuse lives outside it:
//!
//! * `pycompat` — Python runtime parity primitives (repr/str/`%g`/round).
//! * `runtime` — ONNX serving primitives: session seam, tokenizer, collation.
//! * `decision` — the decision contract: question protocol, confidence,
//!   schema mapping, sequence construction, the model traits, and the
//!   backend-agnostic facade types ([`ModelKind`], [`LoadOptions`],
//!   [`DecisionModel`], [`Prediction`]).
//! * `hooks`, `lang`, `shortlist`, `hub` — generic supporting systems.
//! * `laya` — the laya model core (agent, router, presets, email, pins) and
//!   the facade implementation that resolves a [`ModelKind`] to laya
//!   artifacts ([`load`]).
//!
//! Start from [`prelude`]: one import gives the working surface. The root
//! re-exports the headline names (`from laya import Router, load` upstream).

pub mod decision;
pub mod error;
pub mod hooks;
pub mod hub;
pub mod lang;
pub mod laya;
pub mod prelude;
pub mod pycompat;
pub mod runtime;
pub mod shortlist;

pub use crate::decision::model::{DecisionModel, LoadOptions, ModelKind, Prediction};
pub use crate::error::{Error, Result};
pub use crate::laya::facade::load;
pub use crate::laya::router::{RouteDecision, Router};
