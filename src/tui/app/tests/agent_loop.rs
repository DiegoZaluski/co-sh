use super::App;

/// When the agent loop ends (Done/Stopped) before a "next request" message
/// was consumed, it must be promoted to the "next agent loop" queue so it
/// starts a fresh loop instead of being lost. FIFO order is preserved: the
/// promoted message joins the back of the secondary queue.
#[tokio::test]
async fn loop_end_promotes_unconsumed_next_request_to_next_loop() {
    let mut app = App::new("/tmp".to_string());
    let id = super::generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        "promotion test".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id.clone());
    app.active_loop_session_id = Some(id.clone());
    {
        let queues = app.state.pending_queues.entry(id.clone()).or_default();
        queues.next_request.push_back("leftover request msg".into());
        queues.next_loop.push_back("secondary msg".into());
    }

    // Simulate the loop ending WITHOUT consuming the next-request message
    // (e.g. user stopped it before the harness drained the channel). The
    // `start_next` flag mirrors Done/Stopped; Error passes false.
    app.handle_loop_end(false);

    let queues = app.state.pending_queues.get(&id).expect("queues exist");
    assert!(
        queues.next_request.is_empty(),
        "unconsumed next-request message must be promoted"
    );
    assert_eq!(
        queues.next_loop,
        vec![
            "secondary msg".to_string(),
            "leftover request msg".to_string()
        ],
        "promoted message joins the back of the next-loop queue (FIFO)"
    );
    // The sender channel is dropped at loop end so nothing can be injected
    // into the finished loop.
    assert!(app.queued_input_tx.is_none());
}

/// The user's exact scenario: a "next request" message that the loop
/// stopped before consuming must become the FIRST "next agent loop"
/// candidate when no secondary queue message exists. This verifies the
/// promotion state that precedes auto-start: with `start_next = true`
/// (Done/Stopped) `handle_loop_end` pops this front message and starts a
/// fresh loop with it (FIFO). We use `false` here because `true` would
/// spawn a real agent loop.
#[tokio::test]
async fn loop_end_orphaned_next_request_becomes_first_next_loop_candidate() {
    let mut app = App::new("/tmp".to_string());
    let id = super::generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        "orphan test".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id.clone());
    app.active_loop_session_id = Some(id.clone());
    {
        let queues = app.state.pending_queues.entry(id.clone()).or_default();
        queues.next_request.push_back("orphaned request msg".into());
    }

    // Done/Stopped path (start_next = true): promotion happens first, so
    // the orphaned message lands at the FRONT of the next-loop queue and
    // would be the message that starts the fresh loop.
    app.handle_loop_end(false);
    let queues = app.state.pending_queues.get(&id).expect("queues exist");
    assert_eq!(
        queues.next_loop,
        vec!["orphaned request msg".to_string()],
        "orphaned message is the first next-loop candidate"
    );
    assert!(queues.next_request.is_empty());
}
