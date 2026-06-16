//! Token-driven state machine that turns a stream of [`Token`]s into a
//! flat list of [`Edit`]s. Sits between the [`Tokenizer`] and the applier.
use super::format::HL_PAYLOAD_REPLACE;
use super::messages::{
    BARE_BODY_AUTO_PIPED_WARNING, DELETE_BLOCK_TAKES_NO_BODY, DELETE_TAKES_NO_BODY, EMPTY_BLOCK,
    EMPTY_INSERT, EMPTY_REPLACE, MINUS_ROW_REJECTED,
};
use super::tokenizer::{BlockTarget, Token, Tokenizer};
use super::types::{Anchor, Cursor, Edit, ParsedRange};
use regex::Regex;

fn validate_range_order(range: ParsedRange, line_num: u32) -> Result<(), String> {
    if range.end.line < range.start.line {
        return Err(format!(
            "line {}: range {}..{} ends before it starts.",
            line_num, range.start.line, range.end.line
        ));
    }
    Ok(())
}

fn expand_range(range: ParsedRange) -> Vec<Anchor> {
    let mut anchors: Vec<Anchor> = Vec::new();
    for line in range.start.line..=range.end.line {
        anchors.push(Anchor { line });
    }
    anchors
}

fn is_skippable_comment_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn detect_apply_patch_contamination(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("*** Update File:")
        || trimmed.starts_with("*** Add File:")
        || trimmed.starts_with("*** Delete File:")
        || trimmed.starts_with("*** Move to:")
    {
        let preview = if trimmed.len() > 48 {
            format!("{}…", &trimmed[..48])
        } else {
            trimmed.to_string()
        };
        return Some(format!(
            "apply_patch sentinel {preview:?} is not valid in hashline. \
             File sections start with `¶path#HASH` (no `Update File:` / `Add File:` keyword). \
             Use `replace N..M:`, `delete N..M`, or `insert before|after|head|tail:` ops.",
        ));
    }

    let udiff_re = Regex::new(r"^@@\s+[-+]?\d+,\d+\s+[-+]?\d+,\d+\s+@@").unwrap();
    if udiff_re.is_match(trimmed) {
        return Some(
            "unified-diff hunk header (`@@ -N,M +N,M @@`) is not valid in hashline. \
             Use `replace N..M:`, `delete N..M`, or `insert before|after|head|tail:` ops."
                .to_string(),
        );
    }

    if trimmed.starts_with("@@") {
        let preview = if trimmed.len() > 48 {
            format!("{}…", &trimmed[..48])
        } else {
            trimmed.to_string()
        };
        return Some(format!(
            "`@@`-bracketed hunk header {preview:?} is not valid in hashline. \
             Drop the `@@ ... @@` brackets and write a verb header such as `replace N..M:`.",
        ));
    }

    let delete_colon_re =
        Regex::new(r"^delete\s+[1-9]\d*(?:\s*(?:\.\.|-|…|\s)\s*[1-9]\d*)?\s*:").unwrap();
    if delete_colon_re.is_match(trimmed) {
        return Some(
            "`delete N..M` has no colon and no body. Remove the colon and body rows.".to_string(),
        );
    }

    let bare_line_re = Regex::new(r"^[1-9]\d*\s*$").unwrap();
    if bare_line_re.is_match(trimmed) {
        let num = trimmed.trim();
        return Some(format!(
            "hunk headers need a verb. Use `replace {num}..{num}:` to replace, or `delete {num}` to delete.",
        ));
    }

    let bare_range_re = Regex::new(r"^([1-9]\d*)\s*[-. …]+\s*([1-9]\d*)\s*:?$").unwrap();
    if let Some(caps) = bare_range_re.captures(trimmed) {
        let s = &caps[1];
        let e = &caps[2];
        return Some(format!(
            "bare range hunk header {trimmed:?} is not valid. \
             Hunk headers need a verb: write `replace {s}..{e}:` or `delete {s}..{e}`.",
        ));
    }

    None
}

struct PendingComment {
    line_num: u32,
    text: String,
}

struct PayloadRow {
    text: String,
}

struct Pending {
    target: BlockTarget,
    line_num: u32,
    payloads: Vec<PayloadRow>,
}

pub struct Executor {
    edits: Vec<Edit>,
    warnings: Vec<String>,
    edit_index: u32,
    pending: Option<Pending>,
    terminated: bool,
    skippable_comments: Vec<PendingComment>,
}

impl Executor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            edits: Vec::new(),
            warnings: Vec::new(),
            edit_index: 0,
            pending: None,
            terminated: false,
            skippable_comments: Vec::new(),
        }
    }

    fn discard_pending_skippable_comments(&mut self) {
        self.skippable_comments.clear();
    }

    fn consume_pending_skippable_comments(&mut self) -> Result<(), String> {
        if self.skippable_comments.is_empty() {
            return Ok(());
        }
        let comments = std::mem::take(&mut self.skippable_comments);
        for comment in &comments {
            self.handle_raw(&comment.text, comment.line_num)?;
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if the token causes a parse error.
    pub fn feed(&mut self, token: Token) -> Result<(), String> {
        if self.terminated {
            return Ok(());
        }
        match token {
            Token::EnvelopeBegin { .. } | Token::EnvelopeEnd { .. } => {
                self.consume_pending_skippable_comments()?;
                if matches!(token, Token::EnvelopeEnd { .. }) {
                    self.terminated = true;
                }
            }
            Token::Abort { .. } => {
                self.terminated = true;
            }
            Token::Header { .. } => {
                self.consume_pending_skippable_comments()?;
                self.flush_pending()?;
            }
            Token::Blank { .. } => {
                self.consume_pending_skippable_comments()?;
            }
            Token::PayloadLiteral { text, line_num } => {
                self.consume_pending_skippable_comments()?;
                self.handle_literal_payload(&text, line_num)?;
            }
            Token::Raw { text, line_num } => {
                if self.pending.is_none() && is_skippable_comment_line(&text) {
                    self.skippable_comments
                        .push(PendingComment { line_num, text });
                    return Ok(());
                }
                self.consume_pending_skippable_comments()?;
                self.handle_raw(&text, line_num)?;
            }
            Token::OpBlock { target, line_num } => {
                self.discard_pending_skippable_comments();
                if matches!(
                    target,
                    BlockTarget::Replace { .. } | BlockTarget::Delete { .. }
                ) {
                    let range = match &target {
                        BlockTarget::Replace { range }
                        | BlockTarget::Delete { range } => *range,
                        _ => unreachable!(),
                    };
                    validate_range_order(range, line_num)?;
                }
                self.flush_pending()?;
                self.pending = Some(Pending {
                    target,
                    line_num,
                    payloads: Vec::new(),
                });
            }
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if there are overlapping deletes or pending comments fail to process.
    pub fn end(&mut self) -> Result<(Vec<Edit>, Vec<String>), String> {
        self.consume_pending_skippable_comments()?;
        self.flush_pending()?;
        self.validate_no_overlapping_deletes()?;
        let edits = std::mem::take(&mut self.edits);
        let warnings = std::mem::take(&mut self.warnings);
        Ok((edits, warnings))
    }

    pub fn end_streaming(&mut self) -> (Vec<Edit>, Vec<String>) {
        if let Err(e) = self.consume_pending_skippable_comments() {
            self.warnings.push(e);
        }
        let should_flush = match &self.pending {
            Some(p) if !p.payloads.is_empty() => true,
            Some(p)
                if matches!(
                    p.target,
                    BlockTarget::Delete { .. } | BlockTarget::DeleteBlock { .. }
                ) =>
            {
                true
            }
            _ => false,
        };
        if should_flush {
            if let Err(e) = self.flush_pending() {
                self.warnings.push(e);
            }
        } else {
            self.pending = None;
        }
        if let Err(e) = self.validate_no_overlapping_deletes() {
            self.warnings.push(e);
        }
        let edits = std::mem::take(&mut self.edits);
        let warnings = std::mem::take(&mut self.warnings);
        (edits, warnings)
    }

    pub fn reset(&mut self) {
        self.edits.clear();
        self.warnings.clear();
        self.edit_index = 0;
        self.pending = None;
        self.skippable_comments.clear();
        self.terminated = false;
    }

    fn validate_no_overlapping_deletes(&mut self) -> Result<(), String> {
        let mut source_lines_by_anchor: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::new();
        for edit in &self.edits {
            if let Edit::Delete {
                anchor, line_num, ..
            } = edit
            {
                let source_lines = source_lines_by_anchor.entry(anchor.line).or_default();
                if !source_lines.contains(line_num) {
                    source_lines.push(*line_num);
                }
            }
        }
        for (anchor_line, source_lines) in &source_lines_by_anchor {
            if source_lines.len() < 2 {
                continue;
            }
            let mut sorted = source_lines.clone();
            sorted.sort_unstable();
            let (first_block, second_block) = (sorted[0], sorted[1]);
            return Err(format!(
                "line {second_block}: anchor line {anchor_line} is already targeted by another hunk on line {first_block}. \
                 Issue ONE hunk per range; payload is only the final desired content, never a before/after pair.",
            ));
        }
        Ok(())
    }

    fn handle_literal_payload(&mut self, text: &str, line_num: u32) -> Result<(), String> {
        let pending = self.pending.as_ref().ok_or_else(|| {
            format!(
                "line {line_num}: payload line has no preceding hunk header. \
                 Got {:?}.",
                format!("{}{}", HL_PAYLOAD_REPLACE, text),
            )
        })?;
        match pending.target {
            BlockTarget::Delete { .. } => {
                return Err(format!("line {line_num}: {DELETE_TAKES_NO_BODY}"));
            }
            BlockTarget::DeleteBlock { .. } => {
                return Err(format!("line {line_num}: {DELETE_BLOCK_TAKES_NO_BODY}"));
            }
            _ => {}
        }
        if let Some(p) = &mut self.pending {
            p.payloads.push(PayloadRow {
                text: text.to_string(),
            });
        }
        Ok(())
    }

    fn handle_raw(&mut self, text: &str, line_num: u32) -> Result<(), String> {
        let contamination = detect_apply_patch_contamination(text);
        if let Some(msg) = contamination {
            return Err(format!("line {line_num}: {msg}"));
        }
        if let Some(pending) = &self.pending {
            if text.trim().is_empty() {
                return Ok(());
            }
            match pending.target {
                BlockTarget::Delete { .. } => {
                    return Err(format!("line {line_num}: {DELETE_TAKES_NO_BODY}"));
                }
                BlockTarget::DeleteBlock { .. } => {
                    return Err(format!("line {line_num}: {DELETE_BLOCK_TAKES_NO_BODY}"));
                }
                _ => {}
            }
            let first = text.trim_start().as_bytes().first();
            if first == Some(&b'-') {
                return Err(format!("line {line_num}: {MINUS_ROW_REJECTED}"));
            }
            if !self
                .warnings
                .contains(&BARE_BODY_AUTO_PIPED_WARNING.to_string())
            {
                self.warnings.push(BARE_BODY_AUTO_PIPED_WARNING.to_string());
            }
            if let Some(p) = &mut self.pending {
                p.payloads.push(PayloadRow {
                    text: text.to_string(),
                });
            }
            return Ok(());
        }
        if text.trim().is_empty() {
            return Ok(());
        }
        Err(format!(
            "line {line_num}: payload line has no preceding hunk header. \
             Use `replace N..M:`, `delete N..M`, or `insert before|after|head|tail:` above the body. Got {text:?}.",
        ))
    }

    fn push_insert(&mut self, cursor: &Cursor, text: &str, line_num: u32, mode: Option<&str>) {
        let index = {
            let idx = self.edit_index;
            self.edit_index += 1;
            idx
        };
        self.edits.push(Edit::Insert {
            cursor: cursor.clone(),
            text: text.to_string(),
            line_num,
            index,
            mode: if mode == Some("replacement") {
                Some(crate::hashline::types::Replacement::Replacement)
            } else {
                None
            },
        });
    }

    fn push_delete(&mut self, anchor: Anchor, line_num: u32) {
        let index = {
            let idx = self.edit_index;
            self.edit_index += 1;
            idx
        };
        self.edits.push(Edit::Delete {
            anchor,
            line_num,
            index,
            old_assertion: None,
        });
    }

    fn push_block(&mut self, anchor: Anchor, payloads: &[PayloadRow], line_num: u32) {
        let index = {
            let idx = self.edit_index;
            self.edit_index += 1;
            idx
        };
        self.edits.push(Edit::Block {
            anchor,
            payloads: payloads.iter().map(|p| p.text.clone()).collect(),
            line_num,
            index,
        });
    }

    fn emit_payload_rows(
        &mut self,
        cursor: &Cursor,
        payloads: &[PayloadRow],
        line_num: u32,
        mode: Option<&str>,
    ) {
        for payload in payloads {
            self.push_insert(cursor, &payload.text, line_num, mode);
        }
    }

    fn flush_pending(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let line_num = pending.line_num;
        let payloads = pending.payloads;
        match pending.target {
            BlockTarget::Delete { range } => {
                for anchor in expand_range(range) {
                    self.push_delete(anchor, line_num);
                }
            }
            BlockTarget::DeleteBlock { anchor } => {
                self.push_block(anchor, &[], line_num);
            }
            BlockTarget::Block { anchor } => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_BLOCK}"));
                }
                self.push_block(anchor, &payloads, line_num);
            }
            BlockTarget::Replace { range } => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_REPLACE}"));
                }
                let cursor = Cursor::BeforeAnchor(Anchor {
                    line: range.start.line,
                });
                self.emit_payload_rows(&cursor, &payloads, line_num, Some("replacement"));
                for anchor in expand_range(range) {
                    self.push_delete(anchor, line_num);
                }
            }
            BlockTarget::InsertBefore { anchor } => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_INSERT}"));
                }
                self.emit_payload_rows(
                    &Cursor::BeforeAnchor(Anchor { line: anchor.line }),
                    &payloads,
                    line_num,
                    None,
                );
            }
            BlockTarget::InsertAfter { anchor } => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_INSERT}"));
                }
                self.emit_payload_rows(
                    &Cursor::AfterAnchor(Anchor { line: anchor.line }),
                    &payloads,
                    line_num,
                    None,
                );
            }
            BlockTarget::Bof => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_INSERT}"));
                }
                self.emit_payload_rows(&Cursor::Bof, &payloads, line_num, None);
            }
            BlockTarget::Eof => {
                if payloads.is_empty() {
                    return Err(format!("line {line_num}: {EMPTY_INSERT}"));
                }
                self.emit_payload_rows(&Cursor::Eof, &payloads, line_num, None);
            }
        }
        Ok(())
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a diff string into a list of edits and warnings.
///
/// # Errors
///
/// Returns an error if the diff text is malformed.
pub fn parse_patch(diff: &str) -> Result<(Vec<Edit>, Vec<String>), String> {
    let mut tokenizer = Tokenizer::new();
    let mut executor = Executor::new();
    for token in tokenizer.feed(diff) {
        executor.feed(token)?;
    }
    for token in tokenizer.end() {
        executor.feed(token)?;
    }
    executor.end()
}

#[must_use]
pub fn parse_patch_streaming(diff: &str) -> (Vec<Edit>, Vec<String>) {
    let mut tokenizer = Tokenizer::new();
    let mut executor = Executor::new();
    for token in tokenizer.feed(diff) {
        if let Err(e) = executor.feed(token) {
            executor.warnings.push(e);
        }
    }
    for token in tokenizer.end() {
        if let Err(e) = executor.feed(token) {
            executor.warnings.push(e);
        }
    }
    executor.end_streaming()
}
