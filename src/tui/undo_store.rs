//! `/tmp` undo snapshots for destructive display actions (revert).
//!
//! A revert rewrites both session files (JSONL transcript + bincode `.ctx`
//! context) with the tail deleted. Before the rewrite lands, the CURRENT
//! files are copied to `/tmp/cosh/undo/<session-id>/vN/` so the user can roll
//! back while the tmp directory is alive (lost on reboot — accepted: the
//! snapshot is an escape hatch, not a backup).
//!
//! Versions are numbered `v1..vN` per session; the oldest is dropped when the
//! [`UNDO_VERSIONS_CAP`] is reached. Rollback restores BOTH files, so display
//! and context never desync.

use std::path::{Path, PathBuf};

/// Max versions kept per session — the oldest is evicted FIFO (mirrors the
/// sessions-dir eviction policy).
const UNDO_VERSIONS_CAP: usize = 10;

/// Root of the versioned undo snapshots: `/tmp/cosh/undo`.
#[derive(Debug, Clone)]
pub struct UndoStore {
    root: PathBuf,
}

impl Default for UndoStore {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStore {
    /// Create the undo store rooted at `/tmp/cosh/undo` (creating the tree is
    /// best-effort — a read-only /tmp just disables the feature).
    pub fn new() -> Self {
        let root = std::env::temp_dir().join("cosh").join("undo");
        std::fs::create_dir_all(&root).ok();
        Self { root }
    }

    /// Test-only store rooted at an explicit directory (the default root
    /// lives in /tmp and persists across test runs).
    #[cfg(test)]
    fn with_root(root: PathBuf) -> Self {
        std::fs::create_dir_all(&root).ok();
        Self { root }
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id)
    }

    /// Copy the session's CURRENT files into the next version slot
    /// (`v{len+1}`, capped). Returns the version label (`"v3"`) on success.
    /// A version is only counted when BOTH files were captured — a snapshot
    /// that cannot represent the full state is worse than no snapshot.
    pub fn snapshot(&self, session_id: &str, jsonl: &Path, ctx: &Path) -> Option<String> {
        if !jsonl.exists() {
            // Nothing was persisted yet — a revert cannot destroy context
            // that is not on disk.
            return None;
        }
        let version = {
            let versions = self.versions(session_id);
            let next = versions
                .last()
                .and_then(|v| v.strip_prefix('v'))
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
                + 1;
            format!("v{next}")
        };
        let dir = self.session_dir(session_id).join(&version);
        std::fs::create_dir_all(&dir).ok()?;
        let jsonl_ok = std::fs::copy(jsonl, dir.join("session.jsonl"))
            .map(|_| true)
            .unwrap_or(false);
        // The companion may legitimately not exist yet (display-only session
        // so far) — capture its ABSENCE so a rollback does not resurrect a
        // stale `.ctx` from a previous snapshot.
        let ctx_ok = if ctx.exists() {
            std::fs::copy(ctx, dir.join("session.ctx")).is_ok()
        } else {
            true
        };
        if !(jsonl_ok && ctx_ok) {
            // A half-snapshot must never become a restorable version — it
            // would claim a `.ctx` that was never captured.
            std::fs::remove_dir_all(&dir).ok();
            return None;
        }
        // Evict AFTER a successful capture: a failed snapshot must not cost
        // the oldest version.
        let versions = self.versions(session_id);
        if versions.len() > UNDO_VERSIONS_CAP
            && let Some(oldest) = versions.first().cloned()
        {
            std::fs::remove_dir_all(self.session_dir(session_id).join(oldest)).ok();
        }
        Some(version)
    }

    /// Version labels of a session, oldest first (`"v1"`, `"v2"`, …),
    /// numerically ordered.
    pub fn versions(&self, session_id: &str) -> Vec<String> {
        let mut versions: Vec<(u64, String)> = std::fs::read_dir(self.session_dir(session_id))
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().into_string().ok()?;
                        let n = name.strip_prefix('v')?.parse::<u64>().ok()?;
                        Some((n, name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        versions.sort_by_key(|(n, _)| *n);
        versions.into_iter().map(|(_, name)| name).collect()
    }

    /// Copy a stored version back over the session's live files. Returns
    /// `true` when the JSONL was restored (the `.ctx` may legitimately not
    /// exist in the snapshot — its absence is restored too, by deleting the
    /// live companion).
    pub fn restore(&self, session_id: &str, version: &str, jsonl: &Path, ctx: &Path) -> bool {
        let dir = self.session_dir(session_id).join(version);
        let snapshot_ctx = dir.join("session.ctx");
        // Each file is swapped atomically (tmp sibling + rename) and the
        // `.ctx` lands FIRST: if the second step fails the live transcript is
        // still the old one (the opposite mixed state — old transcript with
        // new context — is the dangerous one). A snapshot without a stored
        // `.ctx` restores that absence by deleting the live companion.
        if snapshot_ctx.exists() {
            if let Some(parent) = ctx.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if !atomic_copy(&snapshot_ctx, ctx) {
                return false;
            }
        } else if std::fs::remove_file(ctx).is_err() && ctx.exists() {
            return false;
        }
        atomic_copy(&dir.join("session.jsonl"), jsonl)
    }
}

/// Copy `src` to `dst` through a tmp sibling + rename, so a reader of `dst`
/// only ever sees the complete old or the complete new content (the same
/// guarantee the session store's `atomic_write` gives its own writes).
fn atomic_copy(src: &Path, dst: &Path) -> bool {
    let tmp = dst.with_extension(format!(
        "{}.tmp",
        dst.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    let ok = std::fs::copy(src, &tmp).is_ok() && std::fs::rename(&tmp, dst).is_ok();
    if !ok {
        std::fs::remove_file(&tmp).ok();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path) -> (PathBuf, PathBuf) {
        let jsonl = dir.join("session-1.jsonl");
        let ctx = dir.join("session-1.ctx");
        std::fs::write(&jsonl, "jsonl-body").unwrap();
        std::fs::write(&ctx, "ctx-body").unwrap();
        (jsonl, ctx)
    }

    #[test]
    fn snapshot_versions_are_numbered_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let (jsonl, ctx) = files(dir.path());
        let store = UndoStore::with_root(tempfile::tempdir().unwrap().path().to_path_buf());

        for i in 1..=UNDO_VERSIONS_CAP + 2 {
            std::fs::write(&jsonl, format!("body-{i}")).unwrap();
            let v = store.snapshot("s1", &jsonl, &ctx).expect("snapshot lands");
            assert_eq!(v, format!("v{i}"));
        }
        // Cap reached: the two oldest were evicted FIFO.
        let versions = store.versions("s1");
        assert_eq!(versions.len(), UNDO_VERSIONS_CAP);
        assert_eq!(versions.first().map(String::as_str), Some("v3"));
        assert_eq!(versions.last().map(String::as_str), Some("v12"));
    }

    #[test]
    fn rollback_restores_both_files_and_the_ctx_absence() {
        let dir = tempfile::tempdir().unwrap();
        let (jsonl, ctx) = files(dir.path());
        let store = UndoStore::with_root(tempfile::tempdir().unwrap().path().to_path_buf());

        store.snapshot("s2", &jsonl, &ctx);
        // The live state moves on: new transcript, and the companion deleted.
        std::fs::write(&jsonl, "jsonl-new").unwrap();
        std::fs::remove_file(&ctx).unwrap();

        assert!(store.restore("s2", "v1", &jsonl, &ctx));
        assert_eq!(std::fs::read_to_string(&jsonl).unwrap(), "jsonl-body");
        assert_eq!(std::fs::read_to_string(&ctx).unwrap(), "ctx-body");

        // A snapshot without a companion restores the absence too.
        std::fs::remove_file(&ctx).unwrap();
        store.snapshot("s3", &jsonl, &ctx); // no .ctx on disk now
        std::fs::write(&ctx, "stale-ctx").unwrap();
        store.restore("s3", "v1", &jsonl, &ctx);
        assert!(!ctx.exists(), "a ctx-less snapshot restores the absence");
    }

    #[test]
    fn snapshot_requires_a_jsonl_and_versions_of_a_missing_session_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("missing.jsonl");
        let ctx = dir.path().join("missing.ctx");
        let store = UndoStore::with_root(tempfile::tempdir().unwrap().path().to_path_buf());
        assert!(store.snapshot("s4", &jsonl, &ctx).is_none());
        assert!(store.versions("s4").is_empty());
    }
}
