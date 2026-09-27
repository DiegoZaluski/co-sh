use super::super::{App, HOME_LOCK, isolate_home};
use crate::routes::session::queue_choice::QueueTarget;
use crate::types::{Message, SessionStatus};
use cosh::harness::Mode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Joined non-synthetic text of a message (what the transcript shows).
fn text_of(msg: &Message) -> String {
    msg.parts
        .iter()
        .filter_map(|p| match p {
            crate::types::Part::Text(t) if !t.synthetic => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Queue Actions (Edit/Delete/Copy on pending queued messages) ─────────

fn app_with_queues() -> App {
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session = crate::types::Session {
        id: "t".into(),
        title: "t".into(),
        created_at: 1_000,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages: vec![],
    };
    app.state.add_session(session);
    app.state.current_session_id = Some("t".into());
    // Actions always run against the session that opened the box.
    app.active_queue_actions_session = Some("t".into());
    {
        let queues = app.state.current_pending_queues_mut().unwrap();
        queues.next_loop.push_back("loop-one".into());
        queues.next_loop.push_back("loop-two".into());
        queues.next_request.push_back("request-one".into());
        queues.next_request.push_back("request-two".into());
    }
    app
}

fn open_queue_actions(app: &mut App, queue: QueueTarget, index: usize, preview: &str) {
    app.open_queue_actions_for(queue, index, preview);
}

#[tokio::test]
async fn queue_actions_keyboard_cycles_three_options() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    open_queue_actions(&mut app, QueueTarget::NextLoop, 0, "loop-one");
    assert!(app.handle_queue_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 1);
    assert!(app.handle_queue_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 2);
    // Wraps at the bottom…
    assert!(app.handle_queue_actions_dialog_key(KeyCode::Down));
    assert_eq!(app.dialog.current().unwrap().selected, 0);
    // …and upward back to the last item.
    assert!(app.handle_queue_actions_dialog_key(KeyCode::Up));
    assert_eq!(app.dialog.current().unwrap().selected, 2);
}

#[tokio::test]
async fn queue_actions_edit_moves_message_into_prompt_in_place() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.run_queue_action(0, QueueTarget::NextRequest, 0);
    assert_eq!(app.prompt_view.input, "request-one");
    assert_eq!(app.prompt_view.cursor_pos, app.prompt_view.input.len());
    let queues = app.state.current_pending_queues().unwrap();
    // In-place removal: followers shift left, order preserved.
    assert_eq!(
        queues.next_request.iter().collect::<Vec<_>>(),
        ["request-two"]
    );
    assert_eq!(queues.next_loop.len(), 2);
}

#[tokio::test]
async fn queue_actions_delete_removes_only_target_row() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.run_queue_action(1, QueueTarget::NextLoop, 0);
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-two"],
        "delete shifts followers left without reordering"
    );
    assert_eq!(queues.next_request.len(), 2);
}

#[tokio::test]
async fn queue_actions_copy_missing_index_is_noop() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.run_queue_action(2, QueueTarget::NextRequest, 99);
    // No panic, queues untouched, dialog stack untouched.
    assert_eq!(
        app.state.current_pending_queues().unwrap().queued_count(),
        4
    );
}

#[tokio::test]
async fn edited_message_requeued_into_same_queue_restores_position() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit "loop-two" (index 1 of next_loop) and send it back unchanged.
    app.run_queue_action(0, QueueTarget::NextLoop, 1);
    assert_eq!(app.prompt_view.input, "loop-two");
    app.enqueue_pending_message(QueueTarget::NextLoop, "loop-two".into());
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-one", "loop-two"],
        "same queue + unchanged text → original position"
    );
    assert!(app.edit_requeue_hint.is_none());
}

#[tokio::test]
async fn edited_message_moved_to_other_queue_goes_to_the_end() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit "request-one" (index 0 of next_request) but re-queue it into the
    // OTHER queue: it must be appended at the end.
    app.run_queue_action(0, QueueTarget::NextRequest, 0);
    app.enqueue_pending_message(QueueTarget::NextLoop, "request-one".into());
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-one", "loop-two", "request-one"]
    );
    assert_eq!(
        queues.next_request.iter().collect::<Vec<_>>(),
        ["request-two"]
    );
}

#[tokio::test]
async fn edited_text_change_still_restores_position_in_same_queue() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit "loop-two" (index 1) and re-queue an EDITED version of it into
    // the same queue: editing is the point, so the slot is preserved.
    app.run_queue_action(0, QueueTarget::NextLoop, 1);
    app.enqueue_pending_message(QueueTarget::NextLoop, "loop-two (edited)".into());
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_loop
            .iter()
            .collect::<Vec<_>>(),
        ["loop-one", "loop-two (edited)"]
    );
}

#[tokio::test]
async fn full_edit_resend_flow_restores_position_via_keys() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.state.status = SessionStatus::Working;
    app.active_loop_session_id = Some("t".into());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.queued_input_tx = Some(tx);

    // Edit "request-one" (head of next_request).
    app.run_queue_action(0, QueueTarget::NextRequest, 0);
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-two"]
    );

    // Type it back and press Enter: the queue-choice dialog opens…
    app.prompt_view.input = "request-one".into();
    app.prompt_view.cursor_pos = 11;
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.queue_choice_dialog.visible);
    // …and confirming the default ("Next request") returns the message to
    // its exact original slot.
    app.process_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.queue_choice_dialog.visible);
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-one", "request-two"]
    );
}

#[tokio::test]
async fn requeue_position_tracks_delivered_heads() {
    let _guard = HOME_LOCK.lock();
    use std::time::{Duration, Instant};
    let mut app = app_with_queues();
    // A third survivor makes insert-at-0 distinguishable from append.
    {
        let queues = app.state.current_pending_queues_mut().unwrap();
        queues.next_request.push_back("request-three".into());
    }
    app.state.status = SessionStatus::Working;
    app.active_loop_session_id = Some("t".into());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.queued_input_tx = Some(tx);

    // Edit "request-two" (index 1); deliver and acknowledge "request-one".
    app.run_queue_action(0, QueueTarget::NextRequest, 1);
    app.queue_actions_grace_until = Some(Instant::now() - Duration::from_millis(1));
    app.pump_queued_messages();
    app.event_tx
        .send(cosh::harness::HarnessEvent::UserMessageInjected {
            text: "request-one".into(),
        })
        .unwrap();
    app.poll_events();

    // The hint slid from index 1 to 0: the edited message returns to the
    // FRONT, ahead of the untouched "request-three" — not appended.
    app.enqueue_pending_message(QueueTarget::NextRequest, "request-two".into());
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-two", "request-three"]
    );
}

#[tokio::test]
async fn injection_ack_without_in_flight_marker_pops_nothing() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.state.status = SessionStatus::Working;
    app.active_loop_session_id = Some("t".into());

    // No pump call → nothing in flight; a stray ack event must not retire
    // any queued entry.
    app.event_tx
        .send(cosh::harness::HarnessEvent::UserMessageInjected {
            text: "request-one".into(),
        })
        .unwrap();
    app.poll_events();

    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_request.iter().collect::<Vec<_>>(),
        ["request-one", "request-two"],
        "the ack guard keeps the queue intact"
    );
}

#[tokio::test]
async fn promotion_remaps_the_edit_hint_to_next_loop() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.run_queue_action(0, QueueTarget::NextRequest, 1);
    match &app.edit_requeue_hint {
        Some(h) => assert_eq!(
            (h.session_id.as_str(), h.queue, h.index),
            ("t", QueueTarget::NextRequest, 1)
        ),
        other => panic!("unexpected hint {other:?}"),
    }
    // Loop end promotes leftovers; the hint must keep pointing at the same
    // message, now behind the two existing next-loop entries.
    app.promote_next_request_to_next_loop("t");
    match &app.edit_requeue_hint {
        Some(h) => assert_eq!(
            (h.session_id.as_str(), h.queue, h.index),
            ("t", QueueTarget::NextLoop, 3)
        ),
        other => panic!("unexpected hint {other:?}"),
    }
}

#[tokio::test]
async fn deleting_a_row_before_the_hint_shifts_its_home_slot() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit "request-two" (index 1): queue becomes ["request-one"].
    app.run_queue_action(0, QueueTarget::NextRequest, 1);
    // Delete the row that sat BEFORE the edited message's home slot.
    app.run_queue_action(1, QueueTarget::NextRequest, 0);
    // Re-submit: with "request-one" gone, index 1 clamps to the end — but
    // the hint slid to 0, so the message must land at the FRONT, not after
    // unrelated survivors.
    app.enqueue_pending_message(QueueTarget::NextRequest, "request-two".into());
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-two"]
    );
}

#[tokio::test]
async fn edit_hint_is_dropped_when_submitting_from_another_session() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.run_queue_action(0, QueueTarget::NextRequest, 0);
    assert!(app.edit_requeue_hint.is_some());
    // Switch to a different session and submit a fresh message there: it
    // must be sent directly, never requeued into B's queues at A's slot.
    let session_b = crate::types::Session {
        id: "b".into(),
        title: "b".into(),
        created_at: 2_000,
        title_generated: false,
        provider: None,
        model: None,
        reasoning: None,
        ctx_ids: Default::default(),
        messages: vec![],
    };
    app.state.add_session(session_b);
    app.state.current_session_id = Some("b".into());
    app.submit_prompt_message("fresh for b".into());

    assert_eq!(app.state.status, SessionStatus::Working);
    let last = app
        .state
        .current_session()
        .unwrap()
        .messages
        .last()
        .map(text_of);
    assert_eq!(last.as_deref(), Some("fresh for b"));
    // Session A's queues were not touched by B's submission.
    let a = app.state.pending_queues.get("t").unwrap();
    assert_eq!(a.next_request.len(), 1, "A kept its remaining message");
}

#[tokio::test]
async fn opening_queue_actions_holds_the_next_effective_message() {
    let _guard = HOME_LOCK.lock();
    use std::time::{Duration, Instant};
    let mut app = app_with_queues();
    app.state.status = SessionStatus::Working;
    app.active_loop_session_id = Some("t".into());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.queued_input_tx = Some(tx);

    // Opening the box grants the grace window: nothing may be delivered.
    open_queue_actions(&mut app, QueueTarget::NextLoop, 0, "loop-one");
    assert!(app.queue_actions_hold_active());
    app.pump_queued_messages();
    assert!(rx.try_recv().is_err(), "held while the actions box is open");
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .len(),
        2,
        "no queued message was consumed"
    );

    // Even after the box closes, the 5-second advantage still holds.
    app.dialog.pop();
    assert!(app.queue_actions_hold_active());
    app.pump_queued_messages();
    assert!(rx.try_recv().is_err());

    // Once the window expires, the pump delivers FIFO again — one in-flight
    // message at a time, retired only on acknowledgment.
    app.queue_actions_grace_until = Some(Instant::now() - Duration::from_millis(1));
    assert!(!app.queue_actions_hold_active());
    app.pump_queued_messages();
    assert_eq!(rx.try_recv().as_deref().ok(), Some("request-one"));
    assert!(app.next_request_in_flight);
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-one", "request-two"],
        "the deque entry stays until the injection is acknowledged"
    );
    // The harness acknowledges the injection: retire exactly that entry.
    app.event_tx
        .send(cosh::harness::HarnessEvent::UserMessageInjected {
            text: "request-one".into(),
        })
        .unwrap();
    app.poll_events();
    assert!(!app.next_request_in_flight);
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_request
            .iter()
            .collect::<Vec<_>>(),
        ["request-two"],
        "injection acknowledgment must not drop the next queued message"
    );
    app.pump_queued_messages();
    assert_eq!(rx.try_recv().as_deref().ok(), Some("request-two"));
}

#[tokio::test]
async fn in_flight_message_survives_loop_end_without_injection() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.state.status = SessionStatus::Working;
    app.active_loop_session_id = Some("t".into());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.queued_input_tx = Some(tx);

    // The head is handed to the loop's channel but the harness ends (stop /
    // error / clean Done) before injecting it.
    app.pump_queued_messages();
    assert!(app.next_request_in_flight);

    // Loop end without auto-start: nothing may be lost — the in-flight head
    // is still owned by the queues (promoted to next-loop semantics, the
    // long-standing end-of-run behavior).
    app.handle_loop_end(false);
    assert!(!app.next_request_in_flight);
    let queues = app.state.current_pending_queues().unwrap();
    assert!(queues.next_request.is_empty());
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-one", "loop-two", "request-one", "request-two"]
    );
}

#[tokio::test]
async fn loop_end_while_held_defers_the_auto_start() {
    let _guard = HOME_LOCK.lock();
    use std::time::{Duration, Instant};
    let mut app = app_with_queues();
    app.active_loop_session_id = Some("t".into());
    open_queue_actions(&mut app, QueueTarget::NextLoop, 0, "loop-one");

    // Clean end while held: neither promotion nor auto-start may touch the
    // queues — stored dialog indexes must stay valid.
    assert!(app.handle_loop_end(true));
    assert!(app.queue_actions_deferred_start);
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-one", "loop-two"]
    );
    assert_eq!(
        queues.next_request.iter().collect::<Vec<_>>(),
        ["request-one", "request-two"]
    );

    // Hold expires (box closed, 5s window over): the pump promotes the
    // leftovers and starts the deferred loop from the queue head.
    app.dialog.pop();
    app.queue_actions_grace_until = Some(Instant::now() - Duration::from_millis(1));
    app.pump_queued_messages();
    assert!(!app.queue_actions_deferred_start);
    assert_eq!(
        app.state
            .current_pending_queues()
            .unwrap()
            .next_loop
            .iter()
            .collect::<Vec<_>>(),
        ["loop-two", "request-one", "request-two"],
        "the deferred start consumed exactly one message"
    );
}

#[tokio::test]
async fn queue_actions_dialog_renders_title_and_options() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    open_queue_actions(&mut app, QueueTarget::NextRequest, 0, "request-one");
    let theme = app.theme.clone();
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.dialog.render(
        &mut buf,
        ratatui::layout::Rect::new(0, 0, 80, 24),
        &theme,
        std::time::SystemTime::now(),
    );
    let line = |y: u16| -> String {
        (0..80)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("")
    };
    let all: String = (0..24).map(line).collect();
    assert!(all.contains("Queue Actions"), "title rendered");
    assert!(all.contains("Edit") && all.contains("Delete") && all.contains("Copy"));
}

#[tokio::test]
async fn hovered_queue_row_background_is_white() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Hover now identifies the owning MESSAGE as `(queue_index, msg_index)`:
    // message 0 of the next-request queue. Highlighting a message lights up
    // its whole band (text rows AND its own padding rows).
    app.hovered_queue_row = Some((1, 0));
    // app_with_queues() has 2 next-loop + 2 next-request messages; the
    // layout interleaves [pad, text, pad] per message, so the strip holds
    // 4 × 3 = 12 rows: next-loop rows at y=5..=10, next-request rows at
    // y=11..=16.
    let area = ratatui::layout::Rect::new(2, 5, 60, 12);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.render_pending_queues(&mut buf, area);
    // Layout row 6 is the top pad of next-request message 0: hovered →
    // white.
    assert_eq!(
        buf[(5, 11)].style().bg,
        Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "hovering a message must highlight its own padding too"
    );
    // Layout row 7 is its text row: hovered → white as well.
    assert_eq!(
        buf[(5, 12)].style().bg,
        Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "hovered message text row must be white"
    );
    // Layout row 9 starts the SECOND next-request message (its top pad): it
    // must keep the queue color — hovering one message never bleeds into
    // the next one's band.
    assert_ne!(
        buf[(5, 14)].style().bg,
        Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "the next message's band must not be highlighted"
    );
    // The next-loop band above is untouched too.
    assert_ne!(
        buf[(5, 5)].style().bg,
        Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "the next-loop band must not be highlighted"
    );
}

// ── Mode-colored end cap with NEXT / CLOSURE labels ──────────────────────

/// Each message band ends in a ~20% mode-colored cap labeling its queue:
/// the cap fill uses the current harness mode's color (Yolo → theme.warning
/// here), keeps it even while the band is hovered, and writes CLOSURE
/// (next-loop) / NEXT (next-request) bold on the band's middle row, with
/// black-or-white text per the cap background's luminance.
#[tokio::test]
async fn queue_cap_paints_mode_color_and_labels() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.state.mode = Mode::Yolo;
    // 4 messages × 3 layout rows (pad, text, pad) = 12 rows. Loop rows sit
    // at layout rows 0..=5, request rows at 6..=11; each band's middle row
    // (start + len/2) carries the cap label.
    let area = ratatui::layout::Rect::new(2, 5, 60, 12);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.render_pending_queues(&mut buf, area);

    let (r, g, b, _) = app.theme.warning.to_ints();
    let cap_bg = ratatui::style::Color::Rgb(r, g, b);
    // The ~80% of the band outside the cap is the mode color lightened
    // toward white (15% white blend): a lighter wash of the same hue.
    let up = |c: u8| (f32::from(c) * 0.85 + 255.0 * 0.15).round() as u8;
    let band_bg = ratatui::style::Color::Rgb(up(r), up(g), up(b));
    // Cap width = max(60 * 20%, 9) = 12 columns, right-aligned before the
    // right `┃` (column 61): columns 49..=60. Row 0 is a top pad row — the
    // cap fills it in the mode color.
    assert_eq!(
        buf[(50, 5)].style().bg,
        Some(cap_bg),
        "cap fill must use the current mode's color"
    );
    // The band's own background is the lightened mode color.
    assert_eq!(
        buf[(10, 5)].style().bg,
        Some(band_bg),
        "band background must be the mode color lightened toward white"
    );
    assert_ne!(
        buf[(10, 5)].style().bg,
        Some(cap_bg),
        "the cap must be a deeper chip on the lighter band"
    );
    // The side `┃` rails paint in the mode color too (the same hue the
    // prompt's side borders use), so the strip connects with the prompt.
    // The left rail sits at area.x (column 2), the right at column 61.
    assert_eq!(
        buf[(2, 5)].style().fg,
        Some(cap_bg),
        "left rail must use the current mode's color"
    );
    assert_eq!(
        buf[(61, 5)].style().fg,
        Some(cap_bg),
        "right rail must use the current mode's color"
    );

    let cap_row = |y: u16| -> String { (49..61).map(|x| buf[(x, y)].symbol()).collect() };
    // Loop bands (middle rows at layout rows 1 and 4 → y=6 and y=9) read
    // CLOSURE — those messages only leave the queue when a fresh agent loop
    // starts, closing out the current run.
    assert!(cap_row(6).contains("CLOSURE"), "got {:?}", cap_row(6));
    assert!(cap_row(9).contains("CLOSURE"), "got {:?}", cap_row(9));
    // Request bands (middle rows at layout rows 7 and 10 → y=12 and y=15)
    // read NEXT — those messages go out on the very next request of the
    // running loop.
    assert!(cap_row(12).contains("NEXT"), "got {:?}", cap_row(12));
    assert!(cap_row(15).contains("NEXT"), "got {:?}", cap_row(15));

    // Label cells are bold, and their text is black or white per the cap
    // background's luminance (never the band's own text color).
    let label_cell = &buf[(53, 6)]; // inside "CLOSURE": label_x 49 + (12-7)/2
    assert!(
        label_cell
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "cap label must be bold"
    );
    assert!(
        label_cell.style().fg == Some(ratatui::style::Color::Rgb(0, 0, 0))
            || label_cell.style().fg == Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "cap label must be black or white per the cap luminance"
    );

    // Hovering a message keeps the cap's mode color — it is the identity
    // chip, not part of the white highlight.
    app.hovered_queue_row = Some((1, 0));
    let mut buf2 = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
    app.render_pending_queues(&mut buf2, area);
    assert_ne!(
        buf2[(50, 11)].style().bg,
        Some(ratatui::style::Color::Rgb(255, 255, 255)),
        "hover must not whiten the cap"
    );
    assert_eq!(
        buf2[(50, 11)].style().bg,
        Some(cap_bg),
        "hovered band's cap keeps the mode color"
    );
}

// ── Edited submit racing the loop end (status already Idle) ─────────────

#[tokio::test]
async fn edited_submit_after_loop_end_requeues_instead_of_direct_send() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit "request-two" (index 1 of next_request): it leaves the queue and
    // the edit hint is armed.
    app.run_queue_action(0, QueueTarget::NextRequest, 1);

    // The agent loop ends right at that moment: status is already Idle when
    // the user submits.
    app.state.status = SessionStatus::Idle;
    app.prompt_view.input = "request-two".into();
    app.prompt_view.cursor_pos = 11;
    app.process_key_event(key(KeyCode::Enter)).unwrap();

    let queues = app.state.current_pending_queues().unwrap();
    assert!(
        queues.next_request.is_empty(),
        "leftover next-request messages are promoted like a clean loop end"
    );
    // FIFO chain: "loop-one" heads the queue — the edited message waits in
    // its due slot instead of jumping ahead as a direct input.
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-two", "request-one", "request-two"]
    );
    let session = app.state.current_session().unwrap();
    let last = session.messages.last().map(text_of);
    assert_eq!(last.as_deref(), Some("loop-one"));
    assert!(app.edit_requeue_hint.is_none());
}

#[tokio::test]
async fn edited_next_loop_submit_after_loop_end_starts_from_queue_head() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    // Edit the HEAD of next_loop.
    app.run_queue_action(0, QueueTarget::NextLoop, 0);

    app.state.status = SessionStatus::Idle;
    app.prompt_view.input = "loop-one".into();
    app.prompt_view.cursor_pos = 8;
    app.process_key_event(key(KeyCode::Enter)).unwrap();

    let queues = app.state.current_pending_queues().unwrap();
    // Re-inserted at index 0, leftovers promoted behind it, head starts.
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-two", "request-one", "request-two"]
    );
    let last = app
        .state
        .current_session()
        .unwrap()
        .messages
        .last()
        .map(text_of);
    assert_eq!(last.as_deref(), Some("loop-one"));
}

#[tokio::test]
async fn edited_submit_during_hold_defers_the_chain_start() {
    let _guard = HOME_LOCK.lock();
    use std::time::{Duration, Instant};
    let mut app = app_with_queues();
    app.run_queue_action(0, QueueTarget::NextRequest, 1);

    // Loop ended AND the 5s grace from opening the box is still active.
    app.state.status = SessionStatus::Idle;
    app.queue_actions_grace_until = Some(Instant::now() + Duration::from_secs(5));
    app.prompt_view.input = "request-two".into();
    app.prompt_view.cursor_pos = 11;
    app.process_key_event(key(KeyCode::Enter)).unwrap();

    // Nothing starts while held; the message sits in its slot.
    assert_eq!(app.state.status, SessionStatus::Idle);
    assert!(app.queue_actions_deferred_start);
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_request.iter().collect::<Vec<_>>(),
        ["request-one", "request-two"]
    );

    // Hold expires: the pump promotes and starts the chain from the head.
    app.queue_actions_grace_until = Some(Instant::now() - Duration::from_millis(1));
    app.pump_queued_messages();
    assert_eq!(app.state.status, SessionStatus::Working);
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(
        queues.next_loop.iter().collect::<Vec<_>>(),
        ["loop-two", "request-one", "request-two"]
    );
    let last = app
        .state
        .current_session()
        .unwrap()
        .messages
        .last()
        .map(text_of);
    assert_eq!(last.as_deref(), Some("loop-one"));
}

#[tokio::test]
async fn plain_submit_without_outstanding_edit_still_sends_directly() {
    let _guard = HOME_LOCK.lock();
    let mut app = app_with_queues();
    app.state.status = SessionStatus::Idle;

    app.prompt_view.input = "fresh message".into();
    app.prompt_view.cursor_pos = 13;
    app.process_key_event(key(KeyCode::Enter)).unwrap();

    // No edit hint → normal send semantics are untouched.
    assert_eq!(app.state.status, SessionStatus::Working);
    let session = app.state.current_session().unwrap();
    let last = session.messages.last().map(text_of);
    assert_eq!(last.as_deref(), Some("fresh message"));
    // Queues untouched (no promotion was triggered by a direct send).
    let queues = app.state.current_pending_queues().unwrap();
    assert_eq!(queues.queued_count(), 4);
}
