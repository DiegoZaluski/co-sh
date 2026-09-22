//! Git + LSP status for the file explorer tree (Ctrl+F).
//!
//! A single "status index" the explorer's render loop consults, built from
//! two inputs that already exist in the project — no parallel tracking is
//! introduced:
//!
//! * **LSP diagnostics** — the process-wide singleton engine
//!   ([`cosh::harness::lsp::global_lsp`], the same store the `lsp_*` tools and
//!   the passive tool findings read). Peeked read-only via
//!   [`cosh::harness::lsp::peek_global_lsp`]: the explorer never starts a
//!   server, it only shows what servers have already published.
//! * **Git status** — one `git status --porcelain=v1 -z` call per second in a
//!   background thread. The project's git utilities read branch state
//!   directly from `.git` (see [`crate::util::git`]), but porcelain status
//!   cannot be reproduced that way without reimplementing git; shelling out
//!   is what VS Code and Zed do. Missing `git` binary / non-repo simply means
//!   no git colors — never an error surface.
//!
//! Files map to their own state; every directory aggregates the states of
//! everything below it, so a collapsed directory lights up before it is
//! expanded (VS Code/Zed-style decorations):
//!
//! * file name colored by git state — modified `warning`, added/untracked
//!   `diff_added`, deleted `diff_removed` (the diff palette the transcript
//!   already uses);
//! * a trailing `✗`/`⚠` marker in the theme's `error`/`warning` color for
//!   LSP diagnostics — the same glyphs the passive LSP notes render with;
//! * directories keep the `primary` name color and carry a trailing marker
//!   for the strongest state below them.
//!
//! The index is rebuilt only when a source actually changed, detected via
//! two cheap tokens: the diagnostics engine's global version (bumped on every
//! ingest) and the watcher's snapshot revision. [`StatusIndex::refresh`]
//! returns immediately while both are unchanged, so idling costs one integer
//! compare per frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosh_sdk::lsp::lsp_types::DiagnosticSeverity;

/// Git state of one file, as the explorer renders it. Variant order is the
/// display priority for directory aggregation (modified beats deleted beats
/// added).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum GitFileStatus {
    Added,
    Deleted,
    Modified,
}

/// LSP severity bucket for one file, in display priority order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LspFileStatus {
    Warning,
    Error,
}

/// Combined status of one tree entry. Files carry their own state;
/// directories carry the strongest state of anything below them, per axis.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct EntryStatus {
    /// Uncommitted git state (name color for files, marker for directories).
    pub git: Option<GitFileStatus>,
    /// LSP diagnostics state (trailing marker for files and directories).
    pub lsp: Option<LspFileStatus>,
}

/// Interval between background `git status` polls. One cheap call per second
/// (the order of cadence the footer's branch tracker uses) is plenty for a
/// human-scale signal.
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Result of one `git status` run: repo root plus per-path states.
#[derive(Default, Clone, PartialEq)]
struct GitSnapshot {
    /// Absolute repo root; `None` when not a repo / git unavailable.
    root: Option<PathBuf>,
    /// States by absolute path (porcelain paths are root-relative).
    entries: HashMap<PathBuf, GitFileStatus>,
}

/// Parse `git status --porcelain=v1 -z --untracked-files=all` output into
/// per-path states under `root` (absolute). Each record is `XY PATH\0`; a
/// rename/copy record is followed by an extra `\0`-separated origin path,
/// which is consumed and ignored — the destination is what the tree shows.
fn parse_porcelain(stdout: &str, root: &Path) -> HashMap<PathBuf, GitFileStatus> {
    let mut entries = HashMap::new();
    let mut records = stdout.split('\0');
    while let Some(record) = records.next() {
        // Short records (or a trailing empty field after the final NUL) are
        // not status lines.
        if record.len() < 4 {
            continue;
        }
        let xy = &record[..2];
        let path = &record[3..];
        let state = if xy == "??" {
            GitFileStatus::Added
        } else if xy.contains('D') {
            GitFileStatus::Deleted
        } else if xy.starts_with('A') {
            GitFileStatus::Added
        } else {
            // Everything else (M, conflicts, typechanges) reads as a
            // modification — the state a user thinks of as "not committed".
            GitFileStatus::Modified
        };
        entries.insert(root.join(path), state);
        // Rename/copy records carry the origin path as the next NUL field.
        if xy.starts_with('R') || xy.starts_with('C') {
            records.next();
        }
    }
    entries
}

/// Scope the watcher's repo-wide git entries down to the tree shown by the
/// explorer: paths outside the root are dropped so they can never pollute
/// the directory aggregation (which would otherwise climb ancestors past
/// the root all the way to `/`).
///
/// Paths inside the root are re-rooted as *root-relative then re-joined*
/// only when the prefix matches, so textual path-spelling mismatches
/// degrade to "no git status" rather than wrong entries.
fn scope_git_entries(
    entries: HashMap<PathBuf, GitFileStatus>,
    explorer_root: &Path,
) -> HashMap<PathBuf, EntryStatus> {
    let mut scoped = HashMap::new();
    for (path, state) in entries {
        if path.starts_with(explorer_root) {
            scoped.insert(
                path,
                EntryStatus {
                    git: Some(state),
                    lsp: None,
                },
            );
        }
    }
    scoped
}

/// Run `git status` for the repository governing `dir`. Soft-fails to an
/// empty snapshot (no git, no repo, error) — git color is simply absent.
fn git_status(dir: &Path) -> GitSnapshot {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
    };

    // Repo root is needed because porcelain paths are root-relative while
    // the tree is rooted at the working directory.
    let Some(root) = run(&["rev-parse", "--show-toplevel"]) else {
        return GitSnapshot::default();
    };
    let Ok(root) = String::from_utf8(root.stdout) else {
        return GitSnapshot::default();
    };
    let root = PathBuf::from(root.trim());
    if !root.is_absolute() {
        return GitSnapshot::default();
    }

    let Some(out) = run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]) else {
        return GitSnapshot::default();
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let entries = parse_porcelain(&stdout, &root);
    GitSnapshot {
        root: Some(root),
        entries,
    }
}

/// Background poller: refreshes `git status` every [`GIT_POLL_INTERVAL`] on
/// its own thread and publishes a new snapshot revision only when the status
/// actually changed. Started when the file explorer is first opened; one
/// cheap call per second keeps the index warm for the session.
#[derive(Clone)]
pub struct GitWatcher {
    inner: Arc<GitWatcherInner>,
}

struct GitWatcherInner {
    /// Monotonic snapshot revision; changed ⇒ a new snapshot is published.
    revision: AtomicU64,
    snapshot: Mutex<GitSnapshot>,
}

impl GitWatcher {
    /// Start polling the repository that governs `root`.
    ///
    /// A spawn failure (resource exhaustion) is soft: the watcher stays at
    /// revision 0 and the explorer renders without git colors.
    pub fn start(root: PathBuf) -> Self {
        let watcher = Self {
            inner: Arc::new(GitWatcherInner {
                revision: AtomicU64::new(0),
                snapshot: Mutex::new(GitSnapshot::default()),
            }),
        };
        let inner = Arc::clone(&watcher.inner);
        let spawned = std::thread::Builder::new()
            .name("cosh-explorer-git".into())
            .spawn(move || {
                let mut last = GitSnapshot::default();
                loop {
                    let snapshot = git_status(&root);
                    if snapshot != last {
                        last = snapshot.clone();
                        *inner.snapshot.lock().expect("git watcher lock") = snapshot;
                        inner.revision.fetch_add(1, Ordering::Release);
                    }
                    std::thread::sleep(GIT_POLL_INTERVAL);
                }
            });
        if spawned.is_err() {
            log::warn!("file explorer: git status watcher thread failed to start");
        }
        watcher
    }

    fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn snapshot(&self) -> GitSnapshot {
        self.inner
            .snapshot
            .lock()
            .expect("git watcher lock")
            .clone()
    }
}

/// Aggregated status for every path the explorer shows: files map to their
/// own state, directories to the strongest state of anything below them.
/// Rebuilt (only) when the LSP diagnostics version or the git snapshot
/// revision moves.
#[derive(Default, Clone)]
pub struct StatusIndex {
    files: HashMap<PathBuf, EntryStatus>,
    dirs: HashMap<PathBuf, EntryStatus>,
    /// The root the index was built for (directory aggregation stops
    /// below it; the root itself is not an explorer row).
    explorer_root: PathBuf,
    /// Git axis cache, keyed by the root it was scoped to — re-read only
    /// when the git snapshot revision or the root moves, so an LSP-only
    /// change never pays for another pass over the git state.
    git_files: HashMap<PathBuf, EntryStatus>,
    git_root: PathBuf,
    lsp_version: u64,
    git_revision: u64,
}

impl StatusIndex {
    /// Status for one tree row (`is_dir` selects the aggregated map). Clean
    /// entries come back as the default (all-`None`) status.
    pub fn entry_status(&self, path: &Path, is_dir: bool) -> EntryStatus {
        let entry = if is_dir {
            self.dirs.get(path)
        } else {
            self.files.get(path)
        };
        entry.copied().unwrap_or_default()
    }

    /// Rebuild the index if either source changed. `explorer_root` both
    /// scopes the (global) diagnostics snapshot to the tree's root and keys
    /// the LSP singleton lookup. Returns whether a rebuild happened.
    ///
    /// The LSP side is a pure peek: only an already-running engine bound to
    /// this root is read — the explorer never starts a server. No engine
    /// yet means the LSP axis is simply absent (version 0).
    pub fn refresh(&mut self, explorer_root: &Path, watcher: &GitWatcher) -> bool {
        // One peek per refresh: the engine is resolved once and used both
        // for its version (change detection) and its snapshot below.
        let lsp = cosh::harness::lsp::peek_global_lsp(&explorer_root.to_string_lossy());
        let lsp_version = lsp
            .as_ref()
            .map(|lsp| lsp.diagnostics_engine().version())
            .unwrap_or(0);
        let git_revision = watcher.revision();
        if lsp_version == self.lsp_version && git_revision == self.git_revision {
            return false;
        }

        // Git axis: re-read (and scope to the tree's root) only when the
        // snapshot revision or the root moved — an LSP-only change reuses
        // the cached entries.
        if git_revision != self.git_revision || self.git_root != explorer_root {
            let git = watcher.snapshot();
            self.git_files = scope_git_entries(git.entries, explorer_root);
            self.git_root = explorer_root.to_path_buf();
        }

        let mut files: HashMap<PathBuf, EntryStatus> = HashMap::with_capacity(self.git_files.len());
        for (path, status) in &self.git_files {
            files.insert(path.clone(), *status);
        }

        // LSP: only what the (already running) engine has published — a pure
        // peek, never creating the engine or starting a server.
        if let Some(lsp) = lsp {
            let engine = lsp.diagnostics_engine();
            for (path, diags) in engine.snapshot_all(Some(explorer_root)) {
                let severity = diags
                    .iter()
                    .map(|d| d.severity.unwrap_or(DiagnosticSeverity::ERROR))
                    .map(|s| {
                        if s == DiagnosticSeverity::ERROR {
                            LspFileStatus::Error
                        } else {
                            LspFileStatus::Warning
                        }
                    })
                    .max();
                if let Some(lsp_status) = severity {
                    // Fresh file: default + lsp. Git-tracked file: the two
                    // axes merge — the map entry keeps its git state.
                    files.entry(path).or_default().lsp = Some(lsp_status);
                }
            }
        }

        self.explorer_root = explorer_root.to_path_buf();
        self.files = files;
        self.lsp_version = lsp_version;
        self.git_revision = git_revision;
        self.aggregate();
        true
    }

    /// Roll directory states up from file states: every non-clean file
    /// contributes its (per-axis) strongest state to each ancestor below
    /// (and excluding) the explorer root. Ancestors with no interesting
    /// descendants are absent from the map.
    fn aggregate(&mut self) {
        let mut dirs: HashMap<PathBuf, EntryStatus> = HashMap::new();
        for (path, status) in &self.files {
            if status.git.is_none() && status.lsp.is_none() {
                continue;
            }
            let mut ancestor = path.parent();
            while let Some(dir) = ancestor
                && dir != self.explorer_root
            {
                let cur = dirs.entry(dir.to_path_buf()).or_default();
                cur.git = cur.git.max(status.git);
                cur.lsp = cur.lsp.max(status.lsp);
                ancestor = dir.parent();
            }
        }
        self.dirs = dirs;
    }

    /// Test seam: set one file's status directly and re-aggregate, so the
    /// render tests can exercise the mapping without a real git/LSP source.
    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, path: PathBuf, status: EntryStatus) {
        self.files.insert(path, status);
        self.aggregate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<PathBuf, GitFileStatus> {
        parse_porcelain(
            &entries
                .iter()
                .map(|(xy, p)| format!("{xy} {p}"))
                .collect::<Vec<_>>()
                .join("\0"),
            Path::new("/repo"),
        )
    }

    #[test]
    fn porcelain_states_map_to_display_statuses() {
        let entries = map(&[
            ("??", "new.txt"),
            (" M", "mod.txt"),
            ("M ", "staged.txt"),
            (" D", "gone.txt"),
            ("D ", "staged-gone.txt"),
            ("A ", "added.txt"),
            ("UU", "conflict.txt"),
        ]);
        assert_eq!(
            entries[&PathBuf::from("/repo/new.txt")],
            GitFileStatus::Added
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/mod.txt")],
            GitFileStatus::Modified
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/staged.txt")],
            GitFileStatus::Modified
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/gone.txt")],
            GitFileStatus::Deleted
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/staged-gone.txt")],
            GitFileStatus::Deleted
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/added.txt")],
            GitFileStatus::Added
        );
        assert_eq!(
            entries[&PathBuf::from("/repo/conflict.txt")],
            GitFileStatus::Modified
        );
    }

    #[test]
    fn porcelain_rename_records_skip_origin_path() {
        // `R  new.txt\0old.txt\0 M other.txt\0` — old.txt must be consumed.
        let stdout = "R  new.txt\0old.txt\0 M other.txt\0";
        let entries = parse_porcelain(stdout, Path::new("/repo"));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[&PathBuf::from("/repo/new.txt")],
            GitFileStatus::Modified
        );
        assert!(entries.contains_key(&PathBuf::from("/repo/other.txt")));
        assert!(!entries.contains_key(&PathBuf::from("/repo/old.txt")));
    }

    #[test]
    fn empty_and_short_records_are_skipped() {
        let entries = parse_porcelain("\0\0?? a\0", Path::new("/r"));
        assert_eq!(entries.len(), 1);
        assert!(entries.contains_key(&PathBuf::from("/r/a")));
    }

    #[test]
    fn git_entries_are_scoped_to_the_explorer_root() {
        // A repo-root-wide snapshot scoped down to a sub-tree root: paths
        // outside the root are dropped so directory aggregation never
        // climbs past the root.
        let entries = HashMap::from([
            (PathBuf::from("/repo/pkg/a.rs"), GitFileStatus::Modified),
            (PathBuf::from("/repo/other/b.rs"), GitFileStatus::Added),
            (PathBuf::from("/elsewhere/c.rs"), GitFileStatus::Modified),
        ]);
        let scoped = scope_git_entries(entries, Path::new("/repo/pkg"));
        assert_eq!(scoped.len(), 1);
        let status = scoped[&PathBuf::from("/repo/pkg/a.rs")];
        assert_eq!(status.git, Some(GitFileStatus::Modified));
        assert_eq!(status.lsp, None);
    }

    #[test]
    fn aggregation_rolls_states_up_to_ancestors() {
        let root = Path::new("/work");
        let mut index = StatusIndex::default();
        index.files = HashMap::from([
            (
                PathBuf::from("/work/src/err.rs"),
                EntryStatus {
                    git: None,
                    lsp: Some(LspFileStatus::Error),
                },
            ),
            (
                PathBuf::from("/work/src/sub/warn.rs"),
                EntryStatus {
                    git: None,
                    lsp: Some(LspFileStatus::Warning),
                },
            ),
            (
                PathBuf::from("/work/docs/new.md"),
                EntryStatus {
                    git: Some(GitFileStatus::Added),
                    lsp: None,
                },
            ),
            (PathBuf::from("/work/clean.txt"), EntryStatus::default()),
        ]);
        index.explorer_root = root.to_path_buf();
        index.aggregate();

        // src holds an error AND a (nested) warning: error wins the marker,
        // and both axes are tracked independently.
        let src = index.entry_status(&Path::new("/work/src"), true);
        assert_eq!(src.lsp, Some(LspFileStatus::Error));
        // src/sub only has the warning.
        assert_eq!(
            index.entry_status(&Path::new("/work/src/sub"), true).lsp,
            Some(LspFileStatus::Warning)
        );
        // docs aggregates the added file; the clean file contributes nothing.
        assert_eq!(
            index.entry_status(&Path::new("/work/docs"), true).git,
            Some(GitFileStatus::Added)
        );
        assert_eq!(
            index.entry_status(&Path::new("/work"), true),
            EntryStatus::default()
        );
        // The explorer root itself is not a row and is never aggregated.
        assert!(!index.dirs.contains_key(root));
    }

    #[test]
    fn git_and_lsp_merge_per_axis() {
        // A file with both a modification and diagnostics keeps both signals.
        let mut index = StatusIndex::default();
        index.files = HashMap::from([(
            PathBuf::from("/w/a.rs"),
            EntryStatus {
                git: Some(GitFileStatus::Modified),
                lsp: Some(LspFileStatus::Warning),
            },
        )]);
        index.explorer_root = PathBuf::from("/w");
        index.aggregate();
        let status = index.entry_status(&Path::new("/w/a.rs"), false);
        assert_eq!(status.git, Some(GitFileStatus::Modified));
        assert_eq!(status.lsp, Some(LspFileStatus::Warning));
    }
}
