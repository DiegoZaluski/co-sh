//! Pure data types shared across the hashline parser, applier, and patcher.
//! Nothing in this file references a filesystem, agent runtime, or schema
//! library — keep it that way.
use std::fmt;

/// A line-number anchor (1-indexed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub line: u32,
}

/// Where an `insert` edit should land relative to existing content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    Bof,
    Eof,
    BeforeAnchor(Anchor),
    AfterAnchor(Anchor),
}

/// A single low-level edit produced by the parser and consumed by the applier.
///
/// Multi-line replacements decompose to one `insert` per replacement line plus
/// one `delete` per consumed line. Replacement payloads are tagged so the
/// applier can distinguish literal insertion from new content for a deleted
/// line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert {
        cursor: Cursor,
        text: String,
        line_num: u32,
        index: u32,
        mode: Option<Replacement>,
    },
    Delete {
        anchor: Anchor,
        line_num: u32,
        index: u32,
        old_assertion: Option<String>,
    },
    /// Deferred block edit (`replace block N:` / `delete block N`). The exact
    /// line span is unknown at parse time — it is computed by
    /// [`resolve_block_edits`] once file text + path (→ language) are
    /// available, then expanded into concrete edits: a non-empty `payloads`
    /// (from `replace block`) becomes the same `replacement` inserts + deletes
    /// that `replace start..end:` produces; an empty `payloads` (from `delete
    /// block`) becomes a pure range deletion. `apply_edits` never sees this
    /// variant.
    Block {
        anchor: Anchor,
        payloads: Vec<String>,
        line_num: u32,
        index: u32,
    },
}

/// Marker for replacement-mode inserts emitted during range decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replacement {
    Replacement,
}

impl fmt::Display for Replacement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "replacement")
    }
}

/// Marker for unresolved block edits (used by [`BlockResolver`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAction {
    Throw,
    Drop,
}

/// Result of applying a parsed set of edits to a text body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// Post-edit text body.
    pub text: String,
    /// First line number (1-indexed) that changed, or `None` for a no-op apply.
    pub first_changed_line: Option<u32>,
    /// Diagnostic warnings collected by the parser, patcher, or recovery.
    pub warnings: Vec<String>,
}

/// A parsed `[A..B]` line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedRange {
    pub start: Anchor,
    pub end: Anchor,
}

/// Optional hints for splitting multi-section input.
#[derive(Debug, Clone, Default)]
pub struct SplitOptions {
    /// Resolves absolute paths inside hashline headers to cwd-relative form.
    pub cwd: Option<String>,
    /// Fallback path used when the input lacks a `¶PATH` header but contains
    /// recognizable hashline operations. Lets streaming previews work before
    /// the model has written the header.
    pub path: Option<String>,
}

/// Streaming-formatter knobs for [`crate::hashline::stream::stream_hash_lines`].
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    /// First line number to use when formatting (1-indexed, default 1).
    pub start_line: Option<u32>,
    /// Maximum formatted lines per yielded chunk (default 200).
    pub max_chunk_lines: Option<u32>,
    /// Maximum UTF-8 bytes per yielded chunk (default 64 KiB).
    pub max_chunk_bytes: Option<u32>,
}

/// Result of [`crate::hashline::diff_preview::build_compact_diff_preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactDiffPreview {
    pub preview: String,
    pub added_lines: u32,
    pub removed_lines: u32,
}

/// Optional knobs for [`crate::hashline::diff_preview::build_compact_diff_preview`].
#[derive(Debug, Clone, Default)]
pub struct CompactDiffOptions {
    /// Maximum entries kept on each side of an unchanged-context truncation (default 2).
    pub max_unchanged_run: Option<u32>,
}

/// Resolved 1-indexed inclusive line span of a `replace block N:` target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSpan {
    /// First line of the block (1-indexed, inclusive).
    pub start: u32,
    /// Last line of the block (1-indexed, inclusive).
    pub end: u32,
}

/// Request handed to a [`BlockResolver`] to resolve one `replace block N:` anchor.
#[derive(Debug, Clone)]
pub struct BlockResolverRequest {
    /// Target file path (used to infer language by extension).
    pub path: String,
    /// Full text the block must be resolved against (the snapshot the tag names).
    pub text: String,
    /// 1-indexed line the block must begin on.
    pub line: u32,
}

/// Resolves a `replace block N:` anchor to the line span of the syntactic block
///
/// that begins on line N. Returns `None` when no block can be resolved
/// (unrecognized language, blank/out-of-range line, no node begins there, or the
/// resolved subtree has a syntax error). Pure seam: the hashline core declares
/// the contract; the host injects a tree-sitter-backed implementation.
pub type BlockResolver = fn(BlockResolverRequest) -> Option<BlockSpan>;
