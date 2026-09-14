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
//! plus ONE PLAIN `Enter` PER LINE BREAK. Without coalescing this has two
//! visible symptoms:
//!
//! 1. every plain Enter falls through to the `SendMessage` keymap action and
//!    submits whatever accumulated so far — one message per line, none of
//!    them user-initiated;
//! 2. the characters trickle in one keystroke at a time ("looks like it is
//!    being typed"), and `PromptView::handle_paste` — the only place that
//!    compresses long pastes into the `[Pasted ~N lines]` placeholder — is
//!    NEVER reached, so the raw text goes out verbatim.
//!
//! THE FIX: while a paste burst is ARMED, plain character keys and plain
//! Enters are NOT applied to the prompt one by one — they are accumulated in
//! a side buffer ([`PasteBurstState::buffer`]). As soon as the burst ends
//! (no key of it for [`BURST_WINDOW`], checked once per frame from `run()`
//! and before every key that falls through), the WHOLE buffer is replayed
//! through `PromptView::handle_paste` — exactly the Linux/macOS path. Long
//! pastes therefore land atomically, stream-free, and compress into the
//! `[Pasted ~N lines]` placeholder; a single real Enter then sends the
//! expanded text as ONE message.
//!
//! ARMING (the state machine must be able to reach its armed state, so the
//! bookkeeping runs BEFORE the armed check): every plain text key updates a
//! rolling count of keys seen within [`BURST_WINDOW`]; the burst arms as soon
//! as that count reaches [`BURST_MIN_KEYS`] (>50 chars/s — no human finger;
//! fast digraphs top out around 2 keys per window). The handful of characters
//! that fell through to the prompt BEFORE arming (the threshold run) is
//! RETRACTED from the prompt into the buffer on the arming transition, so the
//! replay is byte-identical to a whole-paste `Event::Paste`.
//!
//! ENTER CHAINING: blank lines inside a paste produce back-to-back Enters
//! with no text keys between them. A plain Enter is absorbed as a newline
//! when the text burst is active, and the chain MAY continue across further
//! Enters within [`BURST_WINDOW`] — but only if the chain STARTED inside a
//! text burst ([`PasteBurstState::chain_started_in_text_burst`]). This bounds
//! the chain: once the buffer is flushed the flag resets, so a user mashing
//! Enter in the empty prompt can never be swallowed indefinitely.
//!
//! Known trade-offs (documented, self-healing): a real Enter pressed within
//! [`BURST_WINDOW`] of the last pasted character is absorbed as a newline —
//! the next press, outside the window, submits normally. Held-key auto-repeat
//! (~33ms/char) arms the burst the same way: it lands as one atomic insert
//! instead of char-by-char trickle. AltGr-produced characters (CTRL+ALT on
//! the Windows console) COUNT as paste text keys: an AltGr-heavy paste keeps
//! coalescing instead of flushing on every `@`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, AppMode, LeftPanelMode};

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
    /// Plain characters that fell through to the prompt while the burst was
    /// not yet armed (the first `BURST_MIN_KEYS - 1` of a run). Retracted
    /// into the buffer when the burst arms, so the atomic replay contains
    /// the WHOLE paste.
    leaked_chars: usize,
    /// Characters (and `\n` for line breaks) absorbed from the in-flight
    /// paste burst, waiting to be replayed through `PromptView::handle_paste`
    /// once the burst ends.
    buffer: String,
}

impl PasteBurstState {
    /// Bookkeeping for a plain text key. MUST run before the armed check so
    /// the machine can reach the armed state. Returns `true` when this key
    /// ARMED the burst (transition), which is the caller's cue to retract the
    /// leaked prefix.
    fn note_text_key(&mut self, now: Instant) -> bool {
        let fresh = self
            .last_text_key_at
            .is_some_and(|t| is_recent(t, now));
        if !fresh {
            // A gap wider than the window breaks the run: the previously
            // leaked characters were typed at human cadence, not pasted.
            self.leaked_chars = 0;
            self.text_keys_in_window = 0;
        }
        self.text_keys_in_window += 1;
        self.last_text_key_at = Some(now);
        self.text_keys_in_window == BURST_MIN_KEYS
    }

    /// Whether a paste burst is in flight: a stream of pasted text keys is
    /// within the window, OR an Enter chain that started inside such a burst
    /// is still running (blank-line run).
    fn burst_armed(&self, now: Instant) -> bool {
        let text_burst = self
            .last_text_key_at
            .is_some_and(|t| is_recent(t, now))
            && self.text_keys_in_window >= BURST_MIN_KEYS;
        let chained_enter =
            self.chain_started_in_text_burst
                && self
                    .last_absorbed_enter_at
                    .is_some_and(|t| is_recent(t, now));
        text_burst || chained_enter
    }

    fn buffer_is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn leaked_chars(&self) -> usize {
        self.leaked_chars
    }

    fn clear_leaked(&mut self) {
        self.leaked_chars = 0;
    }

    fn note_leaked_char(&mut self) {
        self.leaked_chars += 1;
    }

    fn push_char(&mut self, ch: char) {
        self.buffer.push(ch);
    }

    /// Absorb a plain Enter as a paste line break. `text_burst_was_active`
    /// marks whether the ENTER CHAIN may continue after it (only chains that
    /// begin inside a real text burst are extendable).
    fn push_newline(&mut self, now: Instant, text_burst_was_active: bool) {
        self.buffer.push('\n');
        self.last_absorbed_enter_at = Some(now);
        if text_burst_was_active {
            self.chain_started_in_text_burst = true;
        }
    }

    /// Drain the pending paste text (empty → `None`), resetting the run
    /// state so the next paste arms from scratch and no stale chain flag can
    /// swallow a later Enter.
    fn take_buffer(&mut self) -> Option<String> {
        self.chain_started_in_text_burst = false;
        self.leaked_chars = 0;
        self.text_keys_in_window = 0;
        self.last_absorbed_enter_at = None;
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }


    /// A "text" key the way a Windows console reports pasted characters:
    /// printable chars carrying no modifiers beyond SHIFT (uppercase paste
    /// chars arrive SHIFTed) — PLUS the CTRL+ALT pair, the Windows-console
    /// signature of an AltGr character (`@ # $ € ª º` on ABNT). Real
    /// hotkeys never produce Char + CTRL+ALT on Windows (Ctrl+letter keys
    /// arrive as lowercase letters with CTRL only), so counting AltGr as
    /// text keeps an AltGr-heavy paste coalescing instead of flushing the
    /// buffer on every `@`.
    fn is_plain_text_key(key: &KeyEvent) -> bool {
        if !matches!(key.code, KeyCode::Char(_)) {
            return false;
        }
        let altgr = key
            .modifiers
            .contains(KeyModifiers::CONTROL)
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
    fn prompt_owns_keyboard(&self) -> bool {
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
            && !matches!(self.left_panel, LeftPanelMode::Explorer)
            && !self.is_rag_mode()
            && !self.is_theme_dialog_visible()
            && !self.is_tool_call_dialog_visible()
            && !self.is_message_actions_dialog_visible()
            && !self.is_undo_dialog_visible()
    }
    /// Absorb a key into the in-flight paste burst. Returns `true` when the
    /// key was consumed by the coalescer (the caller must stop processing it
    /// — in particular it must NOT reach the `SendMessage` keymap action).
    ///
    /// Only plain text keys and plain Enters participate; any other key
    /// (Esc, arrows, Ctrl combos…) mid-burst flushes the buffer first so the
    /// pasted text lands before the command acts, then falls through.
    pub(super) fn paste_burst_handle_key(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if !self.prompt_owns_keyboard() {
            // Ownership lost mid-burst (a dialog opened). LAND the pending
            // text into the prompt rather than dropping it: a paste is user
            // data — silently discarding it loses work. Divergence from the
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
            // Bookkeeping BEFORE the armed check (the burst must be able to
            // arm from the Default state).
            let just_armed = self.paste_burst.note_text_key(now);
            if just_armed {
                // The threshold run already fell through to the prompt:
                // retract it into the buffer so the atomic replay contains
                // the WHOLE paste, not just its tail.
                self.retract_leaked_prefix();
            }
            if self.paste_burst.burst_armed(now) {
                let KeyCode::Char(ch) = key.code else {
                    unreachable!("is_plain_text_key guarantees a Char code");
                };
                self.paste_burst.push_char(ch);
                return true;
            }
            // Not armed (yet): the key falls through to normal typing and is
            // counted as leaked, for retraction if the burst arms.
            self.paste_burst.note_leaked_char();
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
            if !self.paste_burst.burst_armed(now) {
                // The threshold run has not armed yet (short first line):
                // retract the leaked chars BEFORE the newline so the replay
                // buffer keeps paste order (`"hi"` then `'\n'`), and so the
                // later arming transition does not double-retract.
                self.retract_leaked_prefix();
            }
            self.paste_burst.push_newline(now, text_recent);
            return true;
        }
        // Enter outside any burst: flush the pending paste (if any) so the
        // keymap submits the FULL text, then fall through to SendMessage.
        // (No stale leaked count can survive: take_buffer inside the flush
        // resets leaked_chars before its early return, so the next burst's
        // arming transition can never retract foreign characters.)
        self.flush_paste_buffer();
        false
    }

    /// Pull the last `leaked_chars` characters inserted before the cursor
    /// back out of the prompt (they are re-buffered for the atomic replay).
    fn retract_leaked_prefix(&mut self) {
        let leaked = self.paste_burst.leaked_chars();
        if leaked == 0 {
            return;
        }
        // The leaked chars were inserted sequentially at the cursor within
        // one burst (no cursor movement between them), so they are exactly
        // the `leaked` CHARS immediately before `cursor_pos`. `cursor_pos`
        // and `input` are BYTE offsets — walk chars, never subtract counts
        // (a multibyte char in the leaked run would panic `drain`).
        let end = self.prompt_view.cursor_pos.min(self.prompt_view.input.len());
        let start = self.prompt_view.input[..end]
            .char_indices()
            .rev()
            .nth(leaked - 1)
            .map_or(0, |(i, _)| i);
        let retracted: String = self.prompt_view.input.drain(start..end).collect();
        for ch in retracted.chars() {
            self.paste_burst.push_char(ch);
        }
        self.prompt_view.cursor_pos = start;
        self.paste_burst.clear_leaked();
    }

    /// Replay the pending paste text through the NORMAL paste path
    /// (`PromptView::handle_paste`): line endings normalized, and long pastes
    /// compressed into the `[Pasted ~N lines]` virtual-text placeholder —
    /// identical to what Linux/macOS receive in one `Event::Paste`.
    fn flush_paste_buffer(&mut self) {
        let Some(text) = self.paste_burst.take_buffer() else {
            return;
        };
        self.prompt_view.note_activity();
        self.prompt_view.handle_paste(&text);
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// Called once per main-loop iteration from `run()`: when the burst is
    /// over (no key within [`BURST_WINDOW`]) the buffered paste lands in the
    /// prompt as one atomic paste. `now` is injected (same clock as the key
    /// path) so tests can drive this deterministically.
    pub(super) fn paste_burst_flush_if_due_at(&mut self, now: Instant) {
        // Only land the paste while the prompt owns the keyboard. If a
        // dialog opened mid-burst, the FIRST key processed while the dialog
        // is open hits the ownership check at the top of
        // `paste_burst_handle_key` and flushes (lands) the buffer there;
        // if no key ever arrives, the buffer stays pending and THIS
        // per-frame flush lands it as soon as ownership returns. The two
        // consumers are mutually exclusive per key, never redundant.
        if !self.paste_burst.buffer_is_empty()
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
            assert!(!s.note_text_key(t + i * STEP), "3 keys must not arm");
        }
        s.push_newline(t + 4u32 * STEP, true);
        assert!(s.chain_started_in_text_burst, "chain anchored by text_recent Enter");
        assert_eq!(s.take_buffer().unwrap(), "\n");
        // take_buffer resets the chain flag.
        assert!(!s.chain_started_in_text_burst);
    }

    /// REGRESSION (invariant from fix #6): take_buffer must fully reset run
    /// state so the next burst arms from scratch with no stale flags.
    #[test]
    fn take_buffer_resets_all_run_state() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for i in 1..=4u32 {
            s.note_text_key(t + i * STEP);
        }
        s.push_char('a');
        s.note_leaked_char();
        s.push_newline(t + 5u32 * STEP, true);
        assert!(s.take_buffer().is_some());
        assert!(s.take_buffer().is_none(), "buffer drained");
        assert_eq!(s.leaked_chars(), 0);
        assert!(!s.chain_started_in_text_burst);
        assert!(s.last_absorbed_enter_at.is_none());
        // Old timestamps must not arm a new burst.
        assert!(!s.burst_armed(t + 6u32 * STEP));
    }

    /// A gap wider than the window breaks the run: previously leaked chars
    /// were human typing, must NOT be retracted into a later burst.
    #[test]
    fn wide_gap_drops_leaked_run() {
        let mut s = PasteBurstState::default();
        let t = t0();
        for i in 1..=3u32 {
            s.note_text_key(t + i * STEP);
        }
        s.note_leaked_char();
        s.note_leaked_char();
        assert_eq!(s.leaked_chars(), 2);
        // Human pause > BURST_WINDOW, then typing resumes.
        let late = t + 4u32 * STEP + BURST_WINDOW + Duration::from_millis(50);
        assert!(!s.note_text_key(late), "run broken, not armed");
        assert_eq!(s.leaked_chars(), 0, "stale leaked run discarded");
        assert_eq!(s.text_keys_in_window, 1);
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
}
