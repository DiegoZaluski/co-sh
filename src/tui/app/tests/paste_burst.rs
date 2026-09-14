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

/// THE BUG: a large multi-line paste on Windows used to be auto-split — every
/// line break inside the pasted text arrived as a plain Enter and sent whatever
/// had accumulated so far, so N lines became N messages without the user ever
/// pressing Enter. The paste must be COALESCED (nothing left in the prompt
/// char by char), flushed atomically through `handle_paste` at the burst end
/// (compressing into the `[Pasted ~N lines]` placeholder exactly like on
/// Unix), and a single real Enter must send ONE message with the full
/// expanded text.
#[tokio::test]
async fn windows_paste_burst_is_not_split_into_multiple_sends() {
    let _home = HOME_LOCK.lock();
    let mut app = app_with_session();

    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let pasted = format!("{line}\n{line}\n{line}");

    // The exact event stream a Windows console produces for that paste.
    // While the burst is armed NOTHING may remain in the prompt char by char
    // — that is the visible "streaming/typing" symptom. (The few chars that
    // fell through while the detector armed are retracted on the arming
    // transition.)
    for evt in paste_key_events(&pasted) {
        app.process_key_event(evt).unwrap();
    }
    assert!(
        app.prompt_view.input.is_empty(),
        "during the burst the paste must be buffered, not streamed into the prompt"
    );
    assert!(
        app.state.current_session().is_none_or(|s| s.messages.is_empty()),
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
        app.state.current_session().is_none_or(|s| s.messages.is_empty()),
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
        app.state.current_session().is_none_or(|s| s.messages.is_empty()),
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
    assert_eq!(session.messages.len(), 1, "only 'hi' is sent; the empty second Enter sends nothing");
    assert_eq!(super::message_prompt_text(&session.messages[0]), "hi");
}
