//! Aggregated diagnostics store with settle-wait and LLM formatting.
//!
//! Sits between the manager's event stream and the tools:
//!
//! * **Storage** — `path → server → diagnostics`, replace-whole-document per
//!   publisher (the protocol's own semantics: an empty list clears that
//!   server's entry). Multiple servers reporting on one file coexist; the
//!   store is the replay source, so late subscribers always see the latest
//!   state without having witnessed the pushes.
//! * **Change tracking** — a global monotonic version bumped on every
//!   mutation and mirrored into a watch channel. Cheap change detection for
//!   the settle-wait loop and for callers deciding whether anything changed
//!   since their last look.
//! * **Settle-wait** — [`DiagnosticsEngine::wait_for_settle`] implements the
//!   quiet-period contract the agent needs after editing a file: return as
//!   soon as no store mutation happened for [`SETTLE_DEBOUNCE`], bounded by
//!   the caller's cap (crush ships 300 ms/5 s; opencode 150 ms + early exit —
//!   both shapes are one function here).
//!
//! Ingestion accepts either raw [`PublishDiagnosticsParams`] (push) or the
//! results of explicit pulls ([`LanguageServer::pull_diagnostics`]) through
//! the same [`DiagnosticsEngine::ingest`] door, which is what makes the
//! hybrid push+pull design a storage concern only.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams};
use tokio::sync::watch;

use super::client::Event;

/// Quiet period required before a settle-wait returns.
pub const SETTLE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Shared engine handle. Cheap to clone.
#[derive(Clone)]
pub struct DiagnosticsEngine {
    inner: Arc<Inner>,
}

struct Inner {
    entries: std::sync::Mutex<HashMap<PathBuf, HashMap<String, Vec<Diagnostic>>>>,
    /// Last-change instant per path, driving per-file quiet detection.
    last_change: std::sync::Mutex<HashMap<PathBuf, std::time::Instant>>,
    version: AtomicU64,
    changed_tx: watch::Sender<u64>,
}

impl DiagnosticsEngine {
    pub fn new() -> Self {
        let (changed_tx, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                entries: std::sync::Mutex::new(HashMap::new()),
                last_change: std::sync::Mutex::new(HashMap::new()),
                version: AtomicU64::new(0),
                changed_tx,
            }),
        }
    }

    /// Current global version (bumped on every mutation).
    pub fn version(&self) -> u64 {
        self.inner.version.load(Ordering::SeqCst)
    }

    /// Watch channel mirroring [`Self::version`]; `changed()` fires once per
    /// mutation.
    pub fn subscribe_version(&self) -> watch::Receiver<u64> {
        self.inner.changed_tx.subscribe()
    }

    /// Feed one manager-relayed event. Non-diagnostics events are ignored so
    /// callers may hand over the whole stream unfiltered.
    pub fn ingest_event(&self, event: &super::manager::ManagedEvent) {
        if let Event::PublishDiagnostics(params) = &event.event {
            self.ingest(&event.server, params);
        }
    }

    /// Replace-whole-document ingest for one publisher.
    ///
    /// An empty diagnostics list clears that server's entry; when no server
    /// has anything left for the file the path disappears from the store
    /// entirely (matching what an editor would show).
    pub fn ingest(&self, server: &str, params: &PublishDiagnosticsParams) {
        let Some(path) = uri_to_path(&params.uri) else {
            log::debug!(
                "ignoring diagnostics for non-file uri {}",
                params.uri.as_str()
            );
            return;
        };

        let mutated = {
            let mut entries = self.inner.entries.lock().expect("diagnostics lock");
            if params.diagnostics.is_empty() {
                let mut cleared = false;
                if let Some(per_server) = entries.get_mut(&path) {
                    cleared = per_server.remove(server).is_some();
                    if per_server.is_empty() {
                        entries.remove(&path);
                    }
                }
                cleared
            } else {
                // Servers re-emit identical sets periodically (tsserver,
                // clangd on file events): only a REAL change may bump the
                // version, or settle-wait never sees a quiet window.
                let per_server = entries.entry(path.clone()).or_default();
                let changed = per_server.get(server) != Some(&params.diagnostics);
                if changed {
                    per_server.insert(server.to_owned(), params.diagnostics.clone());
                }
                changed
            }
        };

        if mutated {
            self.record_change(&path);
        }
    }

    /// Latest aggregated diagnostics for one file, sorted by severity then
    /// position. Empty when nothing is known.
    pub fn snapshot_for(&self, path: &Path) -> Vec<Diagnostic> {
        let mut all = Vec::new();
        if let Some(per_server) = self
            .inner
            .entries
            .lock()
            .expect("diagnostics lock")
            .get(path)
        {
            for diags in per_server.values() {
                all.extend(diags.iter().cloned());
            }
        }
        sort_diagnostics(&mut all);
        all
    }

    /// Latest diagnostics for every tracked file under `root` (or everywhere
    /// when `root` is `None`), paths ordered deterministically.
    pub fn snapshot_all(&self, root: Option<&Path>) -> Vec<(PathBuf, Vec<Diagnostic>)> {
        let entries = self.inner.entries.lock().expect("diagnostics lock");
        let mut snapshot: Vec<(PathBuf, Vec<Diagnostic>)> = entries
            .iter()
            .filter(|(path, _)| root.is_none_or(|root| path.starts_with(root)))
            .map(|(path, per_server)| {
                let mut all: Vec<Diagnostic> = per_server
                    .values()
                    .flat_map(|diags| diags.iter().cloned())
                    .collect();
                sort_diagnostics(&mut all);
                (path.clone(), all)
            })
            .collect();
        snapshot.sort_by(|a, b| a.0.cmp(&b.0));
        snapshot
    }

    /// Drop all diagnostics for one path (e.g. the file was deleted).
    ///
    /// Returns whether anything was removed.
    pub fn remove_path(&self, path: &Path) -> bool {
        let removed = self
            .inner
            .entries
            .lock()
            .expect("diagnostics lock")
            .remove(path)
            .is_some();
        if removed {
            self.bump();
        }
        self.inner
            .last_change
            .lock()
            .expect("last-change lock")
            .remove(path);
        removed
    }

    /// Forget everything (session teardown).
    pub fn clear(&self) {
        let had_entries = {
            let mut entries = self.inner.entries.lock().expect("diagnostics lock");
            let had = !entries.is_empty();
            entries.clear();
            had
        };
        self.inner
            .last_change
            .lock()
            .expect("last-change lock")
            .clear();
        if had_entries {
            self.bump();
        }
    }

    /// Wait until the whole store has been quiet for [`SETTLE_DEBOUNCE`], or
    /// `cap` elapses. Returns whether the wait ended in a quiet window
    /// (`true`) or hit the cap (`false`).
    ///
    /// The cap may overshoot by up to one debounce window: the debounce sleep
    /// always runs to completion before the deadline is consulted.
    ///
    /// Polls the global version instead of subscribing per-path: at agent
    /// scale the poll cost is negligible and the semantics stay obvious.
    pub async fn wait_for_settle(&self, cap: Duration) -> bool {
        let started = std::time::Instant::now();
        loop {
            let before = self.version();
            tokio::time::sleep(SETTLE_DEBOUNCE).await;

            if self.version() == before {
                return true; // no mutation during a full debounce window
            }
            if started.elapsed() >= cap {
                return false;
            }
        }
    }

    /// Time since `path` last saw a diagnostic change, if ever tracked.
    #[allow(dead_code)]
    pub fn quiet_for(&self, path: &Path) -> Option<Duration> {
        self.inner
            .last_change
            .lock()
            .expect("last-change lock")
            .get(path)
            .map(|at| at.elapsed())
    }

    fn record_change(&self, path: &Path) {
        self.inner
            .last_change
            .lock()
            .expect("last-change lock")
            .insert(path.to_path_buf(), std::time::Instant::now());
        self.bump();
    }

    fn bump(&self) {
        let version = self.inner.version.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.inner.changed_tx.send(version);
    }
}

impl Default for DiagnosticsEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn sort_diagnostics(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| severity_then_position(a).cmp(&severity_then_position(b)));
}

/// Sort key with a message tie-break so equal-position diagnostics from
/// different publishers render deterministically across runs.
fn severity_then_position(diag: &Diagnostic) -> (DiagnosticSeverity, u32, u32, &str) {
    (
        diag.severity.unwrap_or(DiagnosticSeverity::ERROR),
        diag.range.start.line,
        diag.range.start.character,
        diag.message.as_str(),
    )
}

/// Inverse of the client's percent-encoding: `file://` URIs to paths.
///
/// Non-`file` schemes yield `None` — servers occasionally publish against
/// `untitled:` or `jdt://` documents an agent never touched. Invalid `%`
/// sequences pass through as literal text rather than failing the whole uri.
pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let raw = uri.as_str();
    let rest = raw.strip_prefix("file://")?;

    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    Some(PathBuf::from(
        String::from_utf8_lossy(&decoded).into_owned(),
    ))
}

/// Severity filter applied when rendering diagnostics for a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeverityFilter {
    /// Only errors (severity 1) — used by passive edit/write feedback.
    ErrorsOnly,
    /// Errors and warnings.
    WarningAndUp,
    /// Everything the servers reported.
    All,
}

/// Render diagnostics the way a model reads them best: deterministic order,
/// severity filter, hard item cap with an explicit truncation marker.
///
/// Cross-server duplicates (identical position+message from two publishers)
/// are collapsed — the model does not care that rust-analyzer and clippy
/// agree.
pub fn format_for_model(diags: &[Diagnostic], filter: SeverityFilter, max_items: usize) -> String {
    let max_severity = match filter {
        SeverityFilter::ErrorsOnly => Some(DiagnosticSeverity::ERROR),
        SeverityFilter::WarningAndUp => Some(DiagnosticSeverity::WARNING),
        SeverityFilter::All => None,
    };

    let mut matching: Vec<&Diagnostic> = diags
        .iter()
        .filter(|diag| match max_severity {
            Some(max) => diag.severity.is_none_or(|severity| severity <= max),
            None => true,
        })
        .collect();
    matching.sort_by(|a, b| severity_then_position(a).cmp(&severity_then_position(b)));

    // Cross-server dedupe on (position, message).
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<&Diagnostic> = Vec::new();
    for diag in matching {
        let key = (
            diag.range.start.line,
            diag.range.start.character,
            diag.message.clone(),
        );
        if seen.insert(key) {
            unique.push(diag);
        }
    }

    if unique.is_empty() {
        return String::new();
    }

    let shown = unique.iter().take(max_items);
    let mut lines: Vec<String> = shown.map(|diag| format_diagnostic_line(diag)).collect();
    let hidden = unique.len().saturating_sub(max_items);
    if hidden > 0 {
        lines.push(format!("… and {hidden} more diagnostics"));
    }
    lines.join("\n")
}

fn format_diagnostic_line(diag: &Diagnostic) -> String {
    // Spec default for an omitted severity is ERROR — keep the label
    // consistent with what SeverityFilter::ErrorsOnly already assumed.
    let severity = match diag.severity {
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        Some(DiagnosticSeverity::HINT) => "hint",
        _ => "error",
    };
    let location = format!(
        "{}:{}",
        diag.range.start.line + 1,
        diag.range.start.character + 1
    );
    let code = match &diag.code {
        Some(lsp_types::NumberOrString::Number(number)) => format!(" ({number})"),
        Some(lsp_types::NumberOrString::String(text)) => format!(" ({text})"),
        None => String::new(),
    };
    format!("[{severity} {location}]{code} {}", diag.message)
}
