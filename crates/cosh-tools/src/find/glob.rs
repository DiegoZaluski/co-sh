//! Glob-based filesystem discovery tool.

use cosh_sdk::find::{FileType, GlobOptions, glob as sdk_glob};

use super::types::{GlobEntry, GlobInput, GlobOutput};

/// Find filesystem entries matching a glob pattern.
///
/// Searches `input.path` for entries matching `input.pattern`. Results can be
/// filtered by filesystem kind, sorted by modification time, and bounded by a
/// result count or a timeout.
///
/// # Errors
/// Returns an error when the search path does not exist or is not a directory,
/// the glob pattern is invalid, an unknown `file_type` string is given, or the
/// operation is cancelled by a timeout.
pub fn glob(input: GlobInput) -> Result<GlobOutput, String> {
    let file_type = input
        .file_type
        .as_deref()
        .map(parse_file_type)
        .transpose()?;

    let result = sdk_glob(GlobOptions {
        pattern: input.pattern,
        path: input.path,
        file_type,
        recursive: input.recursive,
        hidden: input.hidden,
        max_results: input.max_results,
        gitignore: input.gitignore,
        sort_by_mtime: input.sort_by_mtime,
        cache: None,
        include_node_modules: None,
        timeout_ms: input.timeout_ms,
    })?;

    let matches = result
        .matches
        .into_iter()
        .map(|m| GlobEntry {
            path: m.path,
            file_type: file_type_str(m.file_type).to_owned(),
            mtime_ms: m.mtime,
            size_bytes: m.size,
        })
        .collect();

    Ok(GlobOutput {
        matches,
        total: result.total_matches,
    })
}

#[must_use]
fn file_type_str(ft: FileType) -> &'static str {
    match ft {
        FileType::File => "file",
        FileType::Dir => "dir",
        FileType::Symlink => "symlink",
    }
}

fn parse_file_type(s: &str) -> Result<FileType, String> {
    match s {
        "file" => Ok(FileType::File),
        "dir" => Ok(FileType::Dir),
        "symlink" => Ok(FileType::Symlink),
        other => Err(format!(
            "invalid file_type `{other}`; expected \"file\", \"dir\", or \"symlink\""
        )),
    }
}
