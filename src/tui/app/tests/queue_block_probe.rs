//! App-level probe for the "queue message right after a block" edge case:
//! real harness-event order, real render geometry (pending-queue strip +
//! spinner shrink the session viewport), real mouse hover/click mapping.

use super::{App, HOME_LOCK, isolate_home};
use crate::routes::session::queue_choice::QueueTarget;
use crate::session_store::generate_session_id;
use crate::types::{Message, MessageRole, Part, SessionStatus, TextPart, ToolPart, ToolStatus};
use ratatui::{Terminal, backend::TestBackend};

fn mouse_move(x: u16, y: u16) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

fn mouse_click(x: u16, y: u16) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}


#[tokio::test]
async fn probe_app_queue_after_block_hover_and_click() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    let id = generate_session_id();
    app.state.add_empty_session(id.clone(), "probe".into(), 0);
    app.state.current_session_id = Some(id.clone());
    app.active_loop_session_id = Some(id.clone());
    {
        let s = app.state.current_session_mut().unwrap();
        s.messages.push(Message {
            id: "msg-0".into(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart { text: "first question".into(), synthetic: false })],
            created_at: 0,
            agent: None,
            model: None,
        });
        s.messages.push(Message {
            id: "msg-1".into(),
            role: MessageRole::Assistant,
            parts: vec![
                Part::Tool(ToolPart {
                    tool: "bash_run".into(),
                    input: serde_json::json!({ "command": "make deploy" }),
                    output: Some((0..8).map(|i| format!("deploy step {i} ok")).collect::<Vec<_>>().join("\n")),
                    status: ToolStatus::Completed,
                    tool_call_id: Some("b1".into()),
                    is_start: true,
                    is_streaming: false,
                    cached_line_count: None,
                    lsp_notes: None,
                }),
                Part::Text(TextPart { text: (0..24).map(|i| format!("streaming line number {i}")).collect::<Vec<_>>().join("\n"), synthetic: false }),
            ],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
    app.state.status = SessionStatus::Working;

    let (w, h) = (100u16, 30u16);
    app.set_test_size(w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();

    // Frame 1-2: stream the turn (warm caches).
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();

    // The user queues a message while Working (queue-choice submit path).
    app.enqueue_pending_message(QueueTarget::NextRequest, "queued follow-up".into());
    // The loop consumes it: pump hands it to the harness; the harness acks.
    app.pump_queued_messages();
    app.event_tx.send(cosh::harness::HarnessEvent::UserMessageInjected {
        text: "queued follow-up".into(),
    }).ok();
    app.poll_events();

    // Frames after injection (still Working: the turn keeps streaming).
    for _ in 0..3 {
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
    }

    // Dump the frame + geometry.
    let buf = terminal.backend().buffer().clone();
    for y in 0..h {
        let row: String = (0..w).filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string())).collect();
        eprintln!("row {y:>2} | {}", row.trim_end());
    }
    let vp = app.session_viewport_area();
    eprintln!("mouse session viewport = {vp:?}");
    eprintln!("hovered = {:?}", app.session_view.hovered_msg_idx);

    // Sweep a Move event over every row of the main area and report what the
    // hover resolves to + where the tint paints.
    let tint = crate::theme::rgba_color(app.theme.background_element);
    for y in 1..h {
        app.handle_mouse_event(mouse_move(50, y)).unwrap();
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
        let b = terminal.backend().buffer();
        let tint_rows: Vec<u16> = (0..h)
            .filter(|&yy| (0..w).any(|x| b.cell((x, yy)).map(|c| c.bg == tint).unwrap_or(false)))
            .collect();
        if app.session_view.hovered_msg_idx.is_some() || !tint_rows.is_empty() {
            eprintln!("hover y={y:>2} -> idx {:?} tint_rows={tint_rows:?}",
                app.session_view.hovered_msg_idx);
        }
    }

    // Click sweep: which rows open Message Actions and for WHICH message.
    app.handle_mouse_event(mouse_move(50, 1)).unwrap();
    for y in 1..h {
        app.handle_mouse_event(mouse_click(50, y)).unwrap();
        app.handle_mouse_event(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 50, row: y, modifiers: crossterm::event::KeyModifiers::NONE,
        }).unwrap();
        if let Some(dialog) = app.dialog.current() {
            if let crate::ui::dialogs::DialogType::MessageActions { message_id, .. } = &dialog.dialog_type {
                eprintln!("click y={y:>2} -> MessageActions for {message_id}");
            }
            app.dialog.pop();
        }
        terminal.draw(|f| app.render(f, 0.016)).unwrap();
    }
}

/// Phase sweep in the REAL delivery order: the strip is visible while the
/// message is queued (Working), then the loop consumes it (deque popped +
/// message mirrored), then deltas keep streaming into the assistant message
/// above. In every phase, sweep a Move over each row and record which
/// message the hover resolves to and which rows the tint paints; flag any
/// tint that lands OUTSIDE the painted box of the message it names
/// (especially above it, on the assistant block).
#[tokio::test]
async fn probe_app_real_order_hover_sweep() {
    use crate::routes::session::queue_choice::QueueTarget;

    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    let id = generate_session_id();
    app.state.add_empty_session(id.clone(), "probe".into(), 0);
    app.state.current_session_id = Some(id.clone());
    app.active_loop_session_id = Some(id.clone());
    {
        let s = app.state.current_session_mut().unwrap();
        s.messages.push(Message {
            id: "msg-0".into(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart { text: "first question".into(), synthetic: false })],
            created_at: 0,
            agent: None,
            model: None,
        });
        s.messages.push(Message {
            id: "msg-1".into(),
            role: MessageRole::Assistant,
            parts: vec![
                Part::Tool(ToolPart {
                    tool: "bash_run".into(),
                    input: serde_json::json!({ "command": "make deploy" }),
                    output: Some((0..8).map(|i| format!("deploy step {i} ok")).collect::<Vec<_>>().join("\n")),
                    status: ToolStatus::Completed,
                    tool_call_id: Some("b1".into()),
                    is_start: true,
                    is_streaming: false,
                    cached_line_count: None,
                    lsp_notes: None,
                }),
                Part::Text(TextPart {
                    text: (0..24).map(|i| format!("streaming line number {i}")).collect::<Vec<_>>().join("\n"),
                    synthetic: false,
                }),
            ],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
    app.state.status = SessionStatus::Working;

    let (w, h) = (100u16, 30u16);
    app.set_test_size(w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();

    let tint = crate::theme::rgba_color(app.theme.background_element);
    let sweep = |app: &mut App, terminal: &mut Terminal<TestBackend>, label: &str| {
        let (w, h) = (terminal.backend().buffer().area.width, terminal.backend().buffer().area.height);
        eprintln!("── sweep {label} ── strip_area={:?} vp={:?}", app.compute_pending_queues_area(), app.session_viewport_area());
        for y in 1..h {
            app.handle_mouse_event(mouse_move(50, y)).unwrap();
            terminal.draw(|f| app.render(f, 0.016)).unwrap();
            let b = terminal.backend().buffer();
            let tint_rows: Vec<u16> = (0..h)
                .filter(|&yy| (0..w).any(|x| b.cell((x, yy)).map(|c| c.bg == tint).unwrap_or(false)))
                .collect();
            if app.session_view.hovered_msg_idx.is_some() {
                eprintln!("hover y={y:>2} -> idx {:?} tint_rows={tint_rows:?}",
                    app.session_view.hovered_msg_idx);
            }
        }
        app.session_view.clear_hover();
    };

    // Phase 1: message queued, strip visible.
    app.enqueue_pending_message(QueueTarget::NextRequest, "queued follow-up".into());
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    sweep(&mut app, &mut terminal, "queued-in-strip");

    // Phase 2: the loop consumes it — pump hands it to the shared queue, the
    // harness acks with UserMessageInjected (deque popped + message mirrored).
    app.pump_queued_messages();
    app.event_tx.send(cosh::harness::HarnessEvent::UserMessageInjected {
        text: "queued follow-up".into(),
    }).ok();
    app.poll_events();
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    sweep(&mut app, &mut terminal, "injected");

    // Phase 3: deltas keep streaming into msg-1 ABOVE the queued message;
    // between two frames a Move arrives (real loop order).
    {
        let s = app.state.current_session_mut().unwrap();
        let m = s.messages.iter_mut().find(|m| m.id == "msg-1").unwrap();
        if let Some(Part::Text(t)) = m.parts.last_mut() {
            t.text.push('\n');
            t.text.push_str(&(24..30).map(|i| format!("streaming line number {i}")).collect::<Vec<_>>().join("\n"));
        }
    }
    // Move BEFORE the next render (stale mapping — the real event order).
    app.handle_mouse_event(mouse_move(50, 20)).unwrap();
    eprintln!("pre-render hover after growth -> {:?}", app.session_view.hovered_msg_idx);
    terminal.draw(|f| app.render(f, 0.016)).unwrap();
    sweep(&mut app, &mut terminal, "post-growth");
}
