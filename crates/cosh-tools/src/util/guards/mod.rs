use std::path::{Component, Path, PathBuf};

/// Result of a path validation check.
#[derive(Debug, PartialEq)]
pub enum GuardResult {
    /// Path is allowed. Contains the normalized (safe) path.
    Allowed(PathBuf),
    /// Path is explicitly denied.
    Denied(String),
    /// Configuration error (path matched both allowlist and blocklist).
    Mismatch(String),
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
        return Err("absolute paths are not allowed".into());
    }
    if requested.components().any(|c| c == Component::ParentDir) {
        return Err("path must not contain '..' components".into());
    }

    let base_canon = base_dir
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize base dir: {e}"))?;
    let resolved = base_canon.join(asset_path);
    let resolved_canon = resolved
        .canonicalize()
        .map_err(|e| format!("asset not found: {e}"))?;

    if !resolved_canon.starts_with(&base_canon) {
        return Err("resolved path escapes the skill directory".into());
    }

    Ok(resolved_canon)
}

#[cfg(test)]
mod tests;
