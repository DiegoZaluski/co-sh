//! Minimal text-shape normalization: line-ending detection / round-trip and
//!
//! BOM stripping. The patcher uses these to canonicalize text to LF before
//! applying edits and to restore the original shape on write-back.

/// Detect the first line ending style in `content`. Defaults to LF when neither is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Crlf,
    Lf,
}

/// Detect the first line ending style in `content`. Defaults to LF when neither is present.
#[must_use]
pub fn detect_line_ending(content: &str) -> LineEnding {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Option::Some(crlf), Option::Some(lf)) if crlf < lf => LineEnding::Crlf,
        _ => LineEnding::Lf,
    }
}

/// Normalize every line ending to LF.
#[must_use]
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Re-encode LF text with the requested line ending.
#[must_use]
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_string(),
    }
}

/// Result of stripping a UTF-8 BOM from content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BomResult {
    /// Either the empty string or the BOM sequence (currently UTF-8 BOM).
    pub bom: String,
    /// Text with any leading BOM removed.
    pub text: String,
}

/// Strip a UTF-8 BOM if present and return both the BOM and the trailing text.
#[must_use]
pub fn strip_bom(content: &str) -> BomResult {
    content.strip_prefix('\u{FEFF}').map_or_else(
        || BomResult {
            bom: String::new(),
            text: content.to_string(),
        },
        |rest| BomResult {
            bom: "\u{FEFF}".to_string(),
            text: rest.to_string(),
        },
    )
}
