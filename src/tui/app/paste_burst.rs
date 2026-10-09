//! Windows paste-burst coalescing — the legacy-console fallback for "a large
//! paste is split into one message per line, without pressing Enter" on
//! Windows.
//!
//! LAYERING (read first): on modern Windows consoles pastes no longer reach
//! this module at all. The patched crossterm (see `[patch.crates-io]` in
//! Cargo.toml — upstream PR crossterm-rs/crossterm#1030) scopes
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` to the bracketed-paste lifetime, so
//! Windows Terminal / modern conhost deliver one real
//! [`crossterm::event::Event::Paste`], handled in `super::events`
//! (`App::process_event` → `PromptView::handle_paste`) — atomic placeholder,
//! zero streaming, exactly like Unix. Everything below exists for consoles
//! WITHOUT VT input (legacy conhost), where stock crossterm 0.29 reads the
//! console through `ReadConsoleInput` with no bracketed-paste support
//! (#737): there the same paste still arrives as ONE KEY EVENT PER
//! CHARACTER plus ONE `Enter` PER LINE BREAK, and this coalescer (cadence +
//! clipboard anchor) reconstructs it. Without it, every plain Enter would
//! fall through to the `SendMessage` keymap action and submit whatever
//! accumulated so far — one message per line, none of them user-initiated.
//!
//! THE FIX — characters stream by default: every plain text key falls
//! through to the prompt through the normal typing path, so typing ALWAYS
//! renders in real time: a paste streams into the prompt exactly like the
//! pre-coalescer console behaviour, and a HELD KEY (OS auto-repeat, ~20-33ms
//! per repeat — the same cadence class as a paste burst) shows its
//! characters the moment they arrive. The vanish-until-release bug is
//! impossible by construction for anything that is not a confirmed paste:
//! nothing is ever withheld on timing suspicion alone.
//!
//! THE ELEGANT PART — the clipboard anchor (Windows only): a console paste
//! IS clipboard content re-injected as key events, so once a burst arms the
//! coalescer reads the clipboard exactly once and compares. When the
//! streamed shadow is a strict prefix of the clipboard text (line endings
//! normalized both sides, like opencode's own `pasteInputText`), the paste
//! is CONFIRMED by content, not just cadence — and from then on nothing
//! more streams: the FULL clipboard text is inserted atomically through
//! `PromptView::handle_paste` (placeholder included, exactly like the Unix
//! `Event::Paste` path) and every further matching key is swallowed after
//! per-character verification. A slow-trickle paste therefore shows a
//! handful of characters and then the placeholder — never minutes of
//! word-by-word streaming. Anything that stops matching (real typing that
//! merely started like the clipboard, a changed clipboard, a mid-paste
//! submit) rolls back to exactly what arrived: the atomic insertion is
//! retracted and the received text lands raw. Two independent signals
//! (inhuman cadence AND byte-identical content) make false positives
//! practically impossible; held keys and fast typing can never match an
//! arbitrary clipboard, so they always keep streaming.
//!
//! Anchor scope notes: only runs that would compress (`paste_would_compress`)
//! anchor — short pastes stream to completion in milliseconds anyway, and
//! skipping them keeps the machinery away from everyday typing. The anchor
//! never fires on non-Windows builds: Unix pastes already arrive atomically
//! via bracketed paste, and a synchronous clipboard round-trip there (X11 /
//! Wayland IPC, potentially hanging on a dead owner) has no business sitting
//! in the key path.
//!
//! What IS classified is the ENTER — plain or modifier-carrying: an Enter
//! arriving within [`BURST_WINDOW`] of recent text keys — armed or not — is
//! a paste line break, not a user submit (a human cannot hit Enter within
//! 80ms of the last character; motor time alone is ~150ms, while a console
//! paste delivers the next record in <1ms). Absorbed Enters go into a SHADOW
//! copy of the run ([`PasteBurstState::shadow`]) alongside a mirror of every
//! streamed character, preserving arrival order. Absorbing modified Enters
//! (Ctrl/Shift+Enter) as well is what keeps shadow and prompt consistent:
//! a modified Enter that fell through would be typed into the prompt by the
//! newline shortcut WITHOUT being mirrored — the burst-end retraction would
//! then drain the wrong span and mix raw text with the placeholder.
//!
//! When the burst ends (no key of it for [`BURST_WINDOW`], checked once per
//! frame from `run()` and before every key that falls through), runs that
//! absorbed at least one Enter — or that are long enough to compress even
//! without one (a single-line paste long enough to trigger compression) — are
//! RETRACTED from the prompt and replayed through `PromptView::handle_paste`
//! — exactly the Linux/macOS path: line endings normalize and long pastes
//! compress into the `[Pasted ~N lines]` placeholder. Short runs with no
//! absorbed Enter (plain typing, held keys, short single-line pastes) are
//! left exactly as typed.
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
//! newline — the next press, outside the window, submits normally.
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
use crate::util::selection;

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

/// Streamed characters required before the clipboard anchor may engage.
/// Eight content-matching characters are far beyond coincidence yet arrive
/// in microseconds for real pastes (and in a fraction of a second even for
/// slow-trickle terminals), so the placeholder lands almost immediately
/// without ever risking everyday typing.
const ANCHOR_MIN_CHARS: usize = 8;

fn is_recent(t: Instant, now: Instant) -> bool {
    now.duration_since(t) <= BURST_WINDOW
}

/// Byte end of the `char_count`-character span starting at byte `start` in
/// `s`, or `None` when `start` is not a char boundary or the span overruns
/// the string. Lets the paste-burst replay retract exactly the streamed
/// span anywhere in the prompt — never a byte-split multibyte char.
fn char_count_to_byte_end(s: &str, start: usize, char_count: usize) -> Option<usize> {
    let mut end = start;
    for ch in s.get(start..)?.chars().take(char_count) {
        end += ch.len_utf8();
    }
    // `take(char_count)` yields fewer chars when the span overruns: only
    // accept the exact length.
    if s.get(start..)?.chars().take(char_count).count() == char_count {
        Some(end)
    } else {
        None
    }
}

/// A confirmed paste in flight: the streamed run matched the clipboard, so
/// the full clipboard text was already inserted atomically and the rest of
/// the stream is verified key by key instead of rendered. See the module
/// docs ("THE ELEGANT PART — the clipboard anchor").
#[derive(Debug, Clone)]
struct AnchorState {
    /// Normalized (`\r\n`/`\r` → `\n`) clipboard text the stream must equal.
    expected: String,
    /// Clipboard characters consumed so far.
    pos: usize,
    /// Byte offset of `pos` into `expected` (always a char boundary).
    pos_bytes: usize,
    /// Prompt byte offset where the atomic insertion landed.
    start: usize,
    /// Byte length of the atomic insertion (for rollback retraction).
    inserted_len: usize,
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
    /// end (when worth compressing), reconstructing the paste —
    /// normalized line endings and `[Pasted ~N lines]` compression — from
    /// text that was never withheld from the prompt (or from an anchored
    /// stream that already landed atomically).
    shadow: String,
    /// How many of the shadow's characters are TEXT characters (the rest are
    /// absorbed newlines). These are exactly the characters currently sitting
    /// in the prompt starting at [`PasteBurstState::burst_start`], retracted
    /// at replay time so the replay does not duplicate them.
    shadow_text_len: usize,
    /// Byte offset in the prompt input where the in-flight run started
    /// streaming (the cursor position when the run's first character was
    /// mirrored). Lets the burst-end replay retract a paste that landed
    /// mid-prompt — not just an end-of-input tail — by draining exactly the
    /// streamed span. Reset with every flush/clear, like the shadow itself.
    burst_start: Option<usize>,
    /// Whether the in-flight run reached PASTE semantics: the burst armed
    /// (4+ text keys in the window) or a plain Enter was absorbed. Only a
    /// replay-worthy run may be flushed — before that, the shadow is just
    /// a mirror of characters that may still become part of an armed run
    /// milliseconds later, and flushing it early would chop the paste into
    /// an unretracted prefix plus a replayed tail.
    run_replayable: bool,
    /// A confirmed clipboard paste currently swallowing its own stream
    /// (`None` while merely suspected). Set by the anchor attempt, cleared
    /// on completion, rollback, or any run reset.
    anchor: Option<AnchorState>,
    /// The clipboard was already read for the in-flight run (no re-reads
    /// mid-run: the paste source must stay fixed while verifying).
    anchor_checked: bool,
    /// The in-flight run proved it is not its clipboard (prefix mismatch
    /// before anchoring). Sticky until the next cadence run so a declined
    /// stream is never re-read key after key.
    anchor_declined: bool,
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
            // A fresh cadence run gets a fresh clipboard judgement (the
            // source may have changed across the silence).
            self.anchor_checked = false;
            self.anchor_declined = false;
            if !self.run_replayable {
                // The broken run never reached paste semantics: its shadow
                // mirror is dead weight (the characters stay in the prompt
                // as typed) and must not leak into a later run's retraction
                // length. A replay-worthy run keeps its shadow across the
                // gap — console/RDP jitter routinely exceeds the window
                // mid-paste, and its prefix is still awaiting replay.
                self.shadow.clear();
                self.shadow_text_len = 0;
                self.burst_start = None;
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

    /// Record where in the prompt the in-flight run started streaming.
    /// Called once per run — when the shadow is still empty, i.e. the very
    /// first mirrored character — with the live cursor position (the
    /// insertion point the fall-through typing path is about to use).
    fn note_burst_start(&mut self, cursor_pos: usize) {
        if self.shadow_text_len == 0 && self.burst_start.is_none() {
            self.burst_start = Some(cursor_pos);
        }
    }

    /// Whether `ch` is exactly the clipboard character the anchor expects
    /// next. Byte-indexed (`pos_bytes` is always a char boundary), so this
    /// stays O(1) per key even for very large pastes.
    fn anchor_expects_text(&self, ch: char) -> bool {
        self.anchor.as_ref().is_some_and(|a| {
            a.expected
                .get(a.pos_bytes..)
                .is_some_and(|rest| rest.starts_with(ch))
        })
    }

    /// Whether the anchor expects a line break next.
    fn anchor_expects_newline(&self) -> bool {
        self.anchor.as_ref().is_some_and(|a| {
            a.expected
                .get(a.pos_bytes..)
                .is_some_and(|rest| rest.starts_with('\n'))
        })
    }

    /// Whether the anchor already received the whole clipboard.
    fn anchor_complete(&self) -> bool {
        self.anchor
            .as_ref()
            .is_some_and(|a| a.pos_bytes == a.expected.len())
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
        self.burst_start = None;
        self.anchor = None;
        self.anchor_checked = false;
        self.anchor_declined = false;
        self.chain_started_in_text_burst = false;
        self.text_keys_in_window = 0;
        self.last_absorbed_enter_at = None;
        self.run_replayable = false;
    }

    /// Take the shadow for replay: `Some(text)` when the run absorbed at
    /// least one Enter (worth reconstructing through `handle_paste`) or when
    /// the run is long enough to compress even without one (a single-line
    /// paste over the shared [`crate::component::prompt::paste_would_compress`]
    /// threshold — the Unix `Event::Paste` path compresses those, so the
    /// Windows replay must too, or a large single-paragraph paste streams
    /// forever and the `[Pasted ~N lines]` placeholder never appears).
    /// `None` for short runs with no line break — those are cleared and left
    /// as typed (plain typing, held keys, short single-line pastes).
    /// Resets the run state either way, so the next burst arms from scratch
    /// and no stale chain flag can swallow a later Enter.
    fn take_replay(&mut self) -> Option<String> {
        let had_enter = self.shadow.contains('\n') || self.shadow.contains('\r');
        // `paste_would_compress` normalizes CRLF/CR itself, so a `\r`-only
        // shadow is judged on the same text `handle_paste` will receive.
        let worth_compressing =
            had_enter || crate::component::prompt::paste_would_compress(&self.shadow);
        let text = std::mem::take(&mut self.shadow);
        self.shadow_text_len = 0;
        self.burst_start = None;
        self.anchor = None;
        self.anchor_checked = false;
        self.anchor_declined = false;
        self.chain_started_in_text_burst = false;
        self.text_keys_in_window = 0;
        self.last_absorbed_enter_at = None;
        self.run_replayable = false;
        if worth_compressing { Some(text) } else { None }
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
}

impl App {
    /// Whether a plain text key landed within the paste-burst window
    /// (`BURST_WINDOW` of the last text character). The Ctrl+Alt+L handler
    /// uses this to tell an AltGr CHARACTER from the chord: on ABNT layouts
    /// `l` arrives encoded as Char+CTRL|ALT, exactly like the shortcut, but
    /// a real user cannot hit a chord within 80 ms of typing text — only a
    /// paste (or AltGr burst) can.
    pub(super) fn paste_burst_text_recent(&self) -> bool {
        self.paste_burst
            .last_text_key_at
            .is_some_and(|t| is_recent(t, Instant::now()))
    }

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
    /// Text characters are NEVER consumed on suspicion: they fall through to
    /// the prompt immediately (streaming render, held keys included) while
    /// being mirrored into the shadow for the burst-end replay — UNLESS a
    /// clipboard anchor already confirmed the stream, in which case matching
    /// keys are swallowed (the full text already landed atomically) and only
    /// a mismatch falls through after rolling back. Only Enters inside the
    /// burst window are absorbed (as paste line breaks) — plain AND
    /// modifier-carrying (a terminal with bracketed-paste interference
    /// delivers pasted `\n` as Ctrl+Enter; absorbing it keeps the shadow
    /// identical to what the prompt would have received); any other key
    /// (Esc, arrows, Ctrl combos…) mid-burst flushes the run first so the
    /// pasted text lands before the command acts, then falls through.
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
        // NOTE: any Enter, not just a plain one. Pasted line breaks usually
        // arrive modifier-free, but a terminal left in bracketed-paste mode
        // tags them with CONTROL (crossterm#962) — and a Shift/Ctrl+Enter
        // typed within 80ms of text is motor-impossible for a human anyway,
        // so absorbing it is always the paste-faithful choice. Outside the
        // window it still falls through to the newline shortcut / submit.
        let is_enter = key.code == KeyCode::Enter;
        if !plain_text && !is_enter {
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
            // Clipboard-anchored fast path: a confirmed paste swallows its
            // own tail key by key (verified, never rendered). Divergence
            // rolls back inside and falls through to the normal path below,
            // so this never withholds on suspicion alone.
            if self.paste_burst.anchor.is_some() && self.paste_burst_anchor_consume_text(ch, now) {
                return true;
            }
            // Anchor the run BEFORE mirroring: when the shadow is still
            // empty this character starts a fresh run, and the live cursor
            // is the exact insertion point the fall-through typing path is
            // about to use. A jittered run keeps its shadow across a wide
            // gap (see `note_text_key`), so its original start survives —
            // only a fresh run re-anchors.
            self.paste_burst
                .note_burst_start(self.prompt_view.cursor_pos);
            self.paste_burst.push_text_char(ch);
            // Anchor attempt: confirm by content once long enough to judge.
            self.paste_burst_try_anchor();
            return false;
        }

        // Enter (plain or modified).
        //
        // Absorb when the burst is armed OR when ANY text key was seen
        // within the window (`text_recent`) — even below the arming
        // threshold. A paste whose FIRST line is shorter than
        // [`BURST_MIN_KEYS`] (e.g. `hi\nbye\nok`) delivers its first Enter
        // before the counter reaches 4; falling through there would
        // split-send the paste exactly like the pre-fix bug. A human
        // cannot hit Enter within 80ms of the last character (motor time
        // alone is ~150ms), while a console paste delivers it in <1ms.
        // Modified Enters (Ctrl/Shift+Enter) absorb under the same rule:
        // terminals with bracketed-paste interference send pasted breaks
        // that way, and letting one fall through would type a newline the
        // shadow never mirrored — desynchronizing the burst-end replay.
        //
        // Clipboard-anchored stream first: a break the clipboard predicts is
        // swallowed (never rendered); anything else rolls back inside and
        // falls through below — a real submit among them.
        if self.paste_burst.anchor.is_some() && self.paste_burst_anchor_consume_enter(now) {
            return true;
        }
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

    /// Attempt to confirm the in-flight run as clipboard content and, on
    /// success, land the whole paste atomically (Windows only — see the
    /// module docs). Runs once the stream is long enough to judge
    /// ([`ANCHOR_MIN_CHARS`]): the clipboard is read a single time per run
    /// and the anchor engages only when the entire shadow is a strict
    /// prefix of it AND the clipboard is placeholder-worthy
    /// (`paste_would_compress`), so short everyday pastes keep their
    /// zero-machinery streaming path. The streamed span is retracted and
    /// the full clipboard goes through `handle_paste`; every later key is
    /// then verified-or-swallowed instead of rendered.
    fn paste_burst_try_anchor(&mut self) {
        // Unix pastes already arrive atomically via bracketed paste, and a
        // synchronous clipboard round-trip (X11/Wayland IPC, potentially
        // hanging) has no business sitting in the key path there.
        if !cfg!(windows) {
            return;
        }
        if !self.paste_burst.run_replayable()
            || self.paste_burst.anchor.is_some()
            || self.paste_burst.anchor_declined
            || self.paste_burst.anchor_checked
            || self.paste_burst.shadow_text_len() < ANCHOR_MIN_CHARS
        {
            return;
        }
        // One read per run: the paste source must stay fixed while verifying.
        self.paste_burst.anchor_checked = true;
        let decline = |burst: &mut PasteBurstState| {
            burst.anchor_declined = true;
        };
        let Some(clipboard) = selection::clipboard_text() else {
            decline(&mut self.paste_burst);
            return;
        };
        // Same normalization as `handle_paste` on both sides, so `\r\n`
        // clipboards compare equal to the absorbed-`\n` shadow.
        let expected = clipboard.replace("\r\n", "\n").replace('\r', "\n");
        let shadow = self.paste_burst.shadow.clone();
        if expected.len() <= shadow.len()
            || !expected.starts_with(&shadow)
            || !crate::component::prompt::paste_would_compress(&expected)
        {
            decline(&mut self.paste_burst);
            return;
        }
        // Retract exactly the streamed span (same verification as the burst-
        // end replay) so the atomic insertion neither duplicates nor loses.
        let text_len = self.paste_burst.shadow_text_len();
        let (Some(start), Some(end)) = (
            self.paste_burst.burst_start,
            self.paste_burst
                .burst_start
                .and_then(|s| char_count_to_byte_end(&self.prompt_view.input, s, text_len)),
        ) else {
            decline(&mut self.paste_burst);
            return;
        };
        let streamed: String = shadow
            .chars()
            .filter(|c| *c != '\n' && *c != '\r')
            .collect();
        if self.prompt_view.input.get(start..end) != Some(streamed.as_str()) {
            decline(&mut self.paste_burst);
            return;
        }
        self.prompt_view.input.drain(start..end);
        self.prompt_view.cursor_pos = start;
        let pre_len = self.prompt_view.input.len();
        self.prompt_view.note_activity();
        self.prompt_view.handle_paste(&expected);
        let inserted_len = self.prompt_view.input.len() - pre_len;
        self.slash_menu.update(&self.prompt_view.input);
        self.paste_burst.anchor = Some(AnchorState {
            pos: shadow.chars().count(),
            pos_bytes: shadow.len(),
            start,
            inserted_len,
            expected,
        });
    }

    /// Swallow one streamed character into an active anchor. Returns `true`
    /// when consumed (verified match, possibly completing the paste). On
    /// mismatch rolls back to exactly what arrived and returns `false`, so
    /// the key types normally through the standard path below.
    fn paste_burst_anchor_consume_text(&mut self, ch: char, now: Instant) -> bool {
        if !self.paste_burst.anchor_expects_text(ch) {
            self.rollback_anchor();
            return false;
        }
        // Keeps the burst clock alive while the stream flows (a stall then
        // resumes against the same anchor instead of looking like typing).
        self.paste_burst.note_text_key(now);
        self.paste_burst.push_text_char(ch);
        let done = {
            let a = self
                .paste_burst
                .anchor
                .as_mut()
                .expect("anchor verified above");
            a.pos += 1;
            a.pos_bytes += ch.len_utf8();
            a.pos_bytes == a.expected.len()
        };
        if done {
            self.complete_anchor();
        }
        true
    }

    /// Swallow one streamed line break into an active anchor. A break the
    /// clipboard does not predict is the user's own submit: roll back to
    /// what arrived and return `false` so it falls through to send.
    fn paste_burst_anchor_consume_enter(&mut self, now: Instant) -> bool {
        if !self.paste_burst.anchor_expects_newline() {
            self.rollback_anchor();
            return false;
        }
        self.paste_burst.note_text_key(now);
        // Inside a confirmed text burst by construction: the chain may extend.
        self.paste_burst.push_newline(now, true);
        let done = {
            let a = self
                .paste_burst
                .anchor
                .as_mut()
                .expect("anchor verified above");
            a.pos += 1;
            // Absorbed breaks are always plain `\n` (one byte).
            a.pos_bytes += 1;
            a.pos_bytes == a.expected.len()
        };
        if done {
            self.complete_anchor();
        }
        true
    }

    /// The whole clipboard arrived and the placeholder already shows it: drop
    /// the mirror and disarm the cadence entirely. The user's next Enter
    /// therefore submits instead of absorbing — the paste is provably over,
    /// which the timed heuristic alone could never know.
    fn complete_anchor(&mut self) {
        self.paste_burst.clear();
        // `clear` keeps the cadence clock; a completed paste must not:
        // anything arriving next is new input, full stop.
        self.paste_burst.last_text_key_at = None;
    }

    /// Abandon an anchor: retract the atomic insertion and land exactly what
    /// actually arrived, through the normal paste path (long received text
    /// may still compress on its own merits). Never invents content — when
    /// the prompt changed underneath, everything is left untouched and the
    /// orphaned mapping is still dropped so undo/expand never see a ghost.
    fn rollback_anchor(&mut self) {
        let Some(a) = self.paste_burst.anchor.take() else {
            return;
        };
        let received = std::mem::take(&mut self.paste_burst.shadow);
        // This stream proved un-anchorable; a fresh cadence run may judge
        // again, but never this one key-by-key.
        self.paste_burst.clear();
        self.paste_burst.anchor_checked = true;
        self.paste_burst.anchor_declined = true;
        let end = a.start + a.inserted_len;
        if self.prompt_view.input.get(a.start..end).is_none() {
            return;
        }
        self.prompt_view.input.drain(a.start..end);
        self.prompt_view.cursor_pos = a.start;
        self.prompt_view
            .pasted_parts
            .retain(|p| self.prompt_view.input.contains(&p.virtual_text));
        self.prompt_view.note_activity();
        self.prompt_view.handle_paste(&received);
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// Replay the pending run through the NORMAL paste path
    /// (`PromptView::handle_paste`): the streamed characters are retracted
    /// from the prompt and re-inserted with real line breaks — line endings
    /// normalized, and long pastes compressed into the `[Pasted ~N lines]`
    /// virtual-text placeholder — identical to what Linux/macOS receive in
    /// one `Event::Paste`. Short runs that would not compress are cleared
    /// without replaying: plain typing and held keys stay exactly as typed.
    ///
    /// An active clipboard anchor never reaches the logic below: the time-
    /// driven flush (`paste_burst_flush_if_due_at`) skips anchored runs, so
    /// arriving here with one means explicit input contradicted the stream
    /// (or keyboard ownership changed) — roll back to exactly what arrived.
    fn flush_paste_buffer(&mut self) {
        if self.paste_burst.anchor.is_some() {
            self.rollback_anchor();
            return;
        }
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
        let burst_start = self.paste_burst.burst_start;
        let Some(text) = self.paste_burst.take_replay() else {
            return;
        };

        // Retract the run's text characters from where they streamed — the
        // span starting at `burst_start` — so the replay below re-inserts
        // them at the same spot with real newlines. The old code only
        // retracted an end-of-input tail (`cursor == input.len()`), so a
        // paste into the MIDDLE of a draft never replayed: no placeholder,
        // and its absorbed line breaks were silently lost. `cursor_pos` and
        // `input` are BYTE offsets — walk chars, never subtract counts (a
        // multibyte char in the run would panic a naive byte drain).
        //
        // The span is verified before draining: if the user moved the cursor
        // or edited mid-burst, the bytes there are no longer the streamed
        // run — retracting would take foreign characters and replaying
        // would DUPLICATE the streamed text. In that case fall back to the
        // end-anchored tail (the common append case), and only give up when
        // neither matches (content stays as streamed, intact).
        let retracted = if text_len == 0 {
            // A pure-Enter run (blank lines only): nothing streamed, so the
            // replay inserts exactly the newlines the prompt never received.
            true
        } else if let Some(start) = burst_start
            && let Some(end) = char_count_to_byte_end(&self.prompt_view.input, start, text_len)
            && self.prompt_view.input.get(start..end).is_some_and(|span| {
                span == text
                    .chars()
                    .filter(|c| *c != '\n' && *c != '\r')
                    .collect::<String>()
            })
        {
            self.prompt_view.input.drain(start..end);
            self.prompt_view.cursor_pos = start;
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
        //
        // Anchored runs are skipped outright: the full paste already landed
        // atomically and the stream is still being verified — silence alone
        // must never disturb it (a stall resumes against the same anchor).
        // Only contradictory input rolls back, via the explicit flush paths.
        if !self.paste_burst.shadow_is_empty()
            && self.paste_burst.run_replayable()
            && self.prompt_owns_keyboard()
            && self.paste_burst.anchor.is_none()
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
        assert!(s.burst_start.is_none(), "start anchor reset with the run");
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

    /// A SHORT run with no absorbed Enter (plain typing, held keys,
    /// short single-line pastes) is cleared WITHOUT a replay: the streamed
    /// text stays in the prompt exactly as typed — no retraction, no
    /// placeholder compression of fast human typing.
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

    /// A LONG single-line run (no Enter, but over the shared compression
    /// threshold) IS replayed: the Unix `Event::Paste` path compresses those
    /// into `[Pasted ~N lines]`, so the Windows replay must too — otherwise
    /// a large single-paragraph paste streams forever and the placeholder
    /// never appears. The threshold lives in ONE place
    /// (`prompt::paste_would_compress`), so this tracks `handle_paste`.
    #[test]
    fn take_replay_with_long_single_line_replays_for_compression() {
        let mut s = PasteBurstState::default();
        s.note_burst_start(0);
        for _ in 0..200 {
            s.push_text_char('x');
        }
        let text = s.take_replay().expect("long single-line run must replay");
        assert_eq!(text.len(), 200);
        assert!(s.shadow.is_empty(), "shadow cleared either way");
        assert_eq!(s.shadow_text_len(), 0);
        assert!(s.burst_start.is_none(), "run state fully reset");
    }
}
