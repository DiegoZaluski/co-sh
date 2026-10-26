//! Persisted per-agent model picks (the CLI-parity "remember my last model").
//!
//! An agent's native CLI remembers the last model the user chose and uses it
//! for every NEW session; the interactive layer that writes that choice never
//! runs in the headless ACP harnesses the panes drive, so without a client-side
//! record a freshly created pane always lands on the harness's own default —
//! frequently a different (often paid-tier) model. This store carries that
//! record: one agent-name → model-id surface, written ONLY when the user
//! actually picks a model through a config box, read at turn dispatch to seed
//! the pick. Nothing else writes it — defaults (harness, registry) never touch
//! the file.
//!
//! Deliberately NOT app configuration: this is domain state of the sub-agent
//! integration, sibling of `palette` and `events`. The crate must stay
//! embeddable independent of cosh, so NO directory is hardcoded here — the
//! caller injects the storage directory (cosh passes its data-dir override
//! resolution; tests pass a temp dir), the same separation
//! [`crate::session::store::SessionStore`] uses with its `with_dir`.
//!
//! Single-file, atomically replaced: every write serializes the WHOLE map to a
//! sibling temp file (fsync'd) and renames it over the target — rename
//! replaces the destination atomically on Unix AND Windows
//! (`MOVEFILE_REPLACE_EXISTING`), so a crash mid-write can never leave a
//! truncated JSON behind. A missing or corrupted file reads as an empty map
//! (convention of every store in this codebase: storage failure degrades to
//! "no pick", never to a panic).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name the caller's directory is expected to hold. Exported so the
/// caller composes `dir.join(FILE_NAME)` and the name lives next to the code
/// that defines the format (no path knowledge leaks INTO the crate, none
/// duplicates outside it).
pub const MODEL_PICKS_FILE: &str = "model-picks.json";

/// The persisted agent → model-id surface.
///
/// `BTreeMap` keeps the serialized file's key order stable across writes, so a
/// diff of the file only ever shows the entry that actually changed.
#[derive(Debug, Default, Clone)]
pub struct ModelPicks {
    path: PathBuf,
    picks: BTreeMap<String, String>,
}

/// The on-disk envelope (top-level object keeps the file extensible — a
/// version or a future sibling field does not reformat every reader).
#[derive(Serialize, Deserialize, Default)]
struct ModelPicksFile {
    picks: BTreeMap<String, String>,
}

impl ModelPicks {
    /// Open (or create) the store rooted at `dir`, loading any existing picks.
    /// The caller owns the directory decision (data-dir override, test temp
    /// dir); this side only composes the file name and reads what is there.
    /// A missing file loads as empty; an unreadable or corrupted one ALSO
    /// loads as empty (degrading, never panicking) with a logged warning —
    /// the cause stays visible in the logs while the store stays usable.
    pub fn new(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).ok();
        let path = dir.join(MODEL_PICKS_FILE);
        let picks = match load_from_disk(&path) {
            Ok(picks) => picks.unwrap_or_default(),
            Err(error) => {
                log::warn!("agent model picks ignored ({error}); starting empty");
                BTreeMap::new()
            }
        };
        Self { path, picks }
    }

    /// Test-only store at an explicit file path (the convention of
    /// `UsageStore::with_file`): the parent directory is created, the file
    /// read, no data-dir policy consulted.
    #[cfg(test)]
    pub(crate) fn with_file(file: PathBuf) -> Self {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let picks = load_from_disk(&file).ok().flatten().unwrap_or_default();
        Self { path: file, picks }
    }

    /// The persisted model id for `agent`, if the user ever picked one there.
    #[must_use]
    pub fn get(&self, agent: &str) -> Option<&str> {
        self.picks.get(agent).map(String::as_str)
    }

    /// Record `agent`'s last user-picked model and persist the whole surface.
    ///
    /// The pick overwrites in memory first, then ONE atomic replace writes the
    /// complete map — the other agents' entries ride along unchanged by
    /// construction (the merge happened in memory before the flush).
    ///
    /// Storage failure is reported, never fatal: the pick stays in memory (the
    /// session keeps serving it) and a later `set` may still land. The caller
    /// surfaces the failure or ignores it; nothing here blocks a turn.
    pub fn set(&mut self, agent: &str, model: &str) -> Result<(), String> {
        self.picks
            .insert(agent.to_string(), model.to_string());
        save_to_disk(&self.path, &self.picks)
    }

    /// Test-only access to the in-memory surface.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.picks.len()
    }
}

/// Read the store file. `Ok(None)` on a missing file (first run), `Err` on an
/// unreadable/corrupted one (the caller decides to warn; the map degrades to
/// empty either way).
fn load_from_disk(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str::<ModelPicksFile>(&content)
        .map(|f| Some(f.picks))
        .map_err(|e| format!("corrupted {}: {e}", path.display()))
}

/// A writer-UNIQUE temp path beside `path`: the fixed `with_extension` name
/// two concurrent writers would share (two cosh processes, or a save racing a
/// reopen) lets one writer's `create` truncate the other's half-written temp
/// before its rename lands — the exact torn-write the atomic replace exists
/// to prevent. The pid plus a process-unique counter gives every writer its
/// own temp; the final rename is still the only step that touches the target.
fn unique_tmp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let file = format!(
        "{}.{}.{}.tmp",
        MODEL_PICKS_FILE,
        std::process::id(),
        seq
    );
    path.with_file_name(file)
}

/// Atomically replace the store file with the full map: serialize → write the
/// sibling temp file fully (fsync'd) → rename over the target. Renaming
/// replaces the destination on Unix AND Windows, so readers never observe a
/// half-written file.
fn save_to_disk(path: &Path, picks: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(&ModelPicksFile {
        picks: picks.clone(),
    })
    .map_err(|e| e.to_string())?;
    let tmp = unique_tmp_path(path);
    // The file is fully written and fsync'd, then closed, BEFORE the rename:
    // the handle drops at the end of the block, so the atomic replace never
    // races an open writer. Failures carry the io error and the temp path —
    // an opaque "create failed" with no errno is undiagnosable in the field.
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        use std::io::Write as _;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("temp write {} failed: {e}", tmp.display()));
    }
    // The rename's durability leans on the DIRECTORY entry surviving the
    // crash too: on Unix a fsync of the parent dir pins the replaced name
    // (Windows needs no separate step — the rename is already durable
    // there). Without it a crash right after the rename could revert the
    // file to the previous generation; harmless (the store degrades to "no
    // pick") but cheap to prevent.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        // Leave the temp file for diagnosis; the next save replaces it.
        format!("atomic rename failed: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (tempfile::TempDir, ModelPicks) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ModelPicks::with_file(dir.path().join(name));
        (dir, store)
    }

    #[test]
    fn set_get_and_reopen_roundtrip() {
        let (_dir, mut store) = temp_store("picks.json");
        store.set("claude", "claude-sonnet-4").expect("save");
        store.set("kilo", "kilo/nvidia/nemotron").expect("save");

        assert_eq!(store.get("claude"), Some("claude-sonnet-4"));
        // A NEW store over the same file sees both entries.
        let reopened = ModelPicks::with_file(_dir.path().join("picks.json"));
        assert_eq!(reopened.get("claude"), Some("claude-sonnet-4"));
        assert_eq!(reopened.get("kilo"), Some("kilo/nvidia/nemotron"));
        assert_eq!(reopened.get("gemini"), None);
    }

    #[test]
    fn set_overwrites_only_that_agent() {
        let (_dir, mut store) = temp_store("picks.json");
        store.set("claude", "old").expect("save");
        store.set("kilo", "kilo/free").expect("save");
        store.set("claude", "new").expect("save");

        assert_eq!(store.get("claude"), Some("new"));
        assert_eq!(store.get("kilo"), Some("kilo/free"));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let (_dir, store) = temp_store("never-written.json");
        assert_eq!(store.get("claude"), None);
    }

    #[test]
    fn corrupted_file_degrades_to_empty_not_panic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("corrupt.json");
        std::fs::write(&file, "this is not json").expect("write");
        let store = ModelPicks::with_file(file);
        assert_eq!(store.get("any"), None);
    }

    #[test]
    fn new_over_a_corrupted_file_repairs_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("corrupt.json");
        std::fs::write(&file, "garbage").expect("write");
        let mut store = ModelPicks::with_file(file);
        store.set("claude", "claude-sonnet-4").expect("save");
        // The write replaced the corrupt content wholesale.
        let reopened = ModelPicks::with_file(dir.path().join("corrupt.json"));
        assert_eq!(reopened.get("claude"), Some("claude-sonnet-4"));
    }

    #[test]
    fn two_writers_on_one_file_never_publish_torn_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("picks.json");
        // Two handles = the two-cosh-processes shape: each writer owns its
        // pid+counter temp file, so one writer's `create` can never truncate
        // the other's half-written temp before its rename lands (the fixed
        // `with_extension` name used to allow exactly that).
        let mut a = ModelPicks::with_file(file.clone());
        let mut b = ModelPicks::with_file(file.clone());
        a.set("claude", "model-a").expect("save a");
        b.set("kilo", "kilo/free").expect("save b");

        // Last-writer-wins is the documented multi-process contract: every
        // rename publishes that writer's COMPLETE in-memory map, so the file
        // on disk is always whole, parseable JSON — never a torn merge.
        let text = std::fs::read_to_string(&file).expect("the file exists");
        serde_json::from_str::<serde_json::Value>(&text)
            .expect("the file is always complete JSON");
        let reopened = ModelPicks::with_file(file);
        // Whichever write survived, its entry is intact and the other writer
        // never corrupted the shape.
        assert!(
            reopened.get("claude").is_some() || reopened.get("kilo").is_some(),
            "the surviving writer's entry is present"
        );
        // A successful rename consumes its temp file: no `.tmp` strays remain.
        let strays = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(strays, 0, "successful writes leave no temp files behind");
    }

    #[test]
    fn file_shape_is_a_flat_agent_model_surface() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("picks.json");
        let mut store = ModelPicks::with_file(file.clone());
        store.set("claude", "claude-sonnet-4").expect("save");
        let text = std::fs::read_to_string(&file).expect("read");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(
            value,
            serde_json::json!({ "picks": { "claude": "claude-sonnet-4" } })
        );
    }
}
