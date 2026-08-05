//! Session-scoped file rollback engine.
//!
//! Maintains a bounded in-memory history of file versions observed during the
//! process lifetime and can restore any previously recorded version by its
//! content hash.
//!
//! # Integration
//!
//! Call [`record`] from every tool that reads or writes a file. The hash
//! returned is the same token the model already sees in hashline headers
//! (`¶path#HASH`), so no additional context is needed to target a version.
//!
//! # Window
//!
//! Default: [`MAX_PATHS`] distinct paths, [`MAX_VERSIONS_PER_PATH`] versions
//! each. LRU eviction handles cold paths automatically. Files larger than
//! [`MAX_SNAPSHOT_BYTES`] or containing null bytes are silently skipped —
//! [`record`] returns `None` for those.
//!
//! # Edge cases handled
//!
//! - No history for path: clear error with guidance.
//! - Hash not found: error listing all known hashes for the path.
//! - Already at target version: early error, no disk write.
//! - No previous version (`hash=None` at oldest slot): error with version count.
//! - External modification (`hash=None`, disk diverged from all history):
//!   error listing known hashes so the caller can pick one explicitly.
//! - File deleted externally + specific hash: recreates the file from snapshot.
//! - File deleted externally + `hash=None`: restores to head with warning.
//! - External modification + specific hash: warns, restore proceeds.
//! - Oversized file: [`record`] returns `None`; [`restore`] surfaces a clear error.
//! - Binary content (null bytes): skipped at record time; same error on restore.
//! - BOM and CRLF round-trip: encoding is detected from the current disk state
//!   and reapplied to the restored content.
//! - Undo of undo: current disk state is snapshotted before every write so any
//!   rollback can itself be rolled back.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::hashline::{
    format::{compute_file_hash, format_hashline_header},
    normalize::{LineEnding, detect_line_ending, normalize_to_lf, restore_line_endings, strip_bom},
    snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, Snapshot, SnapshotStore},
};

/// Maximum byte size of file text that will be snapshotted.
/// Files exceeding this limit are silently skipped by [`record`].
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;

/// Maximum number of distinct file paths tracked in the session store.
pub const MAX_PATHS: usize = 50;

/// Maximum number of versions retained per path. Oldest is dropped first.
pub const MAX_VERSIONS_PER_PATH: usize = 10;

static SESSION: OnceLock<Arc<Mutex<InMemorySnapshotStore>>> = OnceLock::new();

/// Returns the session-scoped snapshot store, initializing it on first call.
///
/// The store is a process-global singleton shared across all file tools.
///
/// The store mutex is recovered transparently if poisoned.
///
/// # Panics
///
/// Panics if `MAX_PATHS` is zero (it is a compile-time constant guaranteed to be nonzero).
#[must_use]
pub fn session_store() -> &'static Arc<Mutex<InMemorySnapshotStore>> {
    SESSION.get_or_init(|| {
        Arc::new(Mutex::new(InMemorySnapshotStore::new(
            &InMemorySnapshotStoreOptions {
                max_paths: Some(
                    #[allow(clippy::expect_used)]
                    NonZeroUsize::new(MAX_PATHS).expect("MAX_PATHS is nonzero"),
                ),
                max_versions_per_path: Some(MAX_VERSIONS_PER_PATH),
            },
        )))
    })
}

/// Record a file version into the session store.
///
/// Returns the content hash when the version was recorded, or `None` when
/// the file was skipped because it exceeds [`MAX_SNAPSHOT_BYTES`] or contains
/// null bytes (binary content).
///
/// The returned hash matches the token in the hashline header `¶path#HASH`
/// and can be passed to [`restore`] to return to this exact version.
///
/// The store mutex is recovered transparently if poisoned.
#[must_use]
pub fn record(path: &str, text: &str) -> Option<String> {
    if text.len() > MAX_SNAPSHOT_BYTES {
        return None;
    }
    if text.contains('\0') {
        return None;
    }
    // Normalize before storing so that restore_line_endings never double-encodes
    // CRLF and BOM does not contaminate the stored text. The content hash is
    // computed on normalized text by the store, so hashes remain consistent.
    let bom_result = strip_bom(text);
    let normalized = normalize_to_lf(&bom_result.text);
    Some(
        session_store()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(path, &normalized),
    )
}

/// Record which lines of `path` the model saw at version `hash`, as surfaced
/// by tool output (grep / read / search). Recovery consults these to flag
/// edits that anchor lines the model never observed. Bounded by the session
/// store's LRU window; no-op for an empty `lines` slice.
///
/// The store mutex is recovered transparently if poisoned.
pub fn record_seen_lines(path: &str, hash: &str, lines: &[(u32, String)]) {
    if lines.is_empty() {
        return;
    }
    session_store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record_seen_lines(path, hash, lines);
}

/// Input for [`restore`].
pub struct RestoreInput {
    /// Path of the file to restore.
    pub path: String,
    /// Hash of the target version, as shown in a `¶path#HASH` header.
    ///
    /// When `None`, the version immediately preceding the current disk state
    /// is restored. Requires the disk content to be present in the session
    /// history; returns an error if the file was modified externally.
    pub hash: Option<String>,
}

/// Successful result from [`restore`].
pub struct RestoreOutput {
    /// Path of the restored file.
    pub path: String,
    /// Content hash of the version now on disk. Use this as the anchor for
    /// follow-up edits.
    pub file_hash: String,
    /// Hashline header `¶path#HASH` for the restored version.
    pub header: String,
    /// Content hash of the content that was on disk before the restore.
    /// Pass this to a subsequent [`restore`] call to undo the restore.
    pub replaced_hash: String,
    /// Non-fatal warning emitted when an anomaly was detected but the restore
    /// still succeeded (e.g., file was modified externally).
    pub warning: Option<String>,
}

/// Normalized view of the current on-disk file state, populated before any
/// write so encoding can be round-tripped.
struct DiskState {
    normalized: String,
    hash: String,
    bom: String,
    ending: LineEnding,
}

fn normalize_disk(raw: &str) -> DiskState {
    let bom_result = strip_bom(raw);
    let ending = detect_line_ending(&bom_result.text);
    let normalized = normalize_to_lf(&bom_result.text);
    let hash = compute_file_hash(&normalized);
    DiskState {
        normalized,
        hash,
        bom: bom_result.bom,
        ending,
    }
}

/// Resolve target and optional warning for an explicit-hash request.
///
/// Returns `Err` when the hash is absent from history or the file is already
/// at the requested version.
fn resolve_by_hash(
    path: &str,
    target_hash: &str,
    history: &[Snapshot],
    disk_hash: &str,
) -> Result<(Snapshot, Option<String>), String> {
    let target = history
        .iter()
        .find(|v| v.hash == target_hash)
        .cloned()
        .ok_or_else(|| {
            let known = known_hashes(history);
            format!(
                "version `{target_hash}` not found in rollback history for `{path}`; \
                 known versions: [{known}]"
            )
        })?;

    if disk_hash == target_hash {
        return Err(format!(
            "`{path}` is already at version `{target_hash}`; nothing to restore"
        ));
    }

    let warning = external_mod_warning(path, disk_hash, history.first().map(|v| v.hash.as_str()));
    Ok((target, warning))
}

/// Resolve target and optional warning for a previous-version request
/// (`hash=None`, file exists on disk).
///
/// Returns `Err` when there is no older version, or when the disk state is not
/// in the session history (external modification).
fn resolve_previous(
    path: &str,
    history: &[Snapshot],
    disk_hash: &str,
) -> Result<(Snapshot, Option<String>), String> {
    if let Some(i) = history.iter().position(|v| v.hash == disk_hash) {
        let prev_idx = i + 1;
        if prev_idx >= history.len() {
            return Err(format!(
                "no previous version for `{path}`; \
                     the current version is the oldest in the rollback window \
                     ({} version{} retained)",
                history.len(),
                if history.len() == 1 { "" } else { "s" }
            ));
        }
        Ok((history[prev_idx].clone(), None))
    } else {
        let known = known_hashes(history);
        Err(format!(
            "`{path}` was modified externally (disk hash `{disk_hash}` not in session history); \
                 cannot determine the previous session version automatically. \
                 Pass an explicit hash to roll back to. Known versions: [{known}]"
        ))
    }
}

fn known_hashes(history: &[Snapshot]) -> String {
    history
        .iter()
        .map(|v| v.hash.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn external_mod_warning(path: &str, disk_hash: &str, head_hash: Option<&str>) -> Option<String> {
    (head_hash != Some(disk_hash)).then(|| {
        format!(
            "`{path}` was modified externally since the last session write \
             (disk `{disk_hash}` differs from last session state `{}`); \
             external changes will be replaced",
            head_hash.unwrap_or("none")
        )
    })
}

/// Restore a file to a previously recorded version.
///
/// Reads the current disk state, resolves the target snapshot from the session
/// history, records the pre-restore state (so the restore can itself be
/// rolled back), writes the restored content preserving the original BOM and
/// line endings, and invalidates the tree-sitter parse cache for the path.
///
/// # Errors
///
/// Returns an error string when:
/// - The path has no rollback history in the current session.
/// - The requested hash is not in the session history (lists known hashes).
/// - The file is already at the requested version.
/// - `hash=None` and the file is already at the oldest version in the window.
/// - `hash=None` and the disk state is not in the session history (external
///   modification); lists known hashes so the caller can pick one explicitly.
/// - A disk write error occurs.
///
/// The store mutex is recovered transparently if poisoned.
///
/// # Panics
///
/// Panics if the history is empty when `hash` is `None` and the file does not
/// exist on disk, as restoring to the head of history is impossible without a
/// recorded version.
#[allow(clippy::significant_drop_tightening)]
pub async fn restore(input: RestoreInput) -> Result<RestoreOutput, String> {
    let path = &input.path;

    // Phase 1: read disk (async, no lock held).
    let disk = match tokio::fs::read_to_string(path).await {
        Ok(raw) => Some(normalize_disk(&raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("failed to read `{path}`: {e}")),
    };

    // Phase 2: resolve target snapshot (lock acquired and released before any await).
    let (target_text, target_hash, warning) = {
        #[allow(clippy::unwrap_used)]
        let mut store = session_store()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let history = store.history(path);
        if history.is_empty() {
            return Err(format!(
                "no rollback history for `{path}`; \
                 the file must be read or written during this session before rollback is available"
            ));
        }

        let (target, warning) = match (&input.hash, &disk) {
            (Some(h), Some(d)) => resolve_by_hash(path, h, &history, &d.hash)?,

            (Some(h), None) => {
                // File deleted externally; specific version requested.
                let target = history
                    .iter()
                    .find(|v| &v.hash == h)
                    .cloned()
                    .ok_or_else(|| {
                        let known = known_hashes(&history);
                        format!(
                            "version `{h}` not found in rollback history for `{path}`; \
                             known versions: [{known}]"
                        )
                    })?;
                let warning = Some(format!(
                    "`{path}` no longer exists on disk; it will be recreated from version `{h}`"
                ));
                (target, warning)
            }

            (None, Some(d)) => resolve_previous(path, &history, &d.hash)?,

            (None, None) => {
                // File deleted and no hash given: restore to head (last session state).
                #[allow(clippy::expect_used)]
                let head = history.first().cloned().expect("history is non-empty");
                let warning = Some(format!(
                    "`{path}` no longer exists on disk; restoring to last session state (`{}`)",
                    head.hash
                ));
                (head, warning)
            }
        };

        (target.text, target.hash, warning)
    };

    // Phase 3: record the pre-restore disk state only when it is not already
    // in the store. Re-recording an existing entry would refresh its timestamp,
    // reordering the history and breaking sequential hash=None navigation.
    // If the version is already stored the user can still reach it by hash.
    if let Some(ref d) = disk
        && d.hash != target_hash
    {
        let already_stored = session_store()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_hash(path, &d.hash)
            .is_some();
        if !already_stored {
            let _ = record(path, &d.normalized);
        }
    }

    // Phase 4: rebuild with original BOM and line ending from disk.
    // When the file did not exist, defaults to LF with no BOM.
    let (bom, ending) = disk
        .as_ref()
        .map_or(("", LineEnding::Lf), |d| (d.bom.as_str(), d.ending));
    let persisted = format!("{bom}{}", restore_line_endings(&target_text, ending));

    // Phase 5: write to disk (async, no lock held).
    tokio::fs::write(path, &persisted)
        .await
        .map_err(|e| format!("failed to write `{path}` during restore: {e}"))?;

    // Phase 6: for explicit-hash restores, move the target to head so it
    // becomes the new anchor for follow-up edits and undo-of-undo navigation.
    // For hash=None (sequential navigation), the target is already in the store
    // at a stable position; re-recording it would shift the LRU order and cause
    // the next hash=None call to jump to the wrong version.
    if input.hash.is_some() {
        session_store()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(path, &target_text);
    }
    crate::tree_sitter::tree_sitter().invalidate(path);

    let replaced_hash = disk.map(|d| d.hash).unwrap_or_default();
    let header = format_hashline_header(path, &target_hash);

    Ok(RestoreOutput {
        path: path.clone(),
        file_hash: target_hash,
        header,
        replaced_hash,
        warning,
    })
}
