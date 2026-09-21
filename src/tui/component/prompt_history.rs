//! Edit history (undo/redo) for the chat prompt draft — the Ctrl+Z / Ctrl+Y
//! feature.
//!
//! # Responsibilities (two contexts, one mechanism)
//!
//! 1. **Plain editing** (`Ctrl+Z`): remove the last burst of typing /
//!    deletion, matching the universal editor convention. This falls out of
//!    *group coalescing*: consecutive single-character type operations merge
//!    into one undo group (capped, so one `Ctrl+Z` removes roughly the last
//!    sentence, not the whole message), and the next different operation kind
//!    (delete ↔ type) or any whole-draft replacement opens a new group.
//! 2. **Prompt correction** (`Ctrl+Z` right after the corrector rewrote the
//!    draft): the correction is recorded as ONE atomic group, so a single
//!    `Ctrl+Z` jumps back to the exact pre-correction text — never to some
//!    mid-correction state. A [`Marker`] on the snapshot detects the
//!    transition so the app can toast "original restored" (and `Ctrl+Y`
//!    toasts "correction reapplied").
//!
//! # Shape
//!
//! A mirror snapshot (`current`) plus an undo stack of *previous* states and
//! a redo stack of *undone-away* states. Every recorded operation receives
//! the post-mutation prompt state, so the mirror always converges back to
//! the real `PromptView::input` even when an intermediate mutation (the
//! paste-burst retraction drain) is not individually recorded: the next
//! record captures the drifted state, and the group it opens still points at
//! the semantically correct "before" snapshot.
//!
//! # Boundaries
//!
//! Sending a message and dismissing the slash menu clear the draft AND the
//! history: the edit history belongs to a single draft context. Sent text
//! stays reachable through the existing ↑/↓ message history; walking undo
//! across draft boundaries would resurface stale slash-command states.

/// Maximum undo steps retained. Each step holds a full prompt snapshot, but
/// the prompt is capped at 20 wrapped lines, so memory is bounded and small.
const MAX_UNDO_ENTRIES: usize = 100;

/// Consecutive same-kind character operations merged into ONE undo group.
/// ~32 chars ≈ a short sentence: one `Ctrl+Z` after a typing pause removes
/// the last sentence-ish burst, matching universal Ctrl+Z granularity.
const MAX_COALESCED_OPS: usize = 32;

/// What a recorded draft mutation was. [`EditKind::Type`] and
/// [`EditKind::Delete`] coalesce into groups; everything else is its own
/// atomic group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditKind {
    /// A character inserted at the cursor (typing, Ctrl+J newline).
    Type,
    /// Characters removed behind/at the cursor (backspace, delete, Ctrl+W).
    Delete,
    /// A paste (bracketed, clipboard, or replayed paste-burst): atomic.
    Paste,
    /// The whole draft replaced programmatically (queue re-edit, message
    /// actions, slash command, gateway restore, ↑/↓ history browsing).
    Replace,
    /// The one-off prompt corrector rewrote the draft: atomic, and marked so
    /// undoing across it reports [`UndoOutcome::OriginalBeforeCorrection`].
    Correction,
}

impl EditKind {
    /// The coalescing class of this kind. `None` = never merges with a
    /// neighbour (each operation is its own group).
    fn group(self) -> Option<GroupKind> {
        match self {
            EditKind::Type => Some(GroupKind::Type),
            EditKind::Delete => Some(GroupKind::Delete),
            EditKind::Paste | EditKind::Replace | EditKind::Correction => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
    Type,
    Delete,
}

/// Tags a snapshot as "the RESULT of a prompt correction". Only used to
/// classify the transition when the user undoes away from / redoes back to
/// such a state; the snapshot text itself is the full state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Correction,
}

/// Compressed-paste mappings captured with a draft state:
/// `(virtual_text, actual_text)` pairs, mirroring `PromptView::pasted_parts`.
pub(crate) type PartsSnapshot = Vec<(String, String)>;

/// A complete prompt state: the editable text, the cursor inside it, the
/// paste mappings, and an optional correction marker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
    parts: PartsSnapshot,
    marker: Option<Marker>,
}

/// An in-progress coalescing group: its class and how many operations have
/// merged into it so far.
#[derive(Clone, Copy, Debug)]
struct Group {
    kind: GroupKind,
    ops: usize,
}

/// Result of a `Ctrl+Z` step, for UI feedback in the key handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UndoOutcome {
    /// Nothing to undo: the stack is at the draft's initial state.
    Noop,
    /// A normal step back (last typing/deletion group, paste, replacement).
    Step,
    /// The undone-away state was a correction result: the draft now holds
    /// the ORIGINAL text from before the corrector ran.
    OriginalBeforeCorrection,
}

/// Result of a `Ctrl+Y` step, for UI feedback in the key handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RedoOutcome {
    /// Nothing to redo.
    Noop,
    /// A normal step forward.
    Step,
    /// The restored state is a correction result: the corrector's rewrite
    /// was reapplied.
    Correction,
}

#[derive(Debug)]
pub(crate) struct PromptHistory {
    /// Mirrors the prompt state as of the last recorded operation.
    current: Snapshot,
    /// States strictly before `current`, newest last. Never contains
    /// `current`.
    undo_stack: Vec<Snapshot>,
    /// States undone away from, newest last; consumed by redo.
    redo_stack: Vec<Snapshot>,
    /// The coalescing group currently absorbing same-kind operations.
    group: Option<Group>,
}

impl PromptHistory {
    pub(crate) fn new() -> Self {
        Self {
            current: Snapshot {
                text: String::new(),
                cursor: 0,
                parts: Vec::new(),
                marker: None,
            },
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            group: None,
        }
    }

    /// Record a draft mutation. `text`/`cursor` are the POST-mutation state.
    ///
    /// Coalescing: a [`EditKind::Type`] operation merges into an open
    /// Type group (and likewise Delete), up to [`MAX_COALESCED_OPS`]; any
    /// other kind, a kind switch, or an exceeded cap closes the group and
    /// opens a new one — the pre-edit mirror becomes the undo target.
    pub(crate) fn record(
        &mut self,
        text: String,
        cursor: usize,
        parts: PartsSnapshot,
        kind: EditKind,
    ) {
        if text == self.current.text
            && cursor == self.current.cursor
            && parts == self.current.parts
        {
            return;
        }

        let coalesces = match (self.group, kind.group()) {
            (Some(group), Some(kind)) => group.kind == kind && group.ops < MAX_COALESCED_OPS,
            _ => false,
        };

        if coalesces {
            self.group.as_mut().expect("checked above").ops += 1;
        } else {
            let previous = self.current.clone();
            self.undo_stack.push(previous);
            if self.undo_stack.len() > MAX_UNDO_ENTRIES {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.group = kind.group().map(|kind| Group { kind, ops: 1 });
        }

        let marker = (kind == EditKind::Correction).then_some(Marker::Correction);
        self.current = Snapshot {
            text,
            cursor,
            parts,
            marker,
        };
    }

    /// Record the prompt corrector's rewrite as one atomic, marked group.
    /// A corrected draft is plain model output: no compressed-paste
    /// placeholders from the old draft survive it.
    pub(crate) fn record_correction(&mut self, corrected: String, cursor: usize) {
        self.record(corrected, cursor, Vec::new(), EditKind::Correction);
    }

    /// `Ctrl+Z`: step back one group. Restores the snapshot the active
    /// group started from — for a fresh correction group that is the exact
    /// pre-correction draft, reported as
    /// [`UndoOutcome::OriginalBeforeCorrection`].
    pub(crate) fn undo(&mut self) -> UndoOutcome {
        // An open typing group ends here: undo always lands on a group
        // boundary, never on a mid-group intermediate state.
        self.group = None;
        let Some(previous) = self.undo_stack.pop() else {
            return UndoOutcome::Noop;
        };
        let leaving = std::mem::replace(&mut self.current, previous);
        let leaving_marker = leaving.marker;
        self.redo_stack.push(leaving);
        // Classify by the state LEFT BEHIND: a marker means the undone-away
        // state was a correction result, so `current` now holds the original
        // pre-correction draft. (The restored state itself never carries a
        // marker — corrections are atomic, so undoing past one always lands
        // directly on its "before" snapshot.)
        if leaving_marker.is_some() {
            UndoOutcome::OriginalBeforeCorrection
        } else {
            UndoOutcome::Step
        }
    }

    /// `Ctrl+Y`: step forward again, restoring the most recently undone
    /// state. Reports [`RedoOutcome::Correction`] when that state is a
    /// correction result.
    pub(crate) fn redo(&mut self) -> RedoOutcome {
        self.group = None;
        let Some(next) = self.redo_stack.pop() else {
            return RedoOutcome::Noop;
        };
        let restored_marker = next.marker;
        let leaving = std::mem::replace(&mut self.current, next);
        self.undo_stack.push(leaving);
        if restored_marker.is_some() {
            RedoOutcome::Correction
        } else {
            RedoOutcome::Step
        }
    }

    /// The mirrored `(text, cursor, parts)` state — what an undo/redo
    /// restored. `PromptView` applies it back to the real input.
    pub(crate) fn snapshot(&self) -> (&str, usize, &PartsSnapshot) {
        (&self.current.text, self.current.cursor, &self.current.parts)
    }

    /// Drop everything: the draft context ended (sent, dismissed).
    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate real typing: one `Type` operation per character, exactly how
    /// the key handler records keystrokes (coalescing is per-operation).
    fn typed(history: &mut PromptHistory, text: &str) {
        let mut current = String::new();
        for ch in text.chars() {
            current.push(ch);
            history.record(current.clone(), current.len(), Vec::new(), EditKind::Type);
        }
    }

    /// Simulate real backspaces: one `Delete` operation per removed character.
    fn deleted_from(history: &mut PromptHistory, text: &mut String, count: usize) {
        for _ in 0..count {
            text.pop();
            history.record(text.clone(), text.len(), Vec::new(), EditKind::Delete);
        }
    }

    fn text_of(history: &PromptHistory) -> &str {
        history.snapshot().0
    }

    #[test]
    fn typing_coalesces_into_one_undo_group() {
        let mut history = PromptHistory::new();
        typed(&mut history, "olá");
        typed(&mut history, "olá mundo");
        typed(&mut history, "olá mundo!");

        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
        // One group = one redo back to the full text.
        assert_eq!(history.redo(), RedoOutcome::Step);
        assert_eq!(text_of(&history), "olá mundo!");
    }

    #[test]
    fn undo_restores_the_last_coalesced_burst_not_the_whole_message() {
        let mut history = PromptHistory::new();
        typed(&mut history, "primeira frase");
        // A kind switch opens a new group: this delete burst is alone.
        let mut draft = "primeira frase".to_string();
        deleted_from(&mut history, &mut draft, 2);

        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "primeira frase");
    }

    #[test]
    fn the_coalescing_cap_opens_a_new_group() {
        let mut history = PromptHistory::new();
        let full: String = "x".repeat(MAX_COALESCED_OPS + 10);
        typed(&mut history, &full);

        // Cap exceeded → second group; first Ctrl+Z only trims the excess.
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history).len(), MAX_COALESCED_OPS);
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
    }

    #[test]
    fn correction_undo_restores_the_original_text() {
        let mut history = PromptHistory::new();
        typed(&mut history, "texto originnal com erros");
        history.record_correction(
            "Texto original, com erros corrigidos e organizados.".into(),
            52,
        );

        // First Ctrl+Z after a correction: the WHOLE rewrite reverts.
        assert_eq!(history.undo(), UndoOutcome::OriginalBeforeCorrection);
        assert_eq!(text_of(&history), "texto originnal com erros");
        // And Ctrl+Y reapplies the correction, flagged as such.
        assert_eq!(history.redo(), RedoOutcome::Correction);
        assert_eq!(
            text_of(&history),
            "Texto original, com erros corrigidos e organizados."
        );
    }

    #[test]
    fn undo_after_editing_a_correction_first_reverts_the_edits() {
        let mut history = PromptHistory::new();
        typed(&mut history, "rascunho");
        history.record_correction("Rascunho corrigido.".into(), 19);
        // The user fixes a typo in the corrected text.
        typed(&mut history, "Rascunho corrigido. Ok");

        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "Rascunho corrigido.");
        // Next Ctrl+Z crosses the correction boundary.
        assert_eq!(history.undo(), UndoOutcome::OriginalBeforeCorrection);
        assert_eq!(text_of(&history), "rascunho");
    }

    #[test]
    fn paste_and_replace_are_atomic_groups() {
        let mut history = PromptHistory::new();
        typed(&mut history, "inicio ");
        history.record(
            "[Pasted ~5 lines]".to_string(),
            17,
            vec![("[Pasted ~5 lines]".to_string(), "a\nb\nc\nd\ne".to_string())],
            EditKind::Paste,
        );
        history.record("/model ".to_string(), 7, Vec::new(), EditKind::Replace);

        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "[Pasted ~5 lines]");
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "inicio ");
        assert_eq!(history.redo(), RedoOutcome::Step);
        assert_eq!(text_of(&history), "[Pasted ~5 lines]");
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut history = PromptHistory::new();
        typed(&mut history, "abc");
        assert_eq!(history.undo(), UndoOutcome::Step);
        typed(&mut history, "x");

        assert_eq!(history.redo(), RedoOutcome::Noop);
        assert_eq!(text_of(&history), "x");
    }

    #[test]
    fn undo_on_a_fresh_history_is_a_noop() {
        let mut history = PromptHistory::new();
        assert_eq!(history.undo(), UndoOutcome::Noop);
        assert_eq!(history.redo(), RedoOutcome::Noop);
    }

    #[test]
    fn clear_resets_everything() {
        let mut history = PromptHistory::new();
        typed(&mut history, "rascunho");
        history.clear();
        assert_eq!(history.undo(), UndoOutcome::Noop);
    }

    #[test]
    fn the_undo_stack_never_grows_past_the_cap() {
        let mut history = PromptHistory::new();
        for i in 0..(MAX_UNDO_ENTRIES + 25) {
            // Replace (never coalesces) — one undo group per iteration, so
            // the stack actually reaches its cap.
            history.record(format!("draft {i}"), 0, Vec::new(), EditKind::Replace);
        }
        for _ in 0..MAX_UNDO_ENTRIES {
            assert_eq!(history.undo(), UndoOutcome::Step);
        }
        assert_eq!(history.undo(), UndoOutcome::Noop);
    }
}
