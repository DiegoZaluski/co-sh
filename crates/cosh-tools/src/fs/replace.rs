//! Content-anchored replace engine (the `edits` argument of `fs_edit`).
//!
//! Each edit addresses its location by content: `old_string` must match the
//! tagged snapshot exactly (unique, unless `replace_all`), and is replaced by
//! `new_string`. The engine is a frontend over the hashline pipeline: it
//! locates the match in the snapshot the tag names, translates the change
//! into line-anchored hashline ops, and applies them through the same
//! patcher path as the `targets` engine — so hash validation, 3-way merge
//! recovery, rollback recording, diffs, and warnings behave identically.
//!
//! Matching is deliberately exact (no fuzzy fallback): a wrong `old_string`
//! is a model error worth surfacing, not guessing away. Diagnostics on
//! failure point at the actual file content so the model can self-correct.

use std::collections::HashMap;
use std::path::Path;
use std::sync::PoisonError;

use cosh_sdk::hashline::normalize::{normalize_to_lf, strip_bom};
use cosh_sdk::hashline::snapshots::{Snapshot, SnapshotStore};
use cosh_sdk::rollback;

use super::edit::{EditBatchError, EditResult, edit_target};
use super::fuzzy;
use super::types::{FsMetadata, ReplaceEdit};
use crate::util::path_guard::assert_editable_file;

/// Apply content-anchored replacements in order.
///
/// Edits are applied sequentially; when edit N fails, the batch stops there.
/// Edits applied before N stay (see [`EditBatchError::applied`]); edits after
/// N are skipped ([`EditBatchError::skipped`]) because they may depend on N.
///
/// Successive edits to the same file chain on the fresh tag each applied
/// edit produces, so a follow-up `ReplaceEdit` in the same call may omit
/// `file_hash`.
pub(crate) async fn content_edit(
    metadata: &FsMetadata,
    edits: &[ReplaceEdit],
    dry_run: bool,
) -> Result<Vec<EditResult>, EditBatchError> {
    let mut results = Vec::new();
    // Guarded path → tag minted by the most recent edit in this batch.
    let mut chained: HashMap<String, String> = HashMap::new();

    for (i, entry) in edits.iter().enumerate() {
        match replace_one(entry, metadata, &mut chained, dry_run).await {
            Ok(result) => results.push(result),
            Err(cause) => {
                return Err(EditBatchError {
                    failed_path: entry.path.clone(),
                    cause,
                    applied: results,
                    skipped: edits[i + 1..].iter().map(|e| e.path.clone()).collect(),
                });
            }
        }
    }

    Ok(results)
}

async fn replace_one(
    entry: &ReplaceEdit,
    metadata: &FsMetadata,
    chained: &mut HashMap<String, String>,
    dry_run: bool,
) -> Result<EditResult, String> {
    if entry.old_string.is_empty() {
        return Err(
            "old_string must not be empty; to create or overwrite whole files use the write tool."
                .to_string(),
        );
    }

    let validated_path = metadata.fs_guard(&entry.path)?;
    let canonical = validated_path.to_string_lossy().to_string();

    // Never modify a file that declares itself machine-generated (same guard
    // as `write`/`edit`); the change would be lost on the next generation run.
    assert_editable_file(Path::new(&canonical))?;

    let hash = entry
        .file_hash
        .clone()
        .or_else(|| chained.get(&canonical).cloned())
        .ok_or_else(|| missing_tag_message(&entry.path))?;

    let snapshot = fetch_snapshot(&entry.path, &canonical, &hash).ok_or_else(|| {
        // `read` skips recording snapshots for files over the snapshot
        // budget or with binary content, so an unresolvable tag there is
        // an anchorability problem, not a stale tag — sending the model
        // to re-read would loop forever. Compare on the normalized text
        // exactly as `rollback::record` does when it declines to record.
        match std::fs::read(&canonical) {
            Ok(bytes) if bytes.contains(&0) => binary_message(&entry.path),
            Ok(bytes) => {
                let text = normalize_to_lf(&strip_bom(&String::from_utf8_lossy(&bytes)).text);
                if text.len() > rollback::MAX_SNAPSHOT_BYTES {
                    oversize_message(&entry.path, text.len())
                } else {
                    unknown_tag_message(&entry.path, &hash)
                }
            }
            Err(_) => unknown_tag_message(&entry.path, &hash),
        }
    })?;

    let old = normalize_to_lf(&entry.old_string);
    let new = normalize_to_lf(&entry.new_string);
    let occurrences = find_occurrences(&snapshot.text, &old);

    if occurrences.is_empty() {
        return Err(no_match_message(&entry.path, &snapshot.text, &old));
    }
    if occurrences.len() > 1 && !entry.replace_all {
        return Err(ambiguous_message(&entry.path, &snapshot.text, &old));
    }

    let ops = build_ops(&snapshot.text, &new, &occurrences)?;
    let expected = if entry.replace_all {
        snapshot.text.replace(&old, &new)
    } else {
        snapshot.text.replacen(&old, &new, 1)
    };
    let target = super::types::EditTarget {
        path: entry.path.clone(),
        file_hash: hash,
        ops,
    };

    let result = edit_target(target, metadata, Some(&expected), dry_run).await?;
    // A preview's tag is not backed by a store snapshot (nothing was
    // committed), so it must not feed the chaining map: a follow-up edit
    // would resolve it into a confusing unknown-tag error.
    if result.dry_run.is_none() {
        chained.insert(canonical, result.file_hash.clone());
    }
    Ok(result)
}

/// Fetch the snapshot a tag names, trying the path as the model passed it
/// (the key `read`/`grep` record under) and then the guard-canonical path.
fn fetch_snapshot(path: &str, canonical: &str, hash: &str) -> Option<Snapshot> {
    let store = rollback::session_store();
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .by_hash(path, hash)
        .or_else(|| guard.by_hash(canonical, hash))
}

/// Non-overlapping exact occurrences of `needle` in `text`, ascending.
fn find_occurrences(text: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(relative) = text[start..].find(needle) {
        let begin = start + relative;
        let end = begin + needle.len();
        out.push((begin, end));
        start = end;
    }
    out
}

/// 1-based line number and 0-based column of a byte offset in LF text.
fn line_col_at(text: &str, offset: usize) -> (u32, usize) {
    let before = &text[..offset];
    let line = u32::try_from(before.matches('\n').count() + 1).unwrap_or(u32::MAX);
    let column = offset - before.rfind('\n').map_or(0, |i| i + 1);
    (line, column)
}

/// Translate content replacements into hashline ops.
///
/// Occurrences whose line ranges overlap (possible for same-string matches
/// sharing a line) are grouped into one hunk; each group becomes a single
/// `replace L1..L2:` (or `delete`) hunk anchored at snapshot coordinates.
///
/// Hashline ops are line-granular, so a partial-line match keeps its
/// surrounding context: the hunk payload is `prefix + replacement + suffix`
/// for the affected line range. A match that consumes a line terminator
/// (ends exactly at the start of the next line) is translated to reproduce
/// the exact character-level replacement:
///
/// - `new_string` ending with `\n` supplies the consumed terminator itself —
///   one trailing newline is stripped so the engine re-supplies it.
/// - otherwise the replacement content is *joined* with the following line
///   (its untouched text is appended to the payload and the range extended),
///   because the consumed terminator no longer separates them.
/// - when the match ends at the file's final newline there is no following
///   line to join: an empty `new_string` with an empty prefix is a pure line
///   deletion, and anything else would drop the trailing newline — inexpressible
///   in line ops, so rejected with a diagnostic instead of misapplied.
fn build_ops(text: &str, new: &str, occurrences: &[(usize, usize)]) -> Result<String, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut hunks: Vec<String> = Vec::new();

    let mut group_start = 0;
    while group_start < occurrences.len() {
        let (span_begin, mut span_end) = occurrences[group_start];
        let mut span_last_line = line_col_at(text, span_end).0;
        let mut group_end = group_start + 1;
        while group_end < occurrences.len() {
            let (begin, end) = occurrences[group_end];
            if line_col_at(text, begin).0 > span_last_line {
                break;
            }
            span_end = end;
            span_last_line = line_col_at(text, end).0;
            group_end += 1;
        }

        let (span_first_line, first_col) = line_col_at(text, span_begin);
        let (end_line, end_col) = line_col_at(text, span_end);
        let consumes_terminator = end_col == 0;

        let prefix = &lines[span_first_line as usize - 1][..first_col];
        let suffix = if consumes_terminator {
            ""
        } else {
            &lines[end_line as usize - 1][end_col..]
        };

        let mut payload_core = String::with_capacity(span_end - span_begin);
        let mut cursor = span_begin;
        for &(begin, end) in &occurrences[group_start..group_end] {
            payload_core.push_str(&text[cursor..begin]);
            payload_core.push_str(new);
            cursor = end;
        }
        payload_core.push_str(&text[cursor..span_end]);

        let mut payload_text;
        let last_line;
        if !consumes_terminator {
            last_line = end_line;
            payload_text = payload_core;
        } else if new.ends_with('\n') {
            payload_core.pop();
            last_line = end_line - 1;
            payload_text = payload_core;
        } else if span_end == text.len() && text.ends_with('\n') {
            // Only a span that collapses to nothing is expressible as a
            // pure line deletion; any surviving replacement content would
            // need to join with a following line that does not exist.
            if payload_core.is_empty() && prefix.is_empty() {
                hunks.push(delete_hunk(span_first_line, end_line - 1));
                group_start = group_end;
                continue;
            }
            return Err(joins_past_eof_message(span_first_line, end_line - 1));
        } else {
            let next_line = lines[end_line as usize - 1];
            last_line = end_line;
            payload_text = payload_core;
            payload_text.push_str(next_line);
        }
        let payload_text = format!("{prefix}{payload_text}{suffix}");

        hunks.push(replace_hunk(span_first_line, last_line, &payload_text));
        group_start = group_end;
    }

    Ok(hunks.join("\n"))
}

/// One hashline hunk for lines `start..=end` receiving `payload_text`.
///
/// Empty payload rows are emitted as lone `+` lines: an empty replacement
/// must leave an empty line behind, never delete it (only [`delete_hunk`]
/// removes whole lines).
fn replace_hunk(start: u32, end: u32, payload_text: &str) -> String {
    let mut hunk = if start == end {
        format!("replace {start}:")
    } else {
        format!("replace {start}..{end}:")
    };
    for line in payload_text.split('\n') {
        hunk.push_str("\n+");
        hunk.push_str(line);
    }
    hunk
}

/// A pure line-range deletion hunk.
fn delete_hunk(start: u32, end: u32) -> String {
    if start == end {
        format!("delete {start}")
    } else {
        format!("delete {start}..{end}")
    }
}

fn missing_tag_message(path: &str) -> String {
    format!(
        "Missing hashline snapshot tag for content edit to {path}; first read the file \
         (the read result carries a ¶path#TAG header), then pass that tag as `file_hash`."
    )
}

fn unknown_tag_message(path: &str, hash: &str) -> String {
    format!(
        "No snapshot recorded for tag #{hash} of {path} in this session. Re-read the \
         file with `read` to mint a current ¶path#TAG header — never invent the tag."
    )
}

fn oversize_message(path: &str, size: usize) -> String {
    format!(
        "Cannot content-replace {path}: it is {size} bytes, over the {}-byte session \
         snapshot budget, so no tag can anchor it. Use the `targets` engine \
         (hashline line/block ops) for this file.",
        rollback::MAX_SNAPSHOT_BYTES
    )
}

fn binary_message(path: &str) -> String {
    format!(
        "Cannot content-replace {path}: its content is binary, and binary files \
         are never anchored by a session snapshot tag."
    )
}

fn joins_past_eof_message(start_line: u32, end_line: u32) -> String {
    format!(
        "old_string spans lines {start_line}..{end_line} up to the file's final newline \
         and new_string would join the replacement with a following line that does \
         not exist (the result would lose the trailing newline). End new_string with \
         a newline, or include the trailing newline situation in the edit, e.g. via \
         the `targets` engine."
    )
}

/// Ambiguous exact match — byte-for-byte the oh-my-pi occurrence error
/// (previews around each recorded occurrence, disambiguation instruction),
/// plus one extra line naming OUR escape hatch (`replace_all`), which the
/// upstream message omits because its schema presents it elsewhere.
fn ambiguous_message(path: &str, text: &str, old: &str) -> String {
    // find_match's exact pass is deterministic, so re-running it with the
    // same needle rebuilds the occurrence previews (bounded windows around
    // each recorded occurrence) without threading them through the caller.
    let outcome = fuzzy::find_match(
        text,
        old,
        &fuzzy::FindMatchOptions {
            allow_fuzzy: false,
            threshold: None,
            excluded_ranges: &[],
        },
    );
    let mut message = fuzzy::format_occurrence_error(path, &outcome);
    message
        .push_str("\nPass `replace_all: true` to replace every occurrence without disambiguating.");
    message
}

/// No exact match anywhere — the fuzzy closest-match search turns the
/// rejection into a one-round-trip correction: when a near-identical region
/// exists (unique above the threshold, or a dominant best), point at it with
/// similarity, line, and the first differing line; otherwise fall back to
/// oh-my-pi's exact `format_match_error` wording. The fuzzy result NEVER
/// substitutes anything — the model re-issues with the corrected text.
fn no_match_message(path: &str, text: &str, old: &str) -> String {
    let outcome = fuzzy::find_match(
        text,
        old,
        &fuzzy::FindMatchOptions {
            allow_fuzzy: true,
            threshold: None,
            excluded_ranges: &[],
        },
    );
    let Some(closest) = &outcome.closest else {
        // Nothing even remotely similar (target longer than the file, etc.).
        return fuzzy::format_match_error(
            path,
            old,
            None,
            false,
            fuzzy::DEFAULT_FUZZY_THRESHOLD,
            None,
        );
    };
    if outcome.matched.is_some() {
        // Unique above-threshold (or dominant) region: the strongest hint.
        let percent = (closest.confidence * 100.0).round() as i64;
        let search_lines: Vec<&str> = old.split('\n').collect();
        let actual_lines: Vec<&str> = closest.actual_text.split('\n').collect();
        let (old_line, new_line) = fuzzy::first_different_line(&search_lines, &actual_lines);
        return format!(
            "Could not find the exact text in {path}.\n\nA near-identical region ({percent}% similar) exists at line {}: copy its text exactly into old_string:\n  - {old_line}\n  + {new_line}",
            closest.start_line
        );
    }
    fuzzy::format_match_error(
        path,
        old,
        Some(closest),
        true,
        fuzzy::DEFAULT_FUZZY_THRESHOLD,
        outcome.fuzzy_matches,
    )
}

#[cfg(test)]
mod test;
