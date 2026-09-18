//! Process-wide LSP on/off switch, driven by the TUI's persisted config, plus
//! the process-wide language-server engine itself.
//!
//! The TUI has no `Setup`-free path to flip the switch, so this `AtomicBool`
//! is the single shared source of truth the TUI flips at startup and from the
//! Settings toggle; every `build_lsp` consults it (in addition to the
//! `COSH_LSP` env override).
//!
//! The engine is a singleton ([`global_lsp`]) on purpose: the TUI rebuilds a
//! [`Harness`](crate::harness::core::Harness) for every agent-loop run, and a
//! per-harness manager would spawn a fresh language-server process each turn
//! only to kill it on drop (`kill_on_drop`). One manager bound to the session
//! workspace means one server process per language, alive for the whole
//! session and shared by the passive diagnostics path and the `lsp_*` tools.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cosh_sdk::lsp::{DiagnosticsEngine, Manager, ManagerConfig};
use cosh_tools::lsp::Lsp;

static LSP_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set whether the LSP engine may be built for this process.
///
/// Called at TUI startup from the persisted config and whenever the user
/// toggles LSP in Settings.
pub fn set_lsp_enabled(enabled: bool) {
    LSP_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether LSP is enabled at the process level (the config-driven default).
pub fn lsp_enabled() -> bool {
    LSP_ENABLED.load(Ordering::Relaxed)
}

/// The singleton engine: `(workspace root, wrapper)`. Rebuilt only if a later
/// call names a different workspace root.
static GLOBAL_LSP: Mutex<Option<(PathBuf, Arc<Lsp>)>> = Mutex::new(None);

/// Return the process-wide engine bound to `cwd`, creating it on first use.
///
/// The event-ingestion task (manager events → diagnostics engine) and the
/// startup auto-discovery spawn run on the *current* runtime's tasks — callers
/// must invoke this from within a long-lived runtime (the TUI's shared agent
/// runtime), not a per-turn one, or the tasks die with it.
///
/// Returns `None` when no runtime is active; the caller's flag/env checks stay
/// with the caller ([`crate::harness::core::build_lsp`]).
pub fn global_lsp(cwd: &str) -> Option<Arc<Lsp>> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let root = PathBuf::from(cwd);

    let mut guard = GLOBAL_LSP.lock().expect("global lsp lock");
    if let Some((bound_root, lsp)) = guard.as_ref()
        && *bound_root == root
    {
        return Some(Arc::clone(lsp));
    }

    let mut config = ManagerConfig::new(root.clone());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    config.events = Some(event_tx);
    let manager = Arc::new(Manager::with_config(config));
    let diagnostics = Arc::new(DiagnosticsEngine::new());

    // Manager events → diagnostics engine, for the whole process lifetime.
    let engine = Arc::clone(&diagnostics);
    handle.spawn(async move {
        while let Some(event) = event_rx.recv().await {
            engine.ingest_event(&event);
        }
    });

    // Startup auto-discovery: detect the workspace language from its root
    // markers and start the matching server(s) eagerly. Soft-skipped (missing
    // binaries, failed spawns) — never fatal.
    let auto = Arc::clone(&manager);
    handle.spawn(async move {
        let _ = auto.ensure_for_root(auto.root()).await;
    });

    let lsp = Arc::new(Lsp::with_manager(Arc::clone(&manager), diagnostics));
    *guard = Some((root, Arc::clone(&lsp)));
    Some(lsp)
}

/// Peek at the singleton WITHOUT creating it: returns the engine only when
/// one is already bound to `cwd`. Purely read-side callers (e.g. the file
/// explorer's status colors) must never spawn servers or runtime tasks as a
/// side effect of a render loop, so they use this instead of [`global_lsp`].
pub fn peek_global_lsp(cwd: &str) -> Option<Arc<Lsp>> {
    // A poisoned lock means some other thread panicked while constructing
    // an engine — the data it protects (an `Option` and `Arc`s) is still
    // structurally valid, and this peek runs on the render loop (up to 30×
    // per second), where propagating the panic would take the whole TUI
    // down. Degrade to "no engine" instead.
    let guard = GLOBAL_LSP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = PathBuf::from(cwd);
    guard
        .as_ref()
        .filter(|(bound_root, _)| *bound_root == root)
        .map(|(_, lsp)| Arc::clone(lsp))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First call creates the engine, a same-root call reuses it, a
    /// different-root call replaces it. Uses marker-free temp dirs so no real
    /// language server is ever spawned.
    #[test]
    fn global_lsp_creates_reuses_and_replaces_by_root() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let (a1, a2, b) = rt.block_on(async {
            let a1 = global_lsp(dir_a.path().to_str().unwrap()).unwrap();
            let a2 = global_lsp(dir_a.path().to_str().unwrap()).unwrap();
            let b = global_lsp(dir_b.path().to_str().unwrap()).unwrap();
            (a1, a2, b)
        });

        assert!(
            Arc::ptr_eq(&a1, &a2),
            "same workspace root must reuse the singleton"
        );
        assert!(
            !Arc::ptr_eq(&a1, &b),
            "a different workspace root must replace the engine"
        );
    }

    /// Outside a tokio runtime there is no engine: the harness treats LSP as
    /// unavailable instead of panicking.
    #[test]
    fn global_lsp_requires_a_runtime() {
        assert!(global_lsp("/tmp").is_none());
    }
}
