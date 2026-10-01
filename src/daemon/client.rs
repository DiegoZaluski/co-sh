//! The generic daemon client: connect (activating on demand), call, fail
//! open. Fully synchronous — see `ipc` for why std is the right runtime.
//!
//! The connection policy is deliberately stateless: **every call gets a
//! fresh connection**. A daemon call happens once per turn end, so a UDS
//! connect is noise next to the inference; statelessness buys immunity to
//! stale-connection bugs (daemon restarted between turns, drained, idle-
//! exited) with no reconnect logic to maintain. Per-daemon modules layer
//! typed methods on top.

use serde_json::Value;

use super::activation;
use super::ipc::ClientConnection;
use super::ipc_error::Ipc;

/// A handle to one daemon. Cheap to clone; no I/O happens until a call.
pub(crate) struct DaemonClient {
    name: &'static str,
    protocol_version: u32,
}

impl DaemonClient {
    /// A client for the daemon named `name`, speaking `protocol_version`.
    pub(crate) fn new(name: &'static str, protocol_version: u32) -> Self {
        Self {
            name,
            protocol_version,
        }
    }

    /// One request/response round trip on a fresh connection, activating
    /// the daemon if its socket is not there.
    ///
    /// Failures propagate: what a failure MEANS (fail open, retry, toast)
    /// belongs to the seam consumer, not here.
    pub(crate) fn call_with_budget(
        &self,
        method: &str,
        params: Value,
        budget: std::time::Duration,
    ) -> Result<Value, Ipc> {
        let stream = match activation::connect_or_activate(self.name, || {
            activation::spawn_sibling(self.name)
        }) {
            Ok(stream) => stream,
            // Both lifecycle failures surface as connect errors: the
            // consumer's fail-open policy does not care whether the spawn
            // failed or the socket never appeared.
            Err(e) => {
                return Err(Ipc::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    e.to_string(),
                )));
            }
        };
        let mut connection =
            ClientConnection::with_budget(stream, self.name, self.protocol_version, budget)
                .map_err(Ipc::Io)?;
        connection.call(method, params)
    }

    /// The JSON-RPC error object of a failed call, for consumers that map
    /// remote error codes to their own policies (e.g. `-32003` busy → one
    /// retry).
    pub(crate) fn remote_error(e: &Ipc) -> Option<&super::ipc::RpcError> {
        match e {
            Ipc::Remote(error) => Some(error),
            _ => None,
        }
    }
}
