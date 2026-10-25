use super::{App, HOME_LOCK, isolate_home};
use crate::app::session_affinity_id;
use crate::session::store::generate_session_id;

/// The affinity id derived from a chat session id must be opaque,
/// deterministic and non-identifying: the same session always produces the
/// same `x-session-id` / `x-session-affinity` value (that's what keeps the
/// session pinned to one cache-warm backend), different sessions hash to
/// different values, and the raw session id never appears in the header.
#[test]
fn session_affinity_id_is_deterministic_opaque_hash() {
    let id = generate_session_id();
    let affinity = session_affinity_id(&id);

    // Deterministic: rebuilding for a fallback / summarization connector
    // must yield the identical routing value.
    assert_eq!(affinity, session_affinity_id(&id));

    // Opaque: the raw session id is never sent on the wire.
    assert!(!affinity.contains(&id));
    assert_ne!(affinity, id);

    // Distinct sessions get distinct routing values (fixed-width hex).
    // `generate_session_id()` is a millisecond timestamp, so brief sleep
    // guarantees the two ids differ (no same-millisecond collision).
    std::thread::sleep(std::time::Duration::from_millis(2));
    let other = session_affinity_id(&generate_session_id());
    assert_ne!(affinity, other);
    assert_eq!(affinity.len(), 16);
    assert!(affinity.chars().all(|c| c.is_ascii_hexdigit()));
}

/// When the agent loop ends (Done/Stopped) before a "next request" message
/// was consumed, it must be promoted to the "next agent loop" queue so it
/// starts a fresh loop instead of being lost. FIFO order is preserved: the
/// promoted message joins the back of the secondary queue.
#[tokio::test]
async fn loop_end_promotes_unconsumed_next_request_to_next_loop() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
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
    // The shared queued-input queue is reset at loop end so nothing can be
    // injected into the finished loop.
    assert!(app.queued_input.is_empty());
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
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
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
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use cosh::harness::HarnessEvent;

    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
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
            call_ref: None,
            tool: "fs_edit".into(),
            input: serde_json::json!({"path": "a.rs"}),
        })
        .ok();
    app.event_tx
        .send(HarnessEvent::ToolResult {
            call_ref: None,
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

/// Resuming a session whose context records are missing must NOT feed any
/// model-facing history: the display transcript is never parsed into the
/// context. The only observable outcome is the "context lost" toast — a
/// session that already shows dialog AND exists on disk must not resume
/// silently as if the model remembered nothing on purpose.
#[tokio::test]
async fn resuming_without_a_ctx_file_warns_and_starts_with_empty_context() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // Isolate the store in a temp dir — persisting tests must never leak
    // session files into the real user data dir.
    let dir = tempfile::tempdir().unwrap();
    app.session_store =
        crate::session::store::SessionStore::with_dir(dir.path().to_path_buf(), "testhash".into());
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
    // Dialog already on screen, but no context was ever persisted for it.
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
    // The session was persisted (display-only save) BEFORE its context went
    // missing — the on-disk JSONL is what distinguishes "lost context" from
    // a brand-new session's first prompt.
    let persisted = app.state.current_session().unwrap().clone();
    app.session_store.save_session(&persisted);

    // The file carries no context records: `load_context` returns None and
    // the loop would start with an empty context.
    assert!(
        app.session_store.load_context(&id).is_none(),
        "no context records on disk — the context source is absent"
    );
    app.warn_if_resuming_without_context(&None);

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
/// context on disk yet (records are only written by Done/Stopped/snapshot
/// saves) — that is the normal path and must never warn. The on-disk JSONL
/// gate (persisted session vs fresh one) is what keeps this quiet.
#[tokio::test]
async fn fresh_session_first_prompt_does_not_warn() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
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
    // ...but NOTHING was persisted yet: no JSONL at all.
    assert!(!app.session_store.has_session(&id));
    app.warn_if_resuming_without_context(&None);
    assert!(
        app.toast_state.current.is_none(),
        "a fresh session must never see the context-lost toast"
    );
}

/// A session WITH persisted context (or a brand-new session with no
/// dialog) must never trigger the context-lost toast: the warning is
/// reserved for sessions that visibly lost context they once had.
#[tokio::test]
async fn resuming_with_a_ctx_file_stays_silent() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // Isolate the store in a temp dir — persisting tests must never leak
    // session files into the real user data dir.
    let dir = tempfile::tempdir().unwrap();
    app.session_store =
        crate::session::store::SessionStore::with_dir(dir.path().to_path_buf(), "testhash".into());
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
    // Persist real context records so the resume path finds context. An
    // empty-but-present snapshot is deliberate: RECORD PRESENCE is the
    // authority — a decodable context means it is whatever the harness
    // last persisted, not a loss.
    let state: cosh::harness::ContextManagerState = Default::default();
    app.session_store
        .save_session_with_context(&app.state.current_session().unwrap().clone(), &state);

    let loaded = app
        .session_store
        .load_context(&id)
        .expect("the context records load");
    app.warn_if_resuming_without_context(&Some(loaded));
    assert!(
        app.toast_state.current.is_none(),
        "no toast when context exists"
    );
}

/// An app wired with one fresh current session, ready to receive harness
/// events through the real intake (`poll_events`).
fn app_with_fresh_session(title: &str) -> App {
    let mut app = App::new("/tmp".to_string());
    let id = generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        title.into(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    app.state.current_session_id = Some(id);
    app
}

/// Assistant text parts currently on screen, in order.
fn assistant_texts(app: &App) -> Vec<String> {
    app.state
        .current_session()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.role == crate::types::MessageRole::Assistant)
        .flat_map(|m| {
            m.parts.iter().filter_map(|p| match p {
                crate::types::Part::Text(t) => Some(t.text.clone()),
                _ => None,
            })
        })
        .collect()
}

/// Providers that emit whitespace-only stream deltas carrying a newline
/// (Gemini's `streamGenerateContent` and several OpenAI-compatible
/// tokenizers) used to lose their paragraph breaks: the token intake
/// dropped EVERY whitespace-only token, so `["Hello", "\n\n", "world"]`
/// glued into "Helloworld" — the "missing line break" that only some
/// providers show. Newline-bearing deltas must survive (the markdown
/// renderer turns `\n\n` into a paragraph break); pure whitespace WITHOUT a
/// newline stays dropped (the old guard's noise filter).
#[tokio::test]
async fn newline_only_stream_deltas_keep_their_line_break() {
    use cosh::harness::HarnessEvent;
    let mut app = app_with_fresh_session("stream intake");
    // Gemini-style standalone break chunks interleaved with word deltas,
    // plus the whitespace-only shapes the old guard must keep dropping.
    for text in ["Hello", "\n\n", "world", "  ", "!", "\t", "\n\n", "Bye"] {
        app.event_tx
            .send(HarnessEvent::Token { text: text.into() })
            .ok();
    }
    app.poll_events();

    // ONE message — the break deltas append, they never split the stream —
    // with every newline intact and the bare-space/tab deltas gone.
    assert_eq!(
        assistant_texts(&app),
        vec!["Hello\n\nworld!\n\nBye"],
        "newline-bearing whitespace deltas are breaks, not noise"
    );
}

/// The two tokenization styles produce the SAME transcript: a provider that
/// streams breaks as standalone chunks (Gemini-like) and one that bundles
/// the newline with the adjacent word (OpenAI-like) must render
/// identically — that is what makes the bug per-provider instead of
/// universal.
#[tokio::test]
async fn standalone_break_chunk_matches_bundled_newline_output() {
    use cosh::harness::HarnessEvent;
    let mut gemini = app_with_fresh_session("standalone breaks");
    for text in ["Hello", "\n\n", "world"] {
        gemini
            .event_tx
            .send(HarnessEvent::Token { text: text.into() })
            .ok();
    }
    gemini.poll_events();

    let mut openai = app_with_fresh_session("bundled breaks");
    for text in ["Hello", "\n\nworld"] {
        openai
            .event_tx
            .send(HarnessEvent::Token { text: text.into() })
            .ok();
    }
    openai.poll_events();

    assert_eq!(
        assistant_texts(&gemini),
        vec!["Hello\n\nworld"],
        "standalone break deltas compose the full text"
    );
    assert_eq!(
        assistant_texts(&gemini),
        assistant_texts(&openai),
        "both tokenization styles render identically"
    );
}

/// A newline-only delta arriving BEFORE any text (stream not open yet)
/// neither opens an assistant bubble nor breaks the next token's intake:
/// the bubble still starts at the first real word, without a leading blank
/// line.
#[tokio::test]
async fn leading_newline_delta_neither_opens_a_bubble_nor_breaks_the_stream() {
    use cosh::harness::HarnessEvent;
    let mut app = app_with_fresh_session("leading break");
    app.event_tx
        .send(HarnessEvent::Token {
            text: "\n\n".into(),
        })
        .ok();
    app.poll_events();
    assert!(
        assistant_texts(&app).is_empty(),
        "whitespace must never open an empty bubble"
    );

    app.event_tx
        .send(HarnessEvent::Token {
            text: "Hello".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(
        assistant_texts(&app),
        vec!["Hello"],
        "the stream continues normally after a dropped leading break"
    );
}

/// The spinner label follows the loop's activity: reasoning narrows it to
/// "pondering", tool calls to "searching"/"recalling", and tool results /
/// streamed tokens restore the default "working". Driven through the real
/// event intake (`poll_events`), like the live loop — no special hooks.
#[tokio::test]
async fn spinner_label_follows_agent_activity() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use cosh::harness::HarnessEvent;

    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(id.clone(), "activity".into(), 0);
    app.state.current_session_id = Some(id);

    // No loop yet: the tracked state defaults to Working, no spinner lives.
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);
    assert!(app.agent_spinner.is_none());

    // Simulate loop start (what `start_agent_loop` does — keep in sync with
    // the spinner init in `agent_loop.rs`, which cannot run in a test since
    // it spawns the real harness thread): fresh spinner with the default
    // label, animation state untouched by later label changes.
    app.agent_activity = crate::types::AgentActivity::Working;
    app.agent_spinner = Some(crate::component::any_spinner::AnySpinner::new(
        crate::types::AgentActivity::Working.label(),
        &app.theme,
        cosh::setup::SpinnerStyle::Bass,
    ));
    for _ in 0..40 {
        app.agent_spinner.as_mut().unwrap().advance();
    }

    // Reasoning streams → pondering.
    app.event_tx
        .send(HarnessEvent::Reasoning {
            text: "thinking it through".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Pondering);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "pondering"
    );

    // A web search starts → searching.
    app.event_tx
        .send(HarnessEvent::ToolCall {
            call_ref: None,
            tool: "web_search".into(),
            input: serde_json::json!({"query": "x"}),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Searching);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "searching"
    );

    // The search finishes; a recall tool starts → recalling.
    app.event_tx
        .send(HarnessEvent::ToolResult {
            call_ref: None,
            output: "hits".into(),
        })
        .ok();
    app.event_tx
        .send(HarnessEvent::ToolCall {
            call_ref: None,
            tool: "recall_search".into(),
            input: serde_json::json!({"query": "y"}),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Recalling);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "recalling"
    );

    // Recall done → back to the default working state.
    app.event_tx
        .send(HarnessEvent::ToolResult {
            call_ref: None,
            output: "found".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "working"
    );

    // Unknown tools keep the default state (no spurious label churn).
    app.event_tx
        .send(HarnessEvent::ToolCall {
            call_ref: None,
            tool: "fs_edit".into(),
            input: serde_json::json!({"path": "a.rs"}),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);

    // A streamed token also restores the default (e.g. after reasoning).
    app.event_tx
        .send(HarnessEvent::Token {
            text: "Answer: ".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);

    // The label churn must never restart the spinner's animation: the birth
    // phase completed before the first event and must still be complete.
    assert!(app.agent_spinner.as_ref().unwrap().is_initialized());
}

/// Noise deltas must not churn the spinner activity: whitespace-only tokens
/// and empty reasoning chunks render nothing, so they keep the current state
/// instead of restarting the status glyph at frame 0 (which froze the
/// pondering animation mid-stream when providers interleave noise deltas).
#[tokio::test]
async fn spinner_activity_ignores_noise_deltas() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use cosh::harness::HarnessEvent;

    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(id.clone(), "noise".into(), 0);
    app.state.current_session_id = Some(id);
    app.agent_activity = crate::types::AgentActivity::Working;
    app.agent_spinner = Some(crate::component::any_spinner::AnySpinner::new(
        crate::types::AgentActivity::Working.label(),
        &app.theme,
        cosh::setup::SpinnerStyle::Bass,
    ));
    for _ in 0..40 {
        app.agent_spinner.as_mut().unwrap().advance();
    }

    // Real reasoning narrows to pondering.
    app.event_tx
        .send(HarnessEvent::Reasoning {
            text: "thinking".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Pondering);

    // A whitespace-only token renders nothing: pondering survives.
    app.event_tx
        .send(HarnessEvent::Token { text: " ".into() })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Pondering);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "pondering"
    );

    // A real token restores the default working state.
    app.event_tx
        .send(HarnessEvent::Token {
            text: "answer".into(),
        })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);

    // An empty reasoning delta renders nothing: working survives.
    app.event_tx
        .send(HarnessEvent::Reasoning { text: "  ".into() })
        .ok();
    app.poll_events();
    assert_eq!(app.agent_activity, crate::types::AgentActivity::Working);
    assert_eq!(
        app.agent_spinner.as_ref().unwrap().label_for_test(),
        "working"
    );
}

/// The `/agents` mode's Right Panel contract, exercised through the real
/// App: the main agent is the panel's SOLE owner — no agent LIST renders
/// even with several pane mirrors alive (the pane count and a stale forced
/// list flag are both overridden while the main mirror exists) — and the
/// mirror opens showing the conversation the user ALREADY had with the main
/// agent, not an empty window.
#[tokio::test]
async fn agents_mode_right_panel_shows_the_main_agent_not_a_list() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = app_with_fresh_session("agents panel");
    // Prior dialog: the user talked to the main agent BEFORE entering the
    // split mode (the exact reported condition for the mirror to already
    // show something).
    app.state
        .current_session_mut()
        .unwrap()
        .messages
        .push(crate::types::Message {
            id: "m1".into(),
            role: crate::types::MessageRole::User,
            parts: vec![crate::types::Part::Text(crate::types::TextPart {
                text: "what is the plan".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    app.state
        .current_session_mut()
        .unwrap()
        .messages
        .push(crate::types::Message {
            id: "m2".into(),
            role: crate::types::MessageRole::Assistant,
            parts: vec![
                crate::types::Part::Reasoning(crate::types::ReasoningPart {
                    text: "thinking about it".into(),
                    collapsed: false,
                }),
                crate::types::Part::Text(crate::types::TextPart {
                    text: "here is the plan".into(),
                    synthetic: false,
                }),
                crate::types::Part::Tool(crate::types::ToolPart {
                    tool: "bash_run".into(),
                    input: serde_json::json!({"command": "ls"}),
                    output: Some("file.txt".into()),
                    status: crate::types::ToolStatus::Completed,
                    tool_call_id: Some("call-1".into()),
                    is_start: false,
                    is_streaming: false,
                    cached_line_count: None,
                    lsp_notes: None,
                }),
            ],
            created_at: 1,
            agent: None,
            model: None,
        });

    // Enter the split mode with TWO panes: their mirrors used to push the
    // section into the automatic list view (>= 2 subagents), which rendered
    // an agent LIST instead of the main agent.
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec![
            crate::types::AgentPaneRef::Name("claude".into()),
            crate::types::AgentPaneRef::Name("kilo".into()),
        ],
    ));
    for (i, pane) in app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let _ = pane;
        app.state.right_panel.open_agents_pane_window(i, "pane");
    }
    // Even a stale FORCED list flag must not survive the sole ownership.
    app.state.right_panel.subagent_list_mode = Some(true);

    // Explicit open (Ctrl+P path): the mirror appears, expanded, with the
    // prior conversation backfilled.
    app.open_main_mirror();
    assert!(app.main_mirror_open());
    assert!(
        !app.state.right_panel.subagent_list_active(),
        "the mirror owns the section as the ONE expanded window — no list"
    );
    assert!(
        app.right_panel_visible(120),
        "the explicit open shows the panel in agent mode"
    );
    let mirror = &app.state.right_panel.pty_sessions
        [app.state.right_panel.main_mirror_index().unwrap()];
    let timeline = &mirror.subagent_activity.timeline;
    assert!(
        timeline.iter().any(|e| matches!(
            e,
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage { text }
                if text == "what is the plan"
        )),
        "the prior USER turn is in the mirror"
    );
    assert!(
        timeline.iter().any(|e| matches!(
            e,
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text }
                if text == "here is the plan"
        )),
        "the prior ASSISTANT answer is in the mirror"
    );
    assert!(
        timeline.iter().any(|e| matches!(
            e,
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Thought { .. }
        )),
        "the prior reasoning is in the mirror"
    );
    assert!(
        timeline.iter().any(|e| matches!(
            e,
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Tool(c)
                if c.tool.as_deref() == Some("bash_run")
                    && c.status == cosh_tools::subagent::events::ToolCallStatus::Completed
        )),
        "the prior tool call is in the mirror, sealed Completed"
    );

    // Closing restores the pre-open panel state verbatim (the list-mode
    // flag the user left is NOT touched by the mirror lifecycle).
    let list_mode_before = app.state.right_panel.subagent_list_mode;
    app.close_main_mirror();
    assert!(!app.main_mirror_open());
    assert_eq!(
        app.state.right_panel.subagent_list_mode,
        list_mode_before,
        "the mirror lifecycle leaves the list-mode state exactly as it was"
    );
    assert!(
        app.state.right_panel.subagent_list_active(),
        "without the mirror the pane mirrors' list returns — normal behavior"
    );
}

/// A mid-stream reset during a Thought (long reasoning phases are where
/// mid-stream retries are most likely) must not make the chat text blink:
/// the failed attempt's partial message STAYS in place and the retried
/// stream's first delta REPLACES its content in the same bubble (same id,
/// same position, no reflow). The old remove-and-recreate behavior made the
/// whole block vanish and reappear on every retry — the "blinking chat"
/// incident. A partial that never gets re-streamed is pruned at turn end,
/// restoring the old discard semantics.
#[tokio::test]
async fn mid_stream_reset_keeps_the_thought_bubble_instead_of_blinking() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use cosh::harness::HarnessEvent;

    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state.add_empty_session(
        id.clone(),
        "reset blink test".into(),
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

    // Attempt 1: the Thought streams.
    app.event_tx.send(HarnessEvent::BeginAssistant).ok();
    for chunk in ["thinking hard", " about the task"] {
        app.event_tx
            .send(HarnessEvent::Reasoning {
                text: chunk.to_string(),
            })
            .ok();
    }
    app.poll_events();
    let staged_id = {
        let session = app.state.current_session().unwrap();
        assert_eq!(
            session.messages.len(),
            2,
            "the Thought bubble is on screen"
        );
        session.messages[1].id.clone()
    };

    // The SDK retries mid-thought: the bubble must NOT vanish.
    app.event_tx.send(HarnessEvent::ClearAssistant).ok();
    app.poll_events();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(
            session.messages.len(),
            2,
            "a mid-stream reset must not remove the in-progress Thought bubble"
        );
        assert_eq!(
            session.messages[1].id, staged_id,
            "the bubble keeps its identity and position across the retry"
        );
    }

    // A leading noise chunk of the retried stream must neither open a
    // bubble nor wipe the staged partial (the replace happens on the first
    // REAL delta only).
    app.event_tx
        .send(HarnessEvent::Token {
            text: " ".into(),
        })
        .ok();
    app.poll_events();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(session.messages.len(), 2, "noise must not open a bubble");
        let crate::types::Part::Reasoning(r) = &session.messages[1].parts[0] else {
            panic!("expected a reasoning part");
        };
        assert!(
            r.text.contains("thinking hard"),
            "a noise chunk must not wipe the staged partial; got {:?}",
            r.text
        );
    }

    // The retry re-streams from zero: its FIRST delta replaces the failed
    // partial in place (no duplicate block, no concatenation).
    app.event_tx.send(HarnessEvent::BeginAssistant).ok();
    app.event_tx
        .send(HarnessEvent::Reasoning {
            text: "retried thought".into(),
        })
        .ok();
    app.poll_events();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(
            session.messages.len(),
            2,
            "the retried attempt re-streams into the SAME bubble"
        );
        assert_eq!(session.messages[1].id, staged_id);
        let crate::types::Part::Reasoning(r) = &session.messages[1].parts[0] else {
            panic!("expected a reasoning part, got {:?}", session.messages[1].parts);
        };
        assert_eq!(
            r.text, "retried thought",
            "the failed partial was replaced, not concatenated"
        );
    }

    // The retried stream continues normally into the same message.
    app.event_tx
        .send(HarnessEvent::Token {
            text: "the answer".into(),
        })
        .ok();
    app.poll_events();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert!(
            session.messages[1]
                .parts
                .iter()
                .any(|p| matches!(p, crate::types::Part::Text(t) if t.text == "the answer")),
            "the retried answer streams into the reused bubble"
        );
    }

    // A partial that never gets re-streamed is pruned at turn end (Error
    // terminal here — the cheapest terminal event to drive). A NEW attempt
    // opens a NEW bubble (BeginAssistant dissociates the old one); its reset
    // stages THAT message for reuse.
    app.event_tx.send(HarnessEvent::BeginAssistant).ok();
    app.event_tx
        .send(HarnessEvent::Token {
            text: "second attempt partial".into(),
        })
        .ok();
    app.poll_events();
    let stale_id = {
        let session = app.state.current_session().unwrap();
        assert_eq!(session.messages.len(), 3, "a new attempt opens a new bubble");
        session.messages[2].id.clone()
    };
    app.event_tx.send(HarnessEvent::ClearAssistant).ok();
    app.poll_events();
    assert_eq!(
        app.state.current_session().unwrap().messages.len(),
        3,
        "the stale partial stays staged (kept, not removed) until the turn ends"
    );
    app.event_tx
        .send(HarnessEvent::Error {
            message: "stream failed for good".into(),
            context: None,
            checkup_verdict: None,
        })
        .ok();
    app.poll_events();
    let session = app.state.current_session().unwrap();
    assert!(
        !session.messages.iter().any(|m| m.id == stale_id),
        "the never-re-streamed partial is pruned at turn end; messages={:?}",
        session.messages.iter().map(|m| (m.id.clone(), m.parts.len())).collect::<Vec<_>>()
    );
    assert!(
        session.messages.iter().any(|m| m.id == staged_id),
        "the earlier reused bubble (with the retried content) survives"
    );
}
