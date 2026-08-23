//! Test scaffolding re-exports plus sdk-internal behavior tests.
//!
//! The shared harness lives in `../test_support.rs` (feature
//! `lsp-test-support`) so cosh-tools can reuse it.

use super::test_support::{FakeServer, auto_serve, serve_initialize, spawn_fake_server};

mod test_client;
mod test_diagnostics;
mod test_jsonrpc;
mod test_manager;
mod test_transport;
