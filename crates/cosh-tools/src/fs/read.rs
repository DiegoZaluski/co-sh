//! Read file(s) and format the output as hashline sections.
//!
//! Each call to [`read`] carries one or more [`Target`] entries.  A target can
//! request the whole file, a syntactic block at a given line, a definition
//! block matching a name (symbol, struct, class, …), or one or more exact line
//! ranges via `line_range` (a plain slice — no AST involved).  When the name
//! search targets a directory the entire tree is walked recursively.
//!
//! Output is hashline-numbered (`N| text`) so the model can anchor edits
//! directly.  Token-saving behaviors:
//!
//! - **Exact range reads** — `line_range: "50-100"` (or comma-separated
//!   `"10-20,200-220"`) returns only the requested lines, with a footer
//!   reporting how many lines remain and how to continue.
//! - **Elided blocks** — blocks larger than [`ELIDE_MIN_BLOCK_LINES`] are
//!   shown as their head/tail with a `…` marker and a footer telling the model
//!   which `line_range` to re-read when it needs the elided body.
//! - **Column truncation** — lines longer than [`MAX_COLUMN`] characters are
//!   cut with a `...` suffix and a notice.
//! - **Seen lines** — every surfaced line is recorded against the snapshot
//!   tag ([`rollback::record_seen_lines`]), so the recovery path warns when an
//!   edit anchors lines the model never saw (including elided interiors).
use super::types::{FsMetadata, FsRead, Target};

use cosh_sdk::hashline::{
    format,
    fs::{DiskFilesystem, Filesystem},
    normalize,
};
use cosh_sdk::rollback;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Blocks at or below this many lines are returned whole; larger blocks are
/// elided to their head/tail.
const ELIDE_MIN_BLOCK_LINES: usize = 24;
/// Lines kept from the head of an elided block.
const ELIDE_KEEP_HEAD: usize = 3;
/// Lines kept from the tail of an elided block.
const ELIDE_KEEP_TAIL: usize = 2;
/// Lines longer than this (characters) are truncated with a `...` suffix.
const MAX_COLUMN: usize = 200;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadResult {
    pub path: String,
    pub file_hash: String,
    pub header: String,
    pub content: String,
    pub warnings: Option<String>,
}

/// Run every target and return hashline-formatted results.
///
/// Each target produces one or more [`ReadResult`] entries. Errors and
/// incomplete reads are recorded as warnings inside each result instead of
/// aborting the entire operation.
pub async fn read(metadata: FsMetadata, tg: FsRead) -> Vec<ReadResult> {
    let fs = DiskFilesystem::new();
    let mut results: Vec<ReadResult> = Vec::new();

    for target in tg.targets {
        results.extend(read_target(fs.clone(), target, &metadata).await);
    }

    results
}

async fn read_target(fs: DiskFilesystem, target: Target, metadata: &FsMetadata) -> Vec<ReadResult> {
    match metadata.fs_guard(&target.path) {
        Ok(validated_path) => {
            let path = validated_path.to_string_lossy().to_string();
            let target = Target {
                path,
                line: target.line,
                symbol: target.symbol,
                line_range: target.line_range,
            };
            read_target_impl(fs, target).await
        }
        Err(e) => vec![ReadResult {
            path: target.path.clone(),
            file_hash: String::new(),
            header: String::new(),
            content: String::new(),
            warnings: Some(e),
        }],
    }
}

/// Split file text into lines, dropping the empty tail element left by a
/// trailing newline so line N maps to `lines[N - 1]`.
fn file_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Parse a comma-separated list of 1-based inclusive line ranges (`"start-end"`).
fn parse_line_ranges(raw: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut ranges = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!(
                "line_range must be \"start-end\" (1-based, inclusive), got: {raw}"
            ));
        }
        let (start, end) = part.split_once('-').ok_or_else(|| {
            format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}")
        })?;
        let start: u32 = start.trim().parse().map_err(|_| {
            format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}")
        })?;
        let end: u32 = end.trim().parse().map_err(|_| {
            format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}")
        })?;
        if start < 1 || end < start {
            return Err(format!(
                "line_range must satisfy 1 <= start <= end, got: {raw}"
            ));
        }
        ranges.push((start, end));
    }
    if ranges.is_empty() {
        return Err("line_range must not be empty".to_string());
    }
    Ok(ranges)
}

/// Truncate a line to [`MAX_COLUMN`] chars with a `...` suffix when longer.
/// Returns the display text and whether truncation happened.
fn truncate_column(line: &str) -> (String, bool) {
    let count = line.chars().count();
    if count <= MAX_COLUMN {
        return (line.to_string(), false);
    }
    let mut s: String = line.chars().take(MAX_COLUMN - 3).collect();
    s.push_str("...");
    (s, true)
}

/// Render lines `start..=end` (1-based, clamped to the file) as numbered
/// hashline lines, truncating over-long columns. Records every surfaced line
/// (with its ORIGINAL text) into `seen` so recovery can distinguish shown
/// anchors from elided ones.
fn render_lines(
    lines: &[String],
    start: u32,
    end: u32,
    seen: &mut Vec<(u32, String)>,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut truncated_any = false;
    for n in start..=end.min(lines.len() as u32) {
        let text = &lines[(n - 1) as usize];
        let (display, was_truncated) = truncate_column(text);
        truncated_any |= was_truncated;
        out.push(format::format_numbered_line(n, &display));
        seen.push((n, text.clone()));
    }
    (out, truncated_any)
}

/// Render a block-relative line slice as numbered hashline lines, labelling
/// each line with its absolute 1-based number: `lines[0]` is `start_line`.
///
/// Unlike [`render_lines`] (whose input is the whole file, indexed by absolute
/// line number), this indexes `lines` relatively so blocks that start past line
/// 1 render their body correctly instead of clamping to an empty range.
fn render_block_lines(
    lines: &[String],
    start_line: u32,
    end: u32,
    seen: &mut Vec<(u32, String)>,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut truncated_any = false;
    for (i, text) in lines.iter().enumerate() {
        let n = start_line + i as u32;
        if n > end {
            break;
        }
        let (display, was_truncated) = truncate_column(text);
        truncated_any |= was_truncated;
        out.push(format::format_numbered_line(n, &display));
        seen.push((n, text.clone()));
    }
    (out, truncated_any)
}

/// Output of [`read_range_body`]: numbered body, seen lines, content notices,
/// and warnings (in that order).
type RangeBody = (String, Vec<(u32, String)>, Vec<String>, Vec<String>);

/// Build the numbered body + seen lines + content notices + warnings for one
/// or more line ranges.
///
/// - Content notices (appended to the body the model reads): elision markers
///   for gaps between ranges, column-truncation, and the remaining-lines hint.
/// - Warnings (delivery problems): ranges past EOF, which were skipped.
fn read_range_body(lines: &[String], ranges: &[(u32, u32)]) -> RangeBody {
    let total = lines.len() as u32;
    let mut parts: Vec<String> = Vec::new();
    let mut seen: Vec<(u32, String)> = Vec::new();
    let mut notices: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut truncated_any = false;
    let mut prev_end: Option<u32> = None;

    for &(start, end) in ranges {
        if start > total {
            warnings.push(format!(
                "[Range {start}-{end} is beyond end of file ({total} lines total); skipped]"
            ));
            continue;
        }
        let eff_end = end.min(total);
        if let Some(pe) = prev_end
            && start > pe + 1
        {
            parts.push(format!("[…{} lines between ranges elided]", start - pe - 1));
        }
        let (rendered, tr) = render_lines(lines, start, eff_end, &mut seen);
        truncated_any |= tr;
        parts.extend(rendered);
        prev_end = Some(eff_end);
    }

    if truncated_any {
        notices.push(format!(
            "[Some lines were truncated to {MAX_COLUMN} columns; use line_range to re-read specific lines for the full content]"
        ));
    }
    if let Some(last_shown) = seen.iter().map(|(n, _)| *n).max()
        && last_shown < total
    {
        notices.push(format!(
            "[{} more lines in file; continue with line_range \"{}-{}\"]",
            total - last_shown,
            last_shown + 1,
            total
        ));
    }
    (parts.join("\n"), seen, notices, warnings)
}

/// Elide a block's interior: blocks at or below [`ELIDE_MIN_BLOCK_LINES`] are
/// returned whole; larger ones keep [`ELIDE_KEEP_HEAD`] head lines, a `…`
/// marker, and [`ELIDE_KEEP_TAIL`] tail lines. Returns the body lines, the
/// elided line range (absolute, 1-based) when interior lines were dropped, and
/// whether any line was column-truncated. Only the KEPT lines are recorded as
/// seen — an edit anchoring an elided interior correctly warns on recovery.
fn elide_block(
    lines: &[String],
    start_line: u32,
    seen: &mut Vec<(u32, String)>,
) -> (Vec<String>, Option<(u32, u32)>, bool) {
    let n = lines.len();
    if n <= ELIDE_MIN_BLOCK_LINES {
        let (out, truncated_any) = render_block_lines(lines, start_line, start_line + n as u32 - 1, seen);
        return (out, None, truncated_any);
    }
    let head_n = ELIDE_KEEP_HEAD.min(n);
    let tail_n = ELIDE_KEEP_TAIL.min(n.saturating_sub(head_n));
    let mut out = Vec::new();
    let mut truncated_any = false;
    let (head, tr1) = render_block_lines(lines, start_line, start_line + head_n as u32 - 1, seen);
    out.extend(head);
    truncated_any |= tr1;
    out.push("…".to_string());
    let tail_start = n - tail_n; // 0-based index of the first kept tail line
    let (tail, tr2) = render_block_lines(
        &lines[tail_start..],
        start_line + tail_start as u32,
        start_line + n as u32 - 1,
        seen,
    );
    out.extend(tail);
    truncated_any |= tr2;
    let elided = (
        start_line + head_n as u32,
        start_line + tail_start as u32 - 1,
    );
    (out, Some(elided), truncated_any)
}

/// Record the lines a read surfaced against the snapshot tag for `path`.
fn record_seen(path: &str, file_hash: &str, seen: &[(u32, String)]) {
    rollback::record_seen_lines(path, file_hash, seen);
}

/// Append informational notice lines to `content` (they are read by the model
/// as part of the output, not surfaced as warnings — warnings are reserved for
/// delivery problems).
fn append_content_notices(content: &mut String, notices: &[String]) {
    for notice in notices {
        content.push('\n');
        content.push_str(notice);
    }
}

async fn read_target_impl(fs: DiskFilesystem, target: Target) -> Vec<ReadResult> {
    if let Some(name) = target.symbol {
        return search_symbol(&fs, &target.path, &name).await;
    }

    if Path::new(&target.path).is_dir() {
        return vec![ReadResult {
            path: target.path.clone(),
            file_hash: String::new(),
            header: String::new(),
            content: String::new(),
            warnings: Some(format!(
                "cannot read directory `{path}` without a `symbol` filter. \
                 When `path` is a directory, a `symbol` (e.g., a function or struct name) must be \
                 provided so the tool searches for matching definitions across all supported source \
                 files in that tree. \
                 To read entire files, list each file path explicitly in the `read` array with no \
                 `line` or `symbol` fields.",
                path = target.path
            )),
        }];
    }

    let text = match read_normalized(&fs, &target.path).await {
        Ok(t) => t,
        Err(e) => {
            return vec![ReadResult {
                path: target.path.clone(),
                file_hash: String::new(),
                header: String::new(),
                content: String::new(),
                warnings: Some(e),
            }];
        }
    };
    let file_lines = file_lines(&text);

    let _ = rollback::record(&target.path, &text);
    let hash = format::compute_file_hash(&text);
    let header = format::format_hashline_header(&target.path, &hash);

    // Exact line ranges — a plain slice, no AST: the model reads only the
    // lines it asked for instead of a whole syntactic block.
    if let Some(range_raw) = target.line_range {
        let ranges = match parse_line_ranges(&range_raw) {
            Ok(r) => r,
            Err(e) => {
                return vec![ReadResult {
                    path: target.path.clone(),
                    file_hash: hash,
                    header: header.clone(),
                    content: format!("{header}\n{}", format::format_numbered_lines(&text, 1)),
                    warnings: Some(e),
                }];
            }
        };
        let (body, seen, notices, warnings) = read_range_body(&file_lines, &ranges);
        record_seen(&target.path, &hash, &seen);
        let mut content = format!("{header}\n{body}");
        append_content_notices(&mut content, &notices);
        let warnings = (!warnings.is_empty()).then(|| warnings.join("\n"));
        return vec![ReadResult {
            path: target.path.clone(),
            file_hash: hash,
            header: header.clone(),
            content,
            warnings,
        }];
    }

    if let Some(line) = target.line {
        let ts = cosh_sdk::tree_sitter::tree_sitter();
        let ln: u32 = match line.try_into() {
            Ok(l) => l,
            Err(e) => {
                return vec![ReadResult {
                    path: target.path.clone(),
                    file_hash: hash,
                    header: header.clone(),
                    content: format!("{header}\n{}", format::format_numbered_lines(&text, 1)),
                    warnings: Some(format!("cannot convert line {line}: {e}")),
                }];
            }
        };
        match ts.resolve_block(&target.path, &text, ln) {
            Some(span) => {
                let end_idx = (span.end as usize).min(file_lines.len());
                let start_idx = (span.start as usize - 1).min(end_idx);
                let block_lines = file_lines[start_idx..end_idx].to_vec();
                let mut seen = Vec::new();
                let (body_lines, elided, truncated_any) =
                    elide_block(&block_lines, span.start, &mut seen);
                record_seen(&target.path, &hash, &seen);
                let mut notices: Vec<String> = Vec::new();
                if let Some((s, e)) = elided {
                    notices.push(format!(
                        "[…{} lines elided; re-read with line_range \"{s}-{e}\"]",
                        e - s + 1
                    ));
                }
                if truncated_any {
                    notices.push(format!(
                        "[Some lines were truncated to {MAX_COLUMN} columns; use line_range to re-read specific lines for the full content]"
                    ));
                }
                let mut content = format!("{header}\n{}", body_lines.join("\n"));
                append_content_notices(&mut content, &notices);
                vec![ReadResult {
                    path: target.path.clone(),
                    file_hash: hash,
                    header: header.clone(),
                    content,
                    warnings: None,
                }]
            }
            None => {
                let content = format!("{header}\n{}", format::format_numbered_lines(&text, 1));
                vec![ReadResult {
                    path: target.path.clone(),
                    file_hash: hash,
                    header: header.clone(),
                    content,
                    warnings: Some(format!(
                        "could not resolve a syntactic block starting at line {line} in `{path}`. \
                         Possible causes: the line does not begin a valid block (e.g. fn, struct, \
                         impl, enum, trait, mod), the line number exceeds the file length, or the \
                         line falls inside a string or comment. \
                         Try a different line number, a `line_range`, or read the whole file instead.",
                        path = target.path
                    )),
                }]
            }
        }
    } else {
        // Whole-file read: record every line as seen.
        let seen: Vec<(u32, String)> = file_lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u32 + 1, l.clone()))
            .collect();
        record_seen(&target.path, &hash, &seen);
        vec![ReadResult {
            path: target.path.clone(),
            file_hash: hash,
            header: header.clone(),
            content: format!("{header}\n{}", format::format_numbered_lines(&text, 1)),
            warnings: None,
        }]
    }
}

async fn search_symbol(fs: &DiskFilesystem, path: &str, name: &str) -> Vec<ReadResult> {
    let paths = match if Path::new(path).is_dir() {
        Ok(collect_source_files(Path::new(path)))
    } else if cosh_sdk::tree_sitter::language::detect_language(path).is_some() {
        Ok(vec![path.to_string()])
    } else {
        Err(format!(
            "no tree-sitter grammar available for `{path}`; use `line` targeting or read the whole file instead"
        ))
    } {
        Ok(p) => p,
        Err(e) => {
            return vec![ReadResult {
                path: path.to_string(),
                file_hash: String::new(),
                header: String::new(),
                content: String::new(),
                warnings: Some(e),
            }];
        }
    };

    let ts = cosh_sdk::tree_sitter::tree_sitter();
    let mut results: Vec<ReadResult> = Vec::new();

    for p in &paths {
        let text = match read_normalized(fs, p).await {
            Ok(t) => t,
            Err(e) => {
                results.push(ReadResult {
                    path: p.clone(),
                    file_hash: String::new(),
                    header: String::new(),
                    content: String::new(),
                    warnings: Some(e),
                });
                continue;
            }
        };
        if let Some(span) = ts.resolve_symbol(p, &text, name) {
            let _ = rollback::record(p, &text);
            let hash = format::compute_file_hash(&text);
            let header = format::format_hashline_header(p, &hash);
            let file_lines = file_lines(&text);
            let end_idx = (span.end as usize).min(file_lines.len());
            let start_idx = (span.start as usize - 1).min(end_idx);
            let block_lines = file_lines[start_idx..end_idx].to_vec();
            let mut seen = Vec::new();
            let (body_lines, elided, truncated_any) =
                elide_block(&block_lines, span.start, &mut seen);
            record_seen(p, &hash, &seen);
            let mut notices: Vec<String> = Vec::new();
            if let Some((s, e)) = elided {
                notices.push(format!(
                    "[…{} lines elided; re-read with line_range \"{s}-{e}\"]",
                    e - s + 1
                ));
            }
            if truncated_any {
                notices.push(format!(
                    "[Some lines were truncated to {MAX_COLUMN} columns; use line_range to re-read specific lines for the full content]"
                ));
            }
            let mut content = format!("{header}\n{}", body_lines.join("\n"));
            append_content_notices(&mut content, &notices);
            results.push(ReadResult {
                path: p.clone(),
                file_hash: hash,
                header: header.clone(),
                content,
                warnings: None,
            });
        }
    }

    if results.is_empty() {
        return vec![ReadResult {
            path: path.to_string(),
            file_hash: String::new(),
            header: String::new(),
            content: String::new(),
            warnings: Some(format!(
                "symbol `{name}` not found in `{path}`. \
                 Verify the symbol name is spelled exactly as defined in source code. \
                 If `{path}` is a directory, it may contain no files with a supported \
                 tree-sitter grammar. \
                 Try using `line` targeting to read specific sections, or read the whole \
                 file to inspect its contents.",
            )),
        }];
    }

    results
}

/// Recursively collects files within a specified directory.
///
/// Returns only files with a supported Tree-sitter grammar.
fn collect_source_files(path: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                files.extend(collect_source_files(&p));
            } else if p.is_file() {
                let s = p.to_string_lossy().to_string();
                if cosh_sdk::tree_sitter::language::detect_language(&s).is_some() {
                    files.push(s);
                }
            }
        }
    }
    files
}

async fn read_normalized(fs: &DiskFilesystem, path: &str) -> Result<String, String> {
    let file_text = fs.read_text(path).await.map_err(|e| e.to_string())?;
    let bom_result = normalize::strip_bom(&file_text);
    Ok(normalize::normalize_to_lf(&bom_result.text))
}
