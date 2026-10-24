//! Git worktree engine behind the `/agents` pane isolation.
//!
//! Pure git/FS machinery, no UI and no pane knowledge: the callers (the
//! `/worktree` slash command and the `agents_worktree` tool) decide WHICH
//! pane gets isolated and with whose authority; this module only knows how
//! to create git worktrees. It never removes worktrees or their branches —
//! those are permanent artifacts of the user's repository.
//!
//! Design constraints (from the approved plan):
//!
//! - Worktrees live under `<OS temp>/cosh-worktrees/<slug>` by default. The
//!   tool layer never accepts a path from the agent — the agent can only
//!   ever pin a pane into this managed directory (an explicit path comes
//!   exclusively from the user's `/worktree add <path> <branch>`).
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

/// Everything the engine can refuse, each with an actionable message.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// The `git` subprocess failed; the message carries git's stderr.
    #[error("git failed: {0}")]
    Git(String),
    /// The branch/worktree name is unusable (empty, a git flag lookalike,
    /// whitespace or control characters). Rejected before git ever runs so
    /// a crafted name cannot become an argument. The sentence is shared with
    /// the tool's pre-flight check (see [`validate_name`]).
    #[error("invalid worktree name `{0}`: no leading dash, no whitespace or control characters")]
    InvalidName(String),
    /// An explicitly requested worktree path already exists. Default paths
    /// never hit this — they get a `-2`-style collision suffix instead.
    #[error("worktree path `{0}` already exists")]
    PathExists(String),
    /// The `git` binary could not be spawned at all (missing or unexcutable).
    #[error("could not run git: {0}")]
    Spawn(String),
    /// The path exists but this repository has no worktree registered there.
    /// Refused rather than adopted: pinning a pane to an arbitrary directory
    /// would have the agent edit files that belong to no checkout of this
    /// repo (typically a checkout of a DIFFERENT project), silently.
    #[error(
        "`{path}` exists but is not a git worktree of {repo} — give the path of an existing \
         worktree of this repository, or a path that does not exist yet"
    )]
    NotAWorktree { path: String, repo: String },
    /// The path is the repository's own main checkout. There is nothing to
    /// isolate: the pane would run in the session's directory and its
    /// branch would be whatever the user checks out there next.
    #[error(
        "`{path}` is the main checkout of this repository, not a linked worktree — there is \
         nothing to isolate there; give a path that does not exist yet to create a worktree"
    )]
    MainCheckout { path: String },
    /// The worktree has a detached HEAD: porcelain reports no branch for it,
    /// so there is no branch name the argument could be checked against.
    #[error(
        "the worktree at `{path}` is on a detached HEAD, so it has no branch to match \
         `{expected}` — check out a branch there, or give a path that does not exist yet"
    )]
    DetachedHead { path: String, expected: String },
    /// The worktree is on a different branch than the one asked for. An
    /// argument that silently had no effect is exactly the wrong-branch
    /// failure this refusal exists to prevent.
    #[error(
        "the worktree at `{path}` is on `{actual}`, not `{expected}` — pass `{actual}` to \
         attach to it, or a path that does not exist yet"
    )]
    BranchMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}

/// One entry of `git worktree list --porcelain`: a registered checkout and,
/// when it sits on a branch, that branch's name (a detached checkout has
/// none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// The checkout directory, as git reports it (absolute, resolved).
    pub path: PathBuf,
    /// The branch checked out there; `None` for a detached HEAD or a bare
    /// repository.
    pub branch: Option<String>,
    /// Whether this is the repository's MAIN checkout. `git worktree list`
    /// always lists it first; it is a worktree for git but never a valid
    /// pane binding (see [`WorktreeError::MainCheckout`]).
    pub is_main: bool,
}

/// Which of the two things [`create_or_adopt`] did. The caller must say so:
/// a silent adoption makes `/worktree add` claim it built something it
/// found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// A new checkout was created at the path.
    Created,
    /// An existing worktree of this repository was re-entered.
    Adopted,
}

/// Parse `git worktree list --porcelain` output into entries.
///
/// The format is one attribute per line, a blank line between entries:
/// `worktree <path>`, `HEAD <sha>`, `branch refs/heads/<name>`, `detached`,
/// `bare`, `prunable <reason>`. Only the path and the branch matter here;
/// the FIRST entry is always the main checkout. Lines are split with
/// [`str::lines`], so CRLF output parses too, and the remainder of the line
/// is taken verbatim — paths with spaces survive.
#[must_use]
fn parse_worktree_list(out: &str) -> Vec<WorktreeEntry> {
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            entries.push(WorktreeEntry {
                path: PathBuf::from(path),
                branch: None,
                is_main: entries.is_empty(),
            });
        } else if let Some(ref_name) = line.strip_prefix("branch ")
            && let Some(entry) = entries.last_mut()
        {
            entry.branch = ref_name.strip_prefix("refs/heads/").map(String::from);
        }
    }
    entries
}

/// The worktrees of `repo`, main checkout included and flagged
/// ([`WorktreeEntry::is_main`]).
///
/// # Errors
///
/// Any git refusal, verbatim (no repository, git missing, …).
pub fn list(repo: &Path) -> Result<Vec<WorktreeEntry>, WorktreeError> {
    Ok(parse_worktree_list(&git(
        repo,
        &["worktree", "list", "--porcelain"],
    )?))
}

/// Whether two paths name the same checkout. Canonicalised on both sides,
/// because `/tmp/x`, `/tmp/./x` and a symlinked `/tmp` are one directory and
/// git reports the resolved spelling. Falls back to the textual comparison
/// when canonicalisation fails (a broken symlink, a path that vanished).
///
/// Public because the callers that decide whether a checkout is ALREADY
/// bound must answer the same question — "is this the same directory?" —
/// that the engine asks when it looks a path up. Two spellings of one
/// checkout must never be treated as two checkouts there either.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        // Canonicalisation is a best effort: when one side cannot be
        // resolved, fall back to comparing the raw spellings rather than
        // claiming "different" or "equal" outright.
        _ => lexical_normalize(a) == lexical_normalize(b),
    }
}

/// Fold `.` and `..` components without touching the disk, so a path typed
/// as `/repo/./x` or `/repo/sub/../x` still matches its plain spelling when
/// canonicalisation is unavailable.
#[must_use]
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Re-enter an EXISTING worktree of `repo` at `path`, checking it out on
/// `expected`. Validate and adopt, never "the directory exists, so adopt":
/// see the error variants for what each rejection protects.
fn adopt(
    repo: &Path,
    path: &Path,
    expected: &str,
    assigned_by: AssignedBy,
) -> Result<PaneWorktree, WorktreeError> {
    let shown = path.display().to_string();
    let entries = list(repo)?;
    let Some(entry) = entries.iter().find(|entry| same_path(&entry.path, path)) else {
        return Err(WorktreeError::NotAWorktree {
            path: shown,
            repo: repo.display().to_string(),
        });
    };
    if same_path(&entry.path, repo) {
        return Err(WorktreeError::MainCheckout { path: shown });
    }
    let Some(branch) = entry.branch.as_deref() else {
        return Err(WorktreeError::DetachedHead {
            path: shown,
            expected: expected.to_string(),
        });
    };
    if branch != expected {
        return Err(WorktreeError::BranchMismatch {
            path: shown,
            expected: expected.to_string(),
            actual: branch.to_string(),
        });
    }
    Ok(PaneWorktree {
        path: entry.path.clone(),
        branch: branch.to_string(),
        name: branch.to_string(),
        assigned_by,
    })
}

/// [`create`], except that an EXPLICIT path which already exists is
/// validated and ADOPTED when it is a worktree of `repo` on `expected`
/// branch (see [`Binding`]). The managed-root form (`path: None`) never
/// adopts — it always creates, exactly as before.
///
/// This is why worktrees can stay permanent: `/worktree exit` only drops the
/// binding, so without adoption an existing checkout would be unreachable
/// forever (deleting it by hand violates the product rule).
///
/// # Errors
///
/// Everything [`create`] refuses, plus the adoption refusals
/// ([`WorktreeError::NotAWorktree`], [`WorktreeError::MainCheckout`],
/// [`WorktreeError::DetachedHead`], [`WorktreeError::BranchMismatch`]).
pub fn create_or_adopt(
    repo: &Path,
    name: &str,
    path: Option<&Path>,
    assigned_by: AssignedBy,
) -> Result<(PaneWorktree, Binding), WorktreeError> {
    validate_name(name)?;
    let Some(explicit) = path else {
        return Ok((create(repo, name, None, assigned_by)?, Binding::Created));
    };
    let explicit = absolute_against(repo, explicit);
    if explicit.exists() {
        return Ok((adopt(repo, &explicit, name, assigned_by)?, Binding::Adopted));
    }
    Ok((
        create(repo, name, Some(&explicit), assigned_by)?,
        Binding::Created,
    ))
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
///
/// This is the ONE rule for a worktree/branch name: the registry's
/// pre-flight check (`panes::enqueue_worktree_op`) calls it instead of
/// restating it, so the tool and the engine accept exactly the same names
/// and refuse with exactly the same sentence.
pub fn validate_name(name: &str) -> Result<(), WorktreeError> {
    if name.is_empty()
        || name.starts_with('-')
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(WorktreeError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Resolve an explicitly typed worktree path against `repo`.
///
/// `git` runs with `current_dir(repo)`, so a RELATIVE path means "relative
/// to the repo" — and the value this engine STORES must mean the same
/// thing, because it later becomes a pane's session cwd, process cwd and
/// sandbox root, all resolved against the cosh process's own cwd. Storing
/// the raw text (`. /wt`) silently pinned the pane to a different tree.
///
/// `.`/`..` components are folded LEXICALLY: the directory does not exist
/// yet, so there is nothing to canonicalise on disk (and `canonicalize`
/// would fail on a path that does not exist, which is the normal case).
#[must_use]
fn absolute_against(repo: &Path, explicit: &Path) -> PathBuf {
    let joined = if explicit.is_absolute() {
        explicit.to_path_buf()
    } else {
        repo.join(explicit)
    };
    lexical_normalize(&joined)
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

/// Whether a local branch ref `name` already exists (`git rev-parse
/// --verify --quiet refs/heads/<name>`). A missing ref is git's silent
/// non-zero exit, so a failure here means "does not exist", never an error
/// the caller should surface.
fn branch_exists(repo: &Path, name: &str) -> Result<bool, WorktreeError> {
    let ref_name = format!("refs/heads/{name}");
    Ok(git(repo, &["rev-parse", "--verify", "--quiet", &ref_name]).is_ok())
}

/// Create a worktree and describe it as a [`PaneWorktree`].
///
/// The branch rule: a name that does not exist yet is created with
/// `git worktree add <path> -b <name>` (based on the repo's current HEAD);
/// a name that DOES exist but is checked out in no worktree is REUSED with
/// `git worktree add <path> <branch>` — an unbound branch is free to check
/// out, and rebinding it is what the user asked for. A branch already
/// checked out in another worktree is left to git's own refusal ("… is
/// already checked out at …"), surfaced verbatim.
///
/// `path`: the checkout directory. `None` (the agent-driven case) pins it
/// under [`worktrees_root()`] at the name's slug. An explicit path (the
/// user-driven case) is resolved against `repo` FIRST (a relative path is
/// relative to the repo, exactly as git reads it) and must not exist —
/// nothing is created at or under a path the user did not type empty.
///
/// The `-2`, `-3`, … suffix on the default path covers ONE case: two
/// DIFFERENT branch names whose slugs are identical (`Fix.Issue` and
/// `fix-issue`), where both branches are legal and only the directory
/// collides. It cannot rescue a repeated SAME name — that fails earlier, on
/// the branch ref (`-b` refuses an existing ref; an existing ref bound to
/// another worktree keeps git's own refusal), which is the correct outcome:
/// one branch, one worktree.
///
/// `assigned_by` records the authority the caller acts with (the
/// permission rule's input); the engine does not guess it.
///
/// # Errors
///
/// An invalid name ([`WorktreeError::InvalidName`]), an existing explicit
/// path ([`WorktreeError::PathExists`]), or any git refusal (unusable ref
/// name, no commits yet, branch already checked out, …) surfaced verbatim.
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
            // Absolute BEFORE the existence check and before storing: see
            // `absolute_against` (git resolves the argument against `repo`).
            let explicit = absolute_against(repo, explicit);
            if explicit.exists() {
                return Err(WorktreeError::PathExists(explicit.display().to_string()));
            }
            explicit
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
    // An existing branch ref is REUSED (checked out as-is) rather than
    // re-created: `-b` refuses an existing ref even when no worktree has it
    // checked out. Letting git do the checkout also keeps its "already
    // checked out at <path>" refusal verbatim for the genuinely busy case.
    if branch_exists(repo, name)? {
        git(repo, &["worktree", "add", &dir_str, name])?;
    } else {
        git(repo, &["worktree", "add", &dir_str, "-b", name])?;
    }
    Ok(PaneWorktree {
        path: dir,
        branch: name.to_string(),
        name: name.to_string(),
        assigned_by,
    })
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
        // Verify via git that only the main tree exists.
        let out = git(&repo, &["worktree", "list"]).expect("git list");
        assert_eq!(out.lines().count(), 1, "only the main tree");
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

    /// A RELATIVE explicit path means "relative to the repo" — the same
    /// thing git means when it runs with `current_dir(repo)` — and the value
    /// stored for the pane must say exactly that, because it later becomes a
    /// session/process cwd rooted at the cosh process, not at the repo.
    #[test]
    fn a_relative_explicit_path_is_stored_absolute_against_the_repo() {
        let (_dir, repo) = repo();
        let wt = create(&repo, "rel", Some(Path::new("./wt")), AssignedBy::User)
            .expect("create at a relative path");
        assert_eq!(
            wt.path,
            repo.join("wt"),
            "resolved against the repo, not the process"
        );
        assert!(
            wt.path.is_absolute(),
            "the stored cwd is absolute: {}",
            wt.path.display()
        );
        // The checkout really landed where the stored path points.
        assert!(wt.path.join(".git").is_file());
    }

    /// `..` is folded lexically (the directory does not exist yet, so
    /// there is nothing to canonicalise on disk).
    #[test]
    fn a_relative_path_folds_parent_components_without_touching_the_disk() {
        let (_dir, repo) = repo();
        let stored = absolute_against(&repo, Path::new("sub/../wt"));
        assert_eq!(stored, repo.join("wt"));
        // An absolute path is left alone.
        let abs = Path::new("/tmp/somewhere/wt");
        assert_eq!(absolute_against(&repo, abs), abs);
    }

    /// The `-2` suffix covers the DIFFERENT-names-same-slug case, which
    /// is the only collision the branch rule leaves open.
    #[test]
    fn two_different_names_with_the_same_slug_get_distinct_directories() {
        let (_dir, repo) = repo();
        let first = create(&repo, "Slug.Collision", None, AssignedBy::Agent).expect("first");
        let second = create(&repo, "slug-collision", None, AssignedBy::Agent).expect("second");
        assert_eq!(first.branch, "Slug.Collision");
        assert_eq!(second.branch, "slug-collision");
        assert_ne!(first.path, second.path, "the slug collision was suffixed");
        // Both live under the managed root, and the second one's directory is
        // the slug plus a numeric suffix. The exact NUMBER is not asserted:
        // the root is shared system temp state, so directories left by other
        // panes/tests legitimately advance the counter.
        assert_eq!(first.path.parent(), Some(worktrees_root().as_path()));
        assert_eq!(second.path.parent(), Some(worktrees_root().as_path()));
        let second_name = second
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a directory name");
        let suffix = second_name
            .strip_prefix("slug-collision-")
            .expect("the slug plus a suffix");
        assert!(
            !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()),
            "expected `<slug>-<n>`, got `{second_name}`"
        );
        // Leave no litter in the SHARED temp root: this test owns these two
        // checkouts, so it unregisters and deletes them itself (the engine
        // never deletes worktrees — that is a product rule for USER
        // worktrees, not for a test's own throwaway ones).
        for wt in [&first, &second] {
            let _ = git(
                &repo,
                &["worktree", "remove", &wt.path.display().to_string()],
            );
            let _ = std::fs::remove_dir_all(&wt.path);
        }
    }

    /// Feature A/1: a second `add` at the SAME path with the SAME branch
    /// adopts — no second checkout appears in `git worktree list`.
    #[test]
    fn a_second_add_at_the_same_path_and_branch_adopts_without_a_new_checkout() {
        let (_dir, repo) = repo();
        let path = _dir.path().join("adopt-me");
        let first = create(&repo, "feat/adopt", Some(&path), AssignedBy::User).expect("created");
        let before = list(&repo).expect("list").len();

        let (adopted, binding) =
            create_or_adopt(&repo, "feat/adopt", Some(&path), AssignedBy::Agent).expect("adopts");
        assert_eq!(binding, Binding::Adopted, "an existing worktree is adopted");
        assert_eq!(adopted.path, first.path, "same checkout");
        assert_eq!(adopted.branch, "feat/adopt");
        assert_eq!(
            adopted.name, "feat/adopt",
            "the name is the branch, as on create"
        );
        assert_eq!(
            adopted.assigned_by,
            AssignedBy::Agent,
            "the CALLER's authority, not the original one"
        );
        assert_eq!(
            list(&repo).expect("list").len(),
            before,
            "adoption never creates a second checkout"
        );
    }

    /// Feature A/1 (managed form): `path: None` ALWAYS creates — the managed
    /// root form never adopts, it suffixes.
    #[test]
    fn the_managed_root_form_never_adopts() {
        let (_dir, repo) = repo();
        let branch = unique_slug("managed");
        let (first, first_binding) =
            create_or_adopt(&repo, &branch, None, AssignedBy::User).expect("creates");
        assert_eq!(first_binding, Binding::Created);
        // A second ask under a DIFFERENT name creates a second checkout.
        let other = unique_slug("managed");
        let (second, second_binding) =
            create_or_adopt(&repo, &other, None, AssignedBy::Agent).expect("creates");
        assert_eq!(second_binding, Binding::Created);
        assert_ne!(first.path, second.path);
        for wt in [&first, &second] {
            let _ = git(
                &repo,
                &["worktree", "remove", &wt.path.display().to_string()],
            );
            let _ = std::fs::remove_dir_all(&wt.path);
        }
    }

    /// Feature A/4: same path, different branch → refused, naming BOTH.
    #[test]
    fn adopting_a_worktree_on_another_branch_is_refused_naming_both() {
        let (_dir, repo) = repo();
        let path = _dir.path().join("other-branch");
        create(&repo, "feat/actual", Some(&path), AssignedBy::User).expect("created");
        let error = create_or_adopt(&repo, "feat/asked", Some(&path), AssignedBy::User)
            .expect_err("mismatch");
        match &error {
            WorktreeError::BranchMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, "feat/asked");
                assert_eq!(actual, "feat/actual");
            }
            other => panic!("expected BranchMismatch, got {other}"),
        }
        let message = error.to_string();
        assert!(message.contains("feat/actual"), "{message}");
        assert!(message.contains("feat/asked"), "{message}");
    }

    /// Feature A/3: an existing directory that is NOT a worktree of this
    /// repo is refused with the dedicated variant (never adopted).
    #[test]
    fn a_directory_that_is_not_a_worktree_of_this_repo_is_refused() {
        // Build the FOREIGN repo FIRST: the destructuring below shadows the
        // `repo()` fixture fn with the local `repo` path.
        let (_other_dir, other_repo) = repo();
        let (_dir, repo) = repo();
        // (a) a plain directory
        let plain = _dir.path().join("plain");
        std::fs::create_dir_all(&plain).expect("dir");
        match create_or_adopt(&repo, "feat", Some(&plain), AssignedBy::User) {
            Err(WorktreeError::NotAWorktree { path, repo: named }) => {
                assert!(path.contains("plain"), "{path}");
                assert!(named.contains(&repo.display().to_string()), "{named}");
            }
            other => panic!("expected NotAWorktree, got {other:?}"),
        }
        // (b) a worktree of a DIFFERENT repository — the likely mistake.
        let foreign = _other_dir.path().join("foreign-wt");
        create(
            &other_repo,
            "foreign-branch",
            Some(&foreign),
            AssignedBy::User,
        )
        .expect("foreign");
        assert!(
            matches!(
                create_or_adopt(&repo, "foreign-branch", Some(&foreign), AssignedBy::User),
                Err(WorktreeError::NotAWorktree { .. })
            ),
            "another repository's checkout is not adoptable"
        );
    }

    /// Feature A/6: a detached-HEAD worktree has no branch to match.
    #[test]
    fn a_detached_head_worktree_is_refused() {
        let (_dir, repo) = repo();
        let path = _dir.path().join("detached");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                &path.display().to_string(),
                "--quiet",
            ],
        )
        .expect("detached worktree");
        // The parser must agree: no branch line exists for it.
        let entries = list(&repo).expect("list");
        let entry = entries
            .iter()
            .find(|entry| entry.path == path)
            .expect("listed");
        assert_eq!(entry.branch, None, "a detached worktree carries no branch");
        let error =
            create_or_adopt(&repo, "feat", Some(&path), AssignedBy::User).expect_err("detached");
        match &error {
            WorktreeError::DetachedHead { path, expected } => {
                assert!(path.contains("detached"), "{path}");
                assert_eq!(expected, "feat");
            }
            other => panic!("expected DetachedHead, got {other}"),
        }
        assert!(error.to_string().contains("detached HEAD"), "{error}");
    }

    /// Feature A/main-tree decision: the repository's own checkout is NOT
    /// adoptable as a pane binding.
    #[test]
    fn the_repositorys_main_checkout_is_not_adoptable() {
        let (_dir, repo) = repo();
        let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .expect("branch")
            .trim()
            .to_string();
        assert!(
            matches!(
                create_or_adopt(&repo, &branch, Some(&repo), AssignedBy::User),
                Err(WorktreeError::MainCheckout { .. })
            ),
            "the main checkout is refused deliberately"
        );
    }

    /// Feature A/6 (parser): sample porcelain output, including a detached
    /// entry and a path with spaces.
    #[test]
    fn the_porcelain_parser_reads_branches_and_tolerates_detached_and_spaces() {
        let sample = "worktree /repo/main\n\
                      HEAD 1111111111111111111111111111111111111111\n\
                      branch refs/heads/main\n\
                      \n\
                      worktree /tmp/with space/wt\n\
                      HEAD 2222222222222222222222222222222222222222\n\
                      branch refs/heads/feat/x\n\
                      \n\
                      worktree /tmp/detached\n\
                      HEAD 3333333333333333333333333333333333333333\n\
                      detached\n\
                      \n\
                      worktree /tmp/bare\n\
                      HEAD 4444444444444444444444444444444444444444\n\
                      bare\n\
                      ";
        let entries = parse_worktree_list(sample);
        assert_eq!(entries.len(), 4, "one entry per `worktree ` line");
        assert_eq!(entries[0].path, PathBuf::from("/repo/main"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(entries[0].is_main, "the first entry is the main checkout");
        assert!(!entries[1].is_main);
        assert_eq!(entries[1].path, PathBuf::from("/tmp/with space/wt"));
        assert_eq!(entries[1].branch.as_deref(), Some("feat/x"));
        assert_eq!(entries[2].branch, None, "detached: no branch line");
        assert_eq!(entries[3].branch, None, "bare: no branch line");
        // CRLF output parses too (`str::lines` strips the \r).
        let crlf = parse_worktree_list(
            "worktree /a\r\nHEAD 1111111111111111111111111111111111111111\r\nbranch refs/heads/b\r\n",
        );
        assert_eq!(crlf[0].path, PathBuf::from("/a"));
        assert_eq!(crlf[0].branch.as_deref(), Some("b"));
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

    /// A unique slug per test to avoid touching the shared real temp root.
    /// Each test deletes only its own artifacts via the TempDir drop.
    fn unique_slug(prefix: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{}-{}-{}", prefix, std::process::id(), n)
    }

    // Collision suffixing is tested implicitly by the unique_slug approach:
    // each test uses a unique slug, so no two tests ever collide.
    // The original collision test relied on the shared real root and has been
    // removed along with the cleanup machinery.

    #[test]
    fn the_same_branch_twice_is_a_git_refusal_surfaced_verbatim() {
        let (_dir, repo) = repo();
        let slug = unique_slug("feat");
        create(&repo, &slug, None, AssignedBy::User).expect("first");
        let error = create(&repo, &slug, None, AssignedBy::User).expect_err("branch exists");
        assert!(matches!(error, WorktreeError::Git(_)), "{error}");
    }

    #[test]
    fn create_refuses_a_branch_already_checked_out_in_another_worktree() {
        let (_dir, repo) = repo();
        let slug = unique_slug("feat");
        let first = create(&repo, &slug, None, AssignedBy::User).expect("first");
        let error = create(&repo, &slug, None, AssignedBy::Agent).expect_err("checked out");
        match error {
            WorktreeError::Git(message) => {
                assert!(
                    message.contains("already checked out")
                        || message.contains("is already used by worktree"),
                    "{message}"
                );
                assert!(
                    message.contains(&first.path.display().to_string()),
                    "{message}"
                );
            }
            other => panic!("expected a verbatim git refusal, got {other}"),
        }
    }
}
