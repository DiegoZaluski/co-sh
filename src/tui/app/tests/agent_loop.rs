use super::App;
use crate::session_store::generate_session_id;

/// When the agent loop ends (Done/Stopped) before a "next request" message
/// was consumed, it must be promoted to the "next agent loop" queue so it
/// starts a fresh loop instead of being lost. FIFO order is preserved: the
/// promoted message joins the back of the secondary queue.
#[tokio::test]
async fn loop_end_promotes_unconsumed_next_request_to_next_loop() {
    let mut app = App::new("/tmp".to_string());
    let id = crate::session_store::generate_session_id();
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
    let id = crate::session_store::generate_session_id();
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

/// A mid-stream reset (the SDK retrying a failed attempt) must discard ONLY
/// the partial message the failed attempt rendered — never the previous
/// iteration's transcript. An attempt that reset BEFORE its first token has
/// nothing to discard: the tool messages of the previous iteration (the
/// diffs/edits the user sees) must survive. Repeated resets on a flaky
/// provider used to pop one transcript message per reset until only the user
/// prompt remained on screen.
#[tokio::test]
async fn mid_stream_reset_never_eats_the_previous_iteration_transcript() {
    use cosh::harness::HarnessEvent;

    let mut app = App::new("/tmp".to_string());
    let id = crate::session_store::generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        "reset test".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id);
    // A user prompt, like a real session.
    {
        let session = app.state.current_session_mut().unwrap();
        session.messages.push(crate::types::Message {
            id: "msg-0".into(),
            role: crate::types::MessageRole::User,
            parts: vec![crate::types::Part::Text(crate::types::TextPart {
                text: "do the task".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    }

    // Iteration 1 completes with a tool call + result.
    app.event_tx
        .send(HarnessEvent::ToolCall {
            tool: "fs_edit".into(),
            input: serde_json::json!({"path": "a.rs"}),
        })
        .ok();
    app.event_tx
        .send(HarnessEvent::ToolResult {
            output: "--- a.rs\n+++ a.rs".into(),
        })
        .ok();
    app.poll_events();
    let tool_msgs = app
        .state
        .current_session()
        .unwrap()
        .messages
        .iter()
        .filter(|m| {
            m.parts
                .iter()
                .any(|p| matches!(p, crate::types::Part::Tool(_)))
        })
        .count();
    assert_eq!(tool_msgs, 1, "the tool chain is on screen");

    // Iteration 2: the attempt resets BEFORE rendering anything (flaky
    // provider retried the request). The old transcript must survive.
    app.event_tx.send(HarnessEvent::BeginAssistant).ok();
    app.event_tx.send(HarnessEvent::ClearAssistant).ok();
    app.poll_events();
    let after = app.state.current_session().unwrap();
    assert_eq!(
        after
            .messages
            .iter()
            .filter(|m| m
                .parts
                .iter()
                .any(|p| matches!(p, crate::types::Part::Tool(_))))
            .count(),
        1,
        "a pre-token reset must not eat the previous iteration's tool message"
    );

    // A reset AFTER the attempt rendered a partial message discards only
    // that partial — the retried attempt then re-renders cleanly.
    app.event_tx.send(HarnessEvent::BeginAssistant).ok();
    app.event_tx
        .send(HarnessEvent::Token {
            text: "partial an".into(),
        })
        .ok();
    app.poll_events();
    app.event_tx.send(HarnessEvent::ClearAssistant).ok();
    app.event_tx
        .send(HarnessEvent::Token {
            text: "the real answer".into(),
        })
        .ok();
    app.poll_events();
    let session = app.state.current_session().unwrap();
    assert!(
        !session.messages.iter().any(|m| m
            .parts
            .iter()
            .any(|p| matches!(p, crate::types::Part::Text(t) if t.text == "partial an"))),
        "the partial attempt text is discarded"
    );
    assert!(
        session.messages.iter().any(|m| m
            .parts
            .iter()
            .any(|p| matches!(p, crate::types::Part::Text(t) if t.text == "the real answer"))),
        "the retried answer renders"
    );
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|m| m
                .parts
                .iter()
                .any(|p| matches!(p, crate::types::Part::Tool(_))))
            .count(),
        1,
        "the tool message is still intact after the mid-attempt reset"
    );
}

/// Resuming a session whose `.ctx` companion is missing must NOT feed any
/// model-facing history: the JSONL transcript is display-only and is never
/// parsed into the context. The only observable outcome is the "context
/// lost" toast — a session that already shows dialog AND exists on disk
/// must not resume silently as if the model remembered nothing on purpose.
#[tokio::test]
async fn resuming_without_a_ctx_file_warns_and_starts_with_empty_context() {
    let mut app = App::new("/tmp".to_string());
    // Isolate the store in a temp dir — persisting tests must never leak
    // session files into the real user data dir.
    let dir = tempfile::tempdir().unwrap();
    app.session_store =
        crate::session_store::SessionStore::with_dir(dir.path().to_path_buf(), "testhash".into());
    let id = generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        "ctx-less resume".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id.clone());
    // Dialog already on screen, but no `.ctx` was ever written for it.
    app.state.current_session_mut().expect("session").messages = vec![crate::types::Message {
        id: "msg-0".into(),
        role: crate::types::MessageRole::User,
        parts: vec![crate::types::Part::Text(crate::types::TextPart {
            text: "earlier prompt".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }];
    // The session was persisted (display-only save) BEFORE its `.ctx` went
    // missing — the on-disk JSONL is what distinguishes "lost context" from
    // a brand-new session's first prompt.
    let persisted = app.state.current_session().unwrap().clone();
    app.session_store.save_session(&persisted);

    // The store has no companion file for this session: `load_context`
    // returns None and the loop would start with an empty context.
    assert!(
        app.session_store.load_context(&id).is_none(),
        "no .ctx on disk — the context source is absent"
    );
    app.warn_if_resuming_without_ctx(&None);

    let toast = app
        .toast_state
        .current
        .expect("the context-lost toast shows");
    assert_eq!(toast.title.as_deref(), Some("Context lost"));
    assert!(matches!(
        toast.variant,
        crate::ui::toast::ToastVariant::Warning
    ));
    assert!(toast.message.contains("without history"));
}

/// A brand-new session's FIRST prompt also puts a message on screen with no
/// `.ctx` on disk yet (the companion is only written by Done/Stopped/snapshot
/// saves) — that is the normal path and must never warn. The on-disk JSONL
/// gate (persisted session vs fresh one) is what keeps this quiet.
#[tokio::test]
async fn fresh_session_first_prompt_does_not_warn() {
    let mut app = App::new("/tmp".to_string());
    let id = format!("{}-fresh", generate_session_id());
    app.state.add_empty_session(
        id.clone(),
        "fresh session".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id.clone());
    // The user message is already in the display when the loop starts...
    app.state.current_session_mut().expect("session").messages = vec![crate::types::Message {
        id: "msg-0".into(),
        role: crate::types::MessageRole::User,
        parts: vec![crate::types::Part::Text(crate::types::TextPart {
            text: "first prompt".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }];
    // ...but NOTHING was persisted yet: no JSONL, no `.ctx`.
    assert!(!app.session_store.has_session(&id));
    app.warn_if_resuming_without_ctx(&None);
    assert!(
        app.toast_state.current.is_none(),
        "a fresh session must never see the context-lost toast"
    );
}

/// A session WITH its `.ctx` companion (or a brand-new session with no
/// dialog) must never trigger the context-lost toast: the warning is
/// reserved for sessions that visibly lost context they once had.
#[tokio::test]
async fn resuming_with_a_ctx_file_stays_silent() {
    let mut app = App::new("/tmp".to_string());
    // Isolate the store in a temp dir — persisting tests must never leak
    // session files into the real user data dir.
    let dir = tempfile::tempdir().unwrap();
    app.session_store =
        crate::session_store::SessionStore::with_dir(dir.path().to_path_buf(), "testhash".into());
    let id = format!("{}-ctx", generate_session_id());
    app.state.add_empty_session(
        id.clone(),
        "ctx present".into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id.clone());
    app.state.current_session_mut().expect("session").messages = vec![crate::types::Message {
        id: "msg-0".into(),
        role: crate::types::MessageRole::User,
        parts: vec![crate::types::Part::Text(crate::types::TextPart {
            text: "earlier prompt".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }];
    // Persist a real companion file so the resume path finds context. An
    // empty-but-present snapshot is deliberate: FILE PRESENCE is the
    // authority — a `.ctx` that decodes means the context is whatever the
    // harness last persisted, not a loss.
    let state: cosh::harness::ContextManagerState = Default::default();
    app.session_store
        .save_session_with_context(&app.state.current_session().unwrap().clone(), &state);

    let loaded = app
        .session_store
        .load_context(&id)
        .expect("the .ctx decodes");
    app.warn_if_resuming_without_ctx(&Some(loaded));
    assert!(
        app.toast_state.current.is_none(),
        "no toast when context exists"
    );
}
