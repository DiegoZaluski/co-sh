//! Centralized error and warning text emitted by the hashline parser, applier,
//!
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
///
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
///
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

/// Internal invariant error: `apply_edits` received an unresolved `replace block N:` edit.
///
/// Block edits must be expanded by `resolve_block_edits` before
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
///
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
///
/// existing file whose snapshot tag is stale (the file drifted since the read).
/// Head/tail insert position is content-independent — "start"/"end" cannot move
/// with drift — so this is non-fatal: the edit applies onto the live content and
/// we surface the drift instead of hard-failing (unlike an anchored mismatch).
pub const HEADTAIL_DRIFT_WARNING: &str = "Applied an `insert head:`/`insert tail:` edit onto the current file \
     content even though the snapshot tag was stale (the file changed since \
     your read). Head/tail position is content-independent, so the insert \
     was not rejected — but re-read if the drift was unexpected.";

/// Error text emitted when a hashline section omits the mandatory snapshot tag.
///
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

/// Replacement body indentation was aligned from unchanged structural rows.
pub const REPLACEMENT_INDENT_AUTO_SHIFT_WARNING: &str =
    "Auto-indented a replacement body to match unchanged structural rows in its source range.";

/// Exact-text boundary rows were removed because the remaining payload covers
/// the selected range and the same rows already survive immediately outside it.
#[must_use]
pub fn textual_boundary_echo_warning(start_line: u32, leading: u32, trailing: u32) -> String {
    let mut parts = Vec::new();
    if leading > 0 {
        parts.push(format!("{leading} leading"));
    }
    if trailing > 0 {
        parts.push(format!("{trailing} trailing"));
    }
    let joined = parts.join(" and ");
    format!(
        "Auto-repaired a replacement boundary echo at line {start_line}: dropped {joined} body \
         line(s) already present outside the range. Issue the body as final content for the \
         selected range only."
    )
}

/// A boundary variant repair fired: a syntax-essential selected boundary row
/// was retained, or exact body echoes of surviving outside rows were removed.
/// The authored result did not parse and the selected result does.
#[must_use]
pub fn boundary_variant_repair_warning(start_line: u32, kept: u32, dropped: u32) -> String {
    let kept_part = if kept == 0 {
        None
    } else {
        Some(format!(
            "retained {kept} syntax-essential source boundary row(s) selected by the range"
        ))
    };
    let dropped_part = if dropped == 0 {
        None
    } else {
        Some(format!(
            "dropped {dropped} body row(s) duplicated just outside the range"
        ))
    };
    let actions: Vec<String> = kept_part.into_iter().chain(dropped_part).collect();
    let action = actions.join(" and ");
    format!(
        "Auto-repaired replacement boundaries at line {start_line}: {action}. The result was \
         verified by the syntax probe — re-issue with the range covering exactly the changed \
         lines and the body as their complete final content."
    )
}

/// Which boundary side an ambiguous echo appeared on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundarySide {
    Leading,
    Trailing,
}

/// A one-sided exact boundary echo cannot cover the selected range.
///
/// After the duplicated body rows are removed, applying or dropping it would
/// lose distinct range content, so the edit is rejected unless a
/// parse-restoring boundary combination proves another reading.
#[must_use]
pub fn ambiguous_boundary_echo_message(
    start_line: u32,
    end_line: u32,
    side: BoundarySide,
    count: u32,
) -> String {
    let where_clause = match side {
        BoundarySide::Leading => {
            format!("opens by restating the {count} line(s) just above the range")
        },
        BoundarySide::Trailing => {
            format!("ends by restating the {count} line(s) just below the range")
        },
    };
    format!(
        "`replace {start_line}..{end_line}:` rejected: the body {where_clause}, but is too \
         short to be the full final content of the selected range. Re-issue with the range \
         covering exactly the lines that change and the body as their complete final content."
    )
}

/// A syntax-essential selected edge can be retained on either side of the
/// payload, but indentation does not establish which placement was intended.
#[must_use]
pub fn ambiguous_boundary_placement_message(start_line: u32, end_line: u32) -> String {
    format!(
        "`replace {start_line}..{end_line}:` rejected: a selected boundary row is required \
         for the file to parse, but the body indentation does not establish whether it belongs \
         before or after that row. Re-read the region and re-issue with a range that excludes \
         every unchanged boundary row."
    )
}

/// The applied result no longer parses while the pre-edit content did.
///
/// The patch introduced a syntax error. Advisory, never a rejection — the
/// applier honors the authored edit — but the breakage is machine-confirmed
/// by tree-sitter and surfaced in the same response instead of waiting for a
/// compiler pass.
#[must_use]
pub fn edit_broke_parse_warning(first_changed_line: Option<u32>) -> String {
    let at = match first_changed_line {
        Some(line) => format!(" near line {line}"),
        None => String::new(),
    };
    format!(
        "This edit introduced a syntax error{at}: the file parsed before the patch and no \
         longer does. It was applied exactly as written, so a line number or range endpoint \
         is likely wrong — re-read the touched region and re-issue a correcting edit."
    )
}

/// `insert after N:` body indented shallower than the anchor: the landing
/// moved forward past trailing closer lines — the common "anchored on the
/// last line I read instead of after the block" mistake.
#[must_use]
pub fn after_insert_landing_shift_warning(
    anchor_line: u32,
    landing_line: u32,
    crossed: u32,
) -> String {
    let s = if crossed == 1 { "" } else { "s" };
    format!(
        "insert after {anchor_line}: body indented shallower than the anchor, so the landing \
         moved past {crossed} closing line{s} to after line {landing_line}. For the deeper \
         position inside the block, re-issue with the body indented to match."
    )
}

/// Plain `insert after N:` anchored on a block-opener line with a shallower
/// body. The landing was moved past the whole block — anchoring on an opener
/// places the body between the opener and its first statement, a position a
/// body at the opener's depth or above never intends.
#[must_use]
pub fn after_insert_opener_escape_warning(anchor_line: u32, landing_line: u32) -> String {
    format!(
        "insert after {anchor_line}: line {anchor_line} opens a block, and the body's \
         indentation claims a position outside it, so the body was landed after line \
         {landing_line} (verified by the syntax probe). To insert after a whole construct, \
         anchor on its closing line."
    )
}
