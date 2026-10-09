use std::time::Duration;

use super::{App, CLIPBOARD_LOCK, HOME_LOCK, isolate_home};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Legacy Windows consoles deliver a paste as PLAIN KEY EVENTS (one `Char`
/// event per character plus one plain `Enter` per line break) because stock
/// crossterm 0.29 has no bracketed-paste support there
/// (crossterm-rs/crossterm#737). Modern consoles (patched crossterm, VT-input
/// hybrid PR #1030) deliver a single `Event::Paste` exactly like Linux/macOS
/// (handled in `app/events.rs`) — these tests pin the legacy-stream fallback
/// that still guards conhost without VT input.
fn paste_key_events(text: &str) -> Vec<KeyEvent> {
    text.chars()
        .map(|ch| {
            if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            } else {
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
            }
        })
        .collect()
}

fn app_with_session() -> App {
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app
}

/// Larger than the burst window (80ms): simulates the human reflex gap
/// between finishing a paste and pressing Enter. After this sleep the burst
/// is no longer armed, so `paste_burst_flush_if_due()` actually flushes.
const HUMAN_KEY_GAP: Duration = Duration::from_millis(120);

fn end_burst_window() {
    std::thread::sleep(HUMAN_KEY_GAP);
}

/// REGRESSION (the held-key vanish bug): holding a key on Linux/macOS/Windows
/// produces OS auto-repeat events (same character, ~20-33ms cadence). The
/// paste-burst coalescer's cadence counter used to arm on those repeats, so
/// the held key's characters vanished into the buffer and only appeared when
/// the key was released. Auto-repeat must stream into the prompt like normal
/// typing, and the Enter that follows must send.
#[tokio::test]
async fn held_key_auto_repeat_streams_into_the_prompt() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    // Press 's' once, then keep it held: OS repeats arrive at ~33ms cadence.
    // The first press is a real keypress; every subsequent one is a repeat.
    app.process_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    let mut expected = 1usize;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(33));
        app.process_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        expected += 1;
        // The repeat must be VISIBLE in the prompt immediately — not parked
        // in the coalescer buffer until key release.
        assert_eq!(
            app.prompt_view.input.chars().filter(|c| *c == 's').count(),
            expected,
            "held-key repeats must stream into the prompt, got: {:?}",
            app.prompt_view.input
        );
    }

    // Release the key, then press Enter: the typed text sends as one message
    // (the repeat run must not make the Enter classify as a paste newline).
    end_burst_window();
    app.paste_burst_flush_if_due();
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    let session = app.state.current_session().expect("session exists");
    assert_eq!(
        session.messages.len(),
        1,
        "held-key text must send on Enter"
    );
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        "sssssssssss",
        "all repeat characters must be present in the sent message"
    );
}

/// THE BUG: a large multi-line paste on Windows used to be auto-split — every
/// line break inside the pasted text arrived as a plain Enter and sent whatever
/// had accumulated so far, so N lines became N messages without the user ever
/// pressing Enter. The coalescer must NEVER split-send: paste line breaks are
/// absorbed as newlines (never reaching `SendMessage`), and at burst end the
/// streamed run is retracted and replayed through `handle_paste`, compressing
/// into the `[Pasted ~N lines]` placeholder exactly like on Unix. Characters
/// themselves stream into the prompt in real time — the coalescer never
/// holds them back (the held-key vanish regression) — so DURING the burst
/// the raw text is visible in the input.
#[tokio::test]
async fn windows_paste_burst_is_not_split_into_multiple_sends() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let pasted = format!("{line}\n{line}\n{line}");

    // The exact event stream a Windows console produces for that paste.
    // Characters stream into the prompt (real-time render); the absorbed
    // Enters are NOT typed, so the input is the concatenation of the lines.
    for evt in paste_key_events(&pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert_eq!(
        app.prompt_view.input,
        pasted.replace('\n', ""),
        "characters must stream in real time; absorbed Enters must not be typed"
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "paste line breaks must NEVER auto-send (the user did not press Enter)"
    );

    // Burst window over: the coalescer replays the WHOLE paste through
    // `PromptView::handle_paste` in one atomic step.
    end_burst_window();
    app.paste_burst_flush_if_due();

    let line_count = pasted.matches('\n').count() + 1;
    assert_eq!(
        app.prompt_view.input,
        format!("[Pasted ~{line_count} lines]"),
        "a long paste must compress into the placeholder, exactly like the Unix Event::Paste path"
    );

    // The real user Enter submits — and the sent message carries the FULL
    // text (the placeholder expands back on send).
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    let session = app.state.current_session().expect("session exists");
    assert_eq!(
        session.messages.len(),
        1,
        "one real Enter after a paste burst must send exactly ONE message, got: {:?}",
        session
            .messages
            .iter()
            .map(super::message_prompt_text)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "the sent message must carry the FULL pasted text (placeholder expanded back)"
    );
}

/// REGRESSION (crossterm#962 interference): with bracketed paste requested on
/// Windows, terminals deliver pasted `\n` as Enter WITH the CONTROL modifier.
/// Those must absorb exactly like plain Enters — letting one fall through
/// types a newline the shadow never mirrors, so the burst-end retraction
/// drains the wrong span and mixes raw text with a bogus `[Pasted ~1 lines]`.
/// (Bracketed paste is no longer requested on Windows in `terminal.rs`, but
/// the coalescer stays immune to any terminal with the same quirk.)
#[tokio::test]
async fn ctrl_enter_line_breaks_absorb_inside_burst() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let pasted = format!("{line}\n{line}\n{line}");

    // The exact Windows stream under bracketed-paste interference: every
    // line break carries CONTROL.
    for ch in pasted.chars() {
        let evt = if ch == '\n' {
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
        } else {
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
        };
        app.process_key_event(evt).unwrap();
    }
    // No split-send, and the Ctrl+Enters were absorbed, never typed.
    assert_eq!(
        app.prompt_view.input,
        pasted.replace('\n', ""),
        "modified Enters must absorb like plain ones, got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "paste line breaks must NEVER auto-send (the user did not press Enter)"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();

    let line_count = pasted.matches('\n').count() + 1;
    assert_eq!(
        app.prompt_view.input,
        format!("[Pasted ~{line_count} lines]"),
        "absorbed Ctrl+Enters must replay with real line breaks and compress, got: {:?}",
        app.prompt_view.input
    );

    // The real user Enter submits — the placeholder expands back on send.
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "the sent message must carry the FULL pasted text (placeholder expanded back)"
    );
}

/// A modified Enter OUTSIDE any burst keeps its shortcut meaning: Shift+Enter
/// after a human pause still inserts a newline instead of submitting.
#[tokio::test]
async fn modified_enter_outside_burst_still_inserts_newline() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    for evt in paste_key_events("hello") {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.prompt_view.input, "hello\n",
        "Shift+Enter outside the burst must insert a newline, got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "a newline shortcut must never submit"
    );
}

/// Even single-line pastes that end in a trailing newline must not send: the
/// trailing Enter is part of the paste, not a user submit.
#[tokio::test]
async fn paste_burst_trailing_newline_does_not_send() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let pasted = "some pasted sentence that goes on for a while\n";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "a paste ending in a newline must not auto-send"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();
    // 43 chars < PASTE_MIN_CHARS(150), 2 lines < PASTE_MIN_LINES(3) → the
    // short-paste branch inserts the text directly.
    assert_eq!(app.prompt_view.input, pasted);
}

/// Blank lines inside a pasted block produce back-to-back Enters with no
/// characters between them — the chained-Enter rule (anchored to the text
/// burst) classifies every one of them as a paste artifact too, and the flush
/// lands the whole block (with its blank lines) through `handle_paste`.
#[tokio::test]
async fn paste_burst_consecutive_newlines_do_not_send() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let pasted = "first chunk line one\n\n\nsecond chunk after blank lines";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "blank lines inside a paste must not auto-send"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();
    // 4 lines >= PASTE_MIN_LINES(3) → compressed into the placeholder.
    assert_eq!(app.prompt_view.input, "[Pasted ~4 lines]");
}

/// Anti-false-positive: a human typing at a realistic cadence (gap larger
/// than the 80ms burst window) and pressing Enter must still send normally —
/// the burst gate only arms on inhumanly fast input (4+ text keys inside
/// 80ms; the fastest human typing is ~15 chars/s).
#[tokio::test]
async fn human_typing_then_enter_still_sends() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    for ch in "hello world".chars() {
        app.process_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
        std::thread::sleep(HUMAN_KEY_GAP);
    }
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1, "typed message must send");
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        "hello world"
    );
}

/// Short pastes followed by a plain Enter must keep working exactly as
/// before: the Enter comes after the human reflex gap, outside the burst
/// window, so it submits instead of becoming a newline. A short paste is
/// inserted verbatim (no placeholder), like the Unix path.
#[tokio::test]
async fn short_paste_then_enter_sends_single_message() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    for evt in paste_key_events("short paste") {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(app.prompt_view.input, "short paste");

    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        "short paste"
    );
}

/// The Enter chain must be anchored to a real text burst: mashing Enter in
/// the EMPTY prompt can never be swallowed — after the first press the chain
/// flag is not set (no text burst preceded it), so every Enter submits.
#[tokio::test]
async fn mashed_enters_in_empty_prompt_still_send() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    for ch in "hi".chars() {
        app.process_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
        end_burst_window();
    }
    // Two Enters faster than the burst window: only the text-burst chain
    // could absorb the second one — and no text burst armed here, so BOTH
    // submit: first sends "hi", the second sends nothing (empty prompt).
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    let session = app.state.current_session().expect("session exists");
    assert_eq!(
        session.messages.len(),
        1,
        "only 'hi' is sent; the empty second Enter sends nothing"
    );
    assert_eq!(super::message_prompt_text(&session.messages[0]), "hi");
}

/// REGRESSION (fix #1): a paste whose lines are SHORTER than the arming
/// threshold (4 text keys) used to split-send: the first Enter arrived
/// before `text_keys_in_window` reached 4 and fell through to SendMessage,
/// submitting `hi` as its own message. An Enter within 80ms of ANY recent
/// text key — armed or not — must be absorbed as a paste newline instead.
#[tokio::test]
async fn short_line_paste_is_not_split_into_multiple_sends() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let pasted = "hi\nbye\nok";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "short lines must not split-send: Enter within 80ms of text is a paste artifact"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();
    // 8 chars < PASTE_MIN_CHARS(150), 3 lines >= PASTE_MIN_LINES(3) → the
    // whole paste compresses into the placeholder, byte-identical content.
    assert_eq!(app.prompt_view.input, "[Pasted ~3 lines]");

    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "placeholder must expand back to the FULL short-line paste"
    );
}

/// REGRESSION (fix #2): when a modal opens mid-burst the pending paste must
/// LAND in the prompt, not be dropped — a paste is user data. The pre-fix
/// behaviour discarded the buffer, silently losing everything past the
/// first characters.
#[tokio::test]
async fn dialog_mid_burst_lands_paste_instead_of_dropping() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let tail = "rest of the pasted content that must survive the dialog";
    // Arm the burst with the first four chars, keep the tail buffered...
    for evt in paste_key_events("head") {
        app.process_key_event(evt).unwrap();
    }
    for ch in tail.chars() {
        app.process_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }

    // A modal opens mid-burst (exactly what Ctrl+C does: show the quit
    // confirm). The very next key must NOT discard the pending paste.
    app.dialog.show(crate::ui::dialogs::DialogType::Confirm {
        message: "Quit cosh?".into(),
    });
    app.process_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();

    // The dialog is sovereign (the modal test below asserts it swallows
    // keys); close it and inspect the prompt.
    app.dialog.pop();
    assert!(
        app.prompt_view.input.contains("head") && app.prompt_view.input.contains(&tail[..20]),
        "pending paste must land in the prompt when ownership is lost, got: {:?}",
        app.prompt_view.input
    );
}

/// REGRESSION (fix #3): AltGr characters (Char + CTRL+ALT — the Windows
/// console signature for `@ # $ €` on ABNT) must count as paste text keys.
/// Pre-fix, each one flushed the coalescer mid-burst, so the paste's own
/// Enters were classified as user submits (split-send). They now count as
/// text: they stream into the prompt like any other character.
#[tokio::test]
async fn altgr_chars_keep_the_burst_coalescing() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let pasted = "user@mail.com COSTS $100 #tag";
    // Simulate ABNT AltGr keys: every Char carries CTRL|ALT.
    for ch in pasted.chars() {
        let evt = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL | KeyModifiers::ALT);
        app.process_key_event(evt).unwrap();
    }
    assert_eq!(
        app.prompt_view.input, pasted,
        "AltGr chars are text keys: they stream into the prompt untouched"
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "no auto-send: this AltGr run contains no Enter"
    );

    // Burst-end flush with no absorbed Enter: the run is left as typed.
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(
        app.prompt_view.input, pasted,
        "single-line run stays as typed (no replay), got: {:?}",
        app.prompt_view.input
    );
}

/// REGRESSION (stale-leak invariant): a sub-threshold run (leaked chars in
/// the prompt, burst never armed) followed by a real send must not leave a
/// stale `leaked_chars` behind — otherwise the NEXT burst's arming
/// transition would retract characters typed after the send, duplicating
/// them into the wrong buffer.
#[tokio::test]
async fn leaked_count_does_not_survive_a_send() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    // Human-cadence typed text: 2 fast chars (leaked, burst NOT armed),
    // then a human pause, then a real Enter that sends "ab".
    app.process_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    end_burst_window();
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.state
            .current_session()
            .expect("session exists")
            .messages
            .len(),
        1,
        "precondition: 'ab' sent as one message"
    );

    // Immediately after the send, a new fast burst arms. If the leaked
    // count from "ab" survived, the arming transition would retract 2
    // chars of THIS paste into the wrong buffer (duplicated later).
    let pasted = "pasted after send";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(
        app.prompt_view.input, pasted,
        "post-send paste must land byte-identical: no stale retraction"
    );
}

/// REGRESSION (fix #5): the flush path accepts an INJECTED clock
/// (`paste_burst_flush_if_due_at`), so the burst-end behaviour is testable
/// with synthetic Instants — no real sleeps, no CI flakiness. While the
/// burst is still armed the flush must NOT land; once the window elapses it
/// must land exactly once (retract the streamed run, replay with its real
/// line break through `handle_paste`).
#[tokio::test]
async fn flush_at_is_deterministic_on_injected_clock() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    // Multi-line paste: the absorbed Enter makes the run replay-worthy at
    // burst end (a single-line run would legitimately stay as typed).
    let pasted = "deterministic flush\nsecond line";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    // Characters streamed (the absorbed Enter was not typed) and the armed
    // burst's per-key flush must NOT land while keys keep arriving.
    assert_eq!(
        app.prompt_view.input,
        pasted.replace('\n', ""),
        "precondition: run streamed, not yet replayed"
    );

    // Synthetic now far past the window: must land exactly once. The window
    // is read through the paste_burst module's re-export so the test tracks
    // the real threshold instead of hard-coding 80ms.
    let later = std::time::Instant::now() + crate::app::paste_burst::BURST_WINDOW;
    app.paste_burst_flush_if_due_at(later);
    // 29 chars < PASTE_MIN_CHARS(150), 2 lines < PASTE_MIN_LINES(3) → the
    // replay retracts the streamed run and inserts the text WITH its newline.
    assert_eq!(
        app.prompt_view.input, pasted,
        "flush past the window must land the run with its real line break"
    );
    // Idempotent: a second flush with nothing buffered must not duplicate.
    app.paste_burst_flush_if_due_at(later + Duration::from_secs(1));
    assert_eq!(
        app.prompt_view.input, pasted,
        "flush must be idempotent once drained"
    );
}

/// REGRESSION (fix #4 companion): a REAL modal (the quit confirm) must
/// disarm the coalescer — while it is open, a text key flushes the pending
/// paste (lands it in the prompt) and is then swallowed by the modal, never
/// typed anywhere. Guards the hand-maintained `prompt_owns_keyboard`
/// mirror: if a new modal gate is forgotten there, this class of test
/// catches the leak.
#[tokio::test]
async fn modal_sovereignty_disarms_the_coalescer() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    for evt in paste_key_events("pasted before modal") {
        app.process_key_event(evt).unwrap();
    }
    app.dialog.show(crate::ui::dialogs::DialogType::Confirm {
        message: "Quit cosh?".into(),
    });
    // Text key while the modal is up: flushes, then falls through to the
    // sovereign gate — which must swallow it (prompt must NOT receive 'x').
    app.process_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !app.prompt_view.input.contains('x'),
        "modal must swallow the key after the flush, got: {:?}",
        app.prompt_view.input
    );
    // The pending paste landed (never dropped): flush happened on the
    // ownership-loss path.
    assert!(
        app.prompt_view.input.contains("pasted before modal"),
        "ownership loss must LAND the paste, got: {:?}",
        app.prompt_view.input
    );
}

/// The opencode mini-port (anomalyco/opencode#13871): Ctrl+V reads the system
/// clipboard and routes the text through `PromptView::handle_paste` — the SAME
/// pipeline as a terminal paste — so a Windows multi-line clipboard lands as
/// `[Pasted ~N lines]` deterministically, with no reliance on bracketed paste
/// (which Windows terminals do not reliably emit and crossterm's Windows
/// backend cannot even deliver).
///
/// Scope note (deliberate divergence from the Unix `Event::Paste` router in
/// `events.rs`, which dispatches into dialogs/fields): this port covers ONLY
/// the chat prompt — the motivating bug is pasting a multi-line message into
/// it on Windows. Surfaces with their own paste routing (registration fields,
/// question dialog, RAG inputs) are untouched; extending Ctrl+V to them is a
/// follow-up that should mirror `events.rs`'s ownership chain.
#[tokio::test]
async fn ctrl_v_pastes_clipboard_text_through_the_normal_paste_path() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    let mut app = app_with_session();

    // Headless CI has no clipboard at all: the paste path can't be exercised
    // there — but the key-consumption behaviour below still applies.
    if !crate::util::selection::clipboard_available() {
        return;
    }
    let pasted = "clipboard line one\nclipboard line two\nclipboard line three";
    assert!(
        crate::util::selection::copy_to_clipboard(pasted),
        "precondition: clipboard write succeeds"
    );

    app.process_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();

    // 3 lines >= PASTE_MIN_LINES(3) → compressed into the placeholder,
    // exactly like the Unix Event::Paste path.
    assert_eq!(
        app.prompt_view.input, "[Pasted ~3 lines]",
        "Ctrl+V must route clipboard text through handle_paste (placeholder compression), got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "paste must not auto-send"
    );

    // The real Enter submits — the placeholder expands back on send.
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "sent message must carry the FULL clipboard text (placeholder expanded back)"
    );
}

/// Ctrl+V with an unavailable/empty clipboard must be CONSUMED, never fall
/// through to the typing path — a fall-through would type a stray `v` into
/// the prompt (the same reason opentui ignores ctrl-modified keys).
#[tokio::test]
async fn ctrl_v_with_no_clipboard_text_never_types_a_stray_v() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    let mut app = app_with_session();

    // Best effort: clear whatever the environment clipboard holds.
    if crate::util::selection::clipboard_available() {
        crate::util::selection::copy_to_clipboard("");
    }

    app.process_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .unwrap();

    assert!(
        app.prompt_view.input.is_empty(),
        "Ctrl+V without clipboard text must not type anything, got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "and must not send"
    );
}

/// AltGr guard: on Windows consoles an AltGr character arrives as Char +
/// CTRL+ALT — including `v` on some layouts. That is TEXT, never the Ctrl+V
/// paste hotkey: it must stream into the prompt like any other character.
#[tokio::test]
async fn altgr_v_types_the_character_instead_of_pasting() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    let mut app = app_with_session();

    // Put text on the clipboard when possible: the assert below is only
    // meaningful if a paste WOULD have been visible.
    if crate::util::selection::clipboard_available() {
        crate::util::selection::copy_to_clipboard("SHOULD NOT PASTE");
    }

    app.process_key_event(KeyEvent::new(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();

    assert_eq!(
        app.prompt_view.input, "v",
        "AltGr+v is a text character: it types, it never pastes"
    );
}

/// REGRESSION (Windows `[Pasted ~N lines]` never appearing): a long
/// SINGLE-LINE paste (>150 chars, no line break) arrives as plain Char keys
/// with no Enter at all, so the old replay gate (`shadow.contains('\n')`)
/// never fired and the streamed paragraph stayed as raw text forever — while
/// the SAME text on Linux/macOS arrives as one `Event::Paste` and compresses
/// into the placeholder. The replay decision is now shared with
/// `PromptView::handle_paste` (`paste_would_compress`), so the burst-end
/// replay retracts the streamed run and compresses it exactly like Unix.
#[tokio::test]
async fn long_single_line_paste_compresses_into_placeholder() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    // 200 chars, no newline: over PASTE_MIN_CHARS (150), one line.
    let pasted: String = std::iter::repeat_n('x', 200).collect();
    for evt in paste_key_events(&pasted) {
        app.process_key_event(evt).unwrap();
    }
    // No Enter was ever absorbed, yet the run streamed visibly.
    assert_eq!(
        app.prompt_view.input, pasted,
        "precondition: single-line run streams into the prompt"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();

    assert_eq!(
        app.prompt_view.input, "[Pasted ~1 lines]",
        "a long single-line paste must compress, exactly like the Unix Event::Paste path"
    );

    // The real Enter submits — the placeholder expands back on send.
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "sent message must carry the FULL single-line paste (placeholder expanded back)"
    );
}

/// REGRESSION (companion): a short single-line burst must STAY as typed —
/// plain typing and held keys never compress, only runs long enough to meet
/// the shared threshold do.
#[tokio::test]
async fn short_single_line_burst_stays_as_typed() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let pasted = "just a short sentence";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(
        app.prompt_view.input, pasted,
        "short single-line run stays as typed (no replay), got: {:?}",
        app.prompt_view.input
    );
}

/// REGRESSION (Windows paste into the middle of a draft): the burst-end
/// replay used to retract only an end-of-input tail
/// (`cursor == input.len()`), so a multi-line paste streamed mid-prompt
/// never replayed — no placeholder AND its absorbed line breaks silently
/// lost. The run now anchors at its start offset, so the span retracts from
/// where it streamed and the placeholder lands mid-draft.
#[tokio::test]
async fn mid_prompt_multiline_paste_compresses_in_place() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    // Draft "AB" with the cursor between them.
    for evt in paste_key_events("AB") {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(app.prompt_view.input, "AB");
    app.process_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.prompt_view.cursor_pos, 1);

    // Multi-line paste at the mid-prompt cursor.
    let pasted = "line1\nline2\nline3";
    for evt in paste_key_events(pasted) {
        app.process_key_event(evt).unwrap();
    }

    end_burst_window();
    app.paste_burst_flush_if_due();

    assert_eq!(
        app.prompt_view.input, "A[Pasted ~3 lines]B",
        "mid-prompt paste must retract its span and compress in place, got: {:?}",
        app.prompt_view.input
    );

    // Submit carries the full text with the paste expanded mid-draft.
    app.process_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        format!("A{pasted}B"),
        "sent message must carry the draft with the paste expanded in place"
    );
}

/// ELEGANCE (opencode parity): a clipboard paste trickling in slowly (VS Code
/// PowerShell and friends inject word-by-word over seconds) must not stream
/// visibly until the end. Once the armed stream content-matches the clipboard
/// the whole paste lands atomically and the tail is swallowed — the
/// placeholder appears after a handful of characters, exactly like the Unix
/// `Event::Paste` path.
///
/// Windows-only: the anchor is `cfg!(windows)`-gated (Unix already pastes
/// atomically; a clipboard round-trip has no place in its key path), and the
/// clipboard dance needs a real system clipboard like the Ctrl+V test above.
#[cfg(windows)]
#[tokio::test]
async fn anchored_trickle_paste_lands_atomically_without_streaming() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    if !crate::util::selection::clipboard_available() {
        return;
    }
    let mut app = app_with_session();

    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let pasted = format!("{line}\n{line}\n{line}");
    assert!(
        crate::util::selection::copy_to_clipboard(&pasted),
        "precondition: clipboard write succeeds"
    );

    // Slow trickle: 25ms gaps keep the burst armed but stay readable —
    // the old design streamed every one of these characters.
    let mut fed = 0usize;
    for evt in paste_key_events(&pasted) {
        app.process_key_event(evt).unwrap();
        fed += 1;
        std::thread::sleep(Duration::from_millis(25));
        if fed == 10 {
            // Anchor engaged on content-match: the placeholder is already
            // there with most of the stream still unsent.
            assert_eq!(
                app.prompt_view.input, "[Pasted ~3 lines]",
                "the paste must land atomically mid-stream, got: {:?}",
                app.prompt_view.input
            );
        }
    }
    // The swallowed tail never re-rendered.
    assert_eq!(
        app.prompt_view.input, "[Pasted ~3 lines]",
        "swallowed keys must never reach the prompt, got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "paste must not auto-send"
    );

    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(app.prompt_view.input, "[Pasted ~3 lines]");

    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(session.messages.len(), 1);
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "sent message must carry the FULL pasted text (placeholder expanded back)"
    );
}

/// Anchor completion disarms the cadence: once the whole clipboard arrived,
/// the paste is provably over, so the very next Enter submits instead of
/// absorbing like the timed heuristic would.
#[cfg(windows)]
#[tokio::test]
async fn anchored_completion_submits_on_the_next_enter() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    if !crate::util::selection::clipboard_available() {
        return;
    }
    let mut app = app_with_session();

    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let pasted = format!("{line}\n{line}\n{line}");
    assert!(
        crate::util::selection::copy_to_clipboard(&pasted),
        "precondition: clipboard write succeeds"
    );

    for evt in paste_key_events(&pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert_eq!(app.prompt_view.input, "[Pasted ~3 lines]");

    // No human gap: under the old timing rule this Enter would absorb as a
    // paste newline. The completed anchor knows better — it submits.
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    let session = app.state.current_session().expect("session exists");
    assert_eq!(
        session.messages.len(),
        1,
        "the Enter right after a completed paste must send exactly ONE message"
    );
    assert_eq!(
        super::message_prompt_text(&session.messages[0]),
        pasted,
        "sent message must carry the FULL pasted text (placeholder expanded back)"
    );
}

/// Divergence rolls back to exactly what arrived: fast typing that merely
/// starts like the clipboard must never conjure the rest of it.
#[cfg(windows)]
#[tokio::test]
async fn anchor_divergence_rolls_back_to_typed_text() {
    let _home = HOME_LOCK.lock();
    let _clipboard = CLIPBOARD_LOCK.lock();
    if !crate::util::selection::clipboard_available() {
        return;
    }
    let mut app = app_with_session();

    // Long single-line clipboard; the stream matches 8 chars, then swerves.
    let clipboard: String = "abcdefgh".to_string() + &"k".repeat(200);
    assert!(
        crate::util::selection::copy_to_clipboard(&clipboard),
        "precondition: clipboard write succeeds"
    );

    for evt in paste_key_events("abcdefgh") {
        app.process_key_event(evt).unwrap();
    }
    assert_eq!(
        app.prompt_view.input, "[Pasted ~1 lines]",
        "precondition: anchor engaged on content-match, got: {:?}",
        app.prompt_view.input
    );

    // 'Z' is not the clipboard's 9th character: roll back, type it for real.
    app.process_key_event(KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.prompt_view.input, "abcdefghZ",
        "divergence must land exactly what arrived, got: {:?}",
        app.prompt_view.input
    );

    for evt in paste_key_events("ij") {
        app.process_key_event(evt).unwrap();
    }
    end_burst_window();
    app.paste_burst_flush_if_due();
    assert_eq!(
        app.prompt_view.input, "abcdefghZij",
        "short unconfirmed text stays as typed, got: {:?}",
        app.prompt_view.input
    );
    assert!(
        app.state
            .current_session()
            .is_none_or(|s| s.messages.is_empty()),
        "nothing must send"
    );
}
