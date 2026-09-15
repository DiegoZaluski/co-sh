use std::time::Duration;

use super::{App, HOME_LOCK, isolate_home};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Windows consoles deliver a paste as PLAIN KEY EVENTS (one `Char` event per
/// character plus one plain `Enter` per line break) because crossterm <= 0.29
/// has no bracketed-paste support on Windows (crossterm-rs/crossterm#737;
/// the hybrid VT-input PR #1030 is still unreleased). On Linux/macOS the same
/// paste arrives as a single `Event::Paste` (handled in `app/events.rs`),
/// which is why the split-send bug is Windows-only.
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
