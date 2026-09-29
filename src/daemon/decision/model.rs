//! The daemon's resident model registry — the `c89c6e5` residency, promoted
//! from per-process statics to the daemon's own state.
//!
//! The harness used to keep the loaded model in process-wide statics so a
//! per-turn assembly would not reload a ~1.3 GB graph on every message. The
//! daemon is the generalization of that: one process owns the residency,
//! every cosh instance shares it, and the registry below is its only copy —
//! the same one-resident-at-a-time contract, the same 60s retry window on a
//! failed load, the same "last store wins" tolerance for racing loads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cosh_onnx::{DecisionModel, LoadOptions, ModelKind};

use super::protocol::Kind;

/// Minimum spacing between retries of a failed load for the same kind —
/// carried over unchanged from the harness residency: a transient failure
/// must not disable the audit forever, but re-reading a multi-GB graph on
/// every request of a broken setup is its own failure mode.
const LOAD_RETRY_AFTER: Duration = Duration::from_secs(60);

struct Resident {
    kind: Kind,
    name: String,
    model: Arc<dyn DecisionModel>,
}

/// The registry: at most one resident model at a time.
///
/// One resident is the point: the cost is the graph, and holding more than
/// one kind resident is exactly the per-process duplication the daemon
/// exists to eliminate. A different kind in a `decide` swaps the resident
/// (drop the old `Arc`, load the new).
pub(crate) struct Registry {
    guard: std::sync::Mutex<RegistryState>,
}

struct RegistryState {
    resident: Option<Resident>,
    last_failure: Option<(String, Instant)>,
    /// Counters for `health`.
    requests_served: u64,
    failed: u64,
}

impl Registry {
    pub(crate) fn new() -> Self {
        Self {
            guard: std::sync::Mutex::new(RegistryState {
                resident: None,
                last_failure: None,
                requests_served: 0,
                failed: 0,
            }),
        }
    }

    /// The resident model for `kind`, loading it when the resident does not
    /// match. `None` when the load failed — just now, or within the retry
    /// window of an earlier failure for the same kind.
    ///
    /// The load runs OUTSIDE the lock (carried over from `c89c6e5`): it can
    /// take seconds, and holding the mutex through it would stall every
    /// other request. Racing loads both pay, the last store wins — never a
    /// correctness issue.
    pub(crate) fn resident(&self, kind: &Kind) -> Option<Arc<dyn DecisionModel>> {
        let kind_name = kind.name();
        {
            let mut state = self.guard.lock().ok()?;
            // Fast path: the resident matches — share it.
            if state
                .resident
                .as_ref()
                .is_some_and(|resident| resident.kind == *kind)
            {
                state.requests_served += 1;
                let model = Arc::clone(&state.resident.as_ref().expect("checked above").model);
                return Some(model);
            }
            // A retry window still open for this kind: no point re-reading
            // a multi-GB graph while the setup is broken.
            if state
                .last_failure
                .as_ref()
                .is_some_and(|(failed, at)| *failed == kind_name && at.elapsed() < LOAD_RETRY_AFTER)
            {
                state.failed += 1;
                return None;
            }
        }

        match load_model(kind) {
            Ok(model) => {
                if let Ok(mut state) = self.guard.lock() {
                    state.resident = Some(Resident {
                        kind: kind.clone(),
                        name: kind_name.clone(),
                        model: Arc::clone(&model),
                    });
                    // Clear THIS kind's failure stamp only (m1): a success
                    // for Y must not erase X's still-open retry window —
                    // a broken X would otherwise re-attempt a multi-GB
                    // load on every request until its own load succeeds.
                    if state
                        .last_failure
                        .as_ref()
                        .is_some_and(|(failed, _)| *failed == kind_name)
                    {
                        state.last_failure = None;
                    }
                    state.requests_served += 1;
                }
                Some(model)
            }
            Err(e) => {
                log::warn!("decision daemon: model load failed: {e}");
                if let Ok(mut state) = self.guard.lock() {
                    state.last_failure = Some((kind_name, Instant::now()));
                    state.failed += 1;
                }
                None
            }
        }
    }

    /// A load succeeded for `kind` at some point (used by `health`).
    pub(crate) fn active_kind(&self) -> Option<String> {
        self.guard.lock().ok()?.resident.as_ref().map(|r| r.name.clone())
    }

    /// The `health` counters.
    pub(crate) fn counters(&self) -> (u64, u64) {
        match self.guard.lock() {
            Ok(state) => (state.requests_served, state.failed),
            Err(_) => (0, 0),
        }
    }
}

/// Load `kind`; the ONNX graph resolves by the checkpoint's origin:
///
/// - a `Custom` kind pointing at a LOCAL DIRECTORY resolves the graph
///   against that directory (`{repo}/laya.onnx`) — the contract the
///   harness adapter implemented before the daemon;
/// - a NAMED kind (and a `Custom` hub repo id / bundled pair) resolves
///   EVERY artifact — weights, config, tokenizer, and the exported graph
///   — from the checkpoint's snapshot in the hf-hub cache, the single
///   rule the cosh-onnx exporter installs into (no override, no second
///   lookup root on this side).
pub(crate) fn load_model(kind: &Kind) -> Result<Arc<dyn DecisionModel>, cosh_onnx::Error> {
    let mut options = LoadOptions::default();
    if let Kind::Custom { repo, .. } = kind
        && std::path::Path::new(repo).is_dir()
    {
        options.onnx_path = Some(format!("{repo}/laya.onnx"));
    }
    let model_kind = match kind {
        Kind::English => ModelKind::English,
        Kind::Multilingual => ModelKind::Multilingual,
        Kind::TypedDecisions => ModelKind::TypedDecisions,
        Kind::Custom { repo, subfolder } => ModelKind::Custom {
            repo: repo.clone(),
            subfolder: subfolder.clone(),
        },
    };
    cosh_onnx::load(model_kind, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kind whose load cannot succeed: `Custom` pointing at an empty
    /// directory (no graph, no checkpoint files). Exercises the failure
    /// path — retry window, counters — without any model artifacts.
    fn broken_kind() -> Kind {
        Kind::Custom {
            repo: std::env::temp_dir().join("cosh-registry-test-empty")
                .display()
                .to_string(),
            subfolder: None,
        }
    }

    #[test]
    fn failed_load_throttles_retries_within_the_window() {
        let dir = std::env::temp_dir().join("cosh-registry-test-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let kind = broken_kind();
        let registry = Registry::new();

        assert!(registry.resident(&kind).is_none(), "load must fail");
        // The window closed the second attempt: it must not re-attempt the
        // load (the counters would double-count `failed` if it did).
        assert!(registry.resident(&kind).is_none());
        let (_, failed) = registry.counters();
        assert_eq!(failed, 2, "first: the real failure, second: throttled");
    }
}
