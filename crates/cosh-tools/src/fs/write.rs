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
use super::types;
use super::types::{FsMetadata, WriteAllFile};

use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
};
use cosh_sdk::rollback;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug)]
pub struct WriteResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub warnings: Option<String>,
}

/// Strip hashline display prefixes (`[path#hash]` headers and `N:` line prefixes)
/// from content the model may have copied from read/search output.
fn strip_write_content(content: &str) -> (String, bool) {
    static BRACKET_HEADER_RE: OnceLock<Regex> = OnceLock::new();
    let bracket_re = BRACKET_HEADER_RE
        .get_or_init(|| Regex::new(r"^\s*\[[^#\r\n]+#[^ \t\r\n]*\]\s*$").unwrap());

    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    let lines: Vec<String> = trimmed.replace('\r', "").split('\n').map(String::from).collect();

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

    let mut without_header = lines.clone();
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
pub async fn write(
    wtarget: WriteAllFile<'_>,
    metadata: FsMetadata<'_>,
) -> Result<Vec<WriteResult>, String> {
    let mut result: Vec<WriteResult> = vec![];
    let fs = DiskFilesystem::new();
    for target in &wtarget.write {
        let (clean_text, stripped) = strip_write_content(target.text);

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
                path: target.path.to_string(),
                warnings: Some(warning),
            };
            result.push(res);
            continue;
        }

        match metadata.fs_guard(target.path) {
            types::FsGuard::Allowed => {
                if let Ok(current) = fs.read_text(target.path).await {
                    let _ = rollback::record(target.path, &current);
                }

                if let Err(err) = fs.write_text(target.path, &clean_text).await {
                    let warning = format!("failed to write `{}`: {}", target.path, err);
                    let res = WriteResult {
                        file_hash: String::new(),
                        header: String::new(),
                        path: target.path.to_string(),
                        warnings: Some(warning),
                    };
                    result.push(res);
                    continue;
                }

                let made_executable = clean_text.starts_with("#!") && maybe_make_executable(target.path).await;

                cosh_sdk::tree_sitter::tree_sitter().invalidate(target.path);
                let _ = rollback::record(target.path, &clean_text);

                let hash = format::compute_file_hash(&clean_text);
                let header = format::format_hashline_header(target.path, &hash);

                let mut warnings: Vec<String> = Vec::new();
                if stripped {
                    warnings.push("auto-stripped hashline display prefixes from content before writing.".to_string());
                }
                if made_executable {
                    warnings.push("made executable via chmod +x (content starts with #! shebang).".to_string());
                }

                let res = WriteResult {
                    file_hash: hash,
                    header,
                    path: target.path.to_string(),
                    warnings: if warnings.is_empty() {
                        None
                    } else {
                        Some(warnings.join("\n"))
                    },
                };

                result.push(res);
            }
            types::FsGuard::Denied => {
                let warning = format!(
                    "write permission denied for `{}`. \
                     Files under `{:?}` are writable by default. \
                     Use the allowlist to grant access to paths outside this directory.",
                    target.path,
                    metadata.root.display()
                );

                let res = WriteResult {
                    file_hash: String::new(),
                    header: String::new(),
                    path: target.path.to_string(),
                    warnings: Some(warning),
                };
                result.push(res);
            }
            types::FsGuard::Mismatch(message) => {
                return Err(message);
            }
        }
    }
    Ok(result)
}
