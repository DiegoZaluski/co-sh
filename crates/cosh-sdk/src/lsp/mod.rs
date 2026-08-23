//! Language Server Protocol engine for cosh.
//!
//! A compact, tokio-native LSP client stack for driving language servers from
//! an agent process (no editor UI involved). Behavioral port of the battle-
//! tested designs in Zed's `crates/lsp`, Helix's `helix-lsp` and opencode's
//! TS implementation — re-expressed with our own architecture on `lsp-types`.
//!
//! # Layering
//!
//! * [`jsonrpc`] — wire framing (`Content-Length`) and message classification.
//! * [`Transport`] — background I/O tasks: request/response routing,
//!   per-request deadlines with cancel-on-drop, bounded incoming queue
//!   (backpressure), stderr draining and a terminal-state watch.
//! * [`client`](super) (upcoming phases) — server lifecycle, capability
//!   negotiation and document synchronization.
//! * [`manager`](self) (upcoming phases) — one client per `(root, server)`
//!   with lazy spawn, fan-out queries and observable states.
//!
//! [`Transport::start`] is generic over its I/O types: production passes the
//! stdio of the spawned server process, while tests drive the whole stack
//! over in-memory duplex pipes (see `src/lsp/test/` for executable usage).
pub(crate) mod catalog;
pub(crate) mod client;
pub(crate) mod diagnostics;
pub(crate) mod error;
pub(crate) mod jsonrpc;
pub(crate) mod manager;
pub(crate) mod text;
pub(crate) mod transport;

#[cfg(test)]
mod test;

/// Re-exported protocol types so call sites never depend on the concrete crate.
pub use lsp_types;

pub use catalog::{CATALOG, ServerSpec};
pub use client::{Event, LanguageServer, LanguageServerConfig, ServerState, TouchOutcome};
pub use diagnostics::{
    DiagnosticsEngine, SETTLE_DEBOUNCE, SeverityFilter, format_for_model, uri_to_path,
};
pub use error::{ExitReason, LspError};
pub use jsonrpc::{IncomingMessage, RequestId, RpcError, error_codes};
pub use manager::{ClientKey, ClientLifecycle, ManagedEvent, Manager, ManagerConfig};
pub use text::{PositionEncoding, offset_to_position, position_to_offset};
pub use transport::{RequestFuture, Transport};
