//! Hashline format primitives: sigils, separators, regex fragments, and
//! display helpers. These are the single source of truth for the parser, the
//! tokenizer, the prompt, and the formal grammar.
use super::types::Cursor;
use xxhash_rust::xxh32::xxh32;

/// File-section header prefix: `¶path#hash`.
pub const HL_FILE_PREFIX: &str = "¶";

/// Payload sigil for literal body rows.
pub const HL_PAYLOAD_REPLACE: &str = "+";

/// Hunk-header keyword for concrete line replacement.
pub const HL_REPLACE_KEYWORD: &str = "replace";
/// Hunk-header sub-keyword: `replace block N:` resolves N to a tree-sitter block range.
pub const HL_BLOCK_KEYWORD: &str = "block";
/// Hunk-header keyword for concrete line deletion.
pub const HL_DELETE_KEYWORD: &str = "delete";
/// Hunk-header keyword for insertion operations.
pub const HL_INSERT_KEYWORD: &str = "insert";
/// Insert position keyword for inserting before a concrete line.
pub const HL_INSERT_BEFORE: &str = "before";
/// Insert position keyword for inserting after a concrete line.
pub const HL_INSERT_AFTER: &str = "after";
/// Insert position keyword for inserting at the start of the file.
pub const HL_INSERT_HEAD: &str = "head";
/// Insert position keyword for inserting at the end of the file.
pub const HL_INSERT_TAIL: &str = "tail";
/// Hunk-header terminator for body-bearing operations.
pub const HL_HEADER_COLON: &str = ":";

/// Separator between a hashline file path and its opaque snapshot tag.
pub const HL_FILE_HASH_SEP: &str = "#";

/// Separator between two line numbers in a range, e.g. `5..10`.
pub const HL_RANGE_SEP: &str = "..";

/// Separator between a line number and displayed line content in hashline mode.
pub const HL_LINE_BODY_SEP: &str = "| ";

fn regex_escape(s: &str) -> String {
    let mut escaped = String::new();
    for c in s.chars() {
        match c {
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Bare positive line-number Lid (no decorations, no captures, no anchors).
pub const HL_LINE_RE_RAW: &str = r"[1-9]\d*";

/// Capture-group form of [`HL_LINE_RE_RAW`].
pub const HL_LINE_CAPTURE_RE_RAW: &str = r"([1-9]\d*)";

/// Number of hex characters in a content-derived file-hash tag.
pub const HL_FILE_HASH_LENGTH: usize = 4;

/// Canonical uppercase hexadecimal content-hash tag carried by a hashline section header.
#[must_use]
pub fn hl_file_hash_re_raw() -> String {
    format!("[0-9A-F]{{{HL_FILE_HASH_LENGTH}}}")
}

/// Capture-group form of [`hl_file_hash_re_raw`].
#[must_use]
pub fn hl_file_hash_capture_re_raw() -> String {
    format!("([0-9A-F]{{{HL_FILE_HASH_LENGTH}}})")
}

/// Regex-escaped form of [`HL_LINE_BODY_SEP`], safe for embedding inside a regex.
#[must_use]
pub fn hl_line_body_sep_re_raw() -> String {
    regex_escape(HL_LINE_BODY_SEP)
}

/// Representative file-hash tags for use in user-facing error messages and
/// prompt examples.
pub const HL_FILE_HASH_EXAMPLES: [&str; 3] = ["1A2B", "3C4D", "9F3E"];

/// Normalize text before hashing: trim trailing `[ \t\r]` from every line (and
/// the final line) in a single pass so CRLF endings and display-trimmed lines
/// do not invalidate a tag.
fn normalize_file_hash_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let has_newline = line.ends_with('\n');
        let content = if has_newline {
            &line[..line.len() - 1]
        } else {
            line
        };
        let trimmed = content.trim_end_matches([' ', '\t', '\r']);
        result.push_str(trimmed);
        if has_newline {
            result.push('\n');
        }
    }
    result
}

/// Compute the content-derived hash tag carried by a hashline section header.
/// The tag is a 4-hex fingerprint of the whole file's normalized text: any read
/// of byte-identical content mints the same tag, and a follow-up edit anchored
/// at any line validates whenever the live file still hashes to it.
#[must_use]
pub fn compute_file_hash(text: &str) -> String {
    let normalized = normalize_file_hash_text(text);
    let hash = xxh32(normalized.as_bytes(), 0);
    let low16 = hash & 0xffff;
    format!("{low16:04X}")
}

/// Format a concrete replacement hunk header.
#[must_use]
pub fn format_replace_header(start: u32, end: u32) -> String {
    format!("{HL_REPLACE_KEYWORD} {start}{HL_RANGE_SEP}{end}{HL_HEADER_COLON}")
}

/// Format a concrete deletion hunk header.
#[must_use]
pub fn format_delete_header(start: u32, end: u32) -> String {
    if start == end {
        format!("{HL_DELETE_KEYWORD} {start}")
    } else {
        format!("{HL_DELETE_KEYWORD} {start}{HL_RANGE_SEP}{end}")
    }
}

/// Format an insertion hunk header for a cursor position.
#[must_use]
pub fn format_insert_header(cursor: &Cursor) -> String {
    match cursor {
        Cursor::BeforeAnchor(anchor) => {
            format!(
                "{} {} {}{}",
                HL_INSERT_KEYWORD, HL_INSERT_BEFORE, anchor.line, HL_HEADER_COLON
            )
        }
        Cursor::AfterAnchor(anchor) => {
            format!(
                "{} {} {}{}",
                HL_INSERT_KEYWORD, HL_INSERT_AFTER, anchor.line, HL_HEADER_COLON
            )
        }
        Cursor::Bof => {
            format!("{HL_INSERT_KEYWORD} {HL_INSERT_HEAD}{HL_HEADER_COLON}")
        }
        Cursor::Eof => {
            format!("{HL_INSERT_KEYWORD} {HL_INSERT_TAIL}{HL_HEADER_COLON}")
        }
    }
}

/// Format a comma-separated list of example anchors with an optional line-number
/// prefix, quoted for inclusion in error messages: `"160", "42", "7"`.
#[must_use]
pub fn describe_anchor_examples(line_prefix: Option<&str>) -> String {
    let examples: Vec<String> = match line_prefix {
        Option::Some(prefix) => {
            let second = if prefix.len() > 1 {
                let trimmed = &prefix[..prefix.len() - 1];
                if trimmed.is_empty() {
                    "42".to_string()
                } else {
                    format!("{trimmed}2")
                }
            } else {
                "42".to_string()
            };
            vec![prefix.to_string(), second, "7".to_string()]
        }
        Option::None => vec!["160".to_string(), "42".to_string(), "7".to_string()],
    };
    examples
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format a hashline section header for a file path and snapshot tag.
#[must_use]
pub fn format_hashline_header(file_path: &str, file_hash: &str) -> String {
    format!("{HL_FILE_PREFIX}{file_path}{HL_FILE_HASH_SEP}{file_hash}")
}

/// Formats a single numbered line as `LINE:TEXT`.
#[must_use]
pub fn format_numbered_line(line_number: u32, line: &str) -> String {
    format!("{line_number}{HL_LINE_BODY_SEP}{line}")
}

/// Format file text with hashline-mode line-number prefixes for display.
///
/// # Panics
///
/// Panics if `start_line` plus the line index overflows a `u32`.
#[must_use]
pub fn format_numbered_lines(text: &str, start_line: u32) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format_numbered_line(start_line + u32::try_from(i).unwrap(), line))
        .collect::<Vec<_>>()
        .join("\n")
}
