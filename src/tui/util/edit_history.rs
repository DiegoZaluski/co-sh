//! Edit history (undo/redo) shared by text-entry surfaces — the Ctrl+Z /
//! Ctrl+Y feature. Extracted from the chat prompt so other editable
//! fields reuse the exact same mechanism instead of reimplementing it.
//!
//! # Responsibilities (three contexts, one mechanism)
//!
//! 1. **Plain editing** (`Ctrl+Z`): undo steps follow the standard
//!    *time-based coalescing of continuous typing*, the same criterion
//!    mainstream editors use. Consecutive `Type`/`Delete` operations
//!    closer than [`COALESCE_WINDOW`] apart (500ms — ProseMirror's
//!    `newGroupDelay` default; Quip uses ~1s, Google Docs ~2s, per the
//!    Lexical history docs) AND moving the caret by at most one character
//!    merge into ONE undo step — one `Ctrl+Z` removes the chunk of text
//!    typed without a pause, not char by char and not the whole draft.
//!    Switching between typing and backspace keeps the session (users fix
//!    typos mid-flow); a longer pause, a caret jump (selection replace,
//!    word delete) or any atomic operation closes it.
//! 2. **Prompt correction** (`Ctrl+Z` right after the corrector rewrote the
//!    draft): the correction is recorded as ONE atomic step, so a single
//!    `Ctrl+Z` jumps back to the exact pre-correction text — never to some
//!    mid-correction state. A [`Marker`] on the snapshot detects the
//!    transition so the app can toast "original restored" (and `Ctrl+Y`
//!    toasts "correction reapplied").
//! 3. **Simple text fields** (question dialog answers, registration
//!    fields): the same stacks and coalescing via [`Self::record_text`] —
//!    a `record` variant without the prompt's compressed-paste mappings.
//!
//! # Shape
//!
//! A mirror snapshot (`current`) plus an undo stack of *previous* states and
//! a redo stack of *undone-away* states. Every recorded operation receives
//! the post-mutation state, so the mirror always converges back to the real
//! field text even when an intermediate mutation (the paste-burst retraction
//! drain) is not individually recorded: the next record captures the
//! drifted state and still points at the semantically correct "before"
//! snapshot.
//!
//! # Boundaries
//!
//! Sending a message and dismissing the slash menu clear the draft AND the
//! history: the edit history belongs to a single draft context. Sent text
//! stays reachable through the existing ↑/↓ message history; walking undo
//! across draft boundaries would resurface stale slash-command states.

use std::time::{Duration, Instant};

/// Maximum undo steps retained. Each step holds a full snapshot, so memory
/// is bounded and small; the cap trims the OLDEST steps first, exactly like
/// editor undo depth.
const MAX_UNDO_ENTRIES: usize = 100;

/// The coalescing window for continuous typing (`Type`/`Delete`): edits
/// closer than this in time merge into ONE undo step. 500ms matches
/// ProseMirror's `newGroupDelay` default; editors commonly sit in the
/// 500ms–2s range (Quip ~1s, Google Docs ~2s per the Lexical history docs).
const COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// The caret delta a continuous-typing edit produces: +1 (typed a char),
/// −1 (backspace) or 0 (forward delete). A bigger delta means the edit was
/// NOT part of the typing flow (selection replace, word delete) and closes
/// the coalescing session.
const MAX_CONTINUOUS_CARET_DELTA: i64 = 1;

/// What a recorded draft mutation was. [`EditKind::Type`] and
/// [`EditKind::Delete`] coalesce by TIME into the continuous-typing step;
/// [`EditKind::Paste`], [`EditKind::Replace`] and [`EditKind::Correction`]
/// are atomic single steps because they were one visible action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// A character inserted at the cursor (typing, Ctrl+J newline).
    Type,
    /// Characters removed behind/at the cursor (backspace, delete, Ctrl+W).
    /// A single-char backspace keeps the typing session; a wider removal
    /// (word delete, selection delete) closes it.
    Delete,
    /// A paste (bracketed, clipboard, or replayed paste-burst): one step.
    Paste,
    /// The whole draft replaced programmatically (queue re-edit, message
    /// actions, slash command, gateway restore, ↑/↓ history browsing): one
    /// step.
    Replace,
    /// The one-off prompt corrector rewrote the draft: one step, and marked
    /// so undoing across it reports
    /// [`UndoOutcome::OriginalBeforeCorrection`].
    Correction,
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
pub type PartsSnapshot = Vec<(String, String)>;

/// A complete prompt state: the editable text, the cursor inside it, the
/// paste mappings, and an optional correction marker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
    parts: PartsSnapshot,
    marker: Option<Marker>,
}

/// Result of a `Ctrl+Z` step, for UI feedback in the key handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoOutcome {
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
pub enum RedoOutcome {
    /// Nothing to redo.
    Noop,
    /// A normal step forward.
    Step,
    /// The restored state is a correction result: the corrector's rewrite
    /// was reapplied.
    Correction,
}

/// `Clone` lets embedding fields derive the same trait; `Default` is the
/// empty history (identical to `new()`).
#[derive(Debug, Clone)]
pub struct EditHistory {
    /// Mirrors the prompt state as of the last recorded operation.
    current: Snapshot,
    /// States strictly before `current`, newest last. Never contains
    /// `current`.
    undo_stack: Vec<Snapshot>,
    /// States undone away from, newest last; consumed by redo.
    redo_stack: Vec<Snapshot>,
    /// Whether a continuous-typing session is open: consecutive
    /// `Type`/`Delete` edits merge into ONE undo step while this stays
    /// true and each edit lands within the coalescing window and moves
    /// the caret by at most one character.
    typing_open: bool,
    /// When the last edit was recorded — the coalescing clock.
    last_edit_at: Option<Instant>,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl EditHistory {
    pub fn new() -> Self {
        Self {
            current: Snapshot {
                text: String::new(),
                cursor: 0,
                parts: Vec::new(),
                marker: None,
            },
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            typing_open: false,
            last_edit_at: None,
        }
    }

    /// Record a draft mutation. `text`/`cursor` are the POST-mutation
    /// state; the pre-edit mirror becomes the undo target.
    ///
    /// Consecutive typing-flow edits (`Type`/`Delete`) merge into ONE undo
    /// step — one `Ctrl+Z` removes the chunk typed without a pause — while
    /// each edit lands within [`COALESCE_WINDOW`] of the previous one and
    /// moves the caret by at most one character. A longer pause, a caret
    /// jump (selection replace, word delete) or an atomic kind
    /// (paste/replace/correction) closes the session and starts a new step.
    pub fn record(&mut self, text: String, cursor: usize, parts: PartsSnapshot, kind: EditKind) {
        self.record_at(text, cursor, parts, kind, Instant::now());
    }

    /// [`Self::record`] with an explicit clock reading. Production callers
    /// use `record`; the explicit timestamp exists so tests can drive the
    /// coalescing window deterministically.
    pub fn record_at(
        &mut self,
        text: String,
        cursor: usize,
        parts: PartsSnapshot,
        kind: EditKind,
        at: Instant,
    ) {
        if text == self.current.text && cursor == self.current.cursor && parts == self.current.parts
        {
            return;
        }

        // The caret delta of THIS edit relative to the mirrored state: the
        // signature of the typing flow (+1 typed, −1 backspace, 0 forward
        // delete). Anything wider is a discrete gesture, not typing. The
        // delta is measured in CHARS — cursors are byte offsets, and a
        // multibyte char (é, ç) would otherwise look like a caret jump and
        // wrongly close the typing session.
        let char_at =
            |text: &str, byte: usize| text.char_indices().take_while(|(i, _)| *i < byte).count();
        let caret_delta =
            char_at(&text, cursor) as i64 - char_at(&self.current.text, self.current.cursor) as i64;
        let in_typing_flow = matches!(kind, EditKind::Type | EditKind::Delete)
            && caret_delta.abs() <= MAX_CONTINUOUS_CARET_DELTA;

        let coalesces = self.typing_open
            && in_typing_flow
            && self
                .last_edit_at
                .is_some_and(|last| at.duration_since(last) < COALESCE_WINDOW);

        if !coalesces {
            let previous = self.current.clone();
            self.undo_stack.push(previous);
            if self.undo_stack.len() > MAX_UNDO_ENTRIES {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            // A typing-flow edit opens a new continuous-typing session; a
            // wide delete (word delete, selection replace) is a discrete
            // gesture that closes it.
            self.typing_open = in_typing_flow;
        }

        self.last_edit_at = Some(at);
        let marker = (kind == EditKind::Correction).then_some(Marker::Correction);
        self.current = Snapshot {
            text,
            cursor,
            parts,
            marker,
        };
    }

    /// Record the prompt corrector's rewrite as one atomic, marked step.
    /// A corrected draft is plain model output: no compressed-paste
    /// placeholders from the old draft survive it.
    pub fn record_correction(&mut self, corrected: String, cursor: usize) {
        self.record(corrected, cursor, Vec::new(), EditKind::Correction);
    }

    /// Record a mutation for a surface with no compressed-paste mappings
    /// (question dialog answers, registration fields): identical semantics
    /// to [`Self::record`], with an empty parts snapshot. The text/cursor
    /// pair is the POST-mutation state.
    pub fn record_text(&mut self, text: String, cursor: usize, kind: EditKind) {
        self.record(text, cursor, Vec::new(), kind);
    }

    /// `Ctrl+Z`: step back one undo step. Continuous typing coalesces into
    /// one step (see [`Self::record`]), so a step removes the whole chunk
    /// typed without a pause; a `Ctrl+Z` right after a correction lands on
    /// the exact pre-correction draft, reported as
    /// [`UndoOutcome::OriginalBeforeCorrection`].
    ///
    /// Undoing closes the open typing session: whatever is typed next
    /// starts a NEW step, never merging into the step just undone (the
    /// standard editor behavior that also keeps the redo branch clean).
    pub fn undo(&mut self) -> UndoOutcome {
        self.typing_open = false;
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
    pub fn redo(&mut self) -> RedoOutcome {
        // Redo also closes the typing session: the next edit opens a fresh
        // step (and a fresh edit discards this redo branch anyway).
        self.typing_open = false;
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
    pub fn snapshot(&self) -> (&str, usize, &PartsSnapshot) {
        (&self.current.text, self.current.cursor, &self.current.parts)
    }

    /// Drop everything: the draft context ended (sent, dismissed).
    pub fn clear(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A controllable clock for the coalescing window: tests advance it in
    /// exact steps instead of sleeping.
    struct Clock(Instant);

    impl Clock {
        fn start() -> Self {
            Self(Instant::now())
        }

        fn advance(&mut self, ms: u64) {
            self.0 += Duration::from_millis(ms);
        }

        /// Simulate real typing: one `Type` operation per character on this
        /// clock, exactly how the key handler records keystrokes. Typing
        /// CONTINUES from the history's mirrored state (real editing
        /// appends to the live draft).
        fn typed(&mut self, history: &mut EditHistory, text: &str) {
            let mut current = history.snapshot().0.to_string();
            let mut cursor = history.snapshot().1;
            for ch in text.chars() {
                current.push(ch);
                // Cursors are BYTE offsets: advance by the char's UTF-8
                // length, like `PromptView::type_char`.
                cursor += ch.len_utf8();
                self.advance(10);
                history.record_at(current.clone(), cursor, Vec::new(), EditKind::Type, self.0);
            }
        }

        /// Simulate real backspaces: one `Delete` operation per removed
        /// character on this clock.
        fn deleted_from(&mut self, history: &mut EditHistory, text: &mut String, count: usize) {
            for _ in 0..count {
                text.pop();
                self.advance(10);
                history.record_at(
                    text.clone(),
                    text.len(),
                    Vec::new(),
                    EditKind::Delete,
                    self.0,
                );
            }
        }
    }

    fn text_of(history: &EditHistory) -> &str {
        history.snapshot().0
    }

    #[test]
    fn continuous_typing_coalesces_into_one_step() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "olá mundo");

        // Standard editor behavior: one Ctrl+Z removes the WHOLE chunk
        // typed without a pause (keystrokes 10ms apart, well inside the
        // 500ms window) — not char by char, not the whole draft history.
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
        // One step = one redo back to the full text.
        assert_eq!(history.redo(), RedoOutcome::Step);
        assert_eq!(text_of(&history), "olá mundo");
    }

    #[test]
    fn a_pause_beyond_the_window_opens_a_new_step() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "olá");
        // The user pauses to think (600ms > the 500ms window)...
        clock.advance(600);
        clock.typed(&mut history, " olá mundo");

        // ...so the second burst is its own step: the first Ctrl+Z removes
        // only the text typed after the pause.
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "olá");
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
    }

    #[test]
    fn backspaces_within_the_window_join_the_typing_session() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "abc");
        // Fixing a typo mid-flow: two backspaces right after the typing.
        clock.deleted_from(&mut history, &mut "abc".to_string(), 2);

        // Kind switches do NOT break the session (the Lexical history doc:
        // users type, backspace and retype in one continuous flow) — the
        // whole session is ONE step back to the pre-typing state.
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
    }

    #[test]
    fn a_caret_jump_opens_a_new_step() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "abc ");
        // Ctrl+W deletes the word before the cursor: the caret jumps from
        // 4 to 0 — a discrete gesture, not the typing flow.
        history.record_text(String::new(), 0, EditKind::Delete);

        // The word delete is its own step...
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "abc ");
        // ...and the typing before it is the next step back.
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
    }

    #[test]
    fn undo_closes_the_typing_session_for_the_next_edit() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "abc");
        assert_eq!(history.undo(), UndoOutcome::Step);

        // Typed right after the undo (well within the window), but the
        // undo CLOSED the session: the new edit starts a fresh step rather
        // than merging into the one just undone.
        clock.typed(&mut history, "d");
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "");
    }

    #[test]
    fn correction_undo_restores_the_original_text() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "texto originnal com erros");
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
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "rascunho");
        history.record_correction("Rascunho corrigido.".into(), 19);
        // The user appends a note to the corrected text in one continuous
        // flow (keystrokes 10ms apart). The correction itself closed the
        // typing session, so the note opens a fresh step that absorbs the
        // whole append.
        clock.typed(&mut history, " Ok");

        // One Ctrl+Z removes the appended note as a whole...
        assert_eq!(history.undo(), UndoOutcome::Step);
        assert_eq!(text_of(&history), "Rascunho corrigido.");
        // ...and the NEXT one crosses the correction boundary in a single
        // atomic step back to the exact pre-correction draft.
        assert_eq!(history.undo(), UndoOutcome::OriginalBeforeCorrection);
        assert_eq!(text_of(&history), "rascunho");
    }

    #[test]
    fn paste_and_replace_are_single_steps() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "inicio ");
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
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "abc");
        assert_eq!(history.undo(), UndoOutcome::Step);
        clock.typed(&mut history, "x");

        assert_eq!(history.redo(), RedoOutcome::Noop);
        assert_eq!(text_of(&history), "x");
    }

    #[test]
    fn undo_on_a_fresh_history_is_a_noop() {
        let mut history = EditHistory::new();
        assert_eq!(history.undo(), UndoOutcome::Noop);
        assert_eq!(history.redo(), RedoOutcome::Noop);
    }

    #[test]
    fn clear_resets_everything() {
        let mut clock = Clock::start();
        let mut history = EditHistory::new();
        clock.typed(&mut history, "rascunho");
        history.clear();
        assert_eq!(history.undo(), UndoOutcome::Noop);
    }

    #[test]
    fn the_undo_stack_never_grows_past_the_cap() {
        let mut history = EditHistory::new();
        for i in 0..(MAX_UNDO_ENTRIES + 25) {
            // One step per iteration, so the stack actually reaches its cap.
            history.record(format!("draft {i}"), 0, Vec::new(), EditKind::Replace);
        }
        for _ in 0..MAX_UNDO_ENTRIES {
            assert_eq!(history.undo(), UndoOutcome::Step);
        }
        assert_eq!(history.undo(), UndoOutcome::Noop);
    }
}
