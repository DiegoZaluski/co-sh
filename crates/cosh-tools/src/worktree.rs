//! Git worktree engine behind the `/agents` pane isolation.
//!
//! Pure git/FS machinery, no UI and no pane knowledge: the callers (the
//! `/worktree` slash command and the `agents_worktree` tool) decide WHICH
//! pane gets isolated and with whose authority; this module only knows how
//! to create, list and remove git worktrees safely.
//!
//! Design constraints (from the approved plan):
//!
//! - Worktrees live under `<OS temp>/cosh-worktrees/<slug>` by default. The
//!   tool layer never accepts a path from the agent — the agent can only
//!   ever pin a pane into this managed directory (an explicit path comes
//!   exclusively from the user's `/worktree add <path> <branch>`).
//! - Removal refuses dirty worktrees: `git worktree remove` without
//!   `--force` already does, and its refusal is surfaced verbatim instead
//!   of being bypassed.
//! - Branch names are pre-validated (empty, leading dash, whitespace) so a
//!   crafted name can never turn into a git flag; everything else is left
//!   to git's own ref-name rules, whose errors surface verbatim.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The managed root under the OS temp dir where default-path worktrees live
/// (`<OS temp>/cosh-worktrees`). Explicit paths (`/worktree add <path> …`)
/// may point anywhere the USER chooses; agent-driven assignment is limited
/// to this root by never accepting a path at the tool layer.
#[must_use]
pub fn worktrees_root() -> PathBuf {
    std::env::temp_dir().join("cosh-worktrees")
}

/// Who assigned a pane's worktree. Drives the permission rule: an
/// agent-side change or clear of a USER-assigned worktree must pass the
/// question dialog even in Yolo mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignedBy {
    /// The user, via the `/worktree` slash command on the focused pane.
    User,
    /// The main agent, via the `agents_worktree` tool.
    Agent,
}

/// A pane's worktree binding: where the pane works and under whose
/// authority. Persisted on the bound pane ref (`AgentPaneRef::Bound`), so a
/// reopened session restores the isolation alongside the agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneWorktree {
    /// The worktree checkout directory the pane's cwd is pinned to.
    pub path: PathBuf,
    /// The git branch checked out in the worktree.
    pub branch: String,
    /// The user-facing label (the footer badge and the pane listings
    /// render it). Defaults to the branch name at creation; renaming the
    /// label never touches git.
    pub name: String,
    /// Who assigned this binding (the permission rule's input).
    pub assigned_by: AssignedBy,
}

/// One entry of `git worktree list`: a registered checkout and, when it
/// sits on a branch, that branch's ref name (detached checkouts report
/// `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// Everything the engine can refuse, each with an actionable message.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// The `git` subprocess failed; the message carries git's stderr.
    #[error("git failed: {0}")]
    Git(String),
    /// The branch/worktree name is unusable (empty, a git flag lookalike,
    /// whitespace or control characters). Rejected before git ever runs so
    /// a crafted name cannot become an argument.
    #[error("invalid worktree name `{0}`: use letters, digits, dashes, slashes and underscores")]
    InvalidName(String),
    /// An explicitly requested worktree path already exists. Default paths
    /// never hit this — they get a `-2`-style collision suffix instead.
    #[error("worktree path `{0}` already exists")]
    PathExists(String),
    /// The `git` binary could not be spawned at all (missing or unexcutable).
    #[error("could not run git: {0}")]
    Spawn(String),
}

/// Slugify a name into a directory-safe component: lowercased ASCII, every
/// non-alphanumeric run collapsed to one dash, trimmed, capped at 64 chars.
/// `"fix issue #42"` → `fix-issue-42`; a name that slugs to nothing falls
/// back to `worktree`.
#[must_use]
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut dashes = 0usize;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            dashes = 0;
        } else if dashes == 0 && !slug.is_empty() {
            slug.push('-');
            dashes = 1;
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(64).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "worktree".to_string()
    } else {
        slug
    }
}

/// Pre-validate a branch/worktree name before it reaches a git argument:
/// non-empty, no leading dash (a git flag lookalike), no whitespace and no
/// control characters. Everything else (ref-name rules, `..`, `.lock`
/// endings) is git's own validation, whose errors surface verbatim.
fn validate_name(name: &str) -> Result<(), WorktreeError> {
    if name.is_empty()
        || name.starts_with('-')
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(WorktreeError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Run `git` inside `repo` and return its stdout. A non-zero exit surfaces
/// git's stderr verbatim — its refusal messages ("contains modified or
/// untracked files, use --force …") are exactly what the caller wants to
/// show, so they are never paraphrased away.
fn git(repo: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|error| WorktreeError::Spawn(error.to_string()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(WorktreeError::Git(stdout.trim().to_string()))
        } else {
            Err(WorktreeError::Git(stderr.to_string()))
        }
    }
}

/// Create a worktree on a NEW branch (`git worktree add <path> -b <name>`,
/// based on the repo's current HEAD) and describe it as a
/// [`PaneWorktree`].
///
/// `path`: the checkout directory. `None` (the agent-driven case) pins it
/// under [`worktrees_root()`] at the name's slug, suffixing `-2`, `-3`, …
/// on collision so a second assignment with the same slug never collides.
/// An explicit path (the user-driven case) must not exist — nothing is
/// created at or under a path the user did not type empty.
///
/// `assigned_by` records the authority the caller acts with (the
/// permission rule's input); the engine does not guess it.
///
/// # Errors
///
/// An invalid name ([`WorktreeError::InvalidName`]), an existing explicit
/// path ([`WorktreeError::PathExists`]), or any git refusal (unusable ref
/// name, no commits yet, …) surfaced verbatim.
pub fn create(
    repo: &Path,
    name: &str,
    path: Option<&Path>,
    assigned_by: AssignedBy,
) -> Result<PaneWorktree, WorktreeError> {
    validate_name(name)?;
    let slug = slugify(name);
    let dir = match path {
        Some(explicit) => {
            if explicit.exists() {
                return Err(WorktreeError::PathExists(explicit.display().to_string()));
            }
            explicit.to_path_buf()
        }
        None => {
            let root = worktrees_root();
            let _ = std::fs::create_dir_all(&root);
            let mut candidate = root.join(&slug);
            let mut suffix = 1usize;
            while candidate.exists() {
                suffix += 1;
                candidate = root.join(format!("{slug}-{suffix}"));
            }
            candidate
        }
    };
    let dir_str = dir.display().to_string();
    git(repo, &["worktree", "add", &dir_str, "-b", name])?;
    Ok(PaneWorktree {
        path: dir,
        branch: name.to_string(),
        name: name.to_string(),
        assigned_by,
    })
}

/// Remove a worktree (`git worktree remove`). Refuses dirty worktrees —
/// uncommitted or untracked content keeps the worktree (and its branch)
/// alive; git's refusal message surfaces verbatim. The branch itself is
/// never deleted: an exit keeps it.
///
/// # Errors
///
/// Any git refusal, verbatim.
pub fn remove(repo: &Path, path: &Path) -> Result<(), WorktreeError> {
    git(repo, &["worktree", "remove", &path.display().to_string()])?;
    Ok(())
}

/// List the repo's registered worktrees (`git worktree list --porcelain`).
///
/// # Errors
///
/// Any git refusal, verbatim.
pub fn list(repo: &Path) -> Result<Vec<WorktreeEntry>, WorktreeError> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
            });
        } else if let Some(ref_name) = line.strip_prefix("branch ") {
            if let Some(entry) = current.as_mut() {
                entry.branch = ref_name.strip_prefix("refs/heads/").map(String::from);
            }
        }
        // `HEAD <sha>` / `bare` / `detached` / blank separators: nothing to
        // record — a detached worktree stays `branch: None`.
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    Ok(entries)
}

/// Clean up ORPHANED temp worktrees: after a crash (or a manual `rm -rf`)
/// a directory under `root` can linger without a live registration (or the
/// registration can linger without its directory). Runs `git worktree
/// prune` first (drops registrations whose directory vanished), then
/// deletes every directory under `root` that is no longer registered —
/// orphaned checkout dirs are pure disk waste; nothing referenced by a
/// pane is ever touched. Returns how many directories were removed.
///
/// `root` is a parameter (production passes [`worktrees_root()`]) so the
/// scan is testable in isolation and never races a parallel test's
/// worktrees in the shared real root.
///
/// # Errors
///
/// A git failure (prune/list); individual directory removals are
/// best-effort (a dir that cannot be deleted is left for the next run).
pub fn cleanup(repo: &Path, root: &Path) -> Result<usize, WorktreeError> {
    git(repo, &["worktree", "prune"])?;
    let registered: Vec<PathBuf> = list(repo)?.into_iter().map(|e| e.path).collect();
    let mut removed = 0usize;
    let Ok(dirs) = std::fs::read_dir(root) else {
        return Ok(0);
    };
    for entry in dirs.flatten() {
        let path = entry.path();
        if path.is_dir() && !registered.iter().any(|r| r == &path) {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real throwaway repo with one commit on its default branch —
    /// `git worktree add` needs a HEAD to branch from.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        git(&path, &["init", "--quiet"]).expect("git init");
        git(&path, &["config", "user.email", "test@example.com"]).expect("email");
        git(&path, &["config", "user.name", "Test"]).expect("name");
        git(&path, &["commit", "--allow-empty", "-m", "init", "--quiet"]).expect("initial commit");
        (dir, path)
    }

    #[test]
    fn slugify_turns_names_into_directory_safe_components() {
        assert_eq!(slugify("fix-issue"), "fix-issue");
        assert_eq!(slugify("Fix Issue #42"), "fix-issue-42");
        assert_eq!(slugify("  --weird__name!!  "), "weird-name");
        assert_eq!(slugify(""), "worktree", "a name that slugs to nothing");
        assert_eq!(slugify("///"), "worktree");
        assert_eq!(
            slugify("feature/long-name").len(),
            "feature/long-name".len()
        );
        // Capped at 64 chars.
        let long = "a".repeat(100);
        assert_eq!(slugify(&long).len(), 64);
    }

    #[test]
    fn invalid_names_are_rejected_before_git_runs() {
        let (_dir, repo) = repo();
        for name in ["", "-b", "has space", "tab\there"] {
            assert!(
                create(&repo, name, None, AssignedBy::User).is_err(),
                "`{name}` must be rejected"
            );
        }
        // Nothing was created: the rejections happen before the git call.
        assert!(list(&repo).expect("list").len() == 1, "only the main tree");
    }

    #[test]
    fn create_at_an_explicit_path_checks_out_the_new_branch() {
        let (_dir, repo) = repo();
        let worktree_dir = _dir.path().join("my-wt");
        let wt = create(&repo, "feature/x", Some(&worktree_dir), AssignedBy::User)
            .expect("create at an explicit path");
        assert_eq!(wt.path, worktree_dir);
        assert_eq!(wt.branch, "feature/x");
        assert_eq!(wt.name, "feature/x");
        assert_eq!(wt.assigned_by, AssignedBy::User);
        // The checkout is real: a linked worktree's `.git` is a FILE whose
        // `gitdir:` target holds this worktree's HEAD — follow it and check
        // the branch there.
        assert!(worktree_dir.join(".git").is_file());
        let git_file = std::fs::read_to_string(worktree_dir.join(".git")).expect("git file");
        let git_dir = git_file
            .trim()
            .strip_prefix("gitdir:")
            .expect("gitdir pointer")
            .trim();
        let head =
            std::fs::read_to_string(PathBuf::from(git_dir).join("HEAD")).expect("worktree HEAD");
        assert!(head.contains("refs/heads/feature/x"), "{head}");
    }

    #[test]
    fn create_at_an_existing_explicit_path_is_refused() {
        let (_dir, repo) = repo();
        let occupied = _dir.path().join("occupied");
        std::fs::create_dir_all(&occupied).expect("dir");
        assert!(matches!(
            create(&repo, "feat", Some(&occupied), AssignedBy::User),
            Err(WorktreeError::PathExists(_))
        ));
    }

    /// Serialize the tests that use the REAL `worktrees_root()` (the shared
    /// OS temp dir): the default-path assertions and their pre-test sweep
    /// would race each other, and leftover directories from earlier runs
    /// would poison the suffix/collision expectations. The sweep only ever
    /// deletes worktrees this test file created (the slug prefixes below)
    /// plus orphaned leftovers with the same prefixes.
    fn shared_root_lock() -> std::sync::MutexGuard<'static, ()> {
        static ROOT_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        ROOT_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
    fn sweep_shared_root() {
        let root = worktrees_root();
        let Ok(dirs) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in dirs.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ours = ["fix-issue", "feat", "cleanup-test", "listed"]
                .iter()
                .any(|slug| {
                    name == *slug
                        || name
                            .strip_prefix(&format!("{slug}-"))
                            .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
                });
            if ours {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    #[test]
    fn create_at_the_default_path_slugs_and_suffixes_collisions() {
        let _lock = shared_root_lock();
        sweep_shared_root();
        let (_dir, repo) = repo();
        let first = create(&repo, "Fix.Issue", None, AssignedBy::Agent).expect("first");
        assert_eq!(first.path, worktrees_root().join("fix-issue"));
        assert_eq!(first.assigned_by, AssignedBy::Agent);
        // A different branch name slugging identically gets the -2 suffix —
        // the dir never collides even though the slug does.
        let second = create(&repo, "fix-issue", None, AssignedBy::Agent).expect("second");
        assert_eq!(second.path, worktrees_root().join("fix-issue-2"));
        assert_eq!(second.branch, "fix-issue", "the branch keeps its own name");
        assert_ne!(first.path, second.path);
        // Both are registered worktrees.
        let entries = list(&repo).expect("list");
        assert!(entries.iter().any(|e| e.path == first.path));
        assert!(entries.iter().any(|e| e.path == second.path));
    }

    #[test]
    fn the_same_branch_twice_is_a_git_refusal_surfaced_verbatim() {
        let _lock = shared_root_lock();
        sweep_shared_root();
        let (_dir, repo) = repo();
        create(&repo, "feat", None, AssignedBy::User).expect("first");
        let error = create(&repo, "feat", None, AssignedBy::User).expect_err("branch exists");
        assert!(matches!(error, WorktreeError::Git(_)), "{error}");
        // The failed attempt left no stray directory behind.
        assert!(!worktrees_root().join("feat-2").exists());
    }

    #[test]
    fn remove_refuses_a_dirty_worktree_and_removes_a_clean_one() {
        let _lock = shared_root_lock();
        sweep_shared_root();
        let (_dir, repo) = repo();
        let wt = create(&repo, "cleanup-test", None, AssignedBy::User).expect("create");
        // Untracked content makes the worktree dirty: removal is refused
        // (nothing is deleted behind the user's data).
        std::fs::write(wt.path.join("uncommitted.txt"), "precious").expect("dirty");
        let error = remove(&repo, &wt.path).expect_err("dirty refusal");
        assert!(matches!(error, WorktreeError::Git(_)), "{error}");
        assert!(wt.path.exists(), "a dirty worktree is never removed");
        // Clean it up and removal succeeds.
        std::fs::remove_file(wt.path.join("uncommitted.txt")).expect("clean again");
        remove(&repo, &wt.path).expect("clean removal");
        assert!(!wt.path.exists());
        assert!(
            !list(&repo).expect("list").iter().any(|e| e.path == wt.path),
            "the registration is gone too"
        );
    }

    #[test]
    fn list_reports_branches_and_skips_detached_entries_cleanly() {
        let _lock = shared_root_lock();
        sweep_shared_root();
        let (_dir, repo) = repo();
        let wt = create(&repo, "listed", None, AssignedBy::User).expect("create");
        let entries = list(&repo).expect("list");
        assert!(entries.len() >= 2, "main tree + the new worktree");
        let listed = entries.iter().find(|e| e.path == wt.path).expect("listed");
        assert_eq!(listed.branch.as_deref(), Some("listed"));
        // The main tree's entry parses too (its branch is whatever the
        // local git defaults to — master or main — presence is the point).
        assert!(entries[0].path.ends_with(_dir.path().file_name().unwrap()));
    }

    #[test]
    fn cleanup_removes_only_orphaned_directories() {
        let (_dir, repo) = repo();
        // An isolated root so parallel tests sharing the real
        // `worktrees_root()` are never touched.
        let root_dir = tempfile::tempdir().expect("root");
        let root = root_dir.path().join("cosh-worktrees");
        // An orphan: a directory with no registration.
        let orphan = root.join("orphan");
        std::fs::create_dir_all(&orphan).expect("orphan dir");
        // A live one: registered in `repo`.
        let slug = "live-wt";
        let live = root.join(slug);
        git(
            &repo,
            &[
                "worktree",
                "add",
                &live.display().to_string(),
                "-b",
                slug,
                "--quiet",
            ],
        )
        .expect("registered worktree inside the isolated root");
        // A stale registration: registered, then its directory deleted.
        let stale = root.join("stale-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                &stale.display().to_string(),
                "-b",
                "stale-wt",
                "--quiet",
            ],
        )
        .expect("stale worktree");
        std::fs::remove_dir_all(&stale).expect("delete the stale dir");

        let removed = cleanup(&repo, &root).expect("cleanup");
        assert_eq!(removed, 1, "only the orphan directory is deleted");
        assert!(!orphan.exists(), "the orphan is gone");
        assert!(live.exists(), "the registered worktree is untouched");
        let entries = list(&repo).expect("list after prune");
        assert!(
            !entries.iter().any(|e| e.path == stale),
            "the stale registration was pruned"
        );
        assert!(entries.iter().any(|e| e.path == live));
    }

    #[test]
    fn cleanup_with_no_root_at_all_is_a_no_op() {
        let (_dir, repo) = repo();
        let missing = _dir.path().join("never-created");
        assert_eq!(cleanup(&repo, &missing).expect("no root → no work"), 0);
    }
}
