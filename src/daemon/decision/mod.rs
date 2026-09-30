//! The decision-model daemon (`cosh-decisiond`).
//!
//! One process owns the resident decision model (the ~1.3 GB ONNX graph and
//! its ORT session); every cosh instance shares it over the daemon IPC
//! layer. This is the `c89c6e5` residency generalized: the resident model
//! lived in per-process statics, which still duplicated the graph per open
//! app — the daemon owns it once for all of them.
//!
//! - [`protocol`] — the wire schema (methods, params, results, error codes).
//! - [`model`] — the resident registry: one model at a time, the 60s retry
//!   window on failed loads.
//! - [`server`] — the accept loop, routing, the single inference slot, drain.
//! - `main` (the binary) — bind the socket, serve, idle-exit.

pub(crate) mod model;
pub(crate) mod protocol;
pub(crate) mod server;

pub(crate) mod client;

use std::time::Duration;

// The public surface the app binary (and future consumers) attaches with:
// the daemon-backed Checkup and the wire kind. The transport, the server
// and the registry stay internal — they are the daemon's implementation.
pub use client::IpcCheckup;
pub use protocol::Kind;

/// The binary entrypoint (`cosh-decisiond`): bind the daemon's socket and
/// serve until idle or drained.
///
/// Losing the bind race (a live daemon already owns the socket) is a
/// SUCCESS outcome — the spawning process that lost the race exits 0, the
/// winner serves. Only a real bind failure is an error.
pub fn run() -> i32 {
    // Daemon-side logging FIRST: the daemon is a separate PROCESS (spawned
    // on demand by whichever client lost the activation race) — without
    // this, every `log::` call in the daemon modules went nowhere, and the
    // decision model's failures (load errors, failed-open reviews) were a
    // black box. `try_init` (never `init`): losing a logger race or a
    // read-only temp dir must never stop the daemon from serving.
    let _ = crate::util::logger::try_init("decisiond");

    // The activation lock first (M1 fix): a live daemon HOLDS this flock
    // for its lifetime, so contending here means one already owns the
    // socket path — this spawner lost the activation race and exits 0
    // WITHOUT ever touching the socket file. Holding the lock is what
    // makes the probe/remove/bind below race-free (two crash-recovery
    // binders can no longer interleave probe and unlink).
    let _activation_lock = match crate::daemon::activation::acquire_activation_lock(protocol::NAME)
    {
        crate::daemon::activation::ActivationLock::Contended => return 0,
        crate::daemon::activation::ActivationLock::Held(file) => file,
    };

    let path = crate::daemon::activation::socket_path(protocol::NAME);
    // m11 note: a live daemon can die between a CLIENT's connect-probe and
    // this bind — the client then burns its activation window and fails
    // open once; its next call re-activates a fresh daemon. Self-healing,
    // one wasted review, by design (no watcher process, ever).
    std::fs::create_dir_all(path.parent().unwrap_or(&path)).ok();
    let listener = match crate::daemon::ipc::bind(&path) {
        Ok(listener) => listener,
        // Under the lock this means a stale socket file a crashed daemon
        // left AND whose connect-probe raced us — the loser of that probe
        // is about to bind; treat as contended (the winner serves).
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            return 0;
        }
        Err(e) => {
            eprintln!("cosh-decisiond: cannot bind {}: {e}", path.display());
            return 1;
        }
    };

    let idle_timeout = std::env::var("COSH_DECISIOND_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(server::DEFAULT_IDLE_TIMEOUT);

    log::info!("decision daemon: serving at {}", path.display());
    let server = std::sync::Arc::new(server::Server::new());
    server.serve(listener, idle_timeout);

    // Clean exit: the socket goes with us (the bind removes any stale file
    // on the next activation anyway, but leaving no file is tidier).
    let _ = std::fs::remove_file(&path);
    0
}
