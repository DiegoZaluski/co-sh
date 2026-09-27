//! Contract tests: the facade surface (`cosh_onnx::prelude::*`, `load`,
//! `DecisionModel`, `Prediction`, `ModelKind`) as an external caller sees it.
//!
//! These are not ports of upstream tests — upstream pins its Python module
//! wiring differently. They pin the Rust facade contract established in the
//! reorganization: the trait is implementable outside the crate, the prelude
//! resolves every advertised name, `load` reports missing local checkpoints
//! with the upstream error text, and `Prediction` reads the answer contract
//! without panicking on absent fields.

mod extensible;
mod facade;
mod model_kind;
mod prediction;
mod prelude;
