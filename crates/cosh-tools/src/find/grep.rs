//! Regex-based file content search tool.

use cosh_sdk::find::{GrepOptions, GrepOutputMode, grep as sdk_grep};

use super::types::{ContextEntry, Grep, GrepMatchEntry, GrepOutput};

/// Search file content for lines matching a regex pattern.
///
/// Searches `path` (a file or directory tree) using the ripgrep engine.
/// Directory searches walk the tree in parallel and support optional glob and
/// language-type filters. Single-file searches run in memory.
///
/// # Errors
/// Returns an error when the path cannot be resolved, `pattern` is an
/// invalid regex, or the operation is cancelled by a timeout.
pub fn grep(grep: &Grep, pattern: &str, path: &str) -> Result<GrepOutput, String> {
    let result = sdk_grep(GrepOptions {
        pattern: pattern.to_owned(),
        path: path.to_owned(),
        glob: grep.glob.clone(),
        r#type: grep.file_type.clone(),
        ignore_case: grep.ignore_case,
        multiline: None,
        hidden: grep.hidden,
        gitignore: grep.gitignore,
        cache: None,
        max_count: grep.max_count,
        offset: None,
        context_before: grep.context_before,
        context_after: grep.context_after,
        context: None,
        max_columns: None,
        mode: Some(GrepOutputMode::Content),
        max_count_per_file: None,
        timeout_ms: grep.timeout_ms,
    })?;

    let matches = result
        .matches
        .into_iter()
        .map(|m| GrepMatchEntry {
            path: m.path,
            line_number: m.line_number,
            line: m.line,
            context_before: m
                .context_before
                .unwrap_or_default()
                .into_iter()
                .map(|c| ContextEntry {
                    line_number: c.line_number,
                    line: c.line,
                })
                .collect(),
            context_after: m
                .context_after
                .unwrap_or_default()
                .into_iter()
                .map(|c| ContextEntry {
                    line_number: c.line_number,
                    line: c.line,
                })
                .collect(),
        })
        .collect();

    Ok(GrepOutput {
        matches,
        total_matches: result.total_matches,
        files_with_matches: result.files_with_matches,
        files_searched: result.files_searched,
    })
}
