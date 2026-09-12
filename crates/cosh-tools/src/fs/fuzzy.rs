//! Fuzzy closest-match search — a behavior port of oh-my-pi's
//! `crates/pi-edit/src/fuzzy.rs` (which itself ports the TypeScript
//! `packages/coding-agent/src/edit/fuzzy.ts`).
//!
//! # Why this exists here
//!
//! The `edits` (content replace) engine matches `old_string` EXACTLY and
//! uniquely — a wrong `old_string` is rejected, never guessed away. But a
//! rejection that only says "not found" forces the model into an expensive
//! recovery loop (re-read the file, re-copy the text). This module gives the
//! rejection a memory: locate the CLOSEST near-match and describe it
//! (similarity, line, `-`/`+` of the first differing line) so the model can
//! re-issue with the correct text in a single round trip.
//!
//! # The one rule that makes fuzzy safe here
//!
//! The fuzzy result is **advisory only** — it never selects what gets
//! replaced. oh-my-pi uses the same machinery to auto-substitute a
//! high-confidence fuzzy hit; we deliberately do not. The worst case of a
//! diagnostic mismatch is a slightly wrong hint; the worst case of a
//! substitution mismatch is silently editing the wrong lines.
//!
//! # Behavioral fidelity
//!
//! Outputs are pinned byte-for-byte against oh-my-pi by ported tests
//! (`fuzzy_equivalence.rs`) and by a differential dump example; a criterion
//! benchmark (`benches/fuzzy.rs`) guards the miss-path cost on
//! representative shapes. Where the TS original used UTF-16
//! code units, the Rust port compares Unicode scalar values (astral
//! characters count as one element) — a deliberate upstream decision kept
//! here so similarity scores agree with the Rust reference.
//!
//! The `js_*` helpers reproduce JavaScript's `String.prototype.trim`
//! whitespace set. That is behavioral fidelity, not a TS habit: models are
//! trained on JS-based editing tools, so "trimmed" must mean what those
//! tools meant.

/// Default similarity threshold for fuzzy matching.
pub const DEFAULT_FUZZY_THRESHOLD: f64 = 0.95;
/// Fallback threshold for line-based matching without indentation depth.
pub const FALLBACK_THRESHOLD: f64 = 0.8;
/// Number of surrounding lines in occurrence previews.
pub const OCCURRENCE_PREVIEW_CONTEXT: usize = 5;
/// Maximum displayed line length in occurrence previews.
pub const OCCURRENCE_PREVIEW_MAX_LEN: usize = 80;
/// Occurrence previews and indices recorded before truncation.
pub const MAX_RECORDED_MATCHES: usize = 5;
/// A fuzzy hit at or above this confidence can dominate weaker siblings.
pub const DOMINANT_FUZZY_MIN_CONFIDENCE: f64 = 0.97;
/// Upper bound on approximate Levenshtein DP cells spent by one
/// [`find_match`] call. The diagnostic scan must never become a latency
/// hazard: a single-line snapshot (minified JS/JSON) times a long
/// single-line `old_string` would otherwise run one O(n·m) DP with ~10¹⁰
/// cells. When the estimate exceeds the budget, the fuzzy pass is skipped
/// entirely (the outcome degrades to "no closest match" — a weaker message,
/// never a wrong one).
pub const MAX_FUZZY_WORK: u64 = 50_000_000;
/// Minimum confidence gap for a dominant fuzzy hit.
pub const DOMINANT_FUZZY_DELTA: f64 = 0.08;

/// A located block of text.
#[derive(Debug, Clone, PartialEq)]
pub struct FuzzyMatch {
    pub actual_text: String,
    /// Byte offset of the match start in the searched content.
    pub start_index: usize,
    /// 1-indexed line of the match start.
    pub start_line: u32,
    pub confidence: f64,
}

/// Outcome of [`find_match`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchOutcome {
    pub matched: Option<FuzzyMatch>,
    pub closest: Option<FuzzyMatch>,
    pub occurrences: Option<usize>,
    pub occurrence_lines: Option<Vec<u32>>,
    pub occurrence_previews: Option<Vec<String>>,
    pub fuzzy_matches: Option<usize>,
    pub dominant_fuzzy: Option<bool>,
}

/// A byte range excluded from matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcludedRange {
    pub start_index: usize,
    pub end_index: usize,
}

/// Knobs for [`find_match`].
#[derive(Debug, Clone, Default)]
pub struct FindMatchOptions<'a> {
    pub allow_fuzzy: bool,
    /// Defaults to [`DEFAULT_FUZZY_THRESHOLD`].
    pub threshold: Option<f64>,
    pub excluded_ranges: &'a [ExcludedRange],
}

/// JavaScript's `\s` / `String.prototype.trim` whitespace set (`WhiteSpace` +
/// `LineTerminator` productions).
pub const fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trim` equivalent.
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// Length in UTF-16 code units — JS `string.length`. Preview truncation uses
/// it so astral characters never get cut in half.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// True when `line` has any non-whitespace content.
pub fn is_non_empty_line(line: &str) -> bool {
    !js_trim(line).is_empty()
}

/// Count leading space/tab characters.
pub fn count_leading_whitespace(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// Normalize a line for fuzzy comparison: trim, fold quotes/dashes to ASCII,
/// collapse runs of spaces and tabs. Pure per line — the result never
/// depends on neighboring lines, which is what makes the window pre-compute
/// in the applier's hot loop safe.
pub fn normalize_for_fuzzy(line: &str) -> String {
    let trimmed = js_trim(line);
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut in_space = false;
    for ch in trimmed.chars() {
        let mapped = match ch {
            '"' | '\u{201E}' | '\u{201F}' | '\u{AB}' | '\u{BB}' => '"',
            '\'' | '\u{201A}' | '\u{201B}' | '`' | '\u{B4}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            ' ' | '\t' => ' ',
            other => other,
        };
        if mapped == ' ' {
            if in_space {
                continue;
            }
            in_space = true;
        } else {
            in_space = false;
        }
        out.push(mapped);
    }
    out
}

#[allow(clippy::suspicious_operation_groupings)]
fn levenshtein_chars(a: &[char], b: &[char]) -> usize {
    if a == b {
        return 0;
    }
    // Trim the shared prefix/suffix first: most comparisons differ in a few
    // characters, so the quadratic DP runs on a much smaller slice.
    let mut start = 0;
    let shared_limit = a.len().min(b.len());
    while start < shared_limit && a[start] == b[start] {
        start += 1;
    }
    let mut a_end = a.len();
    let mut b_end = b.len();
    while a_end > start && b_end > start && a[a_end - 1] == b[b_end - 1] {
        a_end -= 1;
        b_end -= 1;
    }
    let mut longer = &a[start..a_end];
    let mut shorter = &b[start..b_end];
    if longer.is_empty() {
        return shorter.len();
    }
    if shorter.is_empty() {
        return longer.len();
    }
    if shorter.len() > longer.len() {
        std::mem::swap(&mut longer, &mut shorter);
    }

    // Single-row DP: `row` holds the previous row; `diagonal` carries the
    // cell above-left across the column step.
    let mut row: Vec<usize> = (0..=shorter.len()).collect();
    for (line, &a_char) in longer.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = line + 1;
        for (column, &b_char) in shorter.iter().enumerate() {
            let cell = column + 1;
            let above = row[cell];
            row[cell] = if a_char == b_char {
                diagonal
            } else {
                (above + 1).min(row[cell - 1] + 1).min(diagonal + 1)
            };
            diagonal = above;
        }
    }
    row[shorter.len()]
}

/// Levenshtein edit distance over Unicode scalar values.
///
/// The TypeScript source used UTF-16 code units. Rust deliberately uses
/// Unicode scalar values, so astral characters count as one element.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    levenshtein_chars(&a_chars, &b_chars)
}

/// Similarity in `[0, 1]`: `1 - distance / max_len`.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let max_len = a_chars.len().max(b_chars.len());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - levenshtein_chars(&a_chars, &b_chars) as f64 / max_len as f64
}

fn format_preview_window(lines: &[&str], center_index: usize) -> String {
    let start = center_index.saturating_sub(OCCURRENCE_PREVIEW_CONTEXT);
    let end = lines
        .len()
        .min(center_index + OCCURRENCE_PREVIEW_CONTEXT + 1);
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let truncated = if utf16_len(line) > OCCURRENCE_PREVIEW_MAX_LEN {
                let mut units = 0;
                let mut text = String::new();
                for ch in line.chars() {
                    let width = ch.len_utf16();
                    if units + width > OCCURRENCE_PREVIEW_MAX_LEN - 1 {
                        break;
                    }
                    text.push(ch);
                    units += width;
                }
                text.push('…');
                text
            } else {
                (*line).to_owned()
            };
            format!("  {} | {truncated}", start + offset + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn overlaps_excluded(start: usize, end: usize, ranges: &[ExcludedRange]) -> bool {
    ranges
        .iter()
        .any(|range| start < range.end_index && end > range.start_index)
}

fn find_exact_match_outcome(
    content: &str,
    target: &str,
    excluded_ranges: &[ExcludedRange],
) -> Option<MatchOutcome> {
    let mut first_index = None;
    let mut occurrences = 0;
    let mut recorded_indices = Vec::new();
    let mut search_start = 0;
    while search_start <= content.len().saturating_sub(target.len()) {
        let Some(relative) = content[search_start..].find(target) else {
            break;
        };
        let index = search_start + relative;
        let end_index = index + target.len();
        if !overlaps_excluded(index, end_index, excluded_ranges) {
            first_index.get_or_insert(index);
            occurrences += 1;
            if recorded_indices.len() < MAX_RECORDED_MATCHES {
                recorded_indices.push(index);
            }
        }
        search_start = end_index;
    }
    let first_index = first_index?;
    if occurrences > 1 {
        let content_lines: Vec<&str> = content.split('\n').collect();
        let mut occurrence_lines = Vec::with_capacity(recorded_indices.len());
        let mut occurrence_previews = Vec::with_capacity(recorded_indices.len());
        for index in recorded_indices {
            let line_number = content[..index]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            occurrence_lines.push(line_number as u32);
            occurrence_previews.push(format_preview_window(&content_lines, line_number - 1));
        }
        return Some(MatchOutcome {
            occurrences: Some(occurrences),
            occurrence_lines: Some(occurrence_lines),
            occurrence_previews: Some(occurrence_previews),
            ..MatchOutcome::default()
        });
    }
    let start_line = content[..first_index]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
        + 1;
    Some(MatchOutcome {
        matched: Some(FuzzyMatch {
            actual_text: target.to_owned(),
            start_index: first_index,
            start_line,
            confidence: 1.0,
        }),
        ..MatchOutcome::default()
    })
}

/// Relative indentation depth of each line, measured in the smallest
/// non-zero indent step present in the block. Blank lines have depth 0.
/// The depth prefixes are what let the fuzzy score tell "same code at a
/// different nesting" from "different code".
fn relative_indent_depths(lines: &[&str]) -> Vec<usize> {
    let indents: Vec<usize> = lines
        .iter()
        .map(|line| count_leading_whitespace(line))
        .collect();
    let non_empty_indents: Vec<usize> = lines
        .iter()
        .zip(&indents)
        .filter_map(|(line, indent)| is_non_empty_line(line).then_some(*indent))
        .collect();
    let min_indent = non_empty_indents.iter().copied().min().unwrap_or(0);
    let indent_unit = non_empty_indents
        .iter()
        .filter_map(|indent| indent.checked_sub(min_indent))
        .filter(|step| *step > 0)
        .min()
        .unwrap_or(1);
    lines
        .iter()
        .zip(indents)
        .map(|(line, indent)| {
            if !is_non_empty_line(line) || indent_unit == 0 {
                0
            } else {
                ((indent - min_indent) as f64 / indent_unit as f64).round() as usize
            }
        })
        .collect()
}

/// Build the comparison form of one line: a depth (or `|`) prefix followed by
/// the whitespace-folded content. The prefix `|` form (no depth) is the
/// fallback pass used when indentation is inconsistent.
fn normalize_line(depth: Option<usize>, trimmed_normalized: &str) -> String {
    match depth {
        Some(depth) => format!("{depth}|{trimmed_normalized}"),
        None => format!("|{trimmed_normalized}"),
    }
}

/// Byte offset of the start of each line (`content_lines` must be the split
/// of `content` on `'\n'`).
fn line_offsets(lines: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for (index, line) in lines.iter().enumerate() {
        offsets.push(offset);
        offset += line.len() + usize::from(index + 1 < lines.len());
    }
    offsets
}

#[derive(Debug)]
struct BestFuzzyMatch {
    best: Option<FuzzyMatch>,
    above_threshold_count: usize,
    second_best_score: f64,
}

/// One sliding-window pass over the content. Each window of
/// `content_lines.len() == target_lines.len()` lines is scored as the MEAN
/// per-line similarity of the normalized forms; windows at/above the
/// threshold are counted, and the best/second-best scores feed the
/// dominant-match rule.
///
/// Unlike upstream, the whitespace-folded form of every content line is
/// computed ONCE (via `normalized_lines`) instead of once per window — the
/// strings are pure per line, so the results are identical while the hot
/// loop stops re-running the character-level fold.
fn best_fuzzy_match_core(
    content_lines: &[&str],
    target_lines: &[&str],
    offsets: &[usize],
    normalized_lines: &[String],
    threshold: f64,
    include_depth: bool,
    excluded_ranges: &[ExcludedRange],
) -> BestFuzzyMatch {
    let target_normalized = normalize_lines(target_lines, include_depth, None);
    let mut best = None;
    let mut best_score = -1.0;
    let mut second_best_score = -1.0;
    let mut above_threshold_count = 0;
    for start in 0..=content_lines.len() - target_lines.len() {
        let start_index = offsets[start];
        let end_line = start + target_lines.len() - 1;
        let end_index = (offsets[end_line] + content_lines[end_line].len()).max(start_index + 1);
        if overlaps_excluded(start_index, end_index, excluded_ranges) {
            continue;
        }
        let window_normalized = normalize_lines(
            &content_lines[start..start + target_lines.len()],
            include_depth,
            Some(&normalized_lines[start..start + target_lines.len()]),
        );
        let score = target_normalized
            .iter()
            .zip(&window_normalized)
            .map(|(target, actual)| similarity(target, actual))
            .sum::<f64>()
            / target_lines.len() as f64;
        if score >= threshold {
            above_threshold_count += 1;
        }
        if score > best_score {
            second_best_score = best_score;
            best_score = score;
            best = Some(FuzzyMatch {
                actual_text: content_lines[start..start + target_lines.len()].join("\n"),
                start_index,
                start_line: start as u32 + 1,
                confidence: score,
            });
        } else if score > second_best_score {
            second_best_score = score;
        }
    }
    BestFuzzyMatch {
        best,
        above_threshold_count,
        second_best_score,
    }
}

/// Normalize every line of a block. `precomputed` (content side only) may
/// carry the per-line `normalize_for_fuzzy(js_trim(line))` results — the
/// depth prefix still depends on the whole window, so only the prefix is
/// built per call.
fn normalize_lines(
    lines: &[&str],
    include_depth: bool,
    precomputed: Option<&[String]>,
) -> Vec<String> {
    let depths = include_depth.then(|| relative_indent_depths(lines));
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let trimmed_normalized = match precomputed {
                Some(precomputed) => precomputed[index].clone(),
                None => normalize_for_fuzzy(js_trim(line)),
            };
            let depth = depths.as_ref().map(|values| values[index]);
            normalize_line(depth, &trimmed_normalized)
        })
        .collect()
}

/// Best fuzzy location of `target` inside `content` (whole-block windows).
///
/// Two passes: with indentation depths first; if the best score is stuck in
/// the `[FALLBACK_THRESHOLD, threshold)` band, a second pass WITHOUT depth
/// runs (indentation-heavy drift, e.g. a block pasted at a different
/// nesting) and wins only when it scores strictly higher.
fn best_fuzzy_match(
    content: &str,
    target: &str,
    threshold: f64,
    excluded_ranges: &[ExcludedRange],
) -> BestFuzzyMatch {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let target_lines: Vec<&str> = target.split('\n').collect();
    if target.is_empty() || target_lines.len() > content_lines.len() {
        return BestFuzzyMatch {
            best: None,
            above_threshold_count: 0,
            second_best_score: 0.0,
        };
    }
    // Work estimate: each window scores every target line against its
    // counterpart, costing ~len(search line) × len(target line) DP cells.
    // The sum telescopes to content length × the longest target line.
    let max_target_line_len = target_lines
        .iter()
        .map(|line| line.len())
        .max()
        .unwrap_or(0);
    let work = (content.len() as u64) * (max_target_line_len as u64);
    if work > MAX_FUZZY_WORK {
        return BestFuzzyMatch {
            best: None,
            above_threshold_count: 0,
            second_best_score: 0.0,
        };
    }
    let offsets = line_offsets(&content_lines);
    // The per-line fold is window-independent: compute the expensive part of
    // the hot loop exactly once for the whole file.
    let normalized_lines: Vec<String> = content_lines
        .iter()
        .map(|line| normalize_for_fuzzy(js_trim(line)))
        .collect();
    let mut result = best_fuzzy_match_core(
        &content_lines,
        &target_lines,
        &offsets,
        &normalized_lines,
        threshold,
        true,
        excluded_ranges,
    );
    if result
        .best
        .as_ref()
        .is_some_and(|best| best.confidence < threshold && best.confidence >= FALLBACK_THRESHOLD)
    {
        let without_depth = best_fuzzy_match_core(
            &content_lines,
            &target_lines,
            &offsets,
            &normalized_lines,
            threshold,
            false,
            excluded_ranges,
        );
        if without_depth.best.as_ref().is_some_and(|candidate| {
            result
                .best
                .as_ref()
                .is_none_or(|best| candidate.confidence > best.confidence)
        }) {
            result = without_depth;
        }
    }
    result
}

/// Locate `target` in `content`: exact first (non-overlapping scan;
/// multiple occurrences are reported, never auto-chosen), then fuzzy when
/// allowed. Excluded ranges are invisible to both passes.
///
/// In this crate the outcome is consumed by the content-replace engine's
/// REJECTION diagnostics only — `matched` here never means "substitute it".
pub fn find_match(content: &str, target: &str, options: &FindMatchOptions<'_>) -> MatchOutcome {
    if target.is_empty() {
        return MatchOutcome::default();
    }
    if let Some(exact) = find_exact_match_outcome(content, target, options.excluded_ranges) {
        return exact;
    }
    let threshold = options.threshold.unwrap_or(DEFAULT_FUZZY_THRESHOLD);
    let result = best_fuzzy_match(content, target, threshold, options.excluded_ranges);
    let Some(best) = result.best else {
        return MatchOutcome::default();
    };
    if options.allow_fuzzy && best.confidence >= threshold {
        if result.above_threshold_count == 1 {
            return MatchOutcome {
                matched: Some(best.clone()),
                closest: Some(best),
                ..MatchOutcome::default()
            };
        }
        // Several windows scored at/above the threshold: only a DOMINANT
        // best (≥ 0.97 and ≥ 0.08 ahead of the runner-up) can be singled
        // out; otherwise the ambiguity is reported, never resolved.
        if result.above_threshold_count > 1
            && best.confidence >= DOMINANT_FUZZY_MIN_CONFIDENCE
            && best.confidence - result.second_best_score >= DOMINANT_FUZZY_DELTA
        {
            return MatchOutcome {
                matched: Some(best.clone()),
                closest: Some(best),
                fuzzy_matches: Some(result.above_threshold_count),
                dominant_fuzzy: Some(true),
                ..MatchOutcome::default()
            };
        }
    }
    MatchOutcome {
        closest: Some(best),
        fuzzy_matches: Some(result.above_threshold_count),
        ..MatchOutcome::default()
    }
}

pub fn first_different_line<'a>(
    old_lines: &'a [&str],
    new_lines: &'a [&str],
) -> (&'a str, &'a str) {
    for index in 0..old_lines.len().max(new_lines.len()) {
        let old = old_lines.get(index).copied().unwrap_or("");
        let new = new_lines.get(index).copied().unwrap_or("");
        if old != new {
            return (old, new);
        }
    }
    (
        old_lines.first().copied().unwrap_or(""),
        new_lines.first().copied().unwrap_or(""),
    )
}

/// Format the no-match rejection, byte-for-byte compatible with upstream's
/// `EditMatchError.formatMessage` (pinned by ported tests): heading, then —
/// when a closest candidate exists — its similarity, line, and the first
/// differing line as `-`/`+`, then the discriminating hint.
pub fn format_match_error(
    path: &str,
    search_text: &str,
    closest: Option<&FuzzyMatch>,
    allow_fuzzy: bool,
    threshold: f64,
    fuzzy_matches: Option<usize>,
) -> String {
    let Some(closest) = closest else {
        return if allow_fuzzy {
            format!("Could not find a close enough match in {path}.")
        } else {
            format!(
                "Could not find the exact text in {path}. The old text must match exactly including \
                 all whitespace and newlines."
            )
        };
    };
    let similarity_percent = (closest.confidence * 100.0).round() as i64;
    let threshold_percent = (threshold * 100.0).round() as i64;
    let search_lines: Vec<&str> = search_text.split('\n').collect();
    let actual_lines: Vec<&str> = closest.actual_text.split('\n').collect();
    let (old_line, new_line) = first_different_line(&search_lines, &actual_lines);
    let hint = if allow_fuzzy {
        if fuzzy_matches.is_some_and(|count| count > 1) {
            format!(
                "Found {} high-confidence matches. Provide more context to make it unique.",
                fuzzy_matches.unwrap_or(0)
            )
        } else {
            format!("Closest match was below the {threshold_percent}% similarity threshold.")
        }
    } else {
        "Fuzzy matching is disabled. Enable 'Edit fuzzy match' in settings to accept high-confidence \
         matches."
            .to_owned()
    };
    let heading = if allow_fuzzy {
        format!("Could not find a close enough match in {path}.")
    } else {
        format!("Could not find the exact text in {path}.")
    };
    format!(
        "{heading}\n\nClosest match ({similarity_percent}% similar) at line {}:\n  - {old_line}\n  \
         + {new_line}\n{hint}",
        closest.start_line
    )
}

/// Format the ambiguous-exact-match rejection, byte-for-byte compatible with
/// upstream: occurrence count, bounded preview windows around each recorded
/// occurrence, and the disambiguation instruction.
pub fn format_occurrence_error(path: &str, outcome: &MatchOutcome) -> String {
    let occurrences = outcome.occurrences.unwrap_or(0);
    let previews = outcome
        .occurrence_previews
        .as_ref()
        .map_or_else(String::new, |items| items.join("\n\n"));
    let more = if occurrences > MAX_RECORDED_MATCHES {
        format!(" (showing first {MAX_RECORDED_MATCHES} of {occurrences})")
    } else {
        String::new()
    };
    format!(
        "Found {occurrences} occurrences in {path}{more}:\n\n{previews}\n\nAdd more context lines \
         to disambiguate."
    )
}
