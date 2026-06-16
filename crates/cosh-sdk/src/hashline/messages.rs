//! Centralized error and warning text emitted by the hashline parser, applier,
//! and patcher. Consolidating these as named constants makes them easy to
//! audit and keeps wording stable across the rendering paths that surface
//! them.
use super::format::{HL_FILE_HASH_SEP, HL_FILE_PREFIX};

/// Lines of context shown either side of a hash mismatch.
pub const MISMATCH_CONTEXT: u32 = 2;

/// Optional patch envelope start marker; silently consumed when present.
pub const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";

/// Optional patch envelope end marker; terminates parsing when encountered.
pub const END_PATCH_MARKER: &str = "*** End Patch";

/// Recovery sentinel emitted by an agent loop when a contaminated tool-call
/// stream is truncated mid-call. Behaves like [`END_PATCH_MARKER`] for
/// parsing — terminates the line loop — and does not surface a warning.
pub const ABORT_MARKER: &str = "*** Abort";

/// Warning text appended when two consecutive hunks target the exact same
/// concrete range.
pub const REPLACE_PAIR_COALESCED_WARNING: &str = "Detected two identical-range hashline hunks; kept only the second hunk. \
     Issue ONE `replace N..M:` hunk per range — payload is the final desired \
     content, never both old and new.";

/// Warning text appended when an empty bodyless hunk is followed by an
/// overlapping concrete hunk.
pub const REPLACE_PAIR_COALESCED_OVERLAP_WARNING: &str = "Detected an overlapping bare hashline hunk immediately followed by a \
     concrete hunk; dropped the earlier bare hunk. Issue ONE `replace N..M:` \
     hunk per range — payload is the final desired content, never both old and new.";

/// Warning text appended when bare body rows are auto-converted to literal rows.
pub const BARE_BODY_AUTO_PIPED_WARNING: &str = "Auto-prefixed bare body row(s) with `+`. Body rows must be `+TEXT` literal \
     lines; pasting raw code as payload is not a portable shape.";

/// Error text emitted when a hunk body contains a unified-diff-style `-` row.
pub const MINUS_ROW_REJECTED: &str = "`-` rows are not valid; hashline ranges already name the lines being \
     changed. To insert a literal line starting with `-`, write `+-…`.";

/// Error text emitted when a replace hunk has no body.
pub const EMPTY_REPLACE: &str = "`replace N..M:` needs at least one `+TEXT` body row. To delete lines, \
     use `delete N..M`.";

/// Error text emitted when a `replace block N:` hunk has no body.
pub const EMPTY_BLOCK: &str = "`replace block N:` needs at least one `+TEXT` body row. To delete a block, \
     use `delete N..M` with the block's line range.";

/// Error text emitted when a `replace block N:` anchor cannot be resolved to a
/// syntactic block (unrecognized language, blank/out-of-range line, no node
/// begins on line N such as a lone closing delimiter, or the resolved block has
/// a syntax error). Names the offending line and steers back to an explicit
/// `replace N..M:` range.
#[must_use]
pub fn block_unresolved_message(line: u32) -> String {
    format!(
        "`replace block {line}:` could not resolve a syntactic block beginning \
         on line {line}. The language may be unsupported, the line may be blank \
         or a closing delimiter, or the block may not parse. Use `replace \
         {line}..M:` with the block's explicit end line instead."
    )
}

/// Error text emitted when a `replace block N:` edit reaches a code path that
/// has no [`BlockResolver`] wired in. Indicates a host-configuration bug
/// rather than authored-input error.
pub const BLOCK_RESOLVER_UNAVAILABLE: &str = "`replace block N:` is not available here (no tree-sitter block resolver \
     is configured). Use `replace N..M:` with an explicit range.";

/// Internal invariant error: `apply_edits` received an unresolved `replace
/// block N:` edit. Block edits must be expanded by `resolve_block_edits` before
/// reaching the applier; hitting this is a wiring bug, not authored-input error.
pub const UNRESOLVED_BLOCK_INTERNAL: &str = "internal error: unresolved `replace block` edit reached the applier \
     (resolve_block_edits was not run).";

/// Error text emitted when a delete hunk receives a body row.
pub const DELETE_TAKES_NO_BODY: &str =
    "`delete N..M` does not take body rows. Remove the body, or use `replace N..M:`.";

/// Error text emitted when a `delete block N` hunk receives a body row.
pub const DELETE_BLOCK_TAKES_NO_BODY: &str = "`delete block N` does not take body rows. Remove the body, or use \
     `replace block N:` to replace the block.";

/// Error text emitted when an insert hunk has no body.
pub const EMPTY_INSERT: &str = "`insert` needs at least one `+TEXT` body row.";

/// Warning text emitted by [`Recovery`] when an external write fits a cached
/// snapshot.
pub const RECOVERY_EXTERNAL_WARNING: &str = "Recovered from a stale file hash using a previous read snapshot (file \
     changed externally between read and edit).";

/// Warning text emitted by [`Recovery`] when a prior in-session edit advanced
/// the hash.
pub const RECOVERY_SESSION_CHAIN_WARNING: &str = "Recovered from a stale file hash using an earlier in-session snapshot \
     (the file hash advanced after a prior edit in this session).";

/// Warning text emitted by [`Recovery`] when the session-chain replay
/// fast-path was taken. Distinct from [`RECOVERY_SESSION_CHAIN_WARNING`]
/// because replay is the less-certain mode: the structured-patch 3-way
/// merge refused, the anchor-content gate passed, but a coincidental
/// insert+delete pair earlier in the chain could still leave an anchor's
/// line number pointing at a duplicated row. Surface the hedge so the
/// model verifies before continuing.
pub const RECOVERY_SESSION_REPLAY_WARNING: &str = "Recovered by replaying your edits onto the current file content — your \
     previous edit in this session changed line(s) you re-targeted with a stale \
     hash. Verify the diff matches your intent before continuing.";

/// Warning emitted when an `insert head:` / `insert tail:` edit is applied to an
/// existing file whose snapshot tag is stale (the file drifted since the read).
/// Head/tail insert position is content-independent — "start"/"end" cannot move
/// with drift — so this is non-fatal: the edit applies onto the live content and
/// we surface the drift instead of hard-failing (unlike an anchored mismatch).
pub const HEADTAIL_DRIFT_WARNING: &str = "Applied an `insert head:`/`insert tail:` edit onto the current file \
     content even though the snapshot tag was stale (the file changed since \
     your read). Head/tail position is content-independent, so the insert \
     was not rejected — but re-read if the drift was unexpected.";

/// Error text emitted when a hashline section omits the mandatory snapshot tag.
/// The tag is REQUIRED on every section, enforced identically by the apply path
/// and the preview/diff path, so both surfaces reuse this single builder to
/// stay in lockstep.
#[must_use]
pub fn missing_snapshot_tag_message(section_path: &str) -> String {
    format!(
        "Missing hashline snapshot tag for edit to {section_path}; use \
         `{HL_FILE_PREFIX}{section_path}{HL_FILE_HASH_SEP}tag` from your latest read/search output. To create \
         a new file, use the write tool."
    )
}
