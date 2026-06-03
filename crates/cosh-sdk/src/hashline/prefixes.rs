//! When a hashline payload is authored against `read`/`search` output, each
//! line is prefixed with either a hashline-mode line number (`123:`) or, for
//! diff-style echoes, a leading `+`. These helpers detect that and recover
//! the raw text. Two strip modes are exposed:
//!
//! - [`strip_new_line_prefixes`] — opportunistic: strips when the input
//!   clearly carries hashline or diff prefixes, leaves it alone otherwise.
//! - [`strip_hashline_prefixes`] — strict: only strips when every non-empty
//!   content line is hashline-prefixed.
//!
//! These run *before* the tokenizer; they exist because hashline mode is the
//! common case for echoed file content, and erroneously echoed prefixes will
//! otherwise turn every content line into a (malformed) op.
use regex::Regex;
use std::sync::LazyLock;

static HL_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:>>>|>>)?\s*(?:[+*-]\s*)?\d+:").unwrap());

static HL_PREFIX_PLUS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:>>>|>>)?\s*\+\s*\d+:").unwrap());

static HL_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*¶\S+#[0-9a-fA-F]{3}\s*$").unwrap());

// No regex for diff-plus — uses `is_diff_plus_line` directly (rust/regex does
// not support lookahead, and `^\+` alone would match `+++` lines from merges).
static READ_TRUNCATION_NOTICE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\[(?:Showing lines \d+-\d+ of \d+|\d+ more lines? in (?:file|\S+))\b.*\bUse :L?\d+",
    )
    .unwrap()
});

fn is_diff_plus_line(line: &str) -> bool {
    line.starts_with('+') && line.as_bytes().get(1).copied().is_none_or(|b| b != b'+')
}

fn strip_leading_hashline_prefixes(line: &str) -> String {
    let mut result = line.to_string();
    loop {
        let previous = result.clone();
        result = HL_PREFIX_RE.replace(&result, "").to_string();
        if result == previous {
            break;
        }
    }
    result
}

struct LinePrefixStats {
    non_empty: usize,
    header_count: usize,
    hash_prefix_count: usize,
    diff_plus_hash_prefix_count: usize,
    diff_plus_count: usize,
    truncation_notice_count: usize,
}

fn collect_line_prefix_stats(lines: &[String]) -> LinePrefixStats {
    let mut stats = LinePrefixStats {
        non_empty: 0,
        header_count: 0,
        hash_prefix_count: 0,
        diff_plus_hash_prefix_count: 0,
        diff_plus_count: 0,
        truncation_notice_count: 0,
    };

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if READ_TRUNCATION_NOTICE_RE.is_match(line) {
            stats.truncation_notice_count += 1;
            continue;
        }
        if HL_HEADER_RE.is_match(line) {
            stats.non_empty += 1;
            stats.header_count += 1;
            continue;
        }
        stats.non_empty += 1;
        if HL_PREFIX_RE.is_match(line) {
            stats.hash_prefix_count += 1;
        }
        if HL_PREFIX_PLUS_RE.is_match(line) {
            stats.diff_plus_hash_prefix_count += 1;
        }
        if is_diff_plus_line(line) {
            stats.diff_plus_count += 1;
        }
    }

    stats
}

/// Strip whichever prefix scheme the lines appear to be carrying:
/// - hashline line-number prefixes (`123:`) when every content line has one
/// - leading `+` (diff style) when at least half the lines have one
/// - mixed `+<n>:` form when present
///
/// Returns the lines untouched if no scheme is recognized.
pub fn strip_new_line_prefixes(lines: &[String]) -> Vec<String> {
    let stats = collect_line_prefix_stats(lines);
    if stats.non_empty == 0 {
        return lines.to_vec();
    }

    let content_line_count = stats.non_empty - stats.header_count;
    let strip_hash = content_line_count > 0 && stats.hash_prefix_count == content_line_count;
    let strip_plus = !strip_hash
        && stats.diff_plus_hash_prefix_count == 0
        && stats.diff_plus_count > 0
        && stats.diff_plus_count as f64 >= stats.non_empty as f64 * 0.5;

    if !strip_hash && !strip_plus && stats.diff_plus_hash_prefix_count == 0 {
        return lines.to_vec();
    }

    lines
        .iter()
        .filter(|line| {
            !(READ_TRUNCATION_NOTICE_RE.is_match(line) || strip_hash && HL_HEADER_RE.is_match(line))
        })
        .map(|line| {
            if strip_hash {
                strip_leading_hashline_prefixes(line)
            } else if strip_plus {
                if is_diff_plus_line(line) {
                    line[1..].to_string()
                } else {
                    line.clone()
                }
            } else if stats.diff_plus_hash_prefix_count > 0 && HL_PREFIX_PLUS_RE.is_match(line) {
                HL_PREFIX_RE.replace(line, "").to_string()
            } else {
                line.clone()
            }
        })
        .collect()
}

/// Strict variant: strip hashline prefixes only when every content line is
/// hashline-prefixed. Returns the lines unchanged otherwise.
pub fn strip_hashline_prefixes(lines: &[String]) -> Vec<String> {
    let stats = collect_line_prefix_stats(lines);
    if stats.non_empty == 0 {
        return lines.to_vec();
    }
    let content_line_count = stats.non_empty - stats.header_count;
    if content_line_count == 0 || stats.hash_prefix_count != content_line_count {
        return lines.to_vec();
    }
    lines
        .iter()
        .filter(|line| !READ_TRUNCATION_NOTICE_RE.is_match(line) && !HL_HEADER_RE.is_match(line))
        .map(|line| strip_leading_hashline_prefixes(line))
        .collect()
}

/// Normalize line payloads by stripping read/search line prefixes. A single
/// multiline string is split on `\n`.
pub fn hashline_parse_text(text: &str) -> Vec<String> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<String> = trimmed
        .replace('\r', "")
        .split('\n')
        .map(String::from)
        .collect();
    strip_new_line_prefixes(&lines)
}
