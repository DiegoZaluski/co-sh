//! Apply a parsed list of [`Edit`]s to a text body and return the
//! post-edit lines plus any diagnostic warnings. Pure function: no FS, no
//! mutation of the input.
//!
//! Replacement groups are first normalized by [`repair_boundary_balance`],
//! which fixes the common model mistake of a payload that duplicates or drops
//! the closing delimiter bordering the range (balance-validated; see below).
use super::messages::UNRESOLVED_BLOCK_INTERNAL;
use super::types::{Anchor, ApplyResult, Cursor, Edit, Replacement};
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineOrigin {
    Original,
    Insert,
    Replacement,
}

#[derive(Clone)]
struct IndexedEdit {
    edit: Edit,
    idx: usize,
}

static STRUCTURAL_CLOSER_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^\s*[)\]}]+[;,]?\s*$").unwrap());

fn is_replacement_insert(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::Insert {
            mode: Some(Replacement::Replacement),
            ..
        }
    )
}

fn get_cursor_anchors(cursor: &Cursor) -> Vec<Anchor> {
    match cursor {
        Cursor::BeforeAnchor(a) | Cursor::AfterAnchor(a) => vec![*a],
        _ => vec![],
    }
}

fn get_edit_anchors(edit: &Edit) -> Vec<Anchor> {
    match edit {
        Edit::Delete { anchor, .. } => vec![*anchor],
        _ => get_cursor_anchors(match edit {
            Edit::Insert { cursor, .. } => cursor,
            _ => return vec![],
        }),
    }
}

fn validate_line_bounds(edits: &[Edit], file_lines: &[String]) -> Result<(), String> {
    for edit in edits {
        for anchor in get_edit_anchors(edit) {
            if anchor.line < 1 || anchor.line as usize > file_lines.len() {
                return Err(format!(
                    "Line {} does not exist (file has {} lines)",
                    anchor.line,
                    file_lines.len()
                ));
            }
        }
    }
    Ok(())
}

fn clone_applied_edit(edit: &Edit, index: u32) -> Edit {
    match edit {
        Edit::Insert {
            cursor,
            text,
            line_num,
            mode,
            ..
        } => Edit::Insert {
            cursor: cursor.clone(),
            text: text.clone(),
            line_num: *line_num,
            index,
            mode: *mode,
        },
        Edit::Delete {
            anchor,
            line_num,
            old_assertion,
            ..
        } => Edit::Delete {
            anchor: *anchor,
            line_num: *line_num,
            index,
            old_assertion: old_assertion.clone(),
        },
        Edit::Block { .. } => unreachable!(),
    }
}

fn insert_at_start(
    file_lines: &mut Vec<String>,
    line_origins: &mut Vec<LineOrigin>,
    lines: &[String],
) {
    if lines.is_empty() {
        return;
    }
    let origins: Vec<LineOrigin> = lines.iter().map(|_| LineOrigin::Insert).collect();
    if file_lines.len() == 1 && file_lines[0].is_empty() {
        file_lines.splice(0..1, lines.iter().cloned());
        line_origins.splice(0..1, origins);
        return;
    }
    file_lines.splice(0..0, lines.iter().cloned());
    line_origins.splice(0..0, origins);
}

fn insert_at_end(
    file_lines: &mut Vec<String>,
    line_origins: &mut Vec<LineOrigin>,
    lines: &[String],
) -> Option<u32> {
    if lines.is_empty() {
        return None;
    }
    let origins: Vec<LineOrigin> = lines.iter().map(|_| LineOrigin::Insert).collect();
    if file_lines.len() == 1 && file_lines[0].is_empty() {
        file_lines.splice(0..1, lines.iter().cloned());
        line_origins.splice(0..1, origins);
        return Some(1);
    }
    let has_trailing_newline =
        !file_lines.is_empty() && file_lines[file_lines.len() - 1].is_empty();
    let insert_index = if has_trailing_newline {
        file_lines.len() - 1
    } else {
        file_lines.len()
    };
    file_lines.splice(insert_index..insert_index, lines.iter().cloned());
    line_origins.splice(insert_index..insert_index, origins);
    Some(u32::try_from(insert_index).ok()? + 1)
}

fn bucket_anchor_edits_by_line(
    edits: &[IndexedEdit],
) -> std::collections::HashMap<u32, Vec<IndexedEdit>> {
    let mut by_line: std::collections::HashMap<u32, Vec<IndexedEdit>> =
        std::collections::HashMap::new();
    for entry in edits {
        let line = match &entry.edit {
            Edit::Delete { anchor, .. } => anchor.line,
            Edit::Insert {
                cursor: Cursor::BeforeAnchor(a) | Cursor::AfterAnchor(a),
                ..
            } => a.line,
            _ => 0,
        };
        by_line.entry(line).or_default().push(IndexedEdit {
            edit: entry.edit.clone(),
            idx: entry.idx,
        });
    }
    by_line
}

// Boundary-balance repair
//
// Models routinely miscount a replacement range's edges. The payload either
// re-states a closing delimiter that still lives just outside the range
// (producing a DUPLICATE `}` / `);` / `]`) or the range deletes a closer the
// payload never restates (DROPPING it). Both are the same defect — a
// replacement whose payload does not preserve the deleted region's delimiter
// balance — and both leave the file syntactically broken.
//
// A repair fires only when (a) the group's payload balance differs from the
// deleted region's balance and (b) one boundary operation drives that
// difference to exactly zero while leaving the surrounding text byte-identical.
// The operation only ever drops an exact multi-line boundary echo or a single
// pure structural-closer line, or spares a deleted pure structural-closer line,
// so content lines are never moved or lost. Balance-preserving edits are left
// strictly alone.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct DelimiterBalance {
    paren: i32,
    bracket: i32,
    brace: i32,
}

fn compute_delimiter_balance(lines: &[String]) -> DelimiterBalance {
    let mut balance = DelimiterBalance::default();
    let mut in_block_comment = false;
    let mut quote = '\0';
    for line in lines {
        let line_bytes = line.as_bytes();
        let mut i = 0;
        while i < line.len() {
            let ch = line_bytes[i] as char;
            if in_block_comment {
                if ch == '*' && i + 1 < line.len() && line_bytes[i + 1] == b'/' {
                    in_block_comment = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if quote != '\0' {
                if ch == '\\' {
                    i += 2;
                    continue;
                } else if ch == quote {
                    quote = '\0';
                }
                i += 1;
                continue;
            }
            if ch == '"' || ch == '\'' || ch == '`' {
                quote = ch;
                i += 1;
                continue;
            }
            if ch == '/' && i + 1 < line.len() && line_bytes[i + 1] == b'/' {
                break;
            }
            if ch == '/' && i + 1 < line.len() && line_bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
            match ch {
                '(' => balance.paren += 1,
                ')' => balance.paren -= 1,
                '[' => balance.bracket += 1,
                ']' => balance.bracket -= 1,
                '{' => balance.brace += 1,
                '}' => balance.brace -= 1,
                _ => {}
            }
            i += 1;
        }
        if quote == '"' || quote == '\'' {
            quote = '\0';
        }
    }
    balance
}

fn balance_delta(a: DelimiterBalance, b: DelimiterBalance) -> DelimiterBalance {
    DelimiterBalance {
        paren: a.paren - b.paren,
        bracket: a.bracket - b.bracket,
        brace: a.brace - b.brace,
    }
}

fn balance_negate(a: DelimiterBalance) -> DelimiterBalance {
    DelimiterBalance {
        paren: -a.paren,
        bracket: -a.bracket,
        brace: -a.brace,
    }
}

fn balance_equal(a: DelimiterBalance, b: DelimiterBalance) -> bool {
    a.paren == b.paren && a.bracket == b.bracket && a.brace == b.brace
}

fn balance_is_zero(a: DelimiterBalance) -> bool {
    a.paren == 0 && a.bracket == 0 && a.brace == 0
}

struct ReplacementGroup {
    insert_indices: Vec<usize>,
    delete_indices: Vec<usize>,
    payload: Vec<String>,
    start_line: u32,
    end_line: u32,
}

fn find_replacement_group(edits: &[Edit], start: usize) -> Option<ReplacementGroup> {
    let first = edits.get(start)?;
    let (line_num, cursor_anchor_line) = match first {
        Edit::Insert {
            cursor: Cursor::BeforeAnchor(a),
            line_num,
            mode: Some(Replacement::Replacement),
            ..
        } => (*line_num, a.line),
        _ => return None,
    };
    let anchor_line = cursor_anchor_line;
    let mut insert_indices: Vec<usize> = Vec::new();
    let mut payload: Vec<String> = Vec::new();
    let mut i = start;
    while i < edits.len() {
        match &edits[i] {
            Edit::Insert {
                cursor: Cursor::BeforeAnchor(a),
                line_num: ln,
                mode: Some(Replacement::Replacement),
                text,
                ..
            } if *ln == line_num && a.line == anchor_line => {
                insert_indices.push(i);
                payload.push(text.clone());
                i += 1;
            }
            _ => break,
        }
    }
    let mut delete_indices: Vec<usize> = Vec::new();
    let mut expected_line = anchor_line;
    while i < edits.len() {
        match &edits[i] {
            Edit::Delete {
                anchor,
                line_num: ln,
                ..
            } if *ln == line_num && anchor.line == expected_line => {
                delete_indices.push(i);
                expected_line += 1;
                i += 1;
            }
            _ => break,
        }
    }
    if delete_indices.is_empty() {
        return None;
    }

    let total_deletes = u32::try_from(delete_indices.len()).unwrap_or(u32::MAX);

    Some(ReplacementGroup {
        insert_indices,
        delete_indices,
        payload,
        start_line: anchor_line,
        end_line: anchor_line + total_deletes - 1,
    })
}

fn find_duplicate_suffix(
    group: &ReplacementGroup,
    file_lines: &[String],
    delta: DelimiterBalance,
) -> usize {
    let end_line = group.end_line as usize;
    let max_k = std::cmp::min(group.payload.len(), file_lines.len() - end_line);
    for k in (1..=max_k).rev() {
        let mut matches = true;
        for t in 0..k {
            if group.payload[group.payload.len() - k + t] != file_lines[end_line + t] {
                matches = false;
                break;
            }
        }
        if !matches {
            continue;
        }
        if k == 1 && !STRUCTURAL_CLOSER_RE.is_match(&group.payload[group.payload.len() - 1]) {
            continue;
        }
        let payload_slice: Vec<String> = group.payload[group.payload.len() - k..].to_vec();
        if balance_equal(compute_delimiter_balance(&payload_slice), delta) {
            return k;
        }
    }
    0
}

fn find_duplicate_prefix(
    group: &ReplacementGroup,
    file_lines: &[String],
    delta: DelimiterBalance,
) -> usize {
    let start_line = group.start_line as usize;
    let max_j = std::cmp::min(group.payload.len(), start_line - 1);
    for j in (1..=max_j).rev() {
        let mut matches = true;
        for t in 0..j {
            if group.payload[t] != file_lines[start_line - 1 - j + t] {
                matches = false;
                break;
            }
        }
        if !matches {
            continue;
        }
        if j == 1 && !STRUCTURAL_CLOSER_RE.is_match(&group.payload[0]) {
            continue;
        }
        let payload_slice: Vec<String> = group.payload[..j].to_vec();
        if balance_equal(compute_delimiter_balance(&payload_slice), delta) {
            return j;
        }
    }
    0
}

fn find_dropped_suffix_closers(
    group: &ReplacementGroup,
    file_lines: &[String],
    delta: DelimiterBalance,
) -> usize {
    let wanted = balance_negate(delta);
    let max_m = group.delete_indices.len();
    let end_line = group.end_line as usize;
    for m in 1..=max_m {
        let closer_line = &file_lines[group.end_line as usize - m];
        if !STRUCTURAL_CLOSER_RE.is_match(closer_line) {
            break;
        }
        let slice = &file_lines[end_line - m..end_line];
        if balance_equal(compute_delimiter_balance(slice), wanted) {
            return m;
        }
    }
    0
}

fn describe_boundary_repair(group: &ReplacementGroup, action: &str) -> String {
    format!(
        "Auto-repaired a delimiter-balance mismatch in the replacement at line {}: {}. \
         Issue the payload as the final desired content only — never restate or omit a closing bracket bordering the range.",
        group.start_line, action
    )
}

fn repair_boundary_balance(edits: &[Edit], file_lines: &[String]) -> (Vec<Edit>, Vec<String>) {
    let mut out: Vec<Edit> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        let Some(group) = find_replacement_group(edits, i) else {
            out.push(edits[i].clone());
            i += 1;
            continue;
        };
        let inserts: Vec<&Edit> = group
            .insert_indices
            .iter()
            .map(|idx| &edits[*idx])
            .collect();
        let deletes: Vec<&Edit> = group
            .delete_indices
            .iter()
            .map(|idx| &edits[*idx])
            .collect();
        i = group.delete_indices[group.delete_indices.len() - 1] + 1;

        let payload_balance = compute_delimiter_balance(&group.payload);
        let deleted_slice: Vec<String> =
            file_lines[group.start_line as usize - 1..group.end_line as usize].to_vec();
        let deleted_balance = compute_delimiter_balance(&deleted_slice);
        let delta = balance_delta(payload_balance, deleted_balance);

        if balance_is_zero(delta) {
            out.extend(inserts.into_iter().cloned());
            out.extend(deletes.into_iter().cloned());
            continue;
        }

        let dup_suffix = find_duplicate_suffix(&group, file_lines, delta);
        if dup_suffix > 0 {
            warnings.push(describe_boundary_repair(
                &group,
                &format!(
                    "dropped {dup_suffix} duplicated trailing payload line(s) already present below the range",
                ),
            ));
            let keep = group.insert_indices.len() - dup_suffix;
            for idx in &group.insert_indices[..keep] {
                out.push(edits[*idx].clone());
            }
            out.extend(deletes.into_iter().cloned());
            continue;
        }

        let dup_prefix = find_duplicate_prefix(&group, file_lines, delta);
        if dup_prefix > 0 {
            warnings.push(describe_boundary_repair(
                &group,
                &format!(
                    "dropped {dup_prefix} duplicated leading payload line(s) already present above the range",
                ),
            ));
            for idx in &group.insert_indices[dup_prefix..] {
                out.push(edits[*idx].clone());
            }
            out.extend(deletes.into_iter().cloned());
            continue;
        }

        let dropped_closers = find_dropped_suffix_closers(&group, file_lines, delta);
        if dropped_closers > 0 {
            warnings.push(describe_boundary_repair(
                &group,
                &format!(
                    "kept {dropped_closers} structural closing line(s) the range deleted without restating",
                ),
            ));
            out.extend(inserts.into_iter().cloned());
            let keep = group.delete_indices.len() - dropped_closers;
            for idx in &group.delete_indices[..keep] {
                out.push(edits[*idx].clone());
            }
            continue;
        }

        out.extend(inserts.into_iter().cloned());
        out.extend(deletes.into_iter().cloned());
    }
    (out, warnings)
}

/// Apply a parsed list of edits to a text body. Pure function — no I/O.
///
/// Returns the post-edit text and the first changed line number (1-indexed).
/// Returns an error if an anchor is out of bounds.
///
/// # Panics
///
/// Panics if any edit is an unresolved `Edit::Block` variant or an anchor is out of bounds.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn apply_edits(text: &str, edits: &[Edit]) -> ApplyResult {
    if edits.is_empty() {
        return ApplyResult {
            text: text.to_string(),
            first_changed_line: None,
            warnings: vec![],
        };
    }

    for edit in edits {
        if matches!(edit, Edit::Block { .. }) {
            panic!("{}", UNRESOLVED_BLOCK_INTERNAL);
        }
    }

    let mut file_lines: Vec<String> = text.split('\n').map(ToString::to_string).collect();
    let mut line_origins: Vec<LineOrigin> = (0..file_lines.len())
        .map(|_| LineOrigin::Original)
        .collect();

    let mut first_changed_line: Option<u32> = None;
    let track_first_changed = |fcl: &mut Option<u32>, line: u32| {
        if fcl.is_none() || line < fcl.unwrap() {
            *fcl = Some(line);
        }
    };

    let target_edits: Vec<Edit> = edits
        .iter()
        .enumerate()
        .map(|(i, e)| clone_applied_edit(e, u32::try_from(i).unwrap()))
        .collect();
    if let Err(msg) = validate_line_bounds(&target_edits, &file_lines) {
        panic!("{}", msg);
    }
    let (repaired, warnings) = repair_boundary_balance(&target_edits, &file_lines);

    let mut bof_lines: Vec<String> = Vec::new();
    let mut eof_lines: Vec<String> = Vec::new();
    let mut anchor_edits: Vec<IndexedEdit> = Vec::new();
    for (idx, edit) in repaired.iter().enumerate() {
        match edit {
            Edit::Insert {
                cursor: Cursor::Bof,
                text,
                ..
            } => {
                bof_lines.push(text.clone());
            }
            Edit::Insert {
                cursor: Cursor::Eof,
                text,
                ..
            } => {
                eof_lines.push(text.clone());
            }
            _ => {
                anchor_edits.push(IndexedEdit {
                    edit: edit.clone(),
                    idx,
                });
            }
        }
    }

    let by_line = bucket_anchor_edits_by_line(&anchor_edits);
    let mut line_keys: Vec<u32> = by_line.keys().copied().collect();
    line_keys.sort_by(|a, b| b.cmp(a));

    for line in line_keys {
        let bucket = match by_line.get(&line) {
            Some(b) => b.clone(),
            None => continue,
        };
        let mut bucket = bucket;
        bucket.sort_by_key(|a| a.idx);

        let idx = (line - 1) as usize;
        let current_line = file_lines.get(idx).cloned().unwrap_or_default();
        let mut before_insert_lines: Vec<String> = Vec::new();
        let mut after_insert_lines: Vec<String> = Vec::new();
        let mut replacement_lines: Vec<String> = Vec::new();
        let mut delete_line = false;

        for entry in bucket {
            if is_replacement_insert(&entry.edit) {
                if let Edit::Insert { text, .. } = &entry.edit {
                    replacement_lines.push(text.clone());
                }
            } else if matches!(
                &entry.edit,
                Edit::Insert {
                    cursor: Cursor::AfterAnchor(_),
                    ..
                }
            ) {
                if let Edit::Insert { text, .. } = &entry.edit {
                    after_insert_lines.push(text.clone());
                }
            } else if matches!(&entry.edit, Edit::Insert { .. }) {
                if let Edit::Insert { text, .. } = &entry.edit {
                    before_insert_lines.push(text.clone());
                }
            } else if matches!(&entry.edit, Edit::Delete { .. }) {
                delete_line = true;
            }
        }

        if before_insert_lines.is_empty()
            && replacement_lines.is_empty()
            && after_insert_lines.is_empty()
            && !delete_line
        {
            continue;
        }

        let replacement: Vec<String> = if delete_line {
            [
                before_insert_lines.as_slice(),
                replacement_lines.as_slice(),
                after_insert_lines.as_slice(),
            ]
            .concat()
        } else {
            [
                before_insert_lines.as_slice(),
                replacement_lines.as_slice(),
                &[current_line],
                after_insert_lines.as_slice(),
            ]
            .concat()
        };
        let mut origins: Vec<LineOrigin> = Vec::new();
        for _ in 0..before_insert_lines.len() {
            origins.push(LineOrigin::Insert);
        }
        for _ in 0..replacement_lines.len() {
            origins.push(if delete_line {
                LineOrigin::Replacement
            } else {
                LineOrigin::Insert
            });
        }
        if !delete_line {
            origins.push(line_origins[idx]);
        }
        for _ in 0..after_insert_lines.len() {
            origins.push(LineOrigin::Insert);
        }

        file_lines.splice(idx..=idx, replacement);
        line_origins.splice(idx..=idx, origins);
        track_first_changed(&mut first_changed_line, line);
    }

    if !bof_lines.is_empty() {
        insert_at_start(&mut file_lines, &mut line_origins, &bof_lines);
        track_first_changed(&mut first_changed_line, 1);
    }
    let eof_changed_line = insert_at_end(&mut file_lines, &mut line_origins, &eof_lines);
    if let Some(l) = eof_changed_line {
        track_first_changed(&mut first_changed_line, l);
    }

    ApplyResult {
        text: file_lines.join("\n"),
        first_changed_line,
        warnings: if warnings.is_empty() {
            vec![]
        } else {
            warnings
        },
    }
}
