//! Overwrites one or more files completely.
//!
//! Verifies write-scope permissions using an [`FsMetadata`] structure that describes:
//! - The project root directory.
//! - Paths explicitly granted write permission.
//! - Paths explicitly denied write permission.
//!
//! If no paths are explicitly blocked, any file within the root directory
//! is considered writable by default.
//!
//! # Errors
//!
//! Returns `Err` if the [`FsMetadata`] has inconsistent allowlist/blocklist entries.
//! Individual file write failures are reported inline in the returned string
//! rather than aborting the batch.
//!
//! Content normalization is shared with [`read`](crate::fs::read) (LF + no BOM)
//! and auto-generated files are refused via
//! [`assert_editable_file`](crate::util::path_guard::assert_editable_file).
use super::types::{FsMetadata, FsWrite};

use crate::util::path_guard::{GuardResult, assert_editable_file, validate_path};
use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    normalize,
};
use cosh_sdk::rollback;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WriteResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub warnings: Option<String>,
    /// Passive LSP feedback collected after the write (errors by default,
    /// warnings when the caller opted in). `None` when LSP is disabled or
    /// nothing was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp_notes: Option<super::types::LspNotes>,
}

/// Strip hashline display prefixes (`[path#hash]` headers and `N:` line prefixes)
/// from content the model may have copied from read/search output.
///
/// The input must already be LF-normalized (no `\r`, no BOM) — the caller
/// canonicalizes with [`normalize::normalize_to_lf`] first, mirroring `read`.
#[allow(clippy::unwrap_used)]
fn strip_write_content(content: &str) -> (String, bool) {
    static BRACKET_HEADER_RE: OnceLock<Regex> = OnceLock::new();
    let bracket_re =
        BRACKET_HEADER_RE.get_or_init(|| Regex::new(r"^\s*\[[^#\r\n]+#[^ \t\r\n]*\]\s*$").unwrap());

    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    let lines: Vec<String> = trimmed.split('\n').map(String::from).collect();

    let stripped = cosh_sdk::hashline::prefixes::strip_new_line_prefixes(&lines);
    if stripped != lines {
        return (stripped.join("\n"), true);
    }

    let header_idx = lines.iter().position(|l| !l.trim().is_empty());
    let Some(idx) = header_idx else {
        return (content.to_string(), false);
    };
    if !bracket_re.is_match(&lines[idx]) {
        return (content.to_string(), false);
    }

    let mut without_header = lines;
    without_header.remove(idx);
    let stripped2 = cosh_sdk::hashline::prefixes::strip_new_line_prefixes(&without_header);
    if stripped2 != without_header {
        return (stripped2.join("\n"), true);
    }

    (content.to_string(), false)
}

/// Make a file executable when its content starts with a `#!` shebang.
/// Errors are swallowed — chmod failure must not abort a successful write.
#[cfg(unix)]
async fn maybe_make_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let mut perms = metadata.permissions();
    let mode = perms.mode();
    let new_mode = mode | 0o111;
    if new_mode == mode {
        return false;
    }
    perms.set_mode(new_mode);
    tokio::fs::set_permissions(path, perms).await.is_ok()
}

#[cfg(not(unix))]
async fn maybe_make_executable(_path: &str) -> bool {
    false
}

/// Write content to one or more files.
///
/// # Errors
///
/// Returns `Err` if the [`FsMetadata`] has inconsistent allowlist/blocklist entries,
/// or a path is both blocked and allowed simultaneously.
pub async fn write(metadata: FsMetadata, tg: FsWrite) -> Result<Vec<WriteResult>, String> {
    let mut result: Vec<WriteResult> = vec![];
    let fs = DiskFilesystem::new();
    for target in &tg.targets {
        // Canonicalize to LF (and drop a UTF-8 BOM) exactly like `read` does, so
        // the hash/header returned here matches what a follow-up read of the
        // written file would report, and hashline prefix stripping sees one
        // line-ending shape.
        let normalized_input = normalize::normalize_to_lf(&normalize::strip_bom(&target.text).text);
        let (clean_text, stripped) = strip_write_content(&normalized_input);

        // An inconsistent configuration — a path in both the allowlist and
        // the blocklist — is a hard error, not a per-file warning.
        if let GuardResult::Mismatch(msg) = validate_path(
            &target.path,
            &metadata.root,
            metadata.allowlist.as_deref(),
            metadata.blocklist.as_deref(),
        ) {
            return Err(format!("permission denied: `{}` — {msg}", target.path));
        }

        if clean_text.trim().is_empty() {
            let warning = if stripped {
                format!(
                    "text is empty for `{path}` after stripping hashline display prefixes.",
                    path = target.path
                )
            } else {
                format!(
                    "text is empty for `{path}`. Nothing was sent to add to the file.",
                    path = target.path
                )
            };
            let res = WriteResult {
                file_hash: String::new(),
                header: String::new(),
                path: target.path.clone(),
                warnings: Some(warning),
                lsp_notes: None,
            };
            result.push(res);
            continue;
        }

        match metadata.fs_guard(&target.path) {
            Ok(validated_path) => {
                let path_str = validated_path.to_string_lossy();

                // Never clobber a file that declares itself machine-generated:
                // the change would be overwritten by the next generation run.
                // Creating a brand-new file is always allowed (nothing is being
                // overwritten). Per-file warning, not a batch abort.
                if let Err(msg) = assert_editable_file(Path::new(path_str.as_ref())) {
                    let res = WriteResult {
                        file_hash: String::new(),
                        header: String::new(),
                        path: target.path.clone(),
                        warnings: Some(msg),
                        lsp_notes: None,
                    };
                    result.push(res);
                    continue;
                }

                // Existing-file hash guard
                // When the target already exists, the caller must supply a
                // `file_hash` that matches the live content.  This proves
                // the model has read the file before overwriting it.
                let file_exists = fs.read_text(&path_str).await.is_ok();
                if file_exists {
                    match target.file_hash.as_deref() {
                        None => {
                            let res = WriteResult {
                                file_hash: String::new(),
                                header: String::new(),
                                path: target.path.clone(),
                                warnings: Some(format!(
                                    "ERROR: Cannot overwrite `{}` without reading it first. \
                                     The file already exists. To overwrite it you must: \
                                     1) Use `fs_read` to read the file (this returns a `file_hash`). \
                                     2) Include that `file_hash` in your `fs_write` call. \
                                     This ensures you know the current content before overwriting it.",
                                    target.path
                                )),
                                lsp_notes: None,
                            };
                            result.push(res);
                            continue;
                        }
                        Some(expected_hash) => {
                            if let Ok(current) = fs.read_text(&path_str).await {
                                let actual_hash =
                                    cosh_sdk::hashline::format::compute_file_hash(&current);
                                if actual_hash != *expected_hash {
                                    let res = WriteResult {
                                        file_hash: String::new(),
                                        header: String::new(),
                                        path: target.path.clone(),
                                        warnings: Some(format!(
                                            "ERROR: file_hash mismatch for `{}`. \
                                             The file has been modified since you last read it. \
                                             The hash you provided ({}) does not match \
                                             the current file content ({}). \
                                             To fix this: 1) Use `fs_read` to read the file again. \
                                             2) Use the new `file_hash` from the read result.",
                                            target.path, expected_hash, actual_hash
                                        )),
                                        lsp_notes: None,
                                    };
                                    result.push(res);
                                    continue;
                                }
                            }
                        }
                    }
                }

                if let Ok(current) = fs.read_text(&path_str).await {
                    let _ = rollback::record(&path_str, &current);
                }

                if let Err(err) = fs.write_text(&path_str, &clean_text).await {
                    let warning = format!("failed to write `{}`: {}", target.path, err);
                    let res = WriteResult {
                        file_hash: String::new(),
                        header: String::new(),
                        path: target.path.clone(),
                        warnings: Some(warning),
                        lsp_notes: None,
                    };
                    result.push(res);
                    continue;
                }

                let made_executable =
                    clean_text.starts_with("#!") && maybe_make_executable(&path_str).await;

                cosh_sdk::tree_sitter::tree_sitter().invalidate(&path_str);
                let _ = rollback::record(&path_str, &clean_text);

                let hash = format::compute_file_hash(&clean_text);
                let header = format::format_hashline_header(&path_str, &hash);

                let mut warnings: Vec<String> = Vec::new();
                if stripped {
                    warnings.push(
                        "auto-stripped hashline display prefixes from content before writing."
                            .to_string(),
                    );
                }
                if made_executable {
                    warnings.push(
                        "made executable via chmod +x (content starts with #! shebang)."
                            .to_string(),
                    );
                }

                let res = WriteResult {
                    file_hash: hash,
                    header,
                    path: target.path.clone(),
                    warnings: if warnings.is_empty() {
                        None
                    } else {
                        Some(warnings.join("\n"))
                    },
                    lsp_notes: None,
                };

                result.push(res);
            }
            Err(e) => {
                let warning = format!(
                    "write permission denied for `{}`. \
                     Files under `{}` are writable by default. \
                     Use the allowlist to grant access to paths outside this directory. \
                     ({})",
                    target.path,
                    metadata.root.display(),
                    e,
                );
                let res = WriteResult {
                    file_hash: String::new(),
                    header: String::new(),
                    path: target.path.clone(),
                    warnings: Some(warning),
                    lsp_notes: None,
                };
                result.push(res);
            }
        }
    }
    Ok(result)
}
