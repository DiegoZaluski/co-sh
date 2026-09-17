use super::{App, format_tokens};

/// Long queued messages must word-wrap across several visual rows instead of
/// being truncated: `pending_queue_rows` expands every queued message into
/// one or more `(queue_index, message_index, line)` entries using the same
/// grapheme-aware `word_wrap` primitive as the chat transcript.
#[test]
fn pending_queue_rows_wrap_long_messages() {
    use crate::state::PendingQueues;
    use std::collections::VecDeque;

    let queues = PendingQueues {
        next_loop: VecDeque::new(),
        next_request: VecDeque::from([
            "short".to_string(),
            "uma mensagem bem longa que com certeza nao cabe em uma linha so".to_string(),
        ]),
    };

    // Width 30 → text width 24 (┃ + 2 pad left, 2 pad right + ┃), symmetric
    // like the prompt box's chrome.
    let rows = App::pending_queue_rows(&queues, 30);

    // Every wrapped line fits within the text width.
    for (_, _, line) in &rows {
        assert!(
            line.chars().count() <= 24,
            "wrapped line exceeds width: {line:?}"
        );
    }
    // No characters are lost: rejoining the wrapped lines recovers the
    // message (modulo the injected line breaks).
    let joined: String = rows
        .iter()
        .filter(|(qi, mi, _)| *qi == 1 && *mi == 1)
        .map(|(_, _, l)| l.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(
        joined.replace(' ', ""),
        "umamensagembemlongaquecomcertezanaocabeemumalinhaso"
    );
    // The short message is a single row; the long one wraps into several.
    assert_eq!(
        rows.iter()
            .filter(|(qi, mi, _)| *qi == 1 && *mi == 0)
            .count(),
        1
    );
    assert!(
        rows.iter()
            .filter(|(qi, mi, _)| *qi == 1 && *mi == 1)
            .count()
            > 1
    );
}

/// An empty queued message still renders one (blank) row instead of
/// disappearing from the strip.
#[test]
fn pending_queue_rows_keep_empty_messages_visible() {
    use crate::state::PendingQueues;
    use std::collections::VecDeque;

    let queues = PendingQueues {
        next_loop: VecDeque::from([String::new()]),
        next_request: VecDeque::new(),
    };
    let rows = App::pending_queue_rows(&queues, 40);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], (0, 0, String::new()));
}

#[test]
fn format_tokens_small_values_have_no_separator() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(9), "9");
    assert_eq!(format_tokens(999), "999");
}

#[test]
fn format_tokens_groups_thousands() {
    assert_eq!(format_tokens(1_000), "1,000");
    assert_eq!(format_tokens(9_612), "9,612");
    assert_eq!(format_tokens(100_000), "100,000");
    assert_eq!(format_tokens(1_000_000), "1,000,000");
    assert_eq!(format_tokens(1_234_567), "1,234,567");
    assert_eq!(format_tokens(12_345_678), "12,345,678");
}

#[test]
fn format_tokens_handles_large_values() {
    assert_eq!(format_tokens(123_456), "123,456");
    assert_eq!(format_tokens(9_876_543_210), "9,876,543,210");
    assert_eq!(format_tokens(usize::MAX), "18,446,744,073,709,551,615");
}

/// The session chat area must shrink by the right-panel width whenever the
/// panel is visible — and the mouse dispatch shares this exact helper with
/// `render`. A wider mouse area re-wraps every message and shifts `prefix_y`,
/// making tool-box expand/collapse clicks land on the wrong row (the bug was
/// "boxes can't be expanded while the agent loop is active").
#[tokio::test]
async fn session_main_area_matches_render_width_with_right_panel() {
    use ratatui::layout::Rect;

    use super::RIGHT_PANEL_WIDTH;
    use crate::routes::session::right_panel::types::RightPanelState;

    let mut app = App::new("/tmp".to_string());
    let id = super::generate_session_id();
    app.state.add_empty_session(id.clone(), "t".into(), 0);
    app.state.current_session_id = Some(id);
    app.state.right_panel = RightPanelState::new();
    app.state.right_panel.start_pty("echo hi".into(), None);

    let area = Rect::new(0, 0, 140, 30);
    let sa = app.session_main_area(area);
    assert_eq!(sa.main.width, 140 - RIGHT_PANEL_WIDTH);
    assert_eq!(sa.right_panel_w, RIGHT_PANEL_WIDTH);

    app.sidebar.open = true;
    let sa = app.session_main_area(area);
    assert_eq!(
        sa.main.width,
        140 - super::SIDEBAR_WIDTH - RIGHT_PANEL_WIDTH,
        "open sidebar + right panel must both be subtracted"
    );
}

/// Hidden panel (narrow terminal or no content) must not shrink the area.
#[tokio::test]
async fn session_main_area_ignores_hidden_right_panel() {
    use ratatui::layout::Rect;

    use crate::routes::session::right_panel::types::RightPanelState;

    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    app.state.right_panel = RightPanelState::new();

    // Narrow terminal → panel hidden even if there were content.
    app.state.right_panel.start_pty("echo hi".into(), None);
    let sa = app.session_main_area(Rect::new(0, 0, 90, 30));
    assert_eq!(sa.right_panel_w, 0);
    assert_eq!(sa.main.width, 90);

    // Wide terminal but no todos/pty content → panel hidden.
    let mut app2 = App::new("/tmp".to_string());
    app2.state.add_empty_session("t".into(), "t".into(), 0);
    app2.state.current_session_id = Some("t".into());
    let sa2 = app2.session_main_area(Rect::new(0, 0, 140, 30));
    assert_eq!(sa2.right_panel_w, 0);
    assert_eq!(sa2.main.width, 140);
}

/// The DEFINITIVE check for the Charm header bug: a real HarnessEvent::Usage
/// carrying `remaining_credits` must (a) land in `hypercredit_balance` after
/// `poll_events`, and (b) make the rendered header show the ◆ BALANCE — not
/// the dollar total. Regression for "header stays in dollars while using
/// Charm".
#[tokio::test]
async fn charm_usage_event_switches_header_to_credits_balance() {
    use cosh_sdk::connector::TokenUsage;

    let mut app = App::new("/tmp".to_string());
    app.state.add_empty_session("t".into(), "t".into(), 0);
    app.state.current_session_id = Some("t".into());
    // The header cost widget only renders once the session has content
    // (same precondition a real Charm session has after the first exchange).
    app.state
        .current_session_mut()
        .unwrap()
        .messages
        .push(crate::types::Message {
            id: "m1".into(),
            role: crate::types::MessageRole::User,
            parts: vec![crate::types::Part::Text(crate::types::TextPart {
                text: "hi".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });

    let usage = cosh::harness::HarnessEvent::Usage {
        usage: TokenUsage::default(),
        provider: "charm".into(),
        model: "glm-5.3-flash".into(),
        reported_cost: Some(0.000012),
        reported_cost_credits: Some(12.0),
        remaining_credits: Some(88.0),
    };
    app.event_tx.send(usage).unwrap();
    app.poll_events();

    // (a) The balance landed.
    assert_eq!(app.hypercredit_balance, Some(88.0));

    // (b) The header renders the BALANCE in credits mode — scan the top row
    // of a real backend render for what was actually drawn.
    use ratatui::{Terminal, backend::TestBackend};
    let backend = TestBackend::new(140, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    let area = terminal.backend().buffer().area;
    let row: String = (area.x..area.right())
        .filter_map(|x| {
            terminal
                .backend()
                .buffer()
                .cell((x, area.y))
                .map(|c| c.symbol().to_string())
        })
        .collect();
    assert!(
        row.contains('\u{25c6}'),
        "header must show the ◆ credit glyph, got: {row:?}"
    );
    assert!(
        !row.contains('$'),
        "header must NOT show dollars in credits mode, got: {row:?}"
    );
    assert!(
        row.contains(" 88"),
        "header must show the remaining BALANCE 88, got: {row:?}"
    );
}
