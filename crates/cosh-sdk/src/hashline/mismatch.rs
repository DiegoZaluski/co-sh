//! Error type raised when a section's snapshot tag does not match the live file
//! content and recovery is unavailable / has failed.
//!
//! Carries enough context to render a useful diagnostic: the anchored lines
//! plus a couple of lines of surrounding context. The [`MismatchError`]
//! formats this into a message at construction time.
use super::format::{
    HL_FILE_HASH_EXAMPLES, HL_FILE_HASH_SEP, HL_FILE_PREFIX, format_numbered_line,
};
use super::messages::MISMATCH_CONTEXT;
use regex::Regex;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

#[allow(clippy::unwrap_used)]
static LINE_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[>+\-*]*\s*(\d+)(?::.*)?\s*$").unwrap());

/// Format the required-shape diagnostic shown when a line reference is malformed.
#[must_use]
pub fn format_full_anchor_requirement(raw: Option<&str>) -> String {
    let received = raw.map_or_else(String::new, |s| format!(" Received {s:?}."));
    format!(
        "a bare line number from read/search output plus the section header content-hash tag \
         (for example {}{}{}{} and line \"160\"){}",
        HL_FILE_PREFIX, "src/foo.ts", HL_FILE_HASH_SEP, HL_FILE_HASH_EXAMPLES[0], received,
    )
}

/// Parse a decorated bare line-number anchor like `42`, `*42:foo`, ` > 7`.
///
/// # Errors
///
/// Returns an error if the reference is not a valid line number.
pub fn parse_tag(reference: &str) -> Result<u32, String> {
    let captures = LINE_REF_RE.captures(reference).ok_or_else(|| {
        format!(
            "Invalid line reference. Expected {}.",
            format_full_anchor_requirement(Some(reference))
        )
    })?;
    let line: u32 = captures[1].parse().map_err(|_| {
        format!(
            "Line number must be >= 1, got \"{}\" in \"{}\".",
            &captures[1], reference
        )
    })?;
    if line < 1 {
        return Err(format!(
            "Line number must be >= 1, got {line} in \"{reference}\".",
        ));
    }
    Ok(line)
}

#[derive(Debug, Clone)]
pub struct MismatchDetails {
    pub path: Option<String>,
    pub expected_file_hash: String,
    pub actual_file_hash: String,
    pub file_lines: Vec<String>,
    pub anchor_lines: Vec<u32>,
    /// `true` when the section's expected hash resolved to a recorded snapshot
    /// (file content drifted since that snapshot), `false` when no snapshot
    /// was ever recorded for the hash (likely fabricated or carried over from
    /// a prior session). Defaults to `true`.
    pub hash_recognized: bool,
}

impl Default for MismatchDetails {
    fn default() -> Self {
        Self {
            path: None,
            expected_file_hash: String::new(),
            actual_file_hash: String::new(),
            file_lines: Vec::new(),
            anchor_lines: Vec::new(),
            hash_recognized: true,
        }
    }
}

fn get_mismatch_display_lines(anchor_lines: &[u32], file_lines: &[String]) -> Vec<u32> {
    let mut display = BTreeSet::new();
    for &line in anchor_lines {
        if line < 1 || line as usize > file_lines.len() {
            continue;
        }
        let lo = 1.max(line.saturating_sub(MISMATCH_CONTEXT));
        let hi = u32::try_from(file_lines.len())
            .unwrap_or(u32::MAX)
            .min(line + MISMATCH_CONTEXT);
        for line_num in lo..=hi {
            display.insert(line_num);
        }
    }
    display.into_iter().collect()
}

/// Raised when a hashline section's snapshot tag doesn't match the live file's
///
/// content (and recovery, if configured, declined the merge). Carries the
/// file lines plus anchored lines so renderers can produce a richer
/// diagnostic via [`MismatchError::display_message`].
#[derive(Clone)]
pub struct MismatchError {
    pub path: Option<String>,
    pub expected_file_hash: String,
    pub actual_file_hash: String,
    pub file_lines: Vec<String>,
    pub anchor_lines: Vec<u32>,
    pub hash_recognized: bool,
    message: String,
}

impl MismatchError {
    #[must_use]
    pub fn new(details: MismatchDetails) -> Self {
        let message = Self::format_message_inner(&details);
        Self {
            path: details.path,
            expected_file_hash: details.expected_file_hash,
            actual_file_hash: details.actual_file_hash,
            file_lines: details.file_lines,
            anchor_lines: details.anchor_lines,
            hash_recognized: details.hash_recognized,
            message,
        }
    }

    // Replicated to maintain consistency with the original API.
    #[must_use]
    pub fn display_message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn format_message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn format_display_message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn rejection_header(details: &MismatchDetails) -> Vec<String> {
        let path_text = details
            .path
            .as_ref()
            .map_or_else(String::new, |p| format!(" for {p}"));
        if details.hash_recognized {
            vec![
                format!(
                    "Edit rejected{}: file changed between read and edit.",
                    path_text,
                ),
                format!(
                    "Section is bound to {}{}, but the current file hashes to {}{}. \
                     If a prior edit in this session modified this file, copy the \
                     {}{}{}newhash header from that edit's response; otherwise re-read \
                     the file with `read` to refresh the tag before retrying.",
                    HL_FILE_HASH_SEP,
                    details.expected_file_hash,
                    HL_FILE_HASH_SEP,
                    details.actual_file_hash,
                    HL_FILE_PREFIX,
                    "path",
                    HL_FILE_HASH_SEP,
                ),
            ]
        } else {
            vec![
                format!(
                    "Edit rejected{}: hash {}{} is not from this session.",
                    path_text, HL_FILE_HASH_SEP, details.expected_file_hash,
                ),
                format!(
                    "The current file hashes to {}{}. Re-read the file with `read` \
                     to copy a current {}{}{}tag header — never invent the tag and never \
                     reuse one from a prior session.",
                    HL_FILE_HASH_SEP,
                    details.actual_file_hash,
                    HL_FILE_PREFIX,
                    "path",
                    HL_FILE_HASH_SEP,
                ),
            ]
        }
    }

    fn format_message_inner(details: &MismatchDetails) -> String {
        #[allow(clippy::if_not_else)]
        let anchor_set: BTreeSet<u32> = details.anchor_lines.iter().copied().collect();
        let mut lines = Self::rejection_header(details);
        let display_lines = get_mismatch_display_lines(&details.anchor_lines, &details.file_lines);
        if display_lines.is_empty() {
            return lines.join("\n");
        }
        lines.push(String::new());
        let mut previous: i64 = -1;
        for line_num in display_lines {
            if previous != -1 && i64::from(line_num) > previous + 1 {
                lines.push("...".to_string());
            }
            previous = i64::from(line_num);
            let text = details
                .file_lines
                .get((line_num - 1) as usize)
                .map_or("", String::as_str);
            let marker = if anchor_set.contains(&line_num) {
                "*"
            } else {
                " "
            };
            lines.push(format!("{marker}{}", format_numbered_line(line_num, text)));
        }
        lines.join("\n")
    }
}

impl fmt::Debug for MismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MismatchError")
            .field("path", &self.path)
            .field("expected_file_hash", &self.expected_file_hash)
            .field("actual_file_hash", &self.actual_file_hash)
            .field("file_lines", &self.file_lines)
            .field("anchor_lines", &self.anchor_lines)
            .field("hash_recognized", &self.hash_recognized)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for MismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MismatchError {}

/// Returns an error when the line reference is out of bounds for the given file.
///
/// # Errors
///
/// Returns an error if `line` is less than 1 or greater than the number of lines in `file_lines`.
pub fn validate_line_ref(line: u32, file_lines: &[String]) -> Result<(), String> {
    if line < 1 || line as usize > file_lines.len() {
        return Err(format!(
            "Line {line} does not exist (file has {} lines)",
            file_lines.len()
        ));
    }
    Ok(())
}
