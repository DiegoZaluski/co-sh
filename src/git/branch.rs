//! Git repository detection for the session footer.
//!
//! Mirrors what mainstream editors and agents do (Zed, VS Code, OpenCode):
//! locate the containing git directory by walking up from the working
//! directory, then read the current branch straight from `.git/HEAD`.
//!
//! This deliberately avoids shelling out to the `git` binary (slow, may be
//! missing), libgit2 (heavy C dependency, and it breaks on reftable
//! repositories — Zed removed it for exactly that reason) and heuristics of
//! any kind. Reading `HEAD` is the same thing `git symbolic-ref` does
//! internally, so the behavior matches the CLI without invoking it:
//!
//! - `ref: refs/heads/<name>`  → branch `<name>`
//! - anything else             → detached HEAD; the short commit SHA is shown
//!
//! Worktrees are supported the same way git itself supports them: a linked
//! worktree has a `.git` *file* containing `gitdir: <path>`, so we follow it
//! and read that directory's `HEAD`.
//!
//! Environment overrides are intentionally not honored: `GIT_DIR`,
//! `GIT_WORK_TREE` and `GIT_CEILING_DIRECTORIES` affect the CLI's view but
//! not the footer's, which always resolves from the session's working
//! directory on disk (the same choice VS Code makes for its SCM auto-detect).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the footer's branch is allowed to hit the disk. The render loop
/// runs at up to 30 fps; `HEAD` is a tiny file, but there is no reason to read
/// it per frame when branch switches happen at human timescales.
const RESOLVE_INTERVAL: Duration = Duration::from_millis(500);

/// Find the git directory that governs `start`, walking up through parents.
///
/// Returns the resolved git directory (for a linked worktree, the worktree's
/// own state directory, whose `HEAD` holds that worktree's branch). Returns
/// `None` when `start` is outside any repository.
pub fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        let dot_git = current.join(".git");
        match std::fs::metadata(&dot_git) {
            Ok(meta) if meta.is_dir() => return Some(dot_git),
            Ok(meta) if meta.is_file() => {
                // Linked worktree: `.git` is a file with "gitdir: <path>".
                // A malformed marker aborts the walk (returns None): git
                // itself treats a `.git` entry as the repository boundary,
                // so a broken one must not be bypassed in favor of a parent.
                let content = std::fs::read_to_string(&dot_git).ok()?;
                let target = content.trim().strip_prefix("gitdir:")?.trim();
                let git_dir = PathBuf::from(target);
                let git_dir = if git_dir.is_absolute() {
                    git_dir
                } else {
                    current.join(git_dir)
                };
                // Sanity-check the target looks like a git dir before trusting
                // its HEAD: `gitdir:` may point anywhere on disk.
                if git_dir.join("HEAD").exists() {
                    return Some(git_dir);
                }
                return None;
            }
            _ => {}
        }
        dir = current.parent();
    }
    None
}

/// Resolve the current branch (or short SHA when HEAD is detached) from a git
/// directory. Returns `None` when the directory does not look like a git dir
/// (missing/empty `HEAD`), in which case the footer simply shows no branch.
pub fn resolve_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(ref_name) = head.strip_prefix("ref:") {
        let ref_name = ref_name.trim();
        return ref_name
            .strip_prefix("refs/heads/")
            // Reject control characters: HEAD content is rendered verbatim in
            // the footer, so a crafted/odd ref must never inject them.
            .filter(|name| !name.chars().any(char::is_control))
            .map(String::from);
    }
    // Detached HEAD: HEAD holds a raw object id (40 hex chars for SHA-1
    // repositories, 64 for SHA-256). Validate the shape before displaying —
    // anything else is a corrupted or unsupported HEAD, not a commit.
    let is_sha =
        (head.len() == 40 || head.len() == 64) && head.chars().all(|c| c.is_ascii_hexdigit());
    if is_sha {
        Some(head.chars().take(7).collect())
    } else {
        None
    }
}

/// Cached branch resolver for the session footer.
///
/// Re-resolves at most once per [`RESOLVE_INTERVAL`] (or immediately when the
/// working directory changes), so per-frame rendering stays free of I/O.
#[derive(Debug, Default)]
pub struct BranchTracker {
    cached: Option<String>,
    last_resolve: Option<Instant>,
    last_cwd: Option<String>,
}

impl BranchTracker {
    /// Current branch for `cwd`, reading the repository state at most once per
    /// [`RESOLVE_INTERVAL`]. `None` when `cwd` is not inside a git repository.
    pub fn current(&mut self, cwd: &str) -> Option<String> {
        let cwd_changed = self.last_cwd.as_deref() != Some(cwd);
        if !cwd_changed
            && self
                .last_resolve
                .is_some_and(|t| t.elapsed() < RESOLVE_INTERVAL)
        {
            return self.cached.clone();
        }
        self.last_cwd = Some(cwd.to_string());
        self.last_resolve = Some(Instant::now());
        self.cached = if cwd.is_empty() {
            None
        } else {
            find_git_dir(Path::new(cwd)).and_then(|git_dir| resolve_branch(&git_dir))
        };
        self.cached.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/login\n").unwrap();
    }

    #[test]
    fn symbolic_ref_to_non_branch_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git/HEAD"), "ref: refs/tags/v1.0.0\n").unwrap();
        assert_eq!(resolve_branch(&tmp.path().join(".git")), None);
    }

    #[test]
    fn malformed_head_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        fs::write(tmp.path().join(".git/HEAD"), "not a valid head\n").unwrap();
        assert_eq!(resolve_branch(&tmp.path().join(".git")), None);
    }

    #[test]
    fn resolves_branch_from_regular_repo() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let git_dir = find_git_dir(tmp.path()).unwrap();
        assert_eq!(resolve_branch(&git_dir).as_deref(), Some("feature/login"));
    }

    #[test]
    fn finds_repo_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let nested = tmp.path().join("src/tui/routes");
        fs::create_dir_all(&nested).unwrap();
        let git_dir = find_git_dir(&nested).unwrap();
        assert_eq!(git_dir, tmp.path().join(".git"));
        assert_eq!(resolve_branch(&git_dir).as_deref(), Some("feature/login"));
    }

    #[test]
    fn returns_none_outside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(find_git_dir(tmp.path()), None);
        assert_eq!(
            BranchTracker::default().current(tmp.path().to_str().unwrap()),
            None
        );
    }

    #[test]
    fn follows_worktree_git_file() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        let git_dir = tmp.path().join("somewhere/else/.git/worktrees/feat");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", git_dir.display())).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feat\n").unwrap();

        let found = find_git_dir(&wt).unwrap();
        assert_eq!(found, git_dir);
        assert_eq!(resolve_branch(&found).as_deref(), Some("feat"));
    }

    #[test]
    fn follows_relative_worktree_git_file() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        fs::create_dir_all(tmp.path().join("main/.git/worktrees/wt")).unwrap();
        fs::write(
            tmp.path().join("main/.git/worktrees/wt/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        assert_eq!(
            resolve_branch(&find_git_dir(&wt).unwrap()).as_deref(),
            Some("main")
        );
    }

    #[test]
    fn detached_head_reports_short_sha() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        fs::write(
            tmp.path().join(".git/HEAD"),
            "1a2b3c4d5e6f708090a0b0c0d0e0f01020304050\n",
        )
        .unwrap();
        assert_eq!(
            resolve_branch(&tmp.path().join(".git")).as_deref(),
            Some("1a2b3c4")
        );
    }

    #[test]
    fn tracker_caches_within_interval() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let cwd = tmp.path().to_str().unwrap().to_string();
        let mut tracker = BranchTracker::default();
        assert_eq!(tracker.current(&cwd).as_deref(), Some("feature/login"));

        // Switch branches on disk; the cached value must still be served
        // because the resolve interval has not elapsed.
        fs::write(tmp.path().join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
        assert_eq!(tracker.current(&cwd).as_deref(), Some("feature/login"));

        // After the interval elapses the new branch is picked up.
        tracker.last_resolve = Some(Instant::now() - RESOLVE_INTERVAL);
        assert_eq!(tracker.current(&cwd).as_deref(), Some("other"));
    }

    #[test]
    fn tracker_reresolves_when_cwd_changes() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let other = tempfile::tempdir().unwrap();
        fs::create_dir_all(other.path().join(".git")).unwrap();
        fs::write(other.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let mut tracker = BranchTracker::default();
        let a = tmp.path().to_str().unwrap().to_string();
        let b = other.path().to_str().unwrap().to_string();
        assert_eq!(tracker.current(&a).as_deref(), Some("feature/login"));
        assert_eq!(tracker.current(&b).as_deref(), Some("main"));
    }
}
