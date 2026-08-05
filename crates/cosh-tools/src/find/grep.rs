//! Regex-based file content search tool.
//!
//! Results carry a per-file hashline anchor (`¶path#TAG`): the tool records a
//! whole-file snapshot for a bounded window of matched files and returns the
//! content tag so the agent can edit a file directly from grep output without
//! re-reading it just to obtain the current hash (port of the oh-my-pi grep
//! behavior).
//!
//! Output is windowed to keep the model context bounded: at most
//! [`DEFAULT_FILE_LIMIT`] distinct files are surfaced per call (page further
//! files with `skip`), each file's matches are capped
//! ([`MULTI_FILE_PER_FILE_MATCHES`] for multi-file scopes,
//! [`SINGLE_FILE_MATCHES`] for a single-file scope), and lines longer than
//! [`DEFAULT_MAX_COLUMN`] characters are truncated with a `...` suffix and a
//! `truncated` flag. Patterns containing a literal or escaped newline
//! automatically enable multiline matching.
//!
//! Multiple targets can be searched in one call ([`grep_targets`]): each
//! target is guard-resolved individually, matches are rebased to the common
//! ancestor directory so paths stay unique, and duplicates are dropped.

use cosh_sdk::find::{GrepOptions, GrepOutputMode, grep as sdk_grep};
use cosh_sdk::hashline::format::format_hashline_header;
use cosh_sdk::rollback;

use super::types::{ContextEntry, Grep, GrepFileEntry, GrepMatchEntry, GrepOutput};

/// Maximum number of distinct files surfaced in a single call. The agent
/// paginates further pages via `skip` (port of the oh-my-pi file limit).
const DEFAULT_FILE_LIMIT: usize = 20;
/// Per-file match cap for multi-file searches — keeps a single hot file from
/// crowding out diverse hits (port of the oh-my-pi per-file cap).
const MULTI_FILE_PER_FILE_MATCHES: u32 = 20;
/// Per-file match cap for single-file searches — there is no diversity
/// concern when the scope is one file.
const SINGLE_FILE_MATCHES: u32 = 200;
/// Hard safety ceiling on how many matches are fetched from the engine before
/// the windowing below. Sized to cover the file window
/// (DEFAULT_FILE_LIMIT files × MULTI_FILE_PER_FILE_MATCHES matches) with
/// pagination headroom so the caller can see the total file count.
const INTERNAL_TOTAL_CAP: u32 = 2000;
/// Lines longer than this (characters) are truncated with a `...` suffix and
/// flagged via `GrepMatchEntry::truncated` (port of the oh-my-pi column cap).
const DEFAULT_MAX_COLUMN: u32 = 200;

/// Resolve the absolute path of a match: single-file searches return absolute
/// paths, directory searches return paths relative to the searched root.
fn match_absolute_path(root: &str, match_path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(match_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::Path::new(root).join(p)
    }
}

/// Mint a whole-file hashline tag for one matched file, record the snapshot so
/// a follow-up `fs_edit` can validate (or 3-way-recover) against it, and record
/// which lines of the file were surfaced to the model at that tag.
///
/// The recorded key is the canonicalized absolute path — the same form `read`
/// and the hashline patcher use — so recovery finds the snapshot regardless of
/// which tool recorded it. Returns `None` for unreadable, oversized, or binary
/// files (same contract as `rollback::record`); those get plain output. The
/// size guard mirrors `rollback::MAX_SNAPSHOT_BYTES` so files that would not be
/// snapshotted are never read just to be discarded.
fn hashline_entry_for_file(
    root: &str,
    match_path: &str,
    seen_lines: &[(u32, String)],
) -> Option<GrepFileEntry> {
    let abs = match_absolute_path(root, match_path);
    let meta = std::fs::metadata(&abs).ok()?;
    if meta.len() > u64::try_from(rollback::MAX_SNAPSHOT_BYTES).unwrap_or(u64::MAX) {
        return None;
    }
    let text = std::fs::read_to_string(&abs).ok()?;
    let key = std::fs::canonicalize(&abs)
        .unwrap_or(abs)
        .to_string_lossy()
        .to_string();
    let file_hash = rollback::record(&key, &text)?;
    if !seen_lines.is_empty() {
        rollback::record_seen_lines(&key, &file_hash, seen_lines);
    }
    Some(GrepFileEntry {
        path: match_path.to_string(),
        file_hash: file_hash.clone(),
        header: format_hashline_header(&key, &file_hash),
    })
}

/// Group matches by file, preserving encounter order.
///
/// Returns the distinct file paths in first-encounter order and a map from
/// path to its matches. Order-independent: it does not assume the engine
/// emits contiguous per-file runs, so it stays correct if the underlying
/// search is ever fed from a differently-ordered source (e.g. a scan cache).
fn group_by_file(
    shown: &[GrepMatchEntry],
) -> (Vec<&str>, std::collections::HashMap<&str, Vec<GrepMatchEntry>>) {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: std::collections::HashMap<&str, Vec<GrepMatchEntry>> =
        std::collections::HashMap::new();
    for m in shown {
        let path = m.path.as_str();
        if let Some(list) = groups.get_mut(path) {
            list.push(m.clone());
        } else {
            order.push(path);
            groups.insert(path, vec![m.clone()]);
        }
    }
    (order, groups)
}

/// Build per-file hashline anchors for the shown matches.
///
/// Only the first [`DEFAULT_FILE_LIMIT`] files are anchored — minting a tag
/// requires reading the whole file, so the work is bounded per grep call.
/// Files beyond the window surface plain, headerless output. The seen-lines
/// recorded for each anchored file cover every line the model actually saw
/// (matches plus context).
fn build_hashline_files(root: &str, shown: &[GrepMatchEntry]) -> Vec<GrepFileEntry> {
    let (order, groups) = group_by_file(shown);
    let mut entries = Vec::new();
    for path in order.into_iter().take(DEFAULT_FILE_LIMIT) {
        let mut seen_lines: Vec<(u32, String)> = Vec::new();
        for m in groups.get(path).map(Vec::as_slice).unwrap_or_default() {
            seen_lines.push((m.line_number, m.line.clone()));
            for ctx in &m.context_before {
                seen_lines.push((ctx.line_number, ctx.line.clone()));
            }
            for ctx in &m.context_after {
                seen_lines.push((ctx.line_number, ctx.line.clone()));
            }
        }
        if let Some(entry) = hashline_entry_for_file(root, path, &seen_lines) {
            entries.push(entry);
        }
    }
    entries
}

/// Deepest common ancestor directory of the given (existing) paths. A file
/// target contributes its parent directory. Returns `None` only for an empty
/// input.
///
/// Note: when the targets share NO common ancestor (e.g. different mount
/// points), the rebase falls back to each target's raw relative paths, which
/// can collide across targets. On a single-`/` filesystem every absolute path
/// shares `/`, so this only matters for exotic layouts.
fn common_ancestor(paths: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut ancestor = if first.is_file() {
        first.parent()?.to_path_buf()
    } else {
        first.clone()
    };
    for path in iter {
        while !path.starts_with(&ancestor) {
            if !ancestor.pop() {
                return None;
            }
        }
    }
    Some(ancestor)
}

/// Parse `"start-end"` into a 1-based inclusive `(start, end)` line range.
fn parse_line_range(raw: &str) -> Result<(u32, u32), String> {
    let (start, end) = raw
        .split_once('-')
        .ok_or_else(|| format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}"))?;
    let start: u32 = start
        .trim()
        .parse()
        .map_err(|_| format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}"))?;
    let end: u32 = end
        .trim()
        .parse()
        .map_err(|_| format!("line_range must be \"start-end\" (1-based, inclusive), got: {raw}"))?;
    if start < 1 || end < start {
        return Err(format!(
            "line_range must satisfy 1 <= start <= end, got: {raw}"
        ));
    }
    Ok((start, end))
}

/// Keep only matches whose line falls inside `(start, end)` (inclusive).
/// Context lines outside the range are dropped so the window never shows
/// content the caller did not ask for. Returns the kept matches and the count
/// of kept matches.
fn apply_line_range(matches: Vec<GrepMatchEntry>, range: (u32, u32)) -> (Vec<GrepMatchEntry>, u32) {
    let (start, end) = range;
    let mut kept = 0u32;
    let out = matches
        .into_iter()
        .filter_map(|mut m| {
            if m.line_number < start || m.line_number > end {
                return None;
            }
            m.context_before
                .retain(|c| c.line_number >= start && c.line_number <= end);
            m.context_after
                .retain(|c| c.line_number >= start && c.line_number <= end);
            kept += 1;
            Some(m)
        })
        .collect();
    (out, kept)
}

/// Search file content for lines matching a regex pattern.
///
/// Single-target convenience wrapper over [`grep_targets`] — see it for the
/// full behavior and output bounds.
///
/// # Errors
/// Returns an error when the path cannot be resolved, `pattern` is an
/// invalid regex, or the operation is cancelled by a timeout.
pub fn grep(grep: &Grep, pattern: &str, path: &str) -> Result<GrepOutput, String> {
    grep_targets(grep, pattern, &[path.to_string()])
}

/// Search file content for lines matching a regex pattern across one or more
/// targets.
///
/// `targets` are guard-resolved absolute paths. Multi-target searches rebase
/// every match to the targets' common ancestor directory so paths stay unique
/// (e.g. `src/util.rs` vs `tests/util.rs`) and drop duplicates from
/// overlapping targets.
///
/// # Output bounds
///
/// - At most [`DEFAULT_FILE_LIMIT`] distinct files per call for multi-file
///   scopes; `skip` pages the next window. `file_limit_reached` and a
///   `note` tell the caller when more files remain.
/// - Per-file match caps (`20` multi-file, `200` single-file) keep a hot file
///   from crowding out diverse hits; `per_file_limit_reached` flags a cap hit.
/// - Lines longer than [`DEFAULT_MAX_COLUMN`] characters are truncated with
///   `...` and flagged via `GrepMatchEntry::truncated`.
/// - Patterns containing a literal or escaped newline automatically enable
///   multiline matching.
/// - `line_range` (`"start-end"`, 1-based, inclusive) keeps only matches in
///   that range; it requires every target to be a single file. The per-file
///   fetch budget is widened so in-range matches are not starved by
///   out-of-range ones.
/// - Zero selected matches are marked `useless` with a no-match note: such a
///   result carries no new information — adjust the pattern or scope instead
///   of blindly retrying.
/// - `total_matches` is the deduplicated per-line union across targets
///   (before the per-file caps and window trim), so two matches on the SAME
///   line of the same file count once.
///
/// # Errors
/// Returns an error when a target cannot be resolved, `pattern` is an invalid
/// regex, `line_range` is malformed or targets a directory, or the operation
/// is cancelled by a timeout.
pub fn grep_targets(grep: &Grep, pattern: &str, targets: &[String]) -> Result<GrepOutput, String> {
    if targets.is_empty() {
        return Err("no search targets provided".to_string());
    }

    let range = match &grep.line_range {
        Some(raw) => Some(parse_line_range(raw)?),
        None => None,
    };

    let resolved: Vec<std::path::PathBuf> = targets.iter().map(std::path::PathBuf::from).collect();
    let multi = targets.len() > 1;

    let is_file_scope = !multi && resolved[0].is_file();
    let is_multi_scope = multi || !is_file_scope || grep.glob.is_some();
    let per_file_cap = if is_multi_scope {
        MULTI_FILE_PER_FILE_MATCHES
    } else {
        SINGLE_FILE_MATCHES
    };

    // A line range only has a single meaning against one file.
    if range.is_some() {
        for target in &resolved {
            if !target.is_file() {
                return Err(format!(
                    "line_range requires single-file targets, but `{}` is not a file",
                    target.display()
                ));
            }
        }
    }

    // A caller that sets `max_count` is not paginating: the cap bounds the
    // output and there is no `skip` contract to follow a "use skip=N"
    // suggestion with, so the file window is not applied (mirrors oh-my-pi).
    // Otherwise fetch up to the internal ceiling and window in this layer.
    let (sdk_max_count, file_window) = match grep.max_count {
        Some(cap) => (Some(cap), None),
        None => (Some(INTERNAL_TOTAL_CAP), Some(DEFAULT_FILE_LIMIT)),
    };
    // Multiline is enabled automatically when the pattern itself spans lines
    // (literal newline or the `\n` escape); the engine's line-mode would
    // otherwise silently return zero matches for such patterns.
    let effective_multiline = pattern.contains('\n') || pattern.contains("\\n");
    // Widened per-file fetch when a range is set so in-range matches are not
    // starved by out-of-range ones (mirrors oh-my-pi's line-range fetch cap).
    let fetch_per_file = match range {
        Some((start, end)) => end
            .saturating_sub(start)
            .saturating_add(1)
            .max(per_file_cap)
            .min(INTERNAL_TOTAL_CAP - 1)
            .saturating_add(1),
        None => per_file_cap + 1,
    };

    let ancestor = if multi { common_ancestor(&resolved) } else { None };
    let root = ancestor
        .as_ref()
        .map(|a| a.to_string_lossy().to_string())
        .unwrap_or_else(|| targets[0].clone());

    // Run the engine per target and merge.
    let mut raw: Vec<GrepMatchEntry> = Vec::new();
    let mut files_with_matches = 0u32;
    let mut files_searched = 0u32;
    let mut limit_reached = false;
    let mut seen: std::collections::HashSet<(String, u32)> = std::collections::HashSet::new();
    for target in &resolved {
        let result = sdk_grep(GrepOptions {
            pattern: pattern.to_owned(),
            path: target.to_string_lossy().to_string(),
            glob: grep.glob.clone(),
            r#type: grep.file_type.clone(),
            ignore_case: grep.ignore_case,
            multiline: Some(effective_multiline),
            hidden: grep.hidden,
            gitignore: grep.gitignore,
            cache: None,
            max_count: sdk_max_count,
            offset: None,
            context_before: grep.context_before,
            context_after: grep.context_after,
            context: None,
            max_columns: Some(DEFAULT_MAX_COLUMN),
            mode: Some(GrepOutputMode::Content),
            max_count_per_file: Some(fetch_per_file),
            timeout_ms: grep.timeout_ms,
        })?;
        files_with_matches = files_with_matches.saturating_add(result.files_with_matches);
        files_searched = files_searched.saturating_add(result.files_searched);
        limit_reached = limit_reached || result.limit_reached == Some(true);

        for m in result.matches {
            let path_out = if multi {
                let abs = match_absolute_path(&target.to_string_lossy(), &m.path);
                match ancestor.as_deref() {
                    Some(a) => abs
                        .strip_prefix(a)
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                        .unwrap_or(m.path),
                    None => m.path,
                }
            } else {
                m.path
            };
            if multi && !seen.insert((path_out.clone(), m.line_number)) {
                continue;
            }
            raw.push(GrepMatchEntry {
                path: path_out,
                line_number: m.line_number,
                line: m.line,
                truncated: m.truncated,
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
            });
        }
    }

    // Apply the line-range filter before windowing so counts reflect it.
    // `raw.len()` is the deduplicated union count across targets (before the
    // per-file caps and window trim) — for multi-target searches the engine
    // per-target totals would double-count overlapping matches.
    let total_after_filter = match range {
        Some(r) => {
            let (filtered, kept) = apply_line_range(raw, r);
            raw = filtered;
            kept
        }
        None => u32::try_from(raw.len()).unwrap_or(u32::MAX),
    };

    // Group by file in encounter order (order-independent — the engine does
    // not have to emit contiguous per-file runs).
    let (order, mut groups) = group_by_file(&raw);

    // Apply the per-file cap (the engine fetched cap+1 to let us detect
    // overflow).
    let mut per_file_limit_reached = false;
    for path in &order {
        if let Some(list) = groups.get_mut(path)
            && list.len() > per_file_cap as usize
        {
            per_file_limit_reached = true;
            list.truncate(per_file_cap as usize);
        }
    }

    let total_files = order.len();
    // Pagination only applies to the windowed path. A caller that set
    // `max_count` gets a match-bounded fetch (the engine stops early), so
    // `skip` cannot advance past files the fetch never covered — and that
    // caller is not paginating anyway (mirrors oh-my-pi).
    let can_paginate = is_multi_scope && file_window.is_some();
    let skip_files = if can_paginate {
        (grep.skip.unwrap_or(0) as usize).min(total_files)
    } else {
        0
    };

    let window_files: Vec<&str> = if can_paginate {
        match file_window {
            Some(window) => order.iter().skip(skip_files).take(window).copied().collect(),
            None => order.iter().skip(skip_files).copied().collect(),
        }
    } else {
        order.to_vec()
    };
    let file_limit_reached =
        can_paginate && file_window.is_some() && total_files > skip_files + window_files.len();

    let shown: Vec<GrepMatchEntry> = window_files
        .iter()
        .flat_map(|path| groups.get(*path).cloned().unwrap_or_default())
        .collect();

    let total_files_label = if limit_reached {
        format!("{total_files}+")
    } else {
        total_files.to_string()
    };
    let next_skip = skip_files + window_files.len();
    let window_note = file_limit_reached.then(|| {
        format!(
            "Showing files {}-{} of {}. Use skip={} for the next page, or narrow paths/pattern.",
            skip_files + 1,
            next_skip,
            total_files_label,
            next_skip
        )
    });

    let (note, useless) = if shown.is_empty() {
        let skip_past_end = can_paginate
            && grep.skip.unwrap_or(0) > 0
            && total_files > 0
            && skip_files >= total_files;
        let text = if skip_past_end {
            format!(
                "No more results ({} files total; skip={} is past the end)",
                total_files_label,
                grep.skip.unwrap_or(0)
            )
        } else {
            "No matches found".to_string()
        };
        (Some(text), Some(true))
    } else {
        (window_note, None)
    };

    let files = build_hashline_files(&root, &shown);

    Ok(GrepOutput {
        matches: shown,
        total_matches: total_after_filter,
        files_with_matches,
        files_searched,
        file_limit_reached,
        per_file_limit_reached,
        note,
        useless,
        files,
    })
}
