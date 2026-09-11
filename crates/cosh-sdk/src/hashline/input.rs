//! Top-level patch parser. Splits an authored hashline input into a list of
//!
//! [`PatchSection`]s, each rooted at a `¶PATH#HASH` header, then exposes a
//! [`Patch`] struct that gives lazy access to the parsed edits per section.
//!
//! The splitter is purely lexical — it doesn't know whether a section's path
//! actually exists. That's the patcher's job.
use std::fmt;
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use regex::Regex;

use super::apply::apply_edits;
use super::block::{ResolveBlockEditsOptions, resolve_block_edits};
use super::format::{HL_FILE_HASH_LENGTH, HL_FILE_HASH_SEP, HL_FILE_PREFIX};
use super::parser::{parse_patch, parse_patch_streaming};
use super::tokenizer::{Token, Tokenizer};
use super::types::{ApplyResult, BlockResolver, Edit, ResolveAction, SplitOptions};

// Pure classification — single shared tokenizer is safe.
static TOKENIZER: LazyLock<Tokenizer> = LazyLock::new(Tokenizer::new);

fn unquote_hashline_path(path_text: &str) -> &str {
    let bytes = path_text.as_bytes();
    if bytes.len() < 2 {
        return path_text;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first == b'"' || first == b'\'') && first == last {
        &path_text[1..path_text.len() - 1]
    } else {
        path_text
    }
}

/// Strip apply_patch-style noise that models reflexively prepend to the
/// path. Examples observed in benchmark traces:
///
///   `Update File:foo.ts`, `Update:foo.ts`, `UpdateFile:foo.ts`,
///   `Update/File:foo.ts`, `Update-file:foo.ts`, `Update(File):foo.ts`,
///   `Update<File:foo.ts`, `Add File:foo.ts`, `Delete File:foo.ts`,
///   `Move to:foo.ts`, `***foo.ts`, `***Update File:foo.ts`.
///
/// We strip a leading `***` (the model duplicating the header sigil) and a
/// leading `(Update|Add|Delete|Move)[<separator>]*(File|to)?[<separator>]*:`
/// keyword block, case-insensitive. The remaining text is the real path.
#[allow(clippy::expect_used)]
static APPLY_PATCH_PATH_NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\*{0,3}\s*(?:(?:update|add|delete|move)[^A-Za-z0-9]*(?:file|to)?[^A-Za-z0-9]*:)?\s*\*{0,3}\s*")
        .expect("APPLY_PATCH_PATH_NOISE_RE regex should be valid")
});

fn strip_apply_patch_path_noise(path_text: &str) -> String {
    APPLY_PATCH_PATH_NOISE_RE.replace(path_text, "").to_string()
}

/// Best-effort recovery for `¶`-prefixed lines the strict tokenizer
/// rejects. Strips `apply_patch` keyword noise (`Update File:`, `Update:`,
/// etc.) and an extra leading `***` (some models emit a hybrid `¶***foo.ts`
/// shape), then expects `PATH(#HASH)?` with no embedded whitespace.
/// Returns `None` when no clean path can be salvaged.
fn try_parse_recovery_header(line: &str, cwd: Option<&str>) -> Option<RawSection> {
    if !line.starts_with(HL_FILE_PREFIX) {
        return None;
    }
    let body = strip_apply_patch_path_noise(&line[HL_FILE_PREFIX.len()..]);
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let pattern = format!(r"^(\S+?)(?:#([0-9A-Fa-f]{{{HL_FILE_HASH_LENGTH}}}))?\s*$");
    let re = Regex::new(&pattern).ok()?;
    let caps = re.captures(body)?;
    let raw_path = caps.get(1)?.as_str().to_string();
    let path = normalize_hashline_path(&raw_path, cwd);
    if path.is_empty() {
        return None;
    }
    let file_hash = caps.get(2).map(|m| m.as_str().to_uppercase());
    Some(RawSection {
        path,
        file_hash,
        diff: String::new(),
    })
}

fn normalize_hashline_path(raw_path: &str, cwd: Option<&str>) -> String {
    let unquoted = strip_apply_patch_path_noise(unquote_hashline_path(raw_path.trim()));
    let Some(cwd) = cwd else {
        return unquoted;
    };
    let cwd_path = Path::new(cwd);
    let path = Path::new(&unquoted);
    if !path.is_absolute() {
        return unquoted;
    }
    #[allow(clippy::option_if_let_else)]
    if let Ok(relative) = path.strip_prefix(cwd_path) {
        let s = relative.to_string_lossy().to_string();
        if s.is_empty() { ".".to_string() } else { s }
    } else {
        unquoted
    }
}

struct RawSection {
    path: String,
    file_hash: Option<String>,
    diff: String,
}

/// Parse a `¶PATH[#hash]` header line. Returns `None` for lines that do
/// not start with `¶`. Returns an error when a `¶`-prefixed line fails the
/// strict shape (so malformed paths surface immediately instead of being
/// silently re-classified as payload).
fn parse_hashline_header_line(line: &str, cwd: Option<&str>) -> Result<Option<RawSection>, String> {
    let trimmed = line.trim_end();
    if !trimmed.starts_with(HL_FILE_PREFIX) {
        return Ok(None);
    }
    let token = TOKENIZER.tokenize(trimmed, 0);
    let Token::Header {
        path, file_hash, ..
    } = token
    else {
        // Recovery: try to extract a path from the raw line after stripping
        // apply_patch noise. This handles `*** Update File:foo.ts#CB5` and
        // the half-dozen variants models actually emit.
        if let Some(recovered) = try_parse_recovery_header(trimmed, cwd) {
            return Ok(Some(recovered));
        }
        return Err(format!(
            "Input header must be {HL_FILE_PREFIX}PATH or {HL_FILE_PREFIX}PATH{HL_FILE_HASH_SEP}TAG \
                 with a {HL_FILE_HASH_LENGTH}-hex content-hash tag; got {trimmed:?}.",
        ));
    };
    let parsed_path = normalize_hashline_path(&path, cwd);
    if parsed_path.is_empty() {
        return Err(format!(
            "Input header \"{HL_FILE_PREFIX}\" is empty; provide a file path.",
        ));
    }
    Ok(Some(RawSection {
        path: parsed_path,
        file_hash,
        diff: String::new(),
    }))
}

fn strip_leading_blank_lines(input: &str) -> String {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let mut lines: Vec<&str> = input.split('\n').collect();
    while let Some(head) = lines.first() {
        let head = head.trim_end_matches('\r');
        if head.trim().is_empty()
            || matches!(TOKENIZER.tokenize(head, 0), Token::EnvelopeBegin { .. })
        {
            lines.remove(0);
            continue;
        }
        break;
    }
    #[allow(clippy::unnecessary_join)]
    lines.join("\n")
}

/// Returns true when the input contains at least one line that the tokenizer
///
/// recognizes as a hashline op. Used by streaming previews to decide whether
/// the partial input is worth treating as a hashline patch yet.
#[must_use]
pub fn contains_recognizable_hashline_operations(input: &str) -> bool {
    input.lines().any(|line| TOKENIZER.is_op(line))
}

fn normalize_fallback_input(input: &str, options: &SplitOptions) -> String {
    let stripped = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let has_explicit_header = stripped.split('\n').any(|raw_line| {
        matches!(
            parse_hashline_header_line(raw_line, options.cwd.as_deref()),
            Ok(Some(_))
        )
    });
    if has_explicit_header {
        return input.to_string();
    }
    let Some(path) = options.path.as_ref() else {
        return input.to_string();
    };
    if !contains_recognizable_hashline_operations(input) {
        return input.to_string();
    }
    let fallback_path = normalize_hashline_path(path, options.cwd.as_deref());
    if fallback_path.is_empty() {
        return input.to_string();
    }
    format!("{HL_FILE_PREFIX}{fallback_path}\n{input}")
}

fn split_raw_sections(input: &str, options: &SplitOptions) -> Result<Vec<RawSection>, String> {
    let stripped = strip_leading_blank_lines(&normalize_fallback_input(input, options));
    let lines: Vec<&str> = stripped.split('\n').collect();
    let first_line = lines.first().copied().unwrap_or("");

    match parse_hashline_header_line(first_line, options.cwd.as_deref()) {
        Ok(None) => {
            // Catch unified-diff hunk-header contamination on the first line so
            // the model sees a focused error.
            let first_trimmed = first_line.trim_end();
            if first_trimmed.starts_with("@@")
                && first_trimmed.contains("@@")
                && first_trimmed.len() > 4
            {
                return Err(
                    "unified-diff hunk header (`@@ -N,M +N,M @@`) is not valid in hashline. \
                     File sections start with `¶path#HASH`; use `replace`, `delete`, or `insert` ops."
                        .to_string(),
                );
            }
            let truncated: String = first_line.chars().take(120).collect();
            let preview = format!("\"{truncated}\"");
            return Err(format!(
                "input must begin with \"{HL_FILE_PREFIX}PATH{HL_FILE_HASH_SEP}HASH\" on the first non-blank line \
                 for anchored edits; got: {preview}. \
                 Example: \"{HL_FILE_PREFIX}src/foo.ts{HL_FILE_HASH_SEP}0A3\" then edit ops.",
            ));
        }
        Ok(Some(_)) => {} // first line is a valid header, proceed
        Err(e) => return Err(e),
    }

    let mut sections: Vec<RawSection> = Vec::new();
    let mut current: Option<RawSection> = None;
    let mut current_lines: Vec<String> = Vec::new();

    let flush = |sections: &mut Vec<RawSection>,
                 current: &mut Option<RawSection>,
                 current_lines: &mut Vec<String>| {
        let Some(section) = current.take() else {
            return;
        };
        let has_ops = current_lines.iter().any(|l| !l.trim().is_empty());
        if has_ops {
            sections.push(RawSection {
                path: section.path,
                file_hash: section.file_hash,
                diff: current_lines.join("\n"),
            });
        }
        current_lines.clear();
    };

    for line in &lines {
        let trimmed = line.trim_end();
        let token = TOKENIZER.tokenize(line, 0);
        match token {
            Token::EnvelopeEnd { .. } | Token::Abort { .. } => break,
            Token::EnvelopeBegin { .. } => continue,
            _ => {}
        }
        // Route every `¶`-prefixed line through parse_hashline_header_line so
        // malformed headers still raise the strict "Input header must be …"
        // diagnostic (the tokenizer alone would silently classify them as
        // payload).
        if trimmed.starts_with(HL_FILE_PREFIX) {
            match parse_hashline_header_line(line, options.cwd.as_deref()) {
                Ok(Some(header)) => {
                    flush(&mut sections, &mut current, &mut current_lines);
                    current = Some(header);
                    current_lines.clear();
                    continue;
                }
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        }
        current_lines.push(line.to_string());
    }
    flush(&mut sections, &mut current, &mut current_lines);
    Ok(sections)
}

/// Snapshot of one section in a parsed [`Patch`]: a target file plus the
///
/// lazily-parsed list of edits that should land on it. Constructed by
/// [`Patch::parse`]; consumers usually iterate `patch.sections` rather
/// than build these directly.
pub struct PatchSection {
    pub path: String,
    pub file_hash: Option<String>,
    pub diff: String,
    parsed: OnceLock<(Vec<Edit>, Vec<String>)>,
}

impl fmt::Debug for PatchSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PatchSection")
            .field("path", &self.path)
            .field("file_hash", &self.file_hash)
            .field("diff", &self.diff)
            .field("parsed", &self.parsed.get().map(|_| ".."))
            .finish()
    }
}

impl PatchSection {
    fn new(raw: RawSection) -> Self {
        Self {
            path: raw.path,
            file_hash: raw.file_hash,
            diff: raw.diff,
            parsed: OnceLock::new(),
        }
    }

    /// Parse this section's diff body. Cached: subsequent calls return the
    /// same `(edits, warnings)` tuple so callers can safely call this from
    /// multiple paths (preflight, apply, diff-preview).
    pub fn parse(&self) -> &(Vec<Edit>, Vec<String>) {
        self.parsed
            .get_or_init(|| parse_patch(&self.diff).unwrap_or_else(|err| (vec![], vec![err])))
    }

    /// Parsed edits for this section.
    pub fn edits(&self) -> &[Edit] {
        &self.parse().0
    }

    /// Warnings emitted during parsing of this section.
    pub fn warnings(&self) -> &[String] {
        &self.parse().1
    }

    /// True when at least one edit anchors to concrete file content. Pure
    /// `insert head:` / `insert tail:` literal inserts do not count: those are
    /// safe to apply to files that don't yet exist.
    pub fn has_anchor_scoped_edit(&self) -> bool {
        self.edits().iter().any(|edit| match edit {
            Edit::Delete { .. } | Edit::Block { .. } => true,
            Edit::Insert { cursor, .. } => {
                matches!(
                    cursor,
                    super::types::Cursor::BeforeAnchor(_) | super::types::Cursor::AfterAnchor(_)
                )
            }
        })
    }

    /// Anchor lines touched by this section, sorted ascending and deduplicated.
    pub fn collect_anchor_lines(&self) -> Vec<u32> {
        let mut lines: Vec<u32> = Vec::new();
        for edit in self.edits() {
            match edit {
                Edit::Delete { anchor, .. } | Edit::Block { anchor, .. } => {
                    if !lines.contains(&anchor.line) {
                        lines.push(anchor.line);
                    }
                }
                Edit::Insert { cursor, .. } => {
                    if let super::types::Cursor::BeforeAnchor(anchor)
                    | super::types::Cursor::AfterAnchor(anchor) = cursor
                        && !lines.contains(&anchor.line)
                    {
                        lines.push(anchor.line);
                    }
                }
            }
        }
        lines.sort_unstable();
        lines
    }

    /// Apply this section's edits to `text` and return the post-edit result.
    /// Pure: does no I/O, does not validate the section snapshot tag. The
    /// [`super::patcher::Patcher`] owns tag validation and recovery; reach for this
    /// method directly when you've already validated the file content and
    /// just want the result.
    ///
    /// `block_resolver` resolves any `replace block N:` edits against `text`; an
    /// unresolvable block throws (this is the final, authoritative preview path).
    ///
    /// # Errors
    ///
    /// Returns an error when the resolved edits are malformed (unresolved block,
    /// out-of-bounds anchor) or a boundary echo cannot be placed without
    /// ambiguity — such an edit is never applied.
    pub fn apply_to(
        &self,
        text: &str,
        block_resolver: Option<BlockResolver>,
    ) -> Result<ApplyResult, String> {
        let (edits, warnings) = self.parse().clone();
        let resolved = resolve_block_edits(
            &edits,
            text,
            &self.path,
            block_resolver,
            Some(ResolveBlockEditsOptions {
                on_unresolved: ResolveAction::Throw,
            }),
        );
        let mut result = apply_edits(text, &resolved, Some(&self.path))?;
        // Preserve parse warnings so consumers don't need to call `parse()`
        // separately.
        if !warnings.is_empty() {
            let merged: Vec<String> = warnings
                .iter()
                .chain(result.warnings.iter())
                .cloned()
                .collect();
            result.warnings = merged;
        }
        Ok(result)
    }

    /// Streaming-tolerant counterpart to [`apply_to`]. Uses
    /// [`parse_patch_streaming`] so a trailing in-flight op (no payload yet,
    /// or a per-token parse error mid-stream) does not throw or emit a phantom
    /// empty-payload edit. Intended for incremental diff previews; the writer
    /// path should always use [`apply_to`].
    ///
    /// `block_resolver` resolves any `replace block N:` edits against `text`; an
    /// unresolvable block is silently dropped so a half-written file does not
    /// throw mid-stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the resolved edits are malformed or a boundary
    /// echo cannot be placed without ambiguity (same contract as [`apply_to`]).
    pub fn apply_partial_to(
        &self,
        text: &str,
        block_resolver: Option<BlockResolver>,
    ) -> Result<ApplyResult, String> {
        let (edits, warnings) = parse_patch_streaming(&self.diff);
        let resolved = resolve_block_edits(
            &edits,
            text,
            &self.path,
            block_resolver,
            Some(ResolveBlockEditsOptions {
                on_unresolved: ResolveAction::Drop,
            }),
        );
        let mut result = apply_edits(text, &resolved, Some(&self.path))?;
        if !warnings.is_empty() {
            let merged: Vec<String> = warnings
                .iter()
                .chain(result.warnings.iter())
                .cloned()
                .collect();
            result.warnings = merged;
        }
        Ok(result)
    }
}

/// A parsed hashline patch — zero or more [`PatchSection`]s, each rooted
/// at a `¶PATH#HASH` header. Construct via [`Patch::parse`].
///
/// `Patch` is pure data: parsing is line-anchored and does not look at the
/// filesystem. To apply a patch, hand it to [`super::patcher::Patcher::apply`].
#[derive(Debug)]
pub struct Patch {
    pub sections: Vec<PatchSection>,
}

impl Patch {
    const fn new(sections: Vec<PatchSection>) -> Self {
        Self { sections }
    }

    /// Parse `input` into a [`Patch`]. `options.cwd` resolves absolute paths
    /// inside headers to cwd-relative form; `options.path` provides a fallback
    /// when the input lacks a header but contains hashline ops (useful for
    /// streaming previews).
    ///
    /// Consecutive sections targeting the same path are merged into a single
    /// section with concatenated diff bodies. Anchors authored against the
    /// same file snapshot must be applied as one batch; otherwise the first
    /// sub-edit shifts line numbers out from under the second's anchors and
    /// validation fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is malformed or parsing fails.
    pub fn parse(input: &str, options: &SplitOptions) -> Result<Self, String> {
        let raw = merge_same_path_sections(split_raw_sections(input, options)?);
        let sections: Vec<PatchSection> = raw.into_iter().map(PatchSection::new).collect();
        Ok(Self::new(sections))
    }

    /// Parse `input` and return only the first section. Returns an error if the
    /// input has zero sections. Convenience for the single-section case where
    /// the caller already knows the patch is one hunk.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is malformed or parsing fails.
    pub fn parse_single(input: &str, options: &SplitOptions) -> Result<PatchSection, String> {
        let patch = Self::parse(input, options)?;
        let first = patch
            .sections
            .into_iter()
            .next()
            .ok_or_else(|| "Patch input did not produce any sections.".to_string())?;
        Ok(first)
    }
}

/// Collapse consecutive or interleaved sections targeting the same path into a
/// single section with concatenated diffs. Anchors authored against the same
/// file snapshot must be applied as one batch; otherwise the first sub-edit
/// shifts line numbers out from under the second's anchors and validation
/// fails. Path order is preserved by first occurrence.
fn merge_same_path_sections(sections: Vec<RawSection>) -> Vec<RawSection> {
    let mut order: Vec<String> = Vec::new();
    let mut by_path: std::collections::HashMap<String, (Option<String>, Vec<String>)> =
        std::collections::HashMap::new();

    for section in sections {
        let (existing_hash, diffs) = by_path.entry(section.path.clone()).or_insert_with(|| {
            order.push(section.path.clone());
            (None, vec![])
        });

        if let (Some(existing), Some(incoming)) =
            (existing_hash.as_ref(), section.file_hash.as_ref())
        {
            assert_eq!(
                existing, incoming,
                "Conflicting hashline snapshot tags for {}. Re-read the file and retry with one current header.",
                section.path,
            );
        }
        if existing_hash.is_none() && section.file_hash.is_some() {
            *existing_hash = section.file_hash;
        }
        diffs.push(section.diff);
    }

    order
        .into_iter()
        .map(|path| {
            #[allow(clippy::unwrap_used)]
            let (file_hash, diffs) = by_path.remove(&path).unwrap();
            RawSection {
                path,
                file_hash,
                diff: diffs.join("\n"),
            }
        })
        .collect()
}
