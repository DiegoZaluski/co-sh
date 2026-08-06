use std::io::BufRead;
use std::path::{Component, Path, PathBuf};

/// Result of a path validation check.
#[derive(Debug, PartialEq, Eq)]
pub enum GuardResult {
    /// Path is allowed. Contains the normalized (safe) path.
    Allowed(PathBuf),
    /// Path is explicitly denied.
    Denied(String),
    /// Configuration error (path matched both allowlist and blocklist).
    Mismatch(String),
}

/// Centralized path guard that combines lexical validation with filesystem
/// canonicalization.
///
/// Every tool that accepts filesystem paths should use this guard to ensure
/// consistent security checks. Usage:
///
/// ```ignore
/// let guard = PathGuard::new(&self.root, self.allowlist.as_deref(), self.blocklist.as_deref());
/// let safe_path = guard.resolve(path)?;
/// ```
///
/// The error message is uniform across all tools, making it easy for the AI
/// agent to understand why a path was denied.
pub struct PathGuard {
    root: PathBuf,
    allowlist: Option<Vec<PathBuf>>,
    blocklist: Option<Vec<PathBuf>>,
}

impl PathGuard {
    /// Create a new `PathGuard` with the project root and optional allow/block lists.
    ///
    /// The lists are cloned internally so the caller retains ownership.
    #[must_use]
    pub fn new(root: &Path, allowlist: Option<&[PathBuf]>, blocklist: Option<&[PathBuf]>) -> Self {
        Self {
            root: root.to_path_buf(),
            allowlist: allowlist.map(|l| l.to_vec()),
            blocklist: blocklist.map(|l| l.to_vec()),
        }
    }

    /// Validate and canonicalize `path` against the guard's root, allowlist, and blocklist.
    ///
    /// On success, returns the canonicalized (real) path on the filesystem.
    /// On failure, returns a descriptive error string explaining why the path was denied.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path is blocked by the blocklist.
    /// - The path is outside the project root and not in the allowlist.
    /// - The path is in both the allowlist and blocklist simultaneously.
    /// - The path cannot be resolved on the filesystem.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let allowlist = self.allowlist.as_deref();
        let blocklist = self.blocklist.as_deref();

        match validate_path(path, &self.root, allowlist, blocklist) {
            GuardResult::Allowed(normalized) => {
                // ── Filesystem canonicalization ──────────────────────────
                // Resolve symlinks and catch escapes. If canonicalize fails
                // (file doesn't exist yet), try the parent directory. If that
                // also fails and the path is inside the project root, use the
                // normalized path directly.
                let Ok(root_canon) = self.root.canonicalize() else {
                    return Err(format!(
                        "permission denied: `{path}` is outside the project directory"
                    ));
                };

                let root_norm = normalize_path(&self.root, &self.root);
                let in_root = normalized.starts_with(&root_norm);

                let resolved = match normalized.canonicalize() {
                    Ok(canon) => canon,
                    Err(_) => match normalized.parent() {
                        Some(parent) => match parent.canonicalize() {
                            Ok(parent_canon) => {
                                let file_name = normalized.file_name().unwrap_or_default();
                                parent_canon.join(file_name)
                            }
                            Err(_) => {
                                if in_root {
                                    normalized
                                } else {
                                    return Err(format!(
                                        "permission denied: `{path}` is outside the project directory"
                                    ));
                                }
                            }
                        },
                        None => {
                            return Err(format!(
                                "permission denied: `{path}` is outside the project directory"
                            ));
                        }
                    },
                };

                if in_root && !resolved.starts_with(&root_canon) {
                    return Err(format!(
                        "permission denied: `{path}` is outside the project directory"
                    ));
                }

                Ok(resolved)
            }
            GuardResult::Denied(reason) => Err(format!("permission denied: `{path}` — {reason}")),
            GuardResult::Mismatch(msg) => Err(format!("permission denied: `{path}` — {msg}")),
        }
    }

    /// Get the project root path (read-only reference).
    #[must_use]
    pub const fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Get the allowlist (read-only reference).
    #[must_use]
    pub fn allowlist(&self) -> Option<&[PathBuf]> {
        self.allowlist.as_deref()
    }

    /// Get the blocklist (read-only reference).
    #[must_use]
    pub fn blocklist(&self) -> Option<&[PathBuf]> {
        self.blocklist.as_deref()
    }

    /// Add a path to the allowlist (for session-level persistence).
    ///
    /// If the allowlist is `None`, it is created. Duplicate paths are ignored.
    pub fn add_allowlist_path(&mut self, path: PathBuf) {
        let list = self.allowlist.get_or_insert_with(Vec::new);
        if !list.contains(&path) {
            list.push(path);
        }
    }

    /// Remove a path from the allowlist (for AllowOnce cleanup).
    ///
    /// If the path is not in the allowlist, this is a no-op.
    /// If the allowlist becomes empty after removal, it stays as `Some(vec![])`
    /// to preserve the distinction between "no allowlist" (deny everything)
    /// and "empty allowlist" (deny everything outside root).
    pub fn remove_allowlist_path(&mut self, path: &Path) {
        if let Some(list) = self.allowlist.as_mut() {
            list.retain(|p| p != path);
        }
    }
}

/// Normalize a path by resolving `.` and `..` components lexically.
/// If the path is relative, it is first made absolute against the given root.
///
/// This is purely lexical — no filesystem access, no symlink resolution.
#[must_use]
pub fn normalize_path(path: &Path, root: &Path) -> PathBuf {
    let absolute = if path.is_relative() {
        root.join(path)
    } else {
        path.to_path_buf()
    };

    let mut out: Vec<Component> = Vec::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Check that `path` is allowed by the project root, allowlist, and blocklist.
///
/// The path is first normalized (`.`/`..` resolved). The root is also normalized
/// so both sides are compared on equal footing.
///
/// Returns `Allowed(normalized_path)` when the path passes all checks,
/// `Denied(reason)` when it is blocked, and `Mismatch(msg)` when the
/// path appears in both the allowlist and blocklist simultaneously.
#[must_use]
pub fn validate_path(
    path: &str,
    root: &Path,
    allowlist: Option<&[PathBuf]>,
    blocklist: Option<&[PathBuf]>,
) -> GuardResult {
    let path = Path::new(path);
    let normalized = normalize_path(path, root);
    let root_norm = normalize_path(root, root);

    let blocked = blocklist.is_some_and(|list| {
        list.iter().any(|entry| {
            let e = normalize_path(entry, root);
            normalized.starts_with(&e) || normalized == e
        })
    });
    let allowed = allowlist.is_some_and(|list| {
        list.iter().any(|entry| {
            let e = normalize_path(entry, root);
            normalized == e
        })
    });
    let in_root = normalized.starts_with(&root_norm);

    if blocked && allowed {
        return GuardResult::Mismatch(
            "Security Alert: path is in both blocklist and allowlist.".into(),
        );
    }
    if blocked {
        return GuardResult::Denied("path is in blocklist".into());
    }
    if !in_root && !allowed {
        return GuardResult::Denied("path is outside project root".into());
    }

    GuardResult::Allowed(normalized)
}

/// How many leading lines are scanned for auto-generated markers.
///
/// Generated-file headers always live in the first few lines of the file, so a
/// small scan window keeps the check cheap (a lazy line-by-line read of at most
/// this many lines, never a full-file load).
const AUTO_GENERATED_SCAN_LINES: usize = 10;

/// Return the auto-generated marker found in `line` (case-insensitive), or `None`.
///
/// The set is deliberately small and conventional: the Go/Protobuf
/// `DO NOT EDIT.` convention, the TypeScript `@generated` annotation, and the
/// common plain-language "automatically generated" variants. Anything else
/// passes the check.
fn auto_generated_marker(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    [
        "do not edit",
        "@generated",
        "automatically generated",
        "auto-generated",
        "autogenerated",
    ]
    .into_iter()
    .find(|&marker| lower.contains(marker))
}

/// Refuse to modify a file that declares itself machine-generated.
///
/// This guard is called ONLY by write-path tools ([`crate::fs::write`],
/// [`crate::fs::edit`], [`crate::fs::ast_edit`]) because those tools replace
/// file content. Read-only tools (`read`, `grep`) never call it — surfacing a
/// generated file is harmless.
///
/// The check is deliberately cheap: only the first [`AUTO_GENERATED_SCAN_LINES`]
/// lines are scanned, and only for a small set of conventional markers. A file
/// without one of those markers, a new file (nothing is being overwritten), and
/// an unreadable file all pass — the caller surfaces its own read error.
///
/// # Errors
///
/// Returns `Err` when `path` exists and its header carries an auto-generated
/// marker, with an actionable message (regenerate instead, or remove the
/// marker to force the write).
pub fn assert_editable_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(());
    };
    // Lazy line-by-line read: only the first `AUTO_GENERATED_SCAN_LINES` lines
    // are pulled from disk, never the whole file.
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(AUTO_GENERATED_SCAN_LINES) {
        let Ok(line) = line else {
            return Ok(());
        };
        if let Some(marker) = auto_generated_marker(&line) {
            return Err(format!(
                "refusing to modify `{}`: its header marks it as auto-generated \
                 (`{marker}`). Generated files are owned by a tool — your change \
                 would be overwritten on the next generation run. Edit the \
                 generator instead, or remove the marker from the file to force \
                 the write.",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Validate an asset path for skills `read_asset`.
///
/// The asset path must be relative, must not contain `..` components, and
/// after joining with `base_dir` and canonicalizing (filesystem resolution),
/// must stay within `base_dir`.
///
/// # Errors
///
/// Returns an error string describing the violation or the reason the
/// canonicalized path could not be resolved.
pub fn validate_asset_path(base_dir: &Path, asset_path: &str) -> Result<PathBuf, String> {
    let requested = Path::new(asset_path);
    if requested.is_absolute() {
        return Err("absolute path not allowed, use a relative path".into());
    }
    if requested.components().any(|c| c == Component::ParentDir) {
        return Err("path must not contain '..' (parent directory references)".into());
    }

    let base_canon = base_dir
        .canonicalize()
        .map_err(|e| format!("could not resolve base directory: {e}"))?;
    let resolved = base_canon.join(asset_path);
    let resolved_canon = resolved
        .canonicalize()
        .map_err(|e| format!("asset not found: {e}"))?;

    if !resolved_canon.starts_with(&base_canon) {
        return Err("path points outside the skill directory".into());
    }

    Ok(resolved_canon)
}

#[cfg(test)]
mod tests;
