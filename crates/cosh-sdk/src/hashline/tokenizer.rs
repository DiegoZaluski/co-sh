//! Stateful, line-oriented classifier for hashline diff text.
//!
//! Format shape:
//! ```text
//! ¶path/to/file.rs#0A3
//! replace 5..7:
//! +literal new line
//! ```
use super::format::{
    describe_anchor_examples, HL_BLOCK_KEYWORD, HL_DELETE_KEYWORD, HL_FILE_HASH_LENGTH,
    HL_FILE_HASH_SEP, HL_FILE_PREFIX, HL_INSERT_AFTER, HL_INSERT_BEFORE, HL_INSERT_HEAD,
    HL_INSERT_KEYWORD, HL_INSERT_TAIL, HL_PAYLOAD_REPLACE, HL_REPLACE_KEYWORD,
};
use crate::hashline::messages::{ABORT_MARKER, BEGIN_PATCH_MARKER, END_PATCH_MARKER};
use crate::hashline::types::{Anchor, ParsedRange};

fn is_digit_code(c: u8) -> bool {
    c.is_ascii_digit()
}

fn is_non_zero_digit_code(c: u8) -> bool {
    c > b'0' && c <= b'9'
}

fn is_hex_digit_code(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

fn is_whitespace_code(c: u8) -> bool {
    c == b' ' || (b'\t'..=b'\r').contains(&c)
}

fn skip_whitespace(line: &[u8], index: usize, end: usize) -> usize {
    let mut i = index;
    while i < end && is_whitespace_code(line[i]) {
        i += 1;
    }
    i
}

fn trim_end_index(line: &[u8]) -> usize {
    let mut end = line.len();
    while end > 0 && is_whitespace_code(line[end - 1]) {
        end -= 1;
    }
    end
}

fn marker_line_equals(line: &str, marker: &str) -> bool {
    let line_bytes = line.as_bytes();
    let end = trim_end_index(line_bytes);
    end == marker.len() && line.starts_with(marker)
}

pub fn split_hashline_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let bytes = text.as_bytes();
    let mut lines: Vec<String> = Vec::new();
    let mut start = 0;
    for index in 0..bytes.len() {
        if bytes[index] != b'\n' {
            continue;
        }
        let mut end = index;
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        lines.push(String::from(&text[start..end]));
        start = index + 1;
    }
    if start < text.len() {
        let mut end = text.len();
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        lines.push(String::from(&text[start..end]));
    }
    lines
}

struct NumberScan {
    line: u32,
    next_index: usize,
}

fn scan_line_number(text: &str, index: usize, end: usize) -> Option<NumberScan> {
    let bytes = text.as_bytes();
    if index >= end || !is_non_zero_digit_code(bytes[index]) {
        return None;
    }
    let mut line_number: u32 = 0;
    let mut next_index = index;
    while next_index < end {
        let code = bytes[next_index];
        if !is_digit_code(code) {
            break;
        }
        line_number = line_number * 10 + u32::from(code - b'0');
        next_index += 1;
    }
    Option::Some(NumberScan {
        line: line_number,
        next_index,
    })
}

/// Parse a bare line-number anchor. Returns an error on malformed input.
pub fn parse_lid(raw: &str, line_num: u32) -> Result<Anchor, String> {
    let bytes = raw.as_bytes();
    let end = trim_end_index(bytes);
    let number_start = skip_whitespace(bytes, 0, end);
    let number = scan_line_number(raw, number_start, end);
    match number {
        Some(n) if skip_whitespace(bytes, n.next_index, end) == end => Ok(Anchor { line: n.line }),
        _ => Err(format!(
            "line {}: expected a line number such as {}; \
             got {}. Use {}PATH{}hash from your latest read for file-version binding.",
            line_num,
            describe_anchor_examples(Some("119")),
            raw,
            HL_FILE_PREFIX,
            HL_FILE_HASH_SEP,
        )),
    }
}

struct RangeScan {
    range: ParsedRange,
    next_index: usize,
}

fn scan_range_separator(text: &str, index: usize, end: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut cursor = index;
    let mut consumed_separator = false;
    while cursor < end {
        let code = bytes[cursor];
        if is_whitespace_code(code) {
            cursor += 1;
            consumed_separator = true;
            continue;
        }
        if code == b'-' || code == 0xe2 {
            // CHAR_HYPHEN = 45 = '-', CHAR_ELLIPSIS = 0x2026 (multi-byte UTF-8)
            // For 0xe2, check for UTF-8 ellipsis …
            if code == 0xe2
                && cursor + 2 < end
                && bytes[cursor + 1] == 0x80
                && bytes[cursor + 2] == 0xa6
            {
                cursor += 3;
                consumed_separator = true;
                continue;
            }
            if code == b'-' {
                cursor += 1;
                consumed_separator = true;
                continue;
            }
        }
        if code == b'.' && cursor + 1 < end && bytes[cursor + 1] == b'.' {
            cursor += 2;
            consumed_separator = true;
            continue;
        }
        break;
    }
    if !consumed_separator {
        return None;
    }
    if cursor >= end || !is_non_zero_digit_code(bytes[cursor]) {
        return None;
    }
    Option::Some(cursor)
}

fn scan_header_range(
    text: &str,
    index: usize,
    end: usize,
    allow_single: bool,
) -> Option<RangeScan> {
    let number_start = skip_whitespace(text.as_bytes(), index, end);
    let start = scan_line_number(text, number_start, end)?;
    let after_first = scan_range_separator(text, start.next_index, end);
    match after_first {
        Option::None => {
            if !allow_single {
                return Option::None;
            }
            Option::Some(RangeScan {
                range: ParsedRange {
                    start: Anchor { line: start.line },
                    end: Anchor { line: start.line },
                },
                next_index: skip_whitespace(text.as_bytes(), start.next_index, end),
            })
        }
        Option::Some(after) => {
            let end_number = scan_line_number(text, after, end)?;
            let next_index = skip_whitespace(text.as_bytes(), end_number.next_index, end);
            Option::Some(RangeScan {
                range: ParsedRange {
                    start: Anchor { line: start.line },
                    end: Anchor {
                        line: end_number.line,
                    },
                },
                next_index,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockTarget {
    Replace { range: ParsedRange },
    Block { anchor: Anchor },
    Delete { range: ParsedRange },
    DeleteBlock { anchor: Anchor },
    InsertBefore { anchor: Anchor },
    InsertAfter { anchor: Anchor },
    Bof,
    Eof,
}

struct TargetScan {
    target: BlockTarget,
    next_index: usize,
}

fn scan_keyword(line: &str, index: usize, end: usize, keyword: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    if !line[index..].starts_with(keyword) {
        return None;
    }
    let next = index + keyword.len();
    if next < end {
        let code = bytes[next];
        if !is_whitespace_code(code) && code != b':' {
            return None;
        }
    }
    Some(next)
}

fn consume_optional_colon(line: &str, index: usize, end: usize) -> usize {
    let bytes = line.as_bytes();
    let cursor = skip_whitespace(bytes, index, end);
    if cursor < end && bytes[cursor] == b':' {
        skip_whitespace(bytes, cursor + 1, end)
    } else {
        cursor
    }
}

fn scan_insert_target(line: &str, index: usize, end: usize) -> Option<TargetScan> {
    let bytes = line.as_bytes();
    let cursor = skip_whitespace(bytes, index, end);
    let before_end = scan_keyword(line, cursor, end, HL_INSERT_BEFORE);
    if let Some(be) = before_end {
        let anchor = scan_line_number(line, skip_whitespace(bytes, be, end), end)?;
        let next_index = consume_optional_colon(line, anchor.next_index, end);
        return Some(TargetScan {
            target: BlockTarget::InsertBefore {
                anchor: Anchor { line: anchor.line },
            },
            next_index,
        });
    }
    let after_end = scan_keyword(line, cursor, end, HL_INSERT_AFTER);
    if let Some(ae) = after_end {
        let anchor = scan_line_number(line, skip_whitespace(bytes, ae, end), end)?;
        let next_index = consume_optional_colon(line, anchor.next_index, end);
        return Some(TargetScan {
            target: BlockTarget::InsertAfter {
                anchor: Anchor { line: anchor.line },
            },
            next_index,
        });
    }
    if let Some(head_end) = scan_keyword(line, cursor, end, HL_INSERT_HEAD) {
        return Some(TargetScan {
            target: BlockTarget::Bof,
            next_index: consume_optional_colon(line, head_end, end),
        });
    }
    if let Some(tail_end) = scan_keyword(line, cursor, end, HL_INSERT_TAIL) {
        return Some(TargetScan {
            target: BlockTarget::Eof,
            next_index: consume_optional_colon(line, tail_end, end),
        });
    }
    None
}

fn scan_hunk_anchor(line: &str, start: usize, end: usize) -> Option<TargetScan> {
    let bytes = line.as_bytes();
    let cursor = skip_whitespace(bytes, start, end);
    let replace_end = scan_keyword(line, cursor, end, HL_REPLACE_KEYWORD);
    if let Some(re) = replace_end {
        let block_end = scan_keyword(line, skip_whitespace(bytes, re, end), end, HL_BLOCK_KEYWORD);
        if let Some(be) = block_end {
            let anchor = scan_line_number(line, skip_whitespace(bytes, be, end), end)?;
            return Some(TargetScan {
                target: BlockTarget::Block {
                    anchor: Anchor { line: anchor.line },
                },
                next_index: consume_optional_colon(line, anchor.next_index, end),
            });
        }
        let range = scan_header_range(line, re, end, true)?;
        return Some(TargetScan {
            target: BlockTarget::Replace { range: range.range },
            next_index: consume_optional_colon(line, range.next_index, end),
        });
    }
    let delete_end = scan_keyword(line, cursor, end, HL_DELETE_KEYWORD);
    if let Some(de) = delete_end {
        let block_end = scan_keyword(line, skip_whitespace(bytes, de, end), end, HL_BLOCK_KEYWORD);
        if let Some(be) = block_end {
            let anchor = scan_line_number(line, skip_whitespace(bytes, be, end), end)?;
            let next = skip_whitespace(bytes, anchor.next_index, end);
            if next < end && bytes[next] == b':' {
                return None;
            }
            return Some(TargetScan {
                target: BlockTarget::DeleteBlock {
                    anchor: Anchor { line: anchor.line },
                },
                next_index: next,
            });
        }
        let range = scan_header_range(line, de, end, true)?;
        let next = skip_whitespace(bytes, range.next_index, end);
        if next < end && bytes[next] == b':' {
            return None;
        }
        return Some(TargetScan {
            target: BlockTarget::Delete { range: range.range },
            next_index: next,
        });
    }
    let insert_end = scan_keyword(line, cursor, end, HL_INSERT_KEYWORD);
    if let Some(ie) = insert_end {
        return scan_insert_target(line, ie, end);
    }
    None
}

pub fn try_parse_hunk_header(line: &str) -> Option<BlockTarget> {
    let bytes = line.as_bytes();
    let end = trim_end_index(bytes);
    let start = skip_whitespace(bytes, 0, end);
    if start >= end {
        return None;
    }
    let scan = scan_hunk_anchor(line, start, end)?;
    if scan.next_index != end {
        return None;
    }
    Some(scan.target)
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderInfo {
    pub path: String,
    pub file_hash: Option<String>,
}

pub fn try_parse_header(line: &str) -> Option<HeaderInfo> {
    if !line.starts_with(HL_FILE_PREFIX) {
        return None;
    }
    let bytes = line.as_bytes();
    let end = trim_end_index(bytes);
    let mut index = HL_FILE_PREFIX.len();
    if index >= end {
        return None;
    }
    let path_start = index;
    while index < end {
        let code = bytes[index];
        if code == b'#' || code == b' ' || code == b'\t' {
            break;
        }
        index += 1;
    }
    if index == path_start {
        return None;
    }
    let path = line[path_start..index].to_string();
    let mut file_hash: Option<String> = None;
    if index < end && bytes[index] == b'#' {
        let hash_start = index + 1;
        let hash_end = hash_start + HL_FILE_HASH_LENGTH;
        if hash_end > end {
            return None;
        }
        for &b in bytes[hash_start..hash_end].iter() {
            if !is_hex_digit_code(b) {
                return None;
            }
        }
        file_hash = Some(line[hash_start..hash_end].to_uppercase());
        index = hash_end;
    }
    if skip_whitespace(bytes, index, end) != end {
        return None;
    }
    Some(HeaderInfo { path, file_hash })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Blank {
        line_num: u32,
    },
    EnvelopeBegin {
        line_num: u32,
    },
    EnvelopeEnd {
        line_num: u32,
    },
    Abort {
        line_num: u32,
    },
    Header {
        line_num: u32,
        path: String,
        file_hash: Option<String>,
    },
    OpBlock {
        line_num: u32,
        target: BlockTarget,
    },
    PayloadLiteral {
        line_num: u32,
        text: String,
    },
    Raw {
        line_num: u32,
        text: String,
    },
}

pub fn classify_line(line: &str, line_num: u32) -> Token {
    if line.is_empty() {
        return Token::Blank { line_num };
    }
    if marker_line_equals(line, BEGIN_PATCH_MARKER) {
        return Token::EnvelopeBegin { line_num };
    }
    if marker_line_equals(line, END_PATCH_MARKER) {
        return Token::EnvelopeEnd { line_num };
    }
    if marker_line_equals(line, ABORT_MARKER) {
        return Token::Abort { line_num };
    }
    if line.starts_with(HL_FILE_PREFIX) {
        if let Some(header) = try_parse_header(line) {
            return Token::Header {
                line_num,
                path: header.path,
                file_hash: header.file_hash,
            };
        }
    }
    let bytes = line.as_bytes();
    let lead = skip_whitespace(bytes, 0, line.len());
    let is_hunk_lead = line[lead..].starts_with(HL_REPLACE_KEYWORD)
        || line[lead..].starts_with(HL_DELETE_KEYWORD)
        || line[lead..].starts_with(HL_INSERT_KEYWORD);
    if is_hunk_lead {
        if let Some(target) = try_parse_hunk_header(line) {
            return Token::OpBlock { line_num, target };
        }
    }
    if !bytes.is_empty() && bytes[0] == HL_PAYLOAD_REPLACE.as_bytes()[0] {
        return Token::PayloadLiteral {
            line_num,
            text: line[1..].to_string(),
        };
    }
    Token::Raw {
        line_num,
        text: line.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct Tokenizer {
    buffer: String,
    next_line_num: u32,
    closed: bool,
}

impl Tokenizer {
    /// Create a new tokenizer starting at line number 1.
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            next_line_num: 1,
            closed: false,
        }
    }

    /// Feed a chunk of text and drain complete lines into tokens.
    pub fn feed(&mut self, chunk: &str) -> Vec<Token> {
        if self.closed {
            panic!("Tokenizer is closed; call reset() before reusing.");
        }
        if chunk.is_empty() {
            return vec![];
        }
        if self.buffer.is_empty() {
            self.buffer = chunk.to_string();
        } else {
            self.buffer.push_str(chunk);
        }
        self.drain_complete_lines()
    }

    /// Flush remaining buffer as one raw token and mark as closed.
    pub fn end(&mut self) -> Vec<Token> {
        if self.closed {
            return vec![];
        }
        self.closed = true;
        let buf = std::mem::take(&mut self.buffer);
        if buf.is_empty() {
            return vec![];
        }
        let bytes = buf.as_bytes();
        let mut stop = buf.len();
        if stop > 0 && bytes[stop - 1] == b'\r' {
            stop -= 1;
        }
        let line = classify_line(&buf[..stop], self.next_line_num);
        self.next_line_num += 1;
        vec![line]
    }

    /// Reset the tokenizer for reuse.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_line_num = 1;
        self.closed = false;
    }

    /// Convenience: feed all text and flush.
    pub fn tokenize_all(&mut self, text: &str) -> Vec<Token> {
        self.reset();
        let mut tokens = self.feed(text);
        let last = self.end();
        if !last.is_empty() {
            tokens.extend(last);
        }
        tokens
    }

    /// Classify a single line (stateless).
    pub fn tokenize(&self, line: &str, line_num: u32) -> Token {
        classify_line(line, line_num)
    }

    /// Check if a line is a recognized hunk header.
    pub fn is_op(&self, line: &str) -> bool {
        try_parse_hunk_header(line).is_some()
    }

    /// Check if a line is a recognized header.
    pub fn is_header(&self, line: &str) -> bool {
        try_parse_header(line).is_some()
    }

    /// Check if a line is an envelope marker.
    pub fn is_envelope_marker(&self, line: &str) -> bool {
        marker_line_equals(line, BEGIN_PATCH_MARKER)
            || marker_line_equals(line, END_PATCH_MARKER)
            || marker_line_equals(line, ABORT_MARKER)
    }

    fn drain_complete_lines(&mut self) -> Vec<Token> {
        let mut tokens: Vec<Token> = Vec::new();
        let buf = std::mem::take(&mut self.buffer);
        let bytes = buf.as_bytes();
        let mut start = 0;
        for index in 0..buf.len() {
            if bytes[index] != b'\n' {
                continue;
            }
            let mut stop = index;
            if stop > start && bytes[stop - 1] == b'\r' {
                stop -= 1;
            }
            let line = classify_line(&buf[start..stop], self.next_line_num);
            self.next_line_num += 1;
            tokens.push(line);
            start = index + 1;
        }
        if start < buf.len() {
            self.buffer = buf[start..].to_string();
        }
        tokens
    }
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}
