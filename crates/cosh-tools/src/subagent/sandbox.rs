//! Kernel-pinned filesystem sandbox for the ACP sub-agent `fs/*` requests
//! (Unix only — see the `cfg` on the `mod sandbox;` declaration in `mod.rs`).
//!
//! The workspace root is pinned to a VERIFIED directory descriptor once per
//! call ([`PinnedRoot::acquire`]: open → `fstat` → compare against the
//! canonicalized path's `(dev, ino)`, retrying while the two observations
//! disagree — a concurrent root swap makes them diverge). The walk below
//! never re-resolves the root path and never lets the kernel resolve a full
//! path: every component is opened `openat`-relative to the previous
//! descriptor with `O_NOFOLLOW`, so the descriptor used for the actual
//! read/write is obtained DURING the walk — a concurrent swap of any
//! directory for an outside symlink is caught at open time and there is no
//! validate-then-use window, including at the root itself.
//!
//! Symlink policy:
//!
//! - Symlinks are expanded manually, one at a time (`readlinkat`), counted
//!   against the kernel's own `MAXSYMLINKS` budget.
//! - An ABSOLUTE target must be rooted at the workspace (canonical prefix),
//!   and the walk base RESETS to the pinned root before resolving it.
//! - A RELATIVE target may not contain `..` components — resolution stays
//!   under the current directory descriptor, which the walk has proven to
//!   be inside the workspace. There is therefore no escape direction left.
//! - READS follow a final-entry symlink only when the checks above prove
//!   the target stays inside; a link whose target leaves the workspace
//!   (or cannot be proven to stay) fails closed.
//! - WRITES never write THROUGH a symlinked final entry at all (classic
//!   no-follow semantics: `ELOOP` on the final open is an error) — a write
//!   cannot truncate an unexpected target or materialize one through a
//!   dangling link. Intermediate symlinked directories may still be
//!   traversed under the same rules.
//!
//! Missing components are created as-is (`mkdirat` on a write); nothing can
//! hide a symlink below a missing component, so the walk stays exhaustive.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use agent_client_protocol::Error as AcpError;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, mkdirat, openat, readlinkat, statat};
use rustix::io::Errno;

use super::acp::{acp_error, outside_workspace_error, slice_lines};

/// Match the kernel's own `MAXSYMLINKS` resolution budget.
const MAX_SYMLINK_HOPS: usize = 40;

/// Bounded retries while pinning the workspace root: each attempt re-checks
/// that the opened descriptor still names the canonicalized path's directory.
const ACQUIRE_ATTEMPTS: usize = 8;

/// A workspace root pinned to a verified directory descriptor.
///
/// The walk (and the handlers) receive this by shared reference and clone
/// the descriptor locally (`File::try_clone`) wherever an owned handle is
/// needed; every clone still refers to the same verified directory inode.
pub(super) struct PinnedRoot {
    /// The workspace path as given (also accepted as a request prefix).
    raw: PathBuf,
    /// Canonical spelling of the same directory (containment runs here).
    canonical: PathBuf,
    /// The verified directory descriptor the walk is anchored at.
    fd: File,
}

impl PinnedRoot {
    /// Acquire a verified root descriptor for `root`.
    ///
    /// Each attempt opens the directory, `fstat`s the descriptor, and
    /// compares `(dev, ino)` against the canonicalized path's metadata;
    /// a concurrent swap makes the observations disagree and the attempt
    /// retries. Bounded by [`ACQUIRE_ATTEMPTS`].
    ///
    /// # Errors
    ///
    /// Returns an ACP error when the workspace is inaccessible or keeps
    /// changing under the pin.
    pub(super) fn acquire(root: &Path) -> Result<Self, AcpError> {
        let mut last: Option<AcpError> = None;
        for _ in 0..ACQUIRE_ATTEMPTS {
            match Self::try_acquire(root) {
                Ok(pinned) => return Ok(pinned),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| acp_error("failed to pin the workspace root")))
    }

    fn try_acquire(root: &Path) -> Result<Self, AcpError> {
        let inaccessible = |what: String, e: std::io::Error| {
            acp_error(format!("session workspace '{what}' is not accessible: {e}"))
        };
        // Opening a directory with O_RDONLY succeeds on Unix (only reads of
        // the content fail); the descriptor is what gets verified below.
        let fd = File::open(root).map_err(|e| inaccessible(root.display().to_string(), e))?;
        let st_fd = fstat(&fd).map_err(|e| inaccessible(root.display().to_string(), e.into()))?;
        let canonical = root
            .canonicalize()
            .map_err(|e| inaccessible(root.display().to_string(), e))?;
        // The canonical spelling must name the SAME directory the descriptor
        // refers to; a concurrent swap makes the two observations disagree.
        let st_path = std::fs::metadata(&canonical)
            .map_err(|e| inaccessible(canonical.display().to_string(), e))?;
        if st_fd.st_dev != st_path.dev() || st_fd.st_ino != st_path.ino() {
            return Err(acp_error(format!(
                "session workspace '{}' changed while being pinned; retrying",
                root.display()
            )));
        }
        Ok(Self {
            raw: root.to_path_buf(),
            canonical,
            fd,
        })
    }

    /// The workspace-relative components of `requested`, rejecting escapes
    /// lexically (`..`, prefix mismatch, relative paths) before any
    /// descriptor work.
    fn relative_components(&self, requested: &Path) -> Result<VecDeque<OsString>, AcpError> {
        // The ACP spec requires absolute paths; anything else is refused.
        // Both spellings of the workspace are accepted: the canonical one
        // (what the walk is anchored at) and the raw one the session was
        // configured with (they name the same directory).
        let relative = requested
            .strip_prefix(&self.canonical)
            .or_else(|_| requested.strip_prefix(&self.raw))
            .map_err(|_| outside_workspace_error(requested))?;
        let mut queue = VecDeque::new();
        for component in relative.components() {
            match component {
                Component::Normal(name) => queue.push_back(name.to_os_string()),
                // `.` segments are normalized away by `components()`.
                Component::CurDir => {}
                _ => return Err(outside_workspace_error(requested)),
            }
        }
        Ok(queue)
    }
}

/// Read a file inside the workspace through pinned descriptors and apply the
/// protocol's 1-based `line`/`limit` window.
///
/// # Errors
///
/// Returns an ACP error when the path escapes the workspace or cannot be
/// read.
pub(super) fn read(
    root: &PinnedRoot,
    requested: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, AcpError> {
    let mut file = open_pinned(root, requested, false)?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| acp_error(format!("failed to read {}: {e}", requested.display())))?;
    Ok(slice_lines(&content, line, limit))
}

/// Write a file inside the workspace through pinned descriptors, creating
/// missing intermediate directories. Never writes through a symlinked final
/// entry.
///
/// # Errors
///
/// Returns an ACP error when the path escapes the workspace, the final entry
/// is a symlink, or the write fails.
pub(super) fn write(root: &PinnedRoot, requested: &Path, content: &str) -> Result<(), AcpError> {
    let mut file = open_pinned(root, requested, true)?;
    file.write_all(content.as_bytes())
        .map_err(|e| acp_error(format!("failed to write {}: {e}", requested.display())))?;
    Ok(())
}

/// Flags for opening intermediate path components: directories only, never
/// following a final-entry symlink (expanded manually instead).
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Resolve `requested` to an opened file descriptor WITHOUT ever letting the
/// kernel resolve a full path again: every component is opened relative to
/// the previous descriptor, and symlinks are expanded manually — each target
/// must provably stay inside the workspace.
fn open_pinned(root: &PinnedRoot, requested: &Path, create: bool) -> Result<File, AcpError> {
    let mut queue = root.relative_components(requested)?;
    let mut dir = root
        .fd
        .try_clone()
        .map_err(|e| acp_error(format!("failed to pin the workspace root: {e}")))?;
    let mut hops = 0usize;

    loop {
        let Some(name) = queue.pop_front() else {
            return Err(acp_error(format!(
                "path '{}' does not name a file inside the workspace",
                requested.display()
            )));
        };
        let last = queue.is_empty();

        if last {
            let flags = if create {
                OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC
            } else {
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
            };
            return match openat(&dir, &name, flags, Mode::from_raw_mode(0o644)) {
                Ok(fd) => Ok(File::from(fd)),
                // Read of a missing file (a write always CREATEs).
                Err(e) if e == Errno::NOENT && !create => Err(acp_error(format!(
                    "failed to read {}: file does not exist",
                    requested.display()
                ))),
                // Writes never follow a final-entry symlink: a write cannot
                // truncate an unexpected target nor materialize one through
                // a dangling link (see the module docs).
                Err(e) if e == Errno::LOOP && create => Err(outside_workspace_error(requested)),
                // Final entry is a symlink (read): follow it manually — its
                // target must provably stay inside the workspace.
                Err(e) if e == Errno::LOOP => {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err(outside_workspace_error(requested));
                    }
                    follow_symlink(&mut dir, root, &name, requested, &mut queue)?;
                    continue;
                }
                Err(e) => Err(acp_error(format!(
                    "failed to open {}: {e}",
                    requested.display()
                ))),
            };
        }

        // Intermediate component: must be a directory inside the workspace.
        // `lstat` first (SYMLINK_NOFOLLOW): on Linux `O_DIRECTORY|O_NOFOLLOW`
        // reports `ENOTDIR` (not `ELOOP`) for a symlinked directory, so the
        // symlink must be detected before the open.
        if statat(&dir, &name, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::Symlink)
        {
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Err(outside_workspace_error(requested));
            }
            follow_symlink(&mut dir, root, &name, requested, &mut queue)?;
            continue;
        }
        match openat(&dir, &name, dir_flags(), Mode::empty()) {
            Ok(fd) => dir = File::from(fd),
            Err(e) if e == Errno::NOENT && create => {
                // A write legitimately creates intermediate directories.
                // A concurrent creator wins the race benignly: `mkdirat`
                // failing with `EEXIST` is followed by a plain reopen, and
                // if that entry is a symlink the `O_NOFOLLOW` open fails
                // closed.
                if let Err(e) = mkdirat(&dir, &name, Mode::from_raw_mode(0o755))
                    && e != Errno::EXIST
                {
                    return Err(acp_error(format!(
                        "failed to create directory '{}': {e}",
                        name.to_string_lossy()
                    )));
                }
                dir = File::from(
                    openat(&dir, &name, dir_flags(), Mode::empty()).map_err(|e| {
                        acp_error(format!(
                            "failed to open directory '{}': {e}",
                            name.to_string_lossy()
                        ))
                    })?,
                );
            }
            Err(e) if e == Errno::NOENT => {
                return Err(acp_error(format!(
                    "failed to read {}: file does not exist",
                    requested.display()
                )));
            }
            // A symlink materialized between the `lstat` above and this
            // open (or the platform reports `ELOOP`): expand it with the
            // same containment checks.
            Err(e) if e == Errno::LOOP => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return Err(outside_workspace_error(requested));
                }
                follow_symlink(&mut dir, root, &name, requested, &mut queue)?;
            }
            Err(e) => {
                return Err(acp_error(format!(
                    "failed to traverse '{}': {e}",
                    name.to_string_lossy()
                )));
            }
        }
    }
}

/// Expand one symlink `name` under `dir`, pushing its target's components in
/// front of the remaining queue so resolution continues through the target.
///
/// An ABSOLUTE target resets the walk base to the pinned workspace root and
/// must be rooted there; a RELATIVE target may not climb out with `..` —
/// it resolves under the current descriptor, which the walk has proven to
/// be inside the workspace. Dangling or outside targets fail closed.
fn follow_symlink(
    dir: &mut File,
    root: &PinnedRoot,
    name: &OsStr,
    requested: &Path,
    queue: &mut VecDeque<OsString>,
) -> Result<(), AcpError> {
    let target =
        readlinkat(&*dir, name, Vec::new()).map_err(|_| outside_workspace_error(requested))?;
    let target_path = PathBuf::from(OsStr::from_bytes(target.to_bytes()));
    if target_path.is_absolute() {
        // The target must live under the workspace; the walk base resets to
        // the pinned root so the queued components resolve from there (not
        // from the symlink's own directory).
        let relative = target_path
            .strip_prefix(&root.canonical)
            .map_err(|_| outside_workspace_error(requested))?;
        // A `..` inside the target must not reach the walk: the walk resolves
        // components against real descriptors, so `openat(root_fd, "..")`
        // would open the workspace's PARENT and escape the sandbox.
        if relative
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(outside_workspace_error(requested));
        }
        for component in relative.components().rev() {
            queue.push_front(component.as_os_str().to_os_string());
        }
        *dir = root
            .fd
            .try_clone()
            .map_err(|e| acp_error(format!("failed to reset the walk base: {e}")))?;
    } else {
        if target_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(outside_workspace_error(requested));
        }
        for component in target_path.components().rev() {
            queue.push_front(component.as_os_str().to_os_string());
        }
    }
    Ok(())
}
