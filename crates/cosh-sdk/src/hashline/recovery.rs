//! Recover from a stale section snapshot tag by replaying the would-be edit
//! against a cached pre-edit snapshot of the file and 3-way-merging the
//! result onto the current on-disk content.
//!
//! The patcher consults this when a section tag resolves to a snapshot that no
//! longer matches the live file content. The recovery class is stateless apart
//! from the [`SnapshotStore`] it queries; the snapshot store is the seam
//! that lets you plug in your own caching strategy.
use super::apply::apply_edits;
use super::diff::{apply_patch, structured_patch};
use super::messages::{
    RECOVERY_EXTERNAL_WARNING, RECOVERY_SESSION_CHAIN_WARNING, RECOVERY_SESSION_REPLAY_WARNING,
};
use super::snapshots::{Snapshot, SnapshotStore};
use super::types::{Anchor, Cursor, Edit};

/// Arguments for [`Recovery::try_recover`].
#[derive(Debug, Clone)]
pub struct RecoveryArgs {
    pub path: String,
    pub current_text: String,
    pub file_hash: String,
    pub edits: Vec<Edit>,
}

/// Post-recovery state returned by [`Recovery::try_recover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryResult {
    /// Post-recovery text.
    pub text: String,
    /// First changed line (1-indexed) relative to the live `current_text`, or `None`.
    pub first_changed_line: Option<usize>,
    /// Warnings collected during recovery, including the user-facing recovery banner.
    pub warnings: Vec<String>,
}

fn apply_edits_to_snapshot(
    previous_text: &str,
    current_text: &str,
    edits: &[Edit],
    recovery_warning: &str,
) -> Option<RecoveryResult> {
    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        apply_edits(previous_text, edits)
    }))
    .ok()?;

    if applied.text == previous_text {
        return None;
    }

    let patch = structured_patch(previous_text, &applied.text, 3);
    let merged = apply_patch(current_text, &patch)?;

    if merged == current_text {
        return None;
    }

    let first_changed_line = find_first_changed_line(current_text, &merged)
        .or_else(|| applied.first_changed_line.map(|l| l as usize));
    let has_net_change = first_changed_line.is_some();
    let mut warnings = applied.warnings;
    if has_net_change {
        warnings.insert(0, recovery_warning.to_string());
    }

    Some(RecoveryResult {
        text: merged,
        first_changed_line,
        warnings,
    })
}

fn collect_anchor_lines(edits: &[Edit]) -> Vec<u32> {
    edits
        .iter()
        .flat_map(get_edit_anchors)
        .map(|a| a.line)
        .collect()
}

fn get_edit_anchors(edit: &Edit) -> Vec<Anchor> {
    match edit {
        Edit::Delete { anchor, .. } | Edit::Block { anchor, .. } => vec![*anchor],
        Edit::Insert { cursor, .. } => match cursor {
            Cursor::BeforeAnchor(a) | Cursor::AfterAnchor(a) => vec![*a],
            _ => vec![],
        },
    }
}

/// Returns true when every anchor line in `edits` has identical content in
/// `previous_text` and `current_text`. The session-chain replay fast-path
/// requires this: if the prior in-session edit rewrote the line the model is
/// now re-targeting with a stale hash, replaying onto current would silently
/// overwrite the new content with whatever the model authored against the
/// old content — a corruption window, not a recovery.
fn verify_anchor_content(previous_text: &str, current_text: &str, edits: &[Edit]) -> bool {
    let lines = collect_anchor_lines(edits);
    if lines.is_empty() {
        return true;
    }
    let prev: Vec<&str> = previous_text.split('\n').collect();
    let curr: Vec<&str> = current_text.split('\n').collect();
    for line in &lines {
        let idx = (*line as usize).saturating_sub(1);
        if idx >= prev.len() || idx >= curr.len() {
            return false;
        }
        if prev[idx] != curr[idx] {
            return false;
        }
    }
    true
}

fn replay_session_chain_on_current(
    previous_text: &str,
    current_text: &str,
    edits: &[Edit],
) -> Option<RecoveryResult> {
    // Two guards narrow the corruption window. Neither alone is sufficient,
    // and even together they don't fully prove correctness — replay is the
    // less-certain recovery mode and emits RECOVERY_SESSION_REPLAY_WARNING
    // so the caller can verify the diff.
    //   - Equal line counts: every line number in `edits` still resolves to
    //     SOME logical row (no net shift across the prior chain). A
    //     coincidental insert+delete pair can still leave indices pointing
    //     at different logical rows than the model anchored against.
    //   - Anchor-content alignment: the row at each anchor's line index has
    //     identical content in previous and current. Catches the common
    //     case of a prior edit rewriting the targeted line; can still be
    //     coincidentally satisfied by a duplicated row at the shifted
    //     index.
    if previous_text.split('\n').count() != current_text.split('\n').count() {
        return None;
    }
    if !verify_anchor_content(previous_text, current_text, edits) {
        return None;
    }

    let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        apply_edits(current_text, edits)
    }))
    .ok()?;

    if applied.text == current_text {
        return None;
    }

    let mut warnings = applied.warnings;
    warnings.insert(0, RECOVERY_SESSION_REPLAY_WARNING.to_string());

    Some(RecoveryResult {
        text: applied.text,
        first_changed_line: applied.first_changed_line.map(|l| l as usize),
        warnings,
    })
}

/// First 1-indexed line at which `a` and `b` diverge, or `None` if equal.
fn find_first_changed_line(a: &str, b: &str) -> Option<usize> {
    if a == b {
        return None;
    }
    let a_lines: Vec<&str> = a.split('\n').collect();
    let b_lines: Vec<&str> = b.split('\n').collect();
    let max = a_lines.len().max(b_lines.len());
    for i in 0..max {
        if a_lines.get(i) != b_lines.get(i) {
            return Some(i + 1);
        }
    }
    None
}

fn is_head_snapshot(head: Option<&Snapshot>, snapshot: &Snapshot) -> bool {
    head == Some(snapshot)
}

/// Stateless recovery driver over a [`SnapshotStore`]. Construct once and
/// call [`try_recover`](Self::try_recover) per stale-tag incident. The default
/// implementation tries two strategies in order:
///
/// 1. Apply the edits on the full-file version the tag names, then 3-way-merge
///    the resulting patch onto the live content (handles external writes).
/// 2. (Session chain) If that version was not the head, replay the edits onto
///    the live content directly when line counts match AND every edit's anchor
///    line content is unchanged between version and current — a prior in-session
///    edit advanced the tag and the model's anchors still name the same logical
///    rows. Emits a dedicated [`RECOVERY_SESSION_REPLAY_WARNING`] because
///    even with both guards a coincidental insert+delete pair on duplicate rows
///    can still land the edit on the wrong row.
pub struct Recovery<S> {
    store: S,
}

impl<S: SnapshotStore> Recovery<S> {
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    /// Attempt recovery. Returns `None` when no path forward is found — the
    /// caller should then surface a [`MismatchError`].
    pub fn try_recover(&mut self, args: &RecoveryArgs) -> Option<RecoveryResult> {
        let snapshot = self.store.by_hash(&args.path, &args.file_hash)?;

        let head = self.store.head(&args.path);
        let is_head = is_head_snapshot(head.as_ref(), &snapshot);

        let recovery_warning = if is_head {
            RECOVERY_EXTERNAL_WARNING
        } else {
            RECOVERY_SESSION_CHAIN_WARNING
        };

        let mut recovered = apply_edits_to_snapshot(
            &snapshot.text,
            &args.current_text,
            &args.edits,
            recovery_warning,
        );
        if recovered.is_none() && !is_head {
            // Session-chain fallback: the 3-way merge on the version refused.
            // Replay onto current is gated by line-count equality AND
            // anchor-content alignment — see `replay_session_chain_on_current`
            // for why both guards together still don't fully prove correctness.
            recovered = replay_session_chain_on_current(
                &snapshot.text,
                &args.current_text,
                &args.edits,
            );
        }

        let mut result = recovered?;
        self.append_unseen_anchor_warning(&mut result, args);
        Some(result)
    }

    /// Append a warning when the edit anchors target lines the model never saw
    /// surfaced by the tool output that minted the stale tag. The snapshot
    /// itself may be older than what the model saw (the tag fingerprints the
    /// full file, but grep only surfaced a window of lines), so an edit aimed
    /// outside that window is the model working against unseen content — worth
    /// a "verify the diff" hedge. Silent when no seen lines were recorded for
    /// the tag (e.g. the file was read via a tool that does not track them).
    fn append_unseen_anchor_warning(
        &mut self,
        result: &mut RecoveryResult,
        args: &RecoveryArgs,
    ) {
        let seen = self.store.seen_lines(&args.path, &args.file_hash);
        if seen.is_empty() {
            return;
        }
        let seen_set: std::collections::HashSet<u32> = seen.iter().map(|(l, _)| *l).collect();
        let unseen: Vec<u32> = collect_anchor_lines(&args.edits)
            .into_iter()
            .filter(|line| !seen_set.contains(line))
            .collect();
        if unseen.is_empty() {
            return;
        }
        let listed = unseen
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        result.warnings.push(format!(
            "Edit anchors target lines {listed} which were not surfaced by the preceding tool output; verify the diff against the current file."
        ));
    }
}
