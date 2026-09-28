//! Windows paste-burst coalescing — the fix for "a large paste is split into
//! one message per line, without pressing Enter" on Windows.
//!
//! WHY WINDOWS ONLY: on Linux/macOS a terminal paste is delivered as a single
//! [`crossterm::event::Event::Paste`] (bracketed paste `ESC[200~ … ESC[201~`),
//! handled in `super::events` (`App::process_event` → `PromptView::handle_paste`).
//! crossterm 0.29's Windows backend reads the console through
//! `ReadConsoleInput` and has NO bracketed-paste support
//! (crossterm-rs/crossterm#737; the VT-input hybrid PR #1030 is not released),
//! so the same paste reaches the TUI as ONE PLAIN KEY EVENT PER CHARACTER
//! plus ONE PLAIN `Enter` PER LINE BREAK. Without coalescing, every plain
//! Enter falls through to the `SendMessage` keymap action and submits
//! whatever accumulated so far — one message per line, none of them
//! user-initiated.
//!
//! THE FIX — characters are NEVER held back. Every plain text key falls
//! through to the prompt through the normal typing path, so typing ALWAYS
//! renders in real time: a paste streams into the prompt exactly like the
//! pre-coalescer console behaviour, and a HELD KEY (OS auto-repeat, ~20-33ms
//! per repeat — the same cadence class as a paste burst) shows its
//! characters the moment they arrive. The vanish-until-release bug is
//! impossible by construction: there is nothing to vanish into.
//!
//! What IS classified is the PLAIN ENTER: an Enter arriving within
//! [`BURST_WINDOW`] of recent text keys — armed or not — is a paste line
//! break, not a user submit (a human cannot hit Enter within 80ms of the
//! last character; motor time alone is ~150ms, while a console paste
//! delivers the next record in <1ms). Absorbed Enters go into a SHADOW copy
//! of the run ([`PasteBurstState::shadow`]) alongside a mirror of every
//! streamed character, preserving arrival order.
//!
//! When the burst ends (no key of it for [`BURST_WINDOW`], checked once per
//! frame from `run()` and before every key that falls through), runs that
//! absorbed at least one Enter are RETRACTED from the prompt and replayed
//! through `PromptView::handle_paste` — exactly the Linux/macOS path: line
//! endings normalize and long pastes compress into the `[Pasted ~N lines]`
//! placeholder. Runs with no absorbed Enter (plain typing, held keys,
//! single-line pastes) are left exactly as typed — a replay would be a
//! visual no-op at best and would compress fast human typing into a
//! placeholder at worst.
//!
//! ENTER CHAINING: blank lines inside a paste produce back-to-back Enters
//! with no text keys between them. The chain MAY continue across further
//! Enters within [`BURST_WINDOW`] — but only if the chain STARTED inside a
//! text burst ([`PasteBurstState::chain_started_in_text_burst`]). This
//! bounds the chain: once the run is flushed the flag resets, so a user
//! mashing Enter in the empty prompt can never be swallowed indefinitely.
//!
//! Known trade-offs (documented, self-healing): a real Enter pressed within
//! [`BURST_WINDOW`] of the last pasted/typed character is absorbed as a
//! newline — the next press, outside the window, submits normally. A
//! single-line paste (no line break) never triggers the shadow replay, so
//! it stays as typed instead of compressing into the placeholder — a
//! cosmetic divergence from Unix only; the content submitted is identical.
//!
//! WHY NO AUTO-REPEAT DISCRIMINATOR: an earlier iteration tried to tell a
//! held key apart from a paste by gap timing (same character, 15-75ms). It
//! fails on real Windows consoles: a paste containing same-character runs
//! (indentation spaces, "aaaa") under console/RDP jitter produces the SAME
//! gaps, misclassifying a paste as held-key typing — which landed the paste
//! uncoalesced (streaming) and could split-send. Timing cannot separate the
//! two event streams because THEY ARE the same stream at the OS level
//! (`wRepeatCount` is discarded by crossterm's parser). Classifying only the
//! Enter — the only key whose misrouting actually harms — removes the
//! tension entirely: characters stream no matter what, Enters within the
//! window never submit.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, AppMode};
use crate::left_panel::Mode;

/// Maximum gap between two keys of the same paste burst. Console pastes are
/// drained back-to-back (well under 1ms per record); the fastest human
/// typing is ~66ms/char. Generous enough to absorb Remote-Desktop / console
/// load jitter that stretches record gaps past 50ms, still far below any
/// human keystroke cadence.
pub(super) const BURST_WINDOW: Duration = Duration::from_millis(80);

/// Plain character keys that must have arrived inside [`BURST_WINDOW`] for
/// the burst to arm. Four keys within 80ms means >50 chars/s — far beyond any
/// human finger (fast digraphs hit ~2 keys per window), while a console paste
/// delivers hundreds of records in the time the coalescer needs to arm.
const BURST_MIN_KEYS: u32 = 4;

fn is_recent(t: Instant, now: Instant) -> bool {
    now.duration_since(t) <= BURST_WINDOW
}

#[derive(Default)]
pub(super) struct PasteBurstState {
    last_text_key_at: Option<Instant>,
    text_keys_in_window: u32,
    last_absorbed_enter_at: Option<Instant>,
    /// Whether the currently-running Enter chain began INSIDE an active text
    /// burst. Chained (blank-line) Enters are only absorbed while this is
    /// set; it resets on every flush, bounding the chain to one paste.
    chain_started_in_text_burst: bool,
    /// Shadow copy of the in-flight run: a mirror of every text character
    /// that streamed into the prompt plus one `\n` per absorbed Enter, in
    /// arrival order. Replayed through `PromptView::handle_paste` at burst
    /// end (runs with absorbed Enters only), reconstructing the paste —
    /// normalized line endings and `[Pasted ~N lines]` compression — from
    /// text that was never withheld from the prompt.
    shadow: String,
    /// How many of the shadow's characters are TEXT characters (the rest are
    /// absorbed newlines). These are exactly the characters currently sitting
    /// in the prompt immediately before the cursor, retracted at replay time
    /// so the replay does not duplicate them.
    shadow_text_len: usize,
    /// Whether the in-flight run reached PASTE semantics: the burst armed
    /// (4+ text keys in the window) or a plain Enter was absorbed. Only a
    /// replay-worthy run may be flushed — before that, the shadow is just
    /// a mirror of characters that may still become part of an armed run
    /// milliseconds later, and flushing it early would chop the paste into
    /// an unretracted prefix plus a replayed tail.
    run_replayable: bool,
}

impl PasteBurstState {
    /// Bookkeeping for a plain text key. MUST run before the armed check so
    /// the machine can reach the armed state. The key itself always falls
    /// through to the prompt — this only maintains the cadence counters that
    /// classify the NEXT plain Enter.
    fn note_text_key(&mut self, now: Instant) {
        let fresh = self.last_text_key_at.is_some_and(|t| is_recent(t, now));
        if !fresh {
            // A gap wider than the window breaks the run: the streamed
            // characters were typed at human cadence, not pasted.
            self.text_keys_in_window = 0;
            if !self.run_replayable {
                // The broken run never reached paste semantics: its shadow
                // mirror is dead weight (the characters stay in the prompt
                // as typed) and must not leak into a later run's retraction
                // length. A replay-worthy run keeps its shadow across the
                // gap — console/RDP jitter routinely exceeds the window
                // mid-paste, and its prefix is still awaiting replay.
                self.shadow.clear();
                self.shadow_text_len = 0;
            }
        }
        self.text_keys_in_window += 1;
        self.last_text_key_at = Some(now);
    }

    /// Whether a paste burst is in flight: a stream of pasted text keys is
    /// within the window, OR an Enter chain that started inside such a burst
    /// is still running (blank-line run).
    fn burst_armed(&self, now: Instant) -> bool {
        let text_burst = self.last_text_key_at.is_some_and(|t| is_recent(t, now))
            && self.text_keys_in_window >= BURST_MIN_KEYS;
        let chained_enter = self.chain_started_in_text_burst
            && self
                .last_absorbed_enter_at
                .is_some_and(|t| is_recent(t, now));
        text_burst || chained_enter
    }

    fn shadow_is_empty(&self) -> bool {
        self.shadow.is_empty()
    }

    fn shadow_text_len(&self) -> usize {
        self.shadow_text_len
    }

    fn run_replayable(&self) -> bool {
        self.run_replayable
    }

    /// Mark the run as replay-worthy (the burst armed). Sticky until the
    /// shadow is taken or cleared.
    fn mark_replayable(&mut self) {
        self.run_replayable = true;
    }

    fn push_text_char(&mut self, ch: char) {
        self.shadow.push(ch);
        self.shadow_text_len += 1;
    }

    /// Absorb a plain Enter as a paste line break: recorded in the shadow
    /// (the replay turns it into a real newline). An absorbed Enter is
    /// itself paste semantics — the run becomes replay-worthy.
    /// `text_burst_was_active` marks whether the ENTER CHAIN may continue
    /// after it (only chains that begin inside a real text burst are
    /// extendable).
    fn push_newline(&mut self, now: Instant, text_burst_was_active: bool) {
        self.shadow.push('\n');
        self.last_absorbed_enter_at = Some(now);
        self.run_replayable = true;
        if text_burst_was_active {
            self.chain_started_in_text_burst = true;
        }
    }

    /// Drop the shadow WITHOUT replaying it. Used when the streamed run is
    /// already part of the prompt content a fall-through Enter is about to
    /// send — a replay here would duplicate the text.
    fn clear(&mut self) {
        self.shadow.clear();
        self.shadow_text_len = 0;
        self.chain_started_in_text_burst = false;
        self.text_keys_in_window = 0;
        self.last_absorbed_enter_at = None;
        self.run_replayable = false;
    }

    /// Take the shadow for replay: `Some(text)` when the run absorbed at
    /// least one Enter (worth reconstructing through `handle_paste`), `None`
    /// for runs with no line break — those are cleared and left as typed.
    /// Resets the run state either way, so the next burst arms from scratch
    /// and no stale chain flag can swallow a later Enter.
    fn take_replay(&mut self) -> Option<String> {
        let had_enter = self.shadow.contains('\n');
        let text = std::mem::take(&mut self.shadow);
        self.shadow_text_len = 0;
        self.chain_started_in_text_burst = false;
        self.text_keys_in_window = 0;
        self.last_absorbed_enter_at = None;
        self.run_replayable = false;
        if had_enter { Some(text) } else { None }
    }

    /// A "text" key the way a Windows console reports pasted characters:
    /// printable chars carrying no modifiers beyond SHIFT (uppercase paste
    /// chars arrive SHIFTed) — PLUS the CTRL+ALT pair, the Windows-console
    /// signature of an AltGr character (`@ # $ € ª º` on ABNT). Real
    /// hotkeys never produce Char + CTRL+ALT on Windows (Ctrl+letter keys
    /// arrive as lowercase letters with CTRL only), so counting AltGr as
    /// text keeps an AltGr-heavy paste's Enters classified correctly
    /// instead of treating them as user submits.
    fn is_plain_text_key(key: &KeyEvent) -> bool {
        if !matches!(key.code, KeyCode::Char(_)) {
            return false;
        }
        let altgr = key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::ALT);
        if altgr {
            return true;
        }
        (key.modifiers - KeyModifiers::SHIFT).is_empty()
    }

    fn is_plain_enter(key: &KeyEvent) -> bool {
        key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE
    }
}

impl App {
    /// True when the chat prompt is the keyboard owner: no modal dialog,
    /// form, panel or mode-specific input is intercepting keys.
    ///
    /// NOTE: hand-maintained mirror of the "modal sovereignty" gates at the
    /// top of `App::process_key_event` (keys.rs) — when a new overlay gate is
    /// added there, add it here too, or the coalescer will absorb keys that
    /// overlay should receive.
    ///
    /// The SLASH MENU is deliberately NOT a gate: while it is open, plain
    /// text keys still reach the prompt through the menu's own `Char`
    /// handler (keys.rs pushes the char into `prompt_view.input` and
    /// re-filters the menu), so a paste arriving mid-query must keep
    /// coalescing. Treating the menu as an owner made every paste that
    /// begins with `/` lose its tail: the leaked prefix opened the menu,
    /// the ownership check disowned the coalescer, and the buffered rest
    /// was dropped (or worse, the paste's own Enter EXECUTED a slash
    /// command). Modals that truly own the keyboard remain gates.
    pub(super) fn prompt_owns_keyboard(&self) -> bool {
        self.prompt_view.is_focused
            && matches!(self.mode(), AppMode::Session)
            && !self.is_confirm_dialog_visible()
            && !self.is_provider_key_choice_visible()
            && !self.question_dialog.visible
            && !self.queue_choice_dialog.visible
            && !self.free_gateway_dialog.visible
            && !self.permission_dialog.visible
            && !self.is_registration_form_open()
            && !self.is_text_input_visible()
            && !self.dialog.visible()
            && !self.sidebar_focused
            && !self.show_settings
            && !self.show_add_provider
            && !self.show_router
            && !self.show_internal_tools
            && !matches!(self.left_panel, Mode::Explorer)
            && !self.is_rag_mode()
            && !self.is_theme_dialog_visible()
            && !self.is_tool_call_dialog_visible()
            && !self.is_message_actions_dialog_visible()
            && !self.is_undo_dialog_visible()
    }

    /// Classify a key for the in-flight paste burst. Returns `true` when the
    /// key was consumed by the coalescer (the caller must stop processing it
    /// — in particular it must NOT reach the `SendMessage` keymap action).
    ///
    /// Text characters are NEVER consumed: they fall through to the prompt
    /// immediately (streaming render, held keys included) while being
    /// mirrored into the shadow for the burst-end replay. Only plain Enters
    /// inside the burst window are absorbed (as paste line breaks); any
    /// other key (Esc, arrows, Ctrl combos…) mid-burst flushes the run
    /// first so the pasted text lands before the command acts, then falls
    /// through.
    pub(super) fn paste_burst_handle_key(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if !self.prompt_owns_keyboard() {
            // Ownership lost mid-burst (a dialog opened). LAND the run rather
            // than dropping it: a paste is user data. Divergence from the
            // Unix path (deliberate): there, an Event::Paste arriving while
            // an input dialog is open is routed INTO that dialog's field
            // (events.rs); here the text parks in the background prompt —
            // no corruption, no accidental send, fully visible once the
            // prompt regains focus.
            self.flush_paste_buffer();
            return false;
        }
        let plain_text = PasteBurstState::is_plain_text_key(key);
        let plain_enter = PasteBurstState::is_plain_enter(key);
        if !plain_text && !plain_enter {
            // Non-text key mid-burst: land the pasted text first, then let
            // the key dispatch normally against the completed input.
            self.flush_paste_buffer();
            return false;
        }

        if plain_text {
            // Cadence bookkeeping BEFORE the fall-through (the burst must be
            // able to arm from the Default state). The character itself is
            // NEVER withheld: it streams into the prompt through the normal
            // typing path, and the shadow mirrors it so the burst-end replay
            // can reconstruct the run with its real line breaks.
            self.paste_burst.note_text_key(now);
            if self.paste_burst.burst_armed(now) {
                // The run reached paste semantics: its shadow is
                // replay-worthy at burst end (sticky until taken/cleared,
                // so the per-key flush below cannot chop the run while it
                // is still forming).
                self.paste_burst.mark_replayable();
            }
            let KeyCode::Char(ch) = key.code else {
                unreachable!("is_plain_text_key guarantees a Char code");
            };
            self.paste_burst.push_text_char(ch);
            return false;
        }

        // Plain Enter.
        //
        // Absorb when the burst is armed OR when ANY text key was seen
        // within the window (`text_recent`) — even below the arming
        // threshold. A paste whose FIRST line is shorter than
        // [`BURST_MIN_KEYS`] (e.g. `hi\nbye\nok`) delivers its first Enter
        // before the counter reaches 4; falling through there would
        // split-send the paste exactly like the pre-fix bug. A human
        // cannot hit Enter within 80ms of the last character (motor time
        // alone is ~150ms), while a console paste delivers it in <1ms.
        let text_recent = self
            .paste_burst
            .last_text_key_at
            .is_some_and(|t| is_recent(t, now));
        if self.paste_burst.burst_armed(now) || text_recent {
            self.paste_burst.push_newline(now, text_recent);
            return true;
        }
        // Enter outside any burst (>80ms since the last text key). Land the
        // run FIRST, through the normal flush path: a jittered paste (console
        // stall stretching one record gap past the window) arrives here with
        // its line breaks still only in the shadow — replaying restores them
        // (retract the streamed chars, re-insert with real newlines) so this
        // Enter submits the FULL multi-line text as ONE message. The retraction
        // is exact (the shadow mirrors precisely the streamed tail chars), so
        // there is no duplication; for plain typed text the replay at worst
        // leaves a transient trailing newline, which the submit path trims.
        self.flush_paste_buffer();
        false
    }

    /// Replay the pending run through the NORMAL paste path
    /// (`PromptView::handle_paste`): the streamed characters are retracted
    /// from the prompt and re-inserted with real line breaks — line endings
    /// normalized, and long pastes compressed into the `[Pasted ~N lines]`
    /// virtual-text placeholder — identical to what Linux/macOS receive in
    /// one `Event::Paste`. Runs with no absorbed Enter are cleared without
    /// replaying: plain typing and held keys stay exactly as typed.
    fn flush_paste_buffer(&mut self) {
        if self.paste_burst.shadow_is_empty() {
            return;
        }
        if !self.paste_burst.run_replayable() {
            // The run never developed paste semantics (no absorbed Enter, the
            // cadence never armed): the streamed characters ARE the final
            // content — nothing to retract or replay.
            self.paste_burst.clear();
            return;
        }
        let text_len = self.paste_burst.shadow_text_len();
        let Some(text) = self.paste_burst.take_replay() else {
            return;
        };

        // Retract the run's text characters — they sit immediately before the
        // cursor (every streamed insert advanced it; absorbed Enters did not
        // move it) — so the replay below re-inserts them at the same spot
        // with real newlines. `cursor_pos` and `input` are BYTE offsets —
        // walk chars, never subtract counts (a multibyte char in the run
        // would panic a naive byte drain).
        let retracted = if text_len == 0 {
            // A pure-Enter run (blank lines only): nothing streamed, so the
            // replay inserts exactly the newlines the prompt never received.
            true
        } else if self.prompt_view.cursor_pos == self.prompt_view.input.len() {
            let end = self
                .prompt_view
                .cursor_pos
                .min(self.prompt_view.input.len());
            let start = self.prompt_view.input[..end]
                .char_indices()
                .rev()
                .nth(text_len - 1)
                .map_or(0, |(i, _)| i);
            self.prompt_view.input.drain(start..end);
            self.prompt_view.cursor_pos = start;
            true
        } else {
            // The cursor moved mid-burst (a mouse click): the run is no
            // longer the contiguous tail — retracting would take foreign
            // characters and replaying would DUPLICATE the streamed text.
            // Degrade gracefully: the text stays as streamed (its absorbed
            // line breaks are lost, content intact).
            false
        };

        if retracted {
            self.prompt_view.note_activity();
            self.prompt_view.handle_paste(&text);
            self.slash_menu.update(&self.prompt_view.input);
        }
    }

    /// Called once per main-loop iteration from `run()`: when the burst is
    /// over (no key within [`BURST_WINDOW`]) the run is replayed through the
    /// normal paste path. `now` is injected (same clock as the key path) so
    /// tests can drive this deterministically.
    pub(super) fn paste_burst_flush_if_due_at(&mut self, now: Instant) {
        // Only land the paste while the prompt owns the keyboard. If a
        // dialog opened mid-burst, the FIRST key processed while the dialog
        // is open hits the ownership check at the top of
        // `paste_burst_handle_key` and flushes (lands) the run there;
        // if no key ever arrives, the shadow stays pending and THIS
        // per-frame flush lands it as soon as ownership returns. The two
        // consumers are mutually exclusive per key, never redundant.
        if !self.paste_burst.shadow_is_empty()
            && self.paste_burst.run_replayable()
            && self.prompt_owns_keyboard()
            && !self.paste_burst.burst_armed(now)
        {
            self.flush_paste_buffer();
        }
    }

    /// Production entry point: flush against the wall clock.
    pub(super) fn paste_burst_flush_if_due(&mut self) {
        self.paste_burst_flush_if_due_at(Instant::now());
    }

    /// Whether a run is pending whose flush is governed by [`BURST_WINDOW`]:
    /// the replay-worthy shadow is still waiting and the burst window has not
    /// expired since the last absorbed key. The idle loop uses this to keep
    /// its poll timeout short so the run lands atomically on time instead of
    /// up to a full idle-wait late.
    pub(super) fn paste_burst_pending_flush_deadline(&self) -> bool {
        let now = Instant::now();
        !self.paste_burst.shadow_is_empty()
            && self.paste_burst.run_replayable()
            && (self
                .paste_burst
                .last_text_key_at
                .is_some_and(|t| is_recent(t, now))
                || self
                    .paste_burst
                    .last_absorbed_enter_at
                    .is_some_and(|t| is_recent(t, now)))
    }
}

#[cfg(test)]
mod state_machine_tests {
    //! Deterministic unit tests for the [`PasteBurstState`] machine — driven
    //! with a synthetic clock (microsecond steps, no real sleeps). The
    //! integration tests in `super::super::tests::paste_burst` cover the App
    //! wiring; this module pins the state-machine invariants themselves.

    use super::*;

    /// One microsecond-ish step — any delta far below BURST_WINDOW.
    const STEP: Duration = Duration::from_micros(100);

    /// A start instant; every timeline in these tests is relative to it.
    fn t0() -> Instant {
        Instant::now()
    }

    /// REGRESSION (fix #1 invariant): an Enter absorbed while the burst is
    /// NOT yet armed (short first line, `text_recent` only) must still set
    /// the chain flag, so a following blank-line run chains correctly.
    #[test]
    fn enter_below_threshold_still_chains_blank_lines() {
        let mut s = PasteBurstState::default();
        let t = t0();
        // 3 text keys (< BURST_MIN_KEYS = 4): not armed.
        for i in 1..=3u32 {
            s.note_text_key(t + i * STEP);
        }
        s.push_newline(t + 4u32 * STEP, true);
        assert!(
            s.chain_started_in_text_burst,
            "chain anchored by text_recent Enter"
        );
        assert!(
            s.run_replayable(),
            "an absorbed Enter makes the run replay-worthy"
        );
        assert_eq!(s.take_replay().unwrap(), "\n");
        // take_replay resets the chain flag.
        assert!(!s.chain_started_in_text_burst);
    }

    /// REGRESSION (invariant from fix #6): take_replay must fully reset run
    /// state so the next burst arms from scratch with no stale flags.
    #[test]
    fn take_replay_resets_all_run_state() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for i in 1..=4u32 {
            s.note_text_key(t + i * STEP);
        }
        s.mark_replayable();
        s.push_text_char('a');
        s.push_newline(t + 5u32 * STEP, true);
        assert!(s.take_replay().is_some());
        assert!(s.take_replay().is_none(), "shadow drained");
        assert_eq!(s.shadow_text_len(), 0);
        assert!(!s.chain_started_in_text_burst);
        assert!(!s.run_replayable());
        assert!(s.last_absorbed_enter_at.is_none());
        // Old timestamps must not arm a new burst.
        assert!(!s.burst_armed(t + 6u32 * STEP));
    }

    /// A gap wider than the window breaks the run: the cadence counter
    /// resets, so sustained human typing never reaches the arming threshold
    /// that classifies the next Enter as a paste line break. A NOT-yet-
    /// replayable run's shadow is dropped with it (its characters stay in
    /// the prompt as typed); a REPLAYABLE run keeps its shadow — console/RDP
    /// jitter routinely exceeds the window mid-paste, and the run's prefix
    /// is still awaiting the burst-end replay.
    #[test]
    fn wide_gap_resets_the_cadence_count() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for i in 1..=3u32 {
            s.note_text_key(t + i * STEP);
        }
        s.push_text_char('x');
        s.push_text_char('y');
        // Human pause > BURST_WINDOW, then typing resumes.
        let late = t + 4u32 * STEP + BURST_WINDOW + Duration::from_millis(50);
        s.note_text_key(late);
        assert_eq!(s.text_keys_in_window, 1, "stale cadence run discarded");
        assert!(!s.run_replayable());
        assert!(
            s.shadow_is_empty(),
            "dead mirror dropped with the broken run"
        );

        // Same gap, but the run had reached paste semantics: shadow kept.
        let mut s2 = PasteBurstState::default();
        for i in 1..=4u32 {
            s2.note_text_key(t + i * STEP);
        }
        s2.mark_replayable();
        s2.push_text_char('z');
        s2.note_text_key(late);
        assert_eq!(s2.shadow, "z", "replay-worthy run survives the gap");
        assert_eq!(s2.shadow_text_len(), 1);
    }

    /// A text burst stays armed only while keys keep arriving within the
    /// window; after BURST_WINDOW of silence it expires.
    #[test]
    fn burst_expires_after_window_of_silence() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for i in 1..=4u32 {
            s.note_text_key(t + i * STEP);
        }
        assert!(s.burst_armed(t + 4u32 * STEP));
        assert!(!s.burst_armed(t + 4u32 * STEP + BURST_WINDOW + STEP));
    }

    /// The shadow mirrors text characters and absorbed newlines separately:
    /// `shadow_text_len` counts only the TEXT chars (the retraction length);
    /// the newline count is implicit (`shadow.len() - shadow_text_len`).
    #[test]
    fn shadow_tracks_text_and_newline_lengths_separately() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for ch in "abc".chars() {
            s.push_text_char(ch);
        }
        s.push_newline(t, true);
        s.push_newline(t + STEP, true);
        s.push_text_char('d');
        // Two absorbed Enters → two shadow newlines; text chars counted apart.
        assert_eq!(s.shadow, "abc\n\nd");
        assert_eq!(s.shadow_text_len(), 4, "newlines are not text chars");
    }

    /// A run with NO absorbed Enter (plain typing, held keys, single-line
    /// pastes) is cleared WITHOUT a replay: the streamed text stays in the
    /// prompt exactly as typed — no retraction, no placeholder compression
    /// of fast human typing.
    #[test]
    fn take_replay_without_enter_leaves_text_as_typed() {
        let mut s = PasteBurstState::default();
        for ch in "just typing".chars() {
            s.push_text_char(ch);
        }
        assert!(s.take_replay().is_none(), "no Enter → no replay");
        assert!(s.shadow.is_empty(), "shadow cleared either way");
        assert_eq!(s.shadow_text_len(), 0);
    }
}
