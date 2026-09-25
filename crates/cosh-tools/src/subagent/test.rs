//! Tests for the `subagent` module: the [`SubAgent`] wrapper (schema,
//! input reuse, note interpolation) and the ACP engine (agent registry,
//! name validation, launcher building, and the full
//! in-memory ACP client turn driven through `Channel::duplex()`).

// Items from the sibling engine module: some are `pub(crate)` precisely
// because they are exercised here.
use super::SubAgent;
use super::acp::{
    ACP_AGENTS, Agent, acp_error, agent_launcher, ensure_path_within, install_hint, run_session,
    stop_reason_str, validate_agent,
};
use super::events::SubagentEvent;

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, ReadTextFileRequest,
    SessionNotification, SessionUpdate, TextContent, WriteTextFileRequest,
};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[test]
fn fresh_instance_without_input_errors() {
    let sub = SubAgent::new();
    let err = sub.resolve_input(None).unwrap_err();
    assert!(err.contains("no stored sub-agent message"), "{err}");
}

#[test]
fn fresh_instance_with_empty_input_errors() {
    let sub = SubAgent::new();
    let err = sub.resolve_input(Some(String::new())).unwrap_err();
    assert!(err.contains("no stored sub-agent message"), "{err}");
}

#[test]
fn first_call_stores_input_and_returns_it() {
    let sub = SubAgent::new();
    let resolved = sub
        .resolve_input(Some("review this PR".to_string()))
        .unwrap();
    assert_eq!(resolved, "review this PR");
}

#[test]
fn omitted_input_reuses_last_message() {
    let sub = SubAgent::new();
    let first = sub
        .resolve_input(Some("review this PR".to_string()))
        .unwrap();
    assert_eq!(first, "review this PR");

    let reused = sub.resolve_input(None).unwrap();
    assert_eq!(reused, "review this PR");
}

#[test]
fn empty_input_after_a_call_reuses_last_message() {
    let sub = SubAgent::new();
    sub.resolve_input(Some("review this PR".to_string()))
        .unwrap();

    let reused = sub.resolve_input(Some(String::new())).unwrap();
    assert_eq!(reused, "review this PR");
}

#[test]
fn new_input_overwrites_stored_message() {
    let sub = SubAgent::new();
    sub.resolve_input(Some("first message".to_string()))
        .unwrap();
    sub.resolve_input(Some("second message".to_string()))
        .unwrap();

    let reused = sub.resolve_input(None).unwrap();
    assert_eq!(reused, "second message");
}

#[test]
fn instances_do_not_share_stored_input() {
    // Each session owns its SubAgent, so stored input never leaks
    // across instances (no .clean() needed).
    let first = SubAgent::new();
    let second = SubAgent::new();
    first
        .resolve_input(Some("first session".to_string()))
        .unwrap();

    assert!(second.resolve_input(None).is_err());
}

#[test]
fn empty_note_keeps_original_description() {
    let fresh = SubAgent::new();
    let mut with_note = SubAgent::new();
    with_note.set_note("");
    assert_eq!(with_note.description_call, fresh.description_call);
}

#[test]
fn note_is_interpolated_inside_the_description_flow() {
    let mut sub = SubAgent::new();
    let fresh = sub.description_call.clone();
    let before = fresh["description"].as_str().unwrap();
    assert!(
        !before.contains("internal agent runs the task instead"),
        "the note must be absent by default"
    );

    sub.set_note(
        "When the `agent` argument is omitted or empty, an internal \
         agent runs the task instead.",
    );
    let after = sub.description_call["description"].as_str().unwrap();

    // Interpolated as the tail of the FIRST paragraph: right after the
    // opening sentence and BEFORE the `input` paragraph — not prepended
    // at the top.
    assert!(after.contains(
        "return its output. When the `agent` argument is omitted or \
         empty, an internal agent runs the task instead.\n`input` is \
         optional"
    ));
    assert!(
        after.starts_with("Call a supported agent ACP harness"),
        "the description must still start with the original prose"
    );
}

#[test]
fn schema_agent_is_optional_and_required_is_empty() {
    let sub = SubAgent::new();
    let schema = &sub.description_call["inputSchema"];
    let required = schema["required"].as_array().unwrap();
    assert!(
        required.is_empty(),
        "neither `agent` nor `input` is required (empty required array)"
    );
    assert!(
        schema["properties"]["agent"]["enum"].is_array(),
        "the agent enum (installed ACP harnesses) must still be present"
    );
}

#[test]
fn input_parses_without_agent_as_none() {
    let input: super::SubAgentCallInput =
        serde_json::from_value(serde_json::json!({ "input": "hi" })).unwrap();
    assert!(input.agent.is_none());
    assert_eq!(input.input.as_deref(), Some("hi"));
}

#[test]
fn input_parses_empty_agent_as_some_empty() {
    let input: super::SubAgentCallInput =
        serde_json::from_value(serde_json::json!({ "agent": "" })).unwrap();
    assert_eq!(input.agent.as_deref(), Some(""));
}

#[test]
fn input_parses_with_agent() {
    let input: super::SubAgentCallInput = serde_json::from_value(serde_json::json!({
        "agent": "gemini",
        "input": "review this"
    }))
    .unwrap();
    assert_eq!(input.agent.as_deref(), Some("gemini"));
    assert_eq!(input.input.as_deref(), Some("review this"));
}

// -------------------------------------------------------------------------
// ACP engine (`acp.rs`)
// -------------------------------------------------------------------------

fn agent(name: &str) -> &'static Agent {
    ACP_AGENTS
        .iter()
        .find(|a| a.name == name)
        .expect("known agent")
}

/// A minimal in-memory ACP agent fixture: answers `initialize`, echoes
/// the prompt back as one `AgentMessageChunk`, and ends the turn. Driven
/// through `Channel::duplex()`, it exercises `run_session` end-to-end
/// (handshake → session → prompt → streaming → stop) with no subprocess,
/// no network and no authentication.
#[tokio::test(flavor = "current_thread")]
async fn run_session_drives_a_full_acp_turn_end_to_end() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, PromptResponse,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};

    let (client_side, agent_side) = Channel::duplex();

    // The fixture agent side: type-driven dispatch over the channel pair.
    // `connect_to` drives the connection loop until the transport closes,
    // so it runs on its own task alongside the client turn.
    let agent_task = tokio::spawn(async move {
        AgentRole
            .builder()
            .name("echo-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<
                    agent_client_protocol::schema::v1::NewSessionResponse,
                >,
                            _cx| {
                    responder.respond(agent_client_protocol::schema::v1::NewSessionResponse::new(
                        agent_client_protocol::schema::v1::SessionId::new("fixture-session"),
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    // Echo the prompt back as an agent message chunk, then
                    // end the turn — mirroring what a real harness streams.
                    let echoed = request
                        .prompt
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    let _ = _cx.send_notification(SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentMessageChunk(
                            agent_client_protocol::schema::v1::ContentChunk::new(
                                ContentBlock::Text(TextContent::new(echoed)),
                            ),
                        ),
                    ));
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));

    let (turn, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "hello from acp".to_string(),
            None,
            std::env::temp_dir(),
            "",
            accumulated.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    let (stop_reason, _session_id) = turn.unwrap();
    assert_eq!(stop_reason, "end_turn");
    assert_eq!(accumulated.lock().unwrap().output(), "hello from acp");
    assert!(matches!(
        streamed.unwrap(),
        SubagentEvent::Message { text } if text == "hello from acp"
    ));
}

/// The fixture agent drives the client's inbound-request handlers over
/// the same channel pair: it asks for `fs/read_text_file` (1-based
/// line/limit slicing), `fs/write_text_file`, and two permission
/// decisions (with and without options). The report it streams back is
/// asserted against what the client actually served.
#[tokio::test(flavor = "current_thread")]
async fn run_session_serves_fs_requests_and_auto_approves_permissions() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, NewSessionResponse, PermissionOption,
        PermissionOptionId, PermissionOptionKind, PromptResponse, RequestPermissionOutcome,
        RequestPermissionRequest, SessionId, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};

    let scratch = tempfile::tempdir().unwrap();
    let read_path = scratch.path().join("read.txt");
    std::fs::write(&read_path, "l1\nl2\nl3\n").unwrap();
    let write_path = scratch.path().join("write.txt");

    let (client_side, agent_side) = Channel::duplex();
    let agent_task = tokio::spawn(async move {
        AgentRole
            .builder()
            .name("fs-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _cx| {
                    responder.respond(NewSessionResponse::new(SessionId::new("fs-fixture")))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, cx| {
                    // The dispatch loop cannot process new messages while
                    // this handler runs, so the round trips below (whose
                    // responses arrive on that very loop) must escape it
                    // via `spawn`. The prompt is answered from the spawned
                    // task after the report chunk has been streamed, so
                    // the client observes the chunk before `EndTurn`.
                    cx.spawn({
                        let connection = cx.clone();
                        let read_path = read_path.clone();
                        let write_path = write_path.clone();
                        async move {
                            // 1. fs/read_text_file: line 2 (1-based), limit 1 → "l2".
                            let read = connection
                                .send_request(
                                    ReadTextFileRequest::new(
                                        request.session_id.clone(),
                                        read_path.clone(),
                                    )
                                    .line(2u32)
                                    .limit(1u32),
                                )
                                .block_task()
                                .await?
                                .content;

                            // 2. Permission with one option → first option selected.
                            let permission = connection
                                .send_request(RequestPermissionRequest::new(
                                    request.session_id.clone(),
                                    ToolCallUpdate::new(
                                        ToolCallId::new("tc-1"),
                                        ToolCallUpdateFields::new(),
                                    ),
                                    vec![PermissionOption::new(
                                        PermissionOptionId::new("allow-yes"),
                                        "Allow",
                                        PermissionOptionKind::AllowOnce,
                                    )],
                                ))
                                .block_task()
                                .await?;
                            let perm_report = match permission.outcome {
                                RequestPermissionOutcome::Selected(selected) => {
                                    format!("perm: {}", selected.option_id.0)
                                }
                                RequestPermissionOutcome::Cancelled => {
                                    "perm: Cancelled".to_string()
                                }
                                other => format!("perm: other({other:?})"),
                            };

                            // 3. Permission with NO options → Cancelled per spec.
                            let empty = connection
                                .send_request(RequestPermissionRequest::new(
                                    request.session_id.clone(),
                                    ToolCallUpdate::new(
                                        ToolCallId::new("tc-2"),
                                        ToolCallUpdateFields::new(),
                                    ),
                                    vec![],
                                ))
                                .block_task()
                                .await?;
                            let empty_report = match empty.outcome {
                                RequestPermissionOutcome::Selected(_) => "perm-empty: selected",
                                RequestPermissionOutcome::Cancelled => "perm-empty: Cancelled",
                                _ => "perm-empty: other",
                            };

                            // 4. fs/write_text_file: served against the real file.
                            connection
                                .send_request(WriteTextFileRequest::new(
                                    request.session_id.clone(),
                                    write_path.clone(),
                                    "written-by-client".to_string(),
                                ))
                                .block_task()
                                .await?;
                            let written = std::fs::read_to_string(&write_path)
                                .unwrap_or_else(|e| format!("read-back failed: {e}"));

                            // Stream the whole report back as one agent chunk.
                            let report = format!(
                                "read: {read}\n{perm_report}\n{empty_report}\nwrite: {written}"
                            );
                            let _ = connection.send_notification(SessionNotification::new(
                                request.session_id.clone(),
                                SessionUpdate::AgentMessageChunk(
                                    agent_client_protocol::schema::v1::ContentChunk::new(
                                        ContentBlock::Text(TextContent::new(report)),
                                    ),
                                ),
                            ));
                            let _ = responder.respond(PromptResponse::new(
                                agent_client_protocol::schema::v1::StopReason::EndTurn,
                            ));
                            Ok(())
                        }
                    })
                    .map_err(|e| acp_error(format!("fs-fixture prompt failed: {e}")))
                    .ok();
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));

    let (turn, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "exercise handlers".to_string(),
            None,
            scratch.path().to_path_buf(),
            "",
            accumulated.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    let (stop_reason, _session_id) = turn.unwrap();
    assert_eq!(stop_reason, "end_turn");
    assert_eq!(
        accumulated.lock().unwrap().output(),
        "read: l2\nperm: allow-yes\nperm-empty: Cancelled\nwrite: written-by-client"
    );
    assert!(matches!(
        streamed.unwrap(),
        SubagentEvent::Message { text } if text == accumulated.lock().unwrap().output()
    ));
}

/// The fixture harness advertises a `model` config option whose default
/// differs from the preferred model. `run_session` must issue
/// `session/set_config_option` before the prompt — the kilo contract, where
/// the ACP default is a paid-tier model that fails the prompt with "You
/// need to sign in".
#[tokio::test(flavor = "current_thread")]
async fn run_session_selects_the_preferred_session_model() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, NewSessionResponse, PromptResponse, SessionConfigId,
        SessionConfigOption, SessionConfigOptionValue, SessionConfigSelectOption,
        SessionConfigValueId, SessionId, SetSessionConfigOptionRequest,
        SetSessionConfigOptionResponse,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};

    const PREFERRED: &str = "kilo/nvidia/nemotron-3-ultra-550b-a55b:free";

    let (client_side, agent_side) = Channel::duplex();
    let agent_task = tokio::spawn(async move {
        AgentRole
            .builder()
            .name("model-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _cx| {
                    responder.respond(
                        NewSessionResponse::new(SessionId::new("model-fixture")).config_options(
                            vec![SessionConfigOption::select(
                                SessionConfigId::new("model"),
                                "Model",
                                // Default differs from PREFERRED: must be switched.
                                SessionConfigValueId::new("kilo/google/gemini-3-pro-image"),
                                vec![SessionConfigSelectOption::new(
                                    SessionConfigValueId::new(PREFERRED),
                                    "Nemotron free",
                                )],
                            )],
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: SetSessionConfigOptionRequest,
                            responder: Responder<SetSessionConfigOptionResponse>,
                            _cx| {
                    // The switch must carry the preferred model value.
                    assert_eq!(request.config_id.0.as_ref(), "model");
                    match &request.value {
                        SessionConfigOptionValue::ValueId { value } => {
                            assert_eq!(value.0.as_ref(), PREFERRED);
                        }
                        other => panic!("unexpected config value: {other:?}"),
                    }
                    responder.respond(SetSessionConfigOptionResponse::new(vec![
                        SessionConfigOption::select(
                            SessionConfigId::new("model"),
                            "Model",
                            SessionConfigValueId::new(PREFERRED),
                            vec![SessionConfigSelectOption::new(
                                SessionConfigValueId::new(PREFERRED),
                                "Nemotron free",
                            )],
                        ),
                    ]))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    let _ = _cx.send_notification(SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentMessageChunk(
                            agent_client_protocol::schema::v1::ContentChunk::new(
                                ContentBlock::Text(TextContent::new("model-ok")),
                            ),
                        ),
                    ));
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));

    let (turn, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "check model".to_string(),
            None,
            std::env::temp_dir(),
            PREFERRED,
            accumulated.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    let (stop_reason, _session_id) = turn.unwrap();
    assert_eq!(stop_reason, "end_turn");
    assert_eq!(accumulated.lock().unwrap().output(), "model-ok");
    assert!(matches!(
        streamed.unwrap(),
        SubagentEvent::Message { text } if text == "model-ok"
    ));
}

/// The typed event stream (Phase 2): the fixture harness streams a thought,
/// a tool call, its update, a plan, a usage snapshot, and a mode change —
/// plus one update with no display mapping (`UserMessageChunk`). All mapped
/// variants must arrive as typed events, only message text feeds the turn's
/// closure, and the unmapped update must be ignored (logged, not emitted).
#[tokio::test(flavor = "current_thread")]
async fn run_session_maps_session_updates_into_typed_events() {
    use agent_client_protocol::schema::v1 as acp1;
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, PromptResponse, ToolCallUpdateFields,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};

    let (client_side, agent_side) = Channel::duplex();

    let agent_task = tokio::spawn(async move {
        AgentRole
            .builder()
            .name("event-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<
                    agent_client_protocol::schema::v1::NewSessionResponse,
                >,
                            _cx| {
                    responder.respond(agent_client_protocol::schema::v1::NewSessionResponse::new(
                        agent_client_protocol::schema::v1::SessionId::new("event-fixture"),
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    let notify = |update| {
                        _cx.send_notification(SessionNotification::new(
                            request.session_id.clone(),
                            update,
                        ))
                    };

                    // Unmapped: user chunks have no TUI representation.
                    let _ = notify(SessionUpdate::UserMessageChunk(acp1::ContentChunk::new(
                        ContentBlock::Text(TextContent::new("user-never-shown")),
                    )));
                    // Thought chunk → SubagentEvent::Thought.
                    let _ = notify(SessionUpdate::AgentThoughtChunk(acp1::ContentChunk::new(
                        ContentBlock::Text(TextContent::new("thinking hard")),
                    )));
                    // Tool call announcement → SubagentEvent::ToolCall.
                    let _ = notify(SessionUpdate::ToolCall(
                        acp1::ToolCall::new("call-1", "Reading config")
                            .kind(acp1::ToolKind::Read)
                            .status(acp1::ToolCallStatus::InProgress)
                            .raw_input(serde_json::json!({"path": "config.toml"})),
                    ));
                    // Tool call update → SubagentEvent::ToolCallUpdate
                    // (status + text content). The fields struct is
                    // `#[non_exhaustive]`, so build it through the builder.
                    let _ = notify(SessionUpdate::ToolCallUpdate(acp1::ToolCallUpdate::new(
                        "call-1",
                        ToolCallUpdateFields::new()
                            .status(acp1::ToolCallStatus::Completed)
                            .content(Some(vec![acp1::ToolCallContent::Content(
                                acp1::Content::new(ContentBlock::Text(TextContent::new(
                                    "port=8080",
                                ))),
                            )])),
                    )));
                    // Plan → SubagentEvent::Plan (full replacement list).
                    let _ = notify(SessionUpdate::Plan(acp1::Plan::new(vec![
                        acp1::PlanEntry::new(
                            "read config",
                            acp1::PlanEntryPriority::High,
                            acp1::PlanEntryStatus::Completed,
                        ),
                        acp1::PlanEntry::new(
                            "write tests",
                            acp1::PlanEntryPriority::Medium,
                            acp1::PlanEntryStatus::InProgress,
                        ),
                    ])));
                    // Usage → SubagentEvent::Usage.
                    let _ = notify(SessionUpdate::UsageUpdate(acp1::UsageUpdate::new(
                        2048, 8192,
                    )));
                    // Mode change → SubagentEvent::Mode.
                    let _ = notify(SessionUpdate::CurrentModeUpdate(
                        acp1::CurrentModeUpdate::new("code-mode"),
                    ));
                    // The agent's actual answer → SubagentEvent::Message (and
                    // the turn's closure).
                    let _ = notify(SessionUpdate::AgentMessageChunk(acp1::ContentChunk::new(
                        ContentBlock::Text(TextContent::new("all done")),
                    )));
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));

    // Drain events until the turn ends (the sender drops with the session).
    let collector = async {
        let mut events = Vec::new();
        while let Some(event) = chunk_rx.recv().await {
            events.push(event);
        }
        events
    };

    let (turn, _, events) = tokio::join!(
        run_session(
            client_side,
            "do the thing".to_string(),
            None,
            std::env::temp_dir(),
            "",
            accumulated.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        agent_task,
        collector,
    );

    let (stop_reason, _session_id) = turn.unwrap();
    assert_eq!(stop_reason, "end_turn");
    // Only message text enters the accumulator — no thoughts, no tool
    // titles, no plan entries.
    assert_eq!(accumulated.lock().unwrap().output(), "all done");

    use super::events::{PlanEntry, PlanEntryPriority, PlanEntryStatus, ToolCallStatus, ToolKind};
    assert_eq!(
        events,
        vec![
            SubagentEvent::Thought {
                text: "thinking hard".to_string(),
            },
            SubagentEvent::ToolCall {
                id: "call-1".to_string(),
                title: "Reading config".to_string(),
                kind: ToolKind::Read,
                status: ToolCallStatus::InProgress,
                raw_input: Some(serde_json::json!({"path": "config.toml"})),
            },
            SubagentEvent::ToolCallUpdate {
                id: "call-1".to_string(),
                status: Some(ToolCallStatus::Completed),
                title: None,
                raw_output: None,
                content: super::events::ToolOutputBlock {
                    text: "port=8080".to_string(),
                    skipped: 0,
                    diff: None,
                },
            },
            SubagentEvent::Plan {
                entries: vec![
                    PlanEntry {
                        content: "read config".to_string(),
                        priority: PlanEntryPriority::High,
                        status: PlanEntryStatus::Completed,
                    },
                    PlanEntry {
                        content: "write tests".to_string(),
                        priority: PlanEntryPriority::Medium,
                        status: PlanEntryStatus::InProgress,
                    },
                ],
            },
            SubagentEvent::Usage {
                context_window: 8192,
                tokens_in_context: 2048,
            },
            SubagentEvent::Mode {
                id: "code-mode".to_string(),
            },
            SubagentEvent::Message {
                text: "all done".to_string(),
            },
        ]
    );
}

/// The typed events serialize to stable snake_case tagged JSON and
/// deserialize back losslessly — the persistence/rehydration contract for
/// Phase 6. Unknown protocol enum values round-trip through the `Unknown`
/// fallbacks instead of failing.
#[test]
fn subagent_events_round_trip_through_json() {
    use super::events::{PlanEntry, PlanEntryPriority, PlanEntryStatus, ToolCallStatus, ToolKind};

    let event = SubagentEvent::ToolCall {
        id: "call-7".to_string(),
        title: "Running tests".to_string(),
        kind: ToolKind::Execute,
        status: ToolCallStatus::InProgress,
        raw_input: Some(serde_json::json!({"cmd": "cargo test"})),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "type": "tool_call",
            "id": "call-7",
            "title": "Running tests",
            "kind": "execute",
            "status": "in_progress",
            "raw_input": {"cmd": "cargo test"}
        })
    );
    assert_eq!(
        serde_json::from_value::<SubagentEvent>(json).unwrap(),
        event
    );

    // Unknown enum values (future spec kinds) deserialize to the `Unknown`
    // fallback instead of erroring.
    let unknown = serde_json::json!({
        "type": "tool_call",
        "id": "call-8",
        "title": "t",
        "kind": "brand_new_kind",
        "status": "frobnicating",
        "raw_input": null
    });
    let parsed = serde_json::from_value::<SubagentEvent>(unknown).unwrap();
    assert_eq!(
        parsed,
        SubagentEvent::ToolCall {
            id: "call-8".to_string(),
            title: "t".to_string(),
            kind: ToolKind::Unknown,
            status: ToolCallStatus::Unknown,
            raw_input: None,
        }
    );

    // Plan entries and usage round-trip too.
    let plan = SubagentEvent::Plan {
        entries: vec![PlanEntry {
            content: "step".to_string(),
            priority: PlanEntryPriority::Low,
            status: PlanEntryStatus::Pending,
        }],
    };
    assert_eq!(
        serde_json::from_value::<SubagentEvent>(serde_json::to_value(&plan).unwrap()).unwrap(),
        plan
    );
    let usage = SubagentEvent::Usage {
        context_window: 100,
        tokens_in_context: 25,
    };
    assert_eq!(
        serde_json::from_value::<SubagentEvent>(serde_json::to_value(&usage).unwrap()).unwrap(),
        usage
    );

    // The remaining variants pin their serialized tag/field names too — the
    // Phase 6 persistence contract depends on all of them staying stable.
    let thought = SubagentEvent::Thought {
        text: "pondering".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&thought).unwrap(),
        serde_json::json!({"type": "thought", "text": "pondering"})
    );
    let update = SubagentEvent::ToolCallUpdate {
        id: "call-9".to_string(),
        status: Some(ToolCallStatus::Completed),
        title: Some("Renamed".to_string()),
        raw_output: Some(serde_json::json!({"exit": 0})),
        content: super::events::ToolOutputBlock {
            text: "done".to_string(),
            skipped: 2,
            diff: None,
        },
    };
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        serde_json::json!({
            "type": "tool_call_update",
            "id": "call-9",
            "status": "completed",
            "title": "Renamed",
            "raw_output": {"exit": 0},
            "content": {"text": "done", "skipped": 2, "diff": null}
        })
    );
    assert_eq!(
        serde_json::from_value::<SubagentEvent>(serde_json::to_value(&update).unwrap()).unwrap(),
        update
    );
    let mode = SubagentEvent::Mode {
        id: "code".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&mode).unwrap(),
        serde_json::json!({"type": "mode", "id": "code"})
    );
    let info = SubagentEvent::SessionInfo {
        title: Some("Fixing the bug".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        serde_json::json!({"type": "session_info", "title": "Fixing the bug"})
    );
    let message = SubagentEvent::Message {
        text: "hi".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        serde_json::json!({"type": "message", "text": "hi"})
    );
}

#[test]
fn invocation_matches_documented_acp_commands() {
    let expected: &[(&str, &str)] = &[
        ("gemini", "gemini --experimental-acp"),
        ("goose", "goose acp"),
        ("opencode", "opencode acp"),
        ("kilo", "kilo acp"),
        ("cline", "cline --acp"),
        ("devin", "devin acp"),
        (
            "claude",
            "npx -y @agentclientprotocol/claude-agent-acp@latest",
        ),
        ("codex", "npx -y @agentclientprotocol/codex-acp@latest"),
    ];
    assert_eq!(
        ACP_AGENTS.len(),
        expected.len(),
        "registry must stay in sync"
    );
    for (name, expected_inv) in expected {
        let agent = ACP_AGENTS
            .iter()
            .find(|a| a.name == *name)
            .unwrap_or_else(|| panic!("agent '{name}' missing from registry"));
        assert_eq!(agent.invocation(), *expected_inv, "agent '{name}'");
    }
    // Regression guard: every registered agent is covered above exactly once.
    for a in ACP_AGENTS {
        assert!(
            expected.iter().any(|(n, _)| *n == a.name),
            "agent '{}' missing from the expected table",
            a.name
        );
    }
}

#[test]
fn only_acp_capable_agents_are_registered() {
    // Agents without ACP support were removed: any registered agent must
    // launch a harness in ACP server mode (documented command/flag).
    for agent in ACP_AGENTS {
        assert!(
            agent
                .args
                .iter()
                .any(|arg| { arg.contains("acp") || arg.contains("agentclientprotocol") }),
            "agent '{}' does not launch an ACP harness",
            agent.name
        );
    }
}

#[test]
fn removed_agents_are_not_registered() {
    for name in ["cursor", "aider", "interpreter"] {
        assert!(
            validate_agent(name).is_err(),
            "agent '{name}' has no ACP support and must not be registered"
        );
    }
}

#[test]
fn validate_agent_accepts_registered_and_rejects_unknown() {
    assert!(validate_agent("gemini").is_ok());
    let err = validate_agent("nope").unwrap_err();
    assert!(err.contains("Supported agents (ACP)"), "{err}");
}

#[test]
fn adapter_entries_require_their_engine_and_npx() {
    // The claude/codex adapters run through npx: both the engine and the
    // adapter runtime must be present for detection to offer them.
    assert_eq!(agent("claude").requires, &["claude", "npx"]);
    assert_eq!(agent("codex").requires, &["codex", "npx"]);
    assert_eq!(agent("gemini").requires, &["gemini"]);
}

#[test]
fn launcher_builds_for_every_registered_agent() {
    for agent in ACP_AGENTS {
        assert!(
            agent_launcher(agent).is_ok(),
            "agent '{}' must build a valid ACP launcher",
            agent.name
        );
    }
}

#[test]
#[cfg(windows)]
fn npx_launcher_routes_through_cmd_detour_on_windows() {
    // The claude/codex launch commands are npm shims (npx.cmd), which
    // `CreateProcessW` cannot spawn directly; agent_launcher must wrap them
    // in `cmd /d /s /c …` when npx resolves to a batch shim. When no shim is
    // present (e.g. a real npx.exe on PATH) the direct command is kept —
    // either way the launcher must build and carry a valid command.
    for name in ["claude", "codex"] {
        let entry = ACP_AGENTS
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("agent '{name}' missing from registry"));
        let launcher = agent_launcher(entry).expect("launcher must build");
        let command = launcher.config().command().to_string_lossy().to_lowercase();
        assert!(
            command.ends_with("cmd.exe") || command.ends_with("\\cmd") || command == "cmd",
            "agent '{name}' launcher should route through cmd (got '{command}')"
        );
    }
}

#[test]
fn install_hint_resolves_only_registered_agents() {
    assert!(!install_hint("gemini").is_empty());
    assert_eq!(install_hint("nope"), "");
}

// -------------------------------------------------------------------------
// Phase 1: fs path sandbox + stable stop reasons
// -------------------------------------------------------------------------

#[test]
fn stop_reasons_are_stable_snake_case_strings() {
    // The persisted `SubAgentCallOutput.stop_reason` must not depend on
    // `Debug` formatting: the wire snake_case names are stable across
    // crate versions.
    use agent_client_protocol::schema::v1::StopReason;
    assert_eq!(stop_reason_str(StopReason::EndTurn), "end_turn");
    assert_eq!(stop_reason_str(StopReason::MaxTokens), "max_tokens");
    assert_eq!(
        stop_reason_str(StopReason::MaxTurnRequests),
        "max_turn_requests"
    );
    assert_eq!(stop_reason_str(StopReason::Refusal), "refusal");
    assert_eq!(stop_reason_str(StopReason::Cancelled), "cancelled");
}

#[test]
fn ensure_path_within_accepts_paths_inside_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("sub").join("file.txt");
    let resolved = ensure_path_within(root.path(), &inside).unwrap();
    // Existing or not, an in-workspace path resolves under the root.
    // Compare against the CANONICAL root: `/tmp` is a symlink on macOS
    // (`/private/tmp`), so the raw tempdir path is not the resolved prefix.
    assert!(resolved.starts_with(root.path().canonicalize().unwrap()));
}

#[test]
fn ensure_path_within_rejects_paths_outside_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let outside = scratch.path().join("secret.txt");
    let err = ensure_path_within(root.path(), &outside)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[test]
fn ensure_path_within_rejects_parent_dir_escapes() {
    let root = tempfile::tempdir().unwrap();
    // Same absolute prefix, but climbing out through `..`.
    let escaping = root.path().join("..").join("elsewhere.txt");
    let err = ensure_path_within(root.path(), &escaping)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn ensure_path_within_rejects_escapes_through_a_symlinked_directory() {
    // A symlink INSIDE the workspace pointing OUTSIDE it must be rejected,
    // even when the target file does not exist yet (a write that would
    // create it): canonicalizing the deepest existing ancestor (the
    // symlinked directory itself) catches what a plain prefix check cannot
    // see.
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(scratch.path(), &link).unwrap();
    let through_link = link.join("new-file.txt");

    let err = ensure_path_within(root.path(), &through_link)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn ensure_path_within_rejects_dangling_symlinks() {
    // A symlink whose target does not exist cannot be verified to stay
    // inside the workspace: it must fail CLOSED (a `canonicalize`-based
    // check would silently miss it, since `canonicalize` errors on
    // dangling links too).
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("dangling");
    std::os::unix::fs::symlink("/nonexistent/target/dir", &link).unwrap();
    let through = link.join("file.txt");

    let err = ensure_path_within(root.path(), &through)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn ensure_path_within_rejects_symlink_escapes() {
    // A symlink INSIDE the workspace pointing OUTSIDE it must be rejected:
    // the on-disk canonicalize pass catches what the lexical prefix check
    // cannot see.
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let secret = scratch.path().join("secret.txt");
    std::fs::write(&secret, "top secret").unwrap();
    let link = root.path().join("link.txt");
    std::os::unix::fs::symlink(&secret, &link).unwrap();

    let err = ensure_path_within(root.path(), &link).unwrap_err().message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[test]
fn ensure_path_within_resolves_an_existing_file_to_its_real_path() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("real.txt");
    std::fs::write(&inside, "content").unwrap();
    let resolved = ensure_path_within(root.path(), &inside).unwrap();
    assert_eq!(resolved, inside.canonicalize().unwrap());
}

// -------------------------------------------------------------------------
// Phase 1 (fix rounds 2-3): kernel-pinned fs sandbox (`sandbox::read/write`)
// — the path the production Unix handlers actually take.
// -------------------------------------------------------------------------

#[cfg(unix)]
fn pinned_root(root: &Path) -> super::sandbox::PinnedRoot {
    super::sandbox::PinnedRoot::acquire(root).unwrap()
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_round_trips_reads_and_writes() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    // Write through the pinned walk, including a NOT-YET-EXISTING
    // intermediate directory (the write must create it).
    sandbox::write(
        &pinned,
        &root.path().join("nested/dir/file.txt"),
        "one\ntwo\nthree\n",
    )
    .unwrap();
    // Full read.
    assert_eq!(
        sandbox::read(
            &pinned,
            &root.path().join("nested/dir/file.txt"),
            None,
            None
        )
        .unwrap(),
        "one\ntwo\nthree\n"
    );
    // 1-based line window: line 2, limit 1 → "two" (protocol contract).
    assert_eq!(
        sandbox::read(
            &pinned,
            &root.path().join("nested/dir/file.txt"),
            Some(2),
            Some(1)
        )
        .unwrap(),
        "two"
    );
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_rejects_paths_outside_the_workspace() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let outside = scratch.path().join("secret.txt");
    std::fs::write(&outside, "top secret").unwrap();
    let pinned = pinned_root(root.path());

    let err = sandbox::read(&pinned, &outside, None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    let err = sandbox::write(&pinned, &outside, "x").unwrap_err().message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    // The outside file was NOT touched by the attempted write.
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "top secret");
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_rejects_relative_paths() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    // The ACP spec requires absolute paths: a relative request is refused
    // (it would resolve against the process CWD, not the workspace).
    let err = sandbox::read(&pinned, Path::new("file.txt"), None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_rejects_symlink_escapes_at_open_time() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    // 1. Symlink FILE inside → outside: fails closed.
    let secret = scratch.path().join("secret.txt");
    std::fs::write(&secret, "top secret").unwrap();
    let file_link = root.path().join("link.txt");
    std::os::unix::fs::symlink(&secret, &file_link).unwrap();
    let err = sandbox::read(&pinned, &file_link, None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    // 2. Symlinked DIRECTORY inside → outside: fails closed on write (the
    //    target cannot be proven to stay inside).
    let dir_link = root.path().join("link");
    std::os::unix::fs::symlink(scratch.path(), &dir_link).unwrap();
    let err = sandbox::write(&pinned, &dir_link.join("new.txt"), "x")
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    assert!(!scratch.path().join("new.txt").exists(), "no outside write");
    // 3. Dangling symlink: fails closed.
    let dangling = root.path().join("dangling");
    std::os::unix::fs::symlink("/nonexistent/target/dir", &dangling).unwrap();
    let err = sandbox::read(&pinned, &dangling.join("f.txt"), None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_write_never_follows_a_final_symlink() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    // A write THROUGH a final-entry symlink fails closed — it must not
    // truncate an existing target nor materialize one through a dangling
    // link (classic no-follow semantics).
    std::fs::write(root.path().join("real.txt"), "precious").unwrap();
    let alias = root.path().join("alias.txt");
    std::os::unix::fs::symlink(root.path().join("real.txt"), &alias).unwrap();
    let err = sandbox::write(&pinned, &alias, "overwritten")
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("real.txt")).unwrap(),
        "precious"
    );

    let missing = root.path().join("missing.txt");
    let link = root.path().join("to-missing.txt");
    std::os::unix::fs::symlink(&missing, &link).unwrap();
    let err = sandbox::write(&pinned, &link, "created?")
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
    assert!(!missing.exists(), "no file materialized through the link");
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_allows_symlinks_that_stay_inside() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    std::fs::write(root.path().join("real.txt"), "inside").unwrap();
    let link = root.path().join("alias.txt");
    std::os::unix::fs::symlink(root.path().join("real.txt"), &link).unwrap();
    // A symlink pointing INSIDE the workspace is legitimate and served.
    assert_eq!(sandbox::read(&pinned, &link, None, None).unwrap(), "inside");
    // An ABSOLUTE target resets the walk base to the pinned root: it must
    // resolve from the workspace root, not from the symlink's directory.
    let subdir = root.path().join("sub");
    std::fs::create_dir_all(&subdir).unwrap();
    let absolute_link = subdir.join("abs.txt");
    std::os::unix::fs::symlink(
        root.path().canonicalize().unwrap().join("real.txt"),
        &absolute_link,
    )
    .unwrap();
    assert_eq!(
        sandbox::read(&pinned, &absolute_link, None, None).unwrap(),
        "inside"
    );
}

#[cfg(unix)]
#[test]
fn pinned_sandbox_rejects_symlink_targets_climbing_out_with_parent_dirs() {
    use super::sandbox;
    let root = tempfile::tempdir().unwrap();
    let pinned = pinned_root(root.path());
    // A relative target climbing out of the workspace via `..` is rejected
    // regardless of where it actually points.
    let link = root.path().join("up.txt");
    std::os::unix::fs::symlink("../elsewhere/secret.txt", &link).unwrap();
    let err = sandbox::read(&pinned, &link, None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );

    // Same for an ABSOLUTE target inside the workspace prefix whose stripped
    // remainder carries `..`: without the check it would reach
    // `openat(root_fd, "..")` and open the workspace's parent.
    let abs_link = root.path().join("abs-up.txt");
    std::os::unix::fs::symlink(
        root.path().join("sub/../../../elsewhere/secret.txt"),
        &abs_link,
    )
    .unwrap();
    let err = sandbox::read(&pinned, &abs_link, None, None)
        .unwrap_err()
        .message;
    assert!(
        err.contains("resolves outside the session workspace"),
        "{err}"
    );
}

// ── Diff summaries (Phase 3b.2) ───────────────────────────────────────

use super::events::diff_summary;

#[test]
fn diff_summary_new_file_counts_everything_as_added() {
    let s = diff_summary("src/new.rs", None, "a\nb\nc\n");
    assert_eq!(s.added, 3);
    assert_eq!(s.removed, 0);
    assert_eq!(s.path, "src/new.rs");
}

#[test]
fn diff_summary_set_difference_counts_changed_lines() {
    let old = "keep\nchange-me\nremove\n";
    let new = "keep\nchanged\nadd\n";
    let s = diff_summary("f.rs", Some(old), new);
    // added: changed, add (2) — removed: change-me, remove (2).
    assert_eq!(s.added, 2);
    assert_eq!(s.removed, 2);
}

#[test]
fn diff_summary_identical_content_is_zero_on_both_sides() {
    let s = diff_summary("f.rs", Some("same\nlines\n"), "same\nlines\n");
    assert_eq!(s.added, 0);
    assert_eq!(s.removed, 0);
}

#[test]
fn diff_summary_degrades_to_length_delta_past_the_cap() {
    let big_old: String =
        std::iter::repeat_n("x\n", super::events::DIFF_SUMMARY_LINE_CAP + 10).collect();
    let big_new = format!("{big_old}extra\n");
    // Either side past the cap → raw line-count delta, no O(n) set scan.
    let s = diff_summary("huge.rs", Some(&big_old), &big_new);
    assert_eq!(s.added, 1);
    assert_eq!(s.removed, 0);
}

#[test]
fn extract_content_text_joins_texts_and_keeps_the_last_diff() {
    use super::events::extract_content_text;
    use agent_client_protocol::schema::v1::{
        Content, ContentBlock, Diff as AcpDiff, ImageContent, TextContent, ToolCallContent,
    };

    let blocks = vec![
        ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new("first")))),
        // A non-text content block is SKIPPED, not rendered.
        ToolCallContent::Content(Content::new(ContentBlock::Image(ImageContent::new(
            "data:img",
            "image/png",
        )))),
        ToolCallContent::Diff(AcpDiff::new("a.rs", "one\ntwo\n").old_text("one\n")),
        // The LAST diff block wins — a follow-up edit supersedes the
        // previous summary (documented contract).
        ToolCallContent::Diff(AcpDiff::new("b.rs", "new file, all added\nlines\n")),
        ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new("second")))),
    ];

    let block = extract_content_text(blocks);
    // Text chunks are newline-joined in order.
    assert_eq!(block.text, "first\nsecond");
    // Exactly one skipped block (the image); terminals also count here.
    assert_eq!(block.skipped, 1);
    // The second diff replaced the first, and its new-file semantics
    // (old_text = None) count every line as added.
    let diff = block.diff.expect("last diff survives");
    assert_eq!(diff.path, "b.rs");
    assert_eq!(diff.added, 2);
    assert_eq!(diff.removed, 0);
}

// ── Session resume (Phase 4) ──────────────────────────────────────────

#[test]
fn input_defaults_continue_session_to_true() {
    use super::SubAgentCallInput;
    // Omitted → resume (the token-efficient default).
    let input: SubAgentCallInput = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(input.continue_session);
    // Explicit opt-out → fresh session.
    let input: SubAgentCallInput =
        serde_json::from_value(serde_json::json!({"continue_session": false})).unwrap();
    assert!(!input.continue_session);
}

#[test]
fn subagent_stores_sessions_per_agent() {
    let sub = super::SubAgent::new();
    assert_eq!(sub.stored_session("gemini"), None);
    // Per-agent keys do not interfere.
    sub.store_session("gemini", "s-1".to_string());
    sub.store_session("opencode", "s-2".to_string());
    assert_eq!(sub.stored_session("gemini").as_deref(), Some("s-1"));
    assert_eq!(sub.stored_session("opencode").as_deref(), Some("s-2"));
    // A later successful turn REPLACES the stored id.
    sub.store_session("gemini", "s-3".to_string());
    assert_eq!(sub.stored_session("gemini").as_deref(), Some("s-3"));
}

/// The dispatch-level flag→id mapping (`continue_session` → the `resume`
/// argument): resume-by-default hands over the stored id, the opt-out and
/// the nothing-stored case produce `None` (→ `session/new`).
#[test]
fn resume_id_maps_continue_session_to_the_stored_id() {
    let sub = super::SubAgent::new();
    // Nothing stored: both the default and the opt-out open a fresh session.
    assert_eq!(sub.resume_id("gemini", true), None);
    assert_eq!(sub.resume_id("gemini", false), None);
    // With an id stored: the default resumes it, the opt-out ignores it.
    sub.store_session("gemini", "s-1".to_string());
    assert_eq!(sub.resume_id("gemini", true).as_deref(), Some("s-1"));
    assert_eq!(sub.resume_id("gemini", false), None);
    // The mapping is per agent: another agent's id is not leaked.
    sub.store_session("opencode", "s-2".to_string());
    assert_eq!(sub.resume_id("opencode", true).as_deref(), Some("s-2"));
    assert_eq!(sub.resume_id("gemini", true).as_deref(), Some("s-1"));
}

/// Which session-lifecycle request the fixture received, in order.
type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Spawn an ACP fixture harness for the resume tests: it advertises the
/// given agent capabilities, logs every session-lifecycle request it
/// receives (order preserved), and ends the prompt turn immediately.
/// Returns the CLIENT side of the duplex channel (to hand to
/// [`drive_turn`]) plus the agent task handle (abort it when done).
fn spawn_resume_fixture(
    capabilities: agent_client_protocol::schema::v1::AgentCapabilities,
    log: RequestLog,
) -> (agent_client_protocol::Channel, tokio::task::JoinHandle<()>) {
    use agent_client_protocol::schema::v1::{
        InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
        NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
        ResumeSessionResponse, SessionId,
    };
    use agent_client_protocol::{Agent as AgentRole, Responder};

    let (client_side, agent_side) = agent_client_protocol::Channel::duplex();
    let new_log = log.clone();
    let resume_log = log.clone();
    let load_log = log.clone();
    let agent_task = tokio::spawn(async move {
        let _ = AgentRole
            .builder()
            .name("resume-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    // The handler closure may fire more than once: clone
                    // the capabilities instead of moving them out.
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(capabilities.clone()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _cx| {
                    new_log.lock().unwrap().push("new".to_string());
                    responder.respond(NewSessionResponse::new(SessionId::new("fresh-1")))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ResumeSessionRequest,
                            responder: Responder<ResumeSessionResponse>,
                            _cx| {
                    resume_log
                        .lock()
                        .unwrap()
                        .push(format!("resume:{}", request.session_id.0));
                    responder.respond(ResumeSessionResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: LoadSessionRequest,
                            responder: Responder<LoadSessionResponse>,
                            _cx| {
                    load_log
                        .lock()
                        .unwrap()
                        .push(format!("load:{}", request.session_id.0));
                    responder.respond(LoadSessionResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await;
    });
    (client_side, agent_task)
}

/// Drive one `run_session` turn against a fixture channel, returning
/// `(stop_reason, session_id)`.
async fn drive_turn(
    client_side: agent_client_protocol::Channel,
    input: &str,
    resume: Option<String>,
) -> (String, String) {
    let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));
    let (turn, _, _) = tokio::join!(
        run_session(
            client_side,
            input.to_string(),
            resume,
            std::env::temp_dir(),
            "",
            accumulated,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        std::future::ready(()),
        std::future::ready(()),
    );
    turn.unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn run_session_resumes_the_stored_session_when_advertised() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, SessionCapabilities, SessionResumeCapabilities,
    };
    let resume_caps = || {
        AgentCapabilities::new().session_capabilities(
            SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
        )
    };

    let log: RequestLog = Arc::default();
    // First turn: no stored session → plain `session/new`, and the id the
    // harness handed out is returned to the caller for storage.
    let (client_side, agent_task) = spawn_resume_fixture(resume_caps(), log.clone());
    let (reason, session_id) = drive_turn(client_side, "first", None).await;
    agent_task.abort();
    assert_eq!(reason, "end_turn");
    assert_eq!(session_id, "fresh-1");
    assert_eq!(log.lock().unwrap().as_slice(), ["new"]);

    // Second turn (fresh fixture, shared log): the stored id + an
    // advertised resume capability → `session/resume` carrying that id;
    // the resumed session id is what the next call would store again.
    let (client_side, agent_task) = spawn_resume_fixture(resume_caps(), log.clone());
    let (reason, session_id) = drive_turn(client_side, "second", Some(session_id)).await;
    agent_task.abort();
    assert_eq!(reason, "end_turn");
    assert_eq!(session_id, "fresh-1");
    assert_eq!(log.lock().unwrap().as_slice(), ["new", "resume:fresh-1"]);
}

#[tokio::test(flavor = "current_thread")]
async fn fresh_session_opt_out_opens_a_new_session_even_when_resume_works() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, SessionCapabilities, SessionResumeCapabilities,
    };

    // The dispatch layer maps `continue_session: false` to `resume: None`
    // at this boundary: even with resume support and a stored id, a fresh
    // session must be opened.
    let log: RequestLog = Arc::default();
    let (client_side, agent_task) = spawn_resume_fixture(
        AgentCapabilities::new().session_capabilities(
            SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
        ),
        log.clone(),
    );
    let (reason, session_id) = drive_turn(client_side, "fresh please", None).await;
    agent_task.abort();
    assert_eq!(reason, "end_turn");
    assert_eq!(session_id, "fresh-1");
    assert_eq!(log.lock().unwrap().as_slice(), ["new"]);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_without_advertised_capability_falls_back_to_new() {
    use agent_client_protocol::schema::v1::AgentCapabilities;

    // A stored id handed to a harness WITHOUT resume support must not
    // abort the turn: it silently falls back to `session/new`.
    let log: RequestLog = Arc::default();
    let (client_side, agent_task) = spawn_resume_fixture(AgentCapabilities::new(), log.clone());
    let (reason, session_id) =
        drive_turn(client_side, "no resume", Some("stored-9".to_string())).await;
    agent_task.abort();
    assert_eq!(reason, "end_turn");
    assert_eq!(session_id, "fresh-1");
    assert_eq!(log.lock().unwrap().as_slice(), ["new"]);
}

#[tokio::test(flavor = "current_thread")]
async fn failed_resume_falls_back_to_a_fresh_session() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeRequest, InitializeResponse, NewSessionRequest,
        NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
        ResumeSessionResponse, SessionCapabilities, SessionId, SessionResumeCapabilities,
    };
    use agent_client_protocol::{Agent as AgentRole, Responder};

    // A fixture whose `session/resume` handler ERRORS (e.g. the harness
    // restarted and lost the session): the turn must still succeed via
    // `session/new`.
    let log: RequestLog = Arc::default();
    let (client_side, agent_side) = agent_client_protocol::Channel::duplex();
    let resume_log = log.clone();
    let new_log = log.clone();
    let agent_task = tokio::spawn(async move {
        let _ = AgentRole
            .builder()
            .name("resume-error-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version).agent_capabilities(
                            AgentCapabilities::new().session_capabilities(
                                SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
                            ),
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: ResumeSessionRequest,
                            responder: Responder<ResumeSessionResponse>,
                            _cx| {
                    resume_log.lock().unwrap().push("resume-error".to_string());
                    let _ = responder.respond_with_internal_error("session gone");
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _cx| {
                    new_log.lock().unwrap().push("new".to_string());
                    responder.respond(NewSessionResponse::new(SessionId::new("fresh-1")))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await;
    });

    let (reason, session_id) = drive_turn(client_side, "retry", Some("stored-9".to_string())).await;
    agent_task.abort();
    assert_eq!(reason, "end_turn");
    assert_eq!(session_id, "fresh-1");
    assert_eq!(log.lock().unwrap().as_slice(), ["resume-error", "new"]);
}

/// 4.4 on a RESUMED session: the `session/resume` response may carry
/// `config_options` whose `model` default differs from the preferred
/// model — the same set-before-prompt contract as `session/new` must
/// apply (the kilo paid-tier default would fail the prompt otherwise).
#[tokio::test(flavor = "current_thread")]
async fn resumed_session_selects_the_preferred_session_model() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
        PromptRequest, PromptResponse, ResumeSessionRequest, ResumeSessionResponse,
        SessionCapabilities, SessionConfigId, SessionConfigOption, SessionConfigOptionValue,
        SessionConfigSelectOption, SessionConfigValueId, SessionNotification,
        SessionResumeCapabilities, SessionUpdate, SetSessionConfigOptionRequest,
        SetSessionConfigOptionResponse, TextContent,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};

    const PREFERRED: &str = "kilo/nvidia/nemotron-3-ultra-550b-a55b:free";

    let log: RequestLog = Arc::default();
    let (client_side, agent_side) = Channel::duplex();
    let resume_log = log.clone();
    let set_log = log.clone();
    let agent_task = tokio::spawn(async move {
        let _ = AgentRole
            .builder()
            .name("resume-model-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version).agent_capabilities(
                            AgentCapabilities::new().session_capabilities(
                                SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
                            ),
                        ),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ResumeSessionRequest,
                            responder: Responder<ResumeSessionResponse>,
                            _cx| {
                    resume_log
                        .lock()
                        .unwrap()
                        .push(format!("resume:{}", request.session_id.0));
                    responder.respond(ResumeSessionResponse::new().config_options(vec![
                        SessionConfigOption::select(
                            SessionConfigId::new("model"),
                            "Model",
                            // Default differs from PREFERRED: must be switched.
                            SessionConfigValueId::new("kilo/google/gemini-3-pro-image"),
                            vec![SessionConfigSelectOption::new(
                                SessionConfigValueId::new(PREFERRED),
                                "Nemotron free",
                            )],
                        ),
                    ]))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: SetSessionConfigOptionRequest,
                            responder: Responder<SetSessionConfigOptionResponse>,
                            _cx| {
                    // Client-side proof the request was actually SENT: the
                    // assertions below only run when the client issues the
                    // switch, so this log entry makes the test falsifiable
                    // if model selection were skipped for resumed sessions.
                    set_log
                        .lock()
                        .unwrap()
                        .push(format!("set-config:{}", request.config_id.0));
                    // The switch must carry the preferred model value.
                    assert_eq!(request.config_id.0.as_ref(), "model");
                    match &request.value {
                        SessionConfigOptionValue::ValueId { value } => {
                            assert_eq!(value.0.as_ref(), PREFERRED);
                        }
                        other => panic!("unexpected config value: {other:?}"),
                    }
                    responder.respond(SetSessionConfigOptionResponse::new(vec![
                        SessionConfigOption::select(
                            SessionConfigId::new("model"),
                            "Model",
                            SessionConfigValueId::new(PREFERRED),
                            vec![SessionConfigSelectOption::new(
                                SessionConfigValueId::new(PREFERRED),
                                "Nemotron free",
                            )],
                        ),
                    ]))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, _cx| {
                    let _ = _cx.send_notification(SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new("model-resumed-ok"),
                        ))),
                    ));
                    responder.respond(PromptResponse::new(
                        agent_client_protocol::schema::v1::StopReason::EndTurn,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await;
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));
    let (turn, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "check resumed model".to_string(),
            Some("stored-9".to_string()),
            std::env::temp_dir(),
            PREFERRED,
            accumulated.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    let (stop_reason, session_id) = turn.unwrap();
    assert_eq!(stop_reason, "end_turn");
    // The RESUMED id (the requested one) is preserved — not a fresh id.
    assert_eq!(session_id, "stored-9");
    assert_eq!(
        log.lock().unwrap().as_slice(),
        // The set-config entry proves model selection ran on the RESUMED
        // session: skipping `select_session_model` for resumes would leave
        // the log at just ["resume:stored-9"] and fail this assertion.
        ["resume:stored-9", "set-config:model"],
        "the turn must resume AND switch the model before prompting"
    );
    assert_eq!(accumulated.lock().unwrap().output(), "model-resumed-ok");
    assert!(matches!(
        streamed.unwrap(),
        SubagentEvent::Message { text } if text == "model-resumed-ok"
    ));
}

/// Phase 5.4: a user stop during a running sub-agent turn sends ACP
/// `session/cancel` and the turn ends with the spec-mandated
/// `StopReason::Cancelled` — with the output streamed so far preserved
/// and the (still valid) session id propagated.
///
/// The fixture's prompt handler spawns its answering task via `cx.spawn`
/// (a handler that awaited inline would block the dispatch loop and the
/// `CancelNotification` could never be processed — deadlock). The spawned
/// task streams one message chunk, waits for the cancel notification, and
/// only then answers.
#[tokio::test(flavor = "current_thread")]
async fn run_session_cancels_the_remote_turn_on_the_stop_signal() {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, InitializeRequest,
        InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
        SessionId, SessionNotification, SessionUpdate, StopReason, TextContent,
    };
    use agent_client_protocol::{Agent as AgentRole, Channel, Responder};
    use std::sync::atomic::{AtomicBool, Ordering};

    const PARTIAL: &str = "partial output so far";

    let (client_side, agent_side) = Channel::duplex();
    // Shared between the prompt task and the cancel-notification handler.
    let cancel_received = Arc::new(tokio::sync::Notify::new());
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let notify = cancel_received.clone();
    let flag = cancel_flag.clone();
    let agent_task = tokio::spawn(async move {
        let _ = AgentRole
            .builder()
            .name("cancel-fixture")
            .on_receive_request(
                async move |request: InitializeRequest,
                            responder: Responder<InitializeResponse>,
                            _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: NewSessionRequest,
                            responder: Responder<NewSessionResponse>,
                            _cx| {
                    responder.respond(NewSessionResponse::new(SessionId::new("cancel-fixture")))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |_notification: CancelNotification, _cx| {
                    flag.store(true, Ordering::Relaxed);
                    // `notify_one` (not `notify_waiters`): it stores a permit
                    // when no waiter is registered yet, so a cancel that
                    // arrives before the spawned responder task's first poll
                    // cannot be lost (which would hang the test instead of
                    // failing it).
                    notify.notify_one();
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder: Responder<PromptResponse>, cx| {
                    // Stream one chunk immediately, then park the answer on
                    // the cancel signal: the client must see the partial
                    // output BEFORE the stop, mirroring a real long turn.
                    let _ = cx.send_notification(SessionNotification::new(
                        request.session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(PARTIAL),
                        ))),
                    ));
                    let notify = cancel_received.clone();
                    cx.spawn(async move {
                        notify.notified().await;
                        responder.respond(PromptResponse::new(StopReason::Cancelled))
                    })?;
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_side)
            .await;
    });

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let accumulated = Arc::new(Mutex::new(super::closure::TurnClosure::default()));

    // Simulate the user hitting ESC as soon as the first chunk arrives.
    let stop_signal = Arc::new(AtomicBool::new(false));
    let stop_driver = {
        let stop_signal = stop_signal.clone();
        async move {
            if chunk_rx.recv().await.is_some() {
                stop_signal.store(true, Ordering::Relaxed);
            }
        }
    };

    let (turn, _, _) = tokio::join!(
        run_session(
            client_side,
            "long running task".to_string(),
            None,
            std::env::temp_dir(),
            "",
            accumulated.clone(),
            stop_signal.clone(),
            chunk_tx,
        ),
        agent_task,
        stop_driver,
    );

    let (stop_reason, session_id) = turn.unwrap();
    assert_eq!(stop_reason, "cancelled");
    // Partial output streamed before the stop survives.
    assert_eq!(accumulated.lock().unwrap().output(), PARTIAL);
    // The turn ended protocol-clean, so the session id IS propagated.
    assert_eq!(session_id, "cancel-fixture");
    // The fixture actually received the ACP cancel notification.
    assert!(cancel_flag.load(Ordering::Relaxed));
}

/// CAPTURE (`#[ignore]`d): run a REAL opencode turn through
/// [`acp::call`] and dump every typed event with its millisecond timestamp
/// to the TUI's regression testdata
/// (`src/tui/routes/session/right_panel/testdata/
/// subagent_stream_capture2.json`), which the TUI replay tests embed via
/// `include_str!`. This variant drives a MULTI-TOOL turn with narration
/// between calls (the user-reported overlap scenario).
///
/// Run with: `cargo test -p cosh-tools --test-threads=1
/// capture_real_opencode_stream_multi_tool -- --ignored --nocapture`, then
/// commit the refreshed JSON together with any code change that needs it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "live harness capture: needs an installed, authenticated opencode"]
async fn capture_real_opencode_stream_multi_tool() {
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let start = std::time::Instant::now();
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("capture runtime");
        rt.block_on(crate::subagent::acp::call(
            "opencode",
            "In three separate steps, each with its own tool call and a \
             one-sentence narration BEFORE and AFTER every call: (1) read \
             crates/cosh-tools/src/subagent/closure.rs and report its first \
             line; (2) grep the workspace for 'TurnClosure' and report how \
             many files mention it; (3) list the files in \
             crates/cosh-tools/src/subagent/ and count them. Think step by \
             step; narrate between the steps.",
            None,
            std::path::PathBuf::from("/home/inky/co-sh"),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ))
    });
    let mut captured: Vec<serde_json::Value> = Vec::new();
    while let Some(event) = chunk_rx.recv().await {
        let shape = match &event {
            SubagentEvent::Message { text } => serde_json::json!({
                "kind": "Message", "text": text,
            }),
            SubagentEvent::Thought { text } => serde_json::json!({
                "kind": "Thought", "text": text,
            }),
            SubagentEvent::ToolCall {
                id,
                title,
                kind,
                status,
                raw_input,
            } => serde_json::json!({
                "kind": "ToolCall", "id": id, "title": title,
                "tool_kind": format!("{kind:?}"), "status": format!("{status:?}"),
                "raw_input": raw_input,
            }),
            SubagentEvent::ToolCallUpdate {
                id,
                status,
                title,
                raw_output,
                content,
            } => serde_json::json!({
                "kind": "ToolCallUpdate", "id": id, "status": status.map(|s| format!("{s:?}")),
                "title": title, "raw_output": raw_output, "content": content,
            }),
            SubagentEvent::Plan { entries } => serde_json::json!({
                "kind": "Plan",
                "entries": entries.iter().map(|e| serde_json::json!({
                    "content": e.content,
                    "status": format!("{:?}", e.status),
                    "priority": format!("{:?}", e.priority),
                })).collect::<Vec<_>>(),
            }),
            SubagentEvent::Usage {
                context_window,
                tokens_in_context,
            } => serde_json::json!({
                "kind": "Usage", "context_window": context_window,
                "tokens_in_context": tokens_in_context,
            }),
            SubagentEvent::Mode { id } => serde_json::json!({
                "kind": "Mode", "id": id,
            }),
            SubagentEvent::SessionInfo { title } => serde_json::json!({
                "kind": "SessionInfo", "title": title,
            }),
            // No catch-all: a new SubagentEvent variant must be added here
            // explicitly so captures never silently drop it.
        };
        captured.push(serde_json::json!({
            "ms": start.elapsed().as_millis() as u64,
            "event": shape,
        }));
    }
    let (result,) = tokio::join!(handle);
    let Ok((report, stop_reason, session_id)) = result.expect("capture join failed") else {
        panic!("capture call failed");
    };
    eprintln!(
        "CAPTURE done: {} events, stop_reason={stop_reason}, session_id={session_id:?}",
        captured.len()
    );
    eprintln!(
        "CAPTURE report tail: {}",
        &report[report.len().saturating_sub(200)..]
    );
    let out = serde_json::json!({ "events": captured, "report": report });
    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_path = format!(
        "{manifest}/../../src/tui/routes/session/right_panel/testdata/subagent_stream_capture2.json"
    );
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&out).expect("serialize capture"),
    )
    .unwrap_or_else(|e| panic!("write capture to {out_path}: {e}"));
}
/// CAPTURE (`#[ignore]`d): run a REAL opencode turn through
/// [`acp::call`] and dump every typed event with its millisecond timestamp
/// to the TUI's regression testdata
/// (`src/tui/routes/session/right_panel/testdata/
/// subagent_stream_capture.json`), which the TUI replay tests embed via
/// `include_str!`. Requires the opencode harness to be installed and
/// authenticated.
///
/// Run with: `cargo test -p cosh-tools --test-threads=1
/// capture_real_opencode_stream -- --ignored --nocapture`, then commit the
/// refreshed JSON together with any code change that needs it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "live harness capture: needs an installed, authenticated opencode"]
async fn capture_real_opencode_stream() {
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let start = std::time::Instant::now();
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("capture runtime");
        rt.block_on(crate::subagent::acp::call(
            "opencode",
            "Read the file crates/cosh-tools/src/subagent/events.rs and, \
             in one short sentence each: (1) say what module it defines, \
             (2) list the first three event variants you saw, \
             (3) state how many lines the file has. Think step by step \
             before answering, then answer.",
            None,
            std::path::PathBuf::from("/home/inky/co-sh"),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chunk_tx,
        ))
    });
    let mut captured: Vec<serde_json::Value> = Vec::new();
    while let Some(event) = chunk_rx.recv().await {
        let shape = match &event {
            SubagentEvent::Message { text } => serde_json::json!({
                "kind": "Message", "text": text,
            }),
            SubagentEvent::Thought { text } => serde_json::json!({
                "kind": "Thought", "text": text,
            }),
            SubagentEvent::ToolCall {
                id,
                title,
                kind,
                status,
                raw_input,
            } => serde_json::json!({
                "kind": "ToolCall", "id": id, "title": title,
                "tool_kind": format!("{kind:?}"), "status": format!("{status:?}"),
                "raw_input": raw_input,
            }),
            SubagentEvent::ToolCallUpdate {
                id,
                status,
                title,
                raw_output,
                content,
            } => serde_json::json!({
                "kind": "ToolCallUpdate", "id": id, "status": status.map(|s| format!("{s:?}")),
                "title": title, "raw_output": raw_output, "content": content,
            }),
            SubagentEvent::Plan { entries } => serde_json::json!({
                "kind": "Plan",
                "entries": entries.iter().map(|e| serde_json::json!({
                    "content": e.content,
                    "status": format!("{:?}", e.status),
                    "priority": format!("{:?}", e.priority),
                })).collect::<Vec<_>>(),
            }),
            SubagentEvent::Usage {
                context_window,
                tokens_in_context,
            } => serde_json::json!({
                "kind": "Usage", "context_window": context_window,
                "tokens_in_context": tokens_in_context,
            }),
            SubagentEvent::Mode { id } => serde_json::json!({
                "kind": "Mode", "id": id,
            }),
            SubagentEvent::SessionInfo { title } => serde_json::json!({
                "kind": "SessionInfo", "title": title,
            }),
            // No catch-all: a new SubagentEvent variant must be added here
            // explicitly so captures never silently drop it.
        };
        captured.push(serde_json::json!({
            "ms": start.elapsed().as_millis() as u64,
            "event": shape,
        }));
    }
    let (result,) = tokio::join!(handle);
    let Ok((report, stop_reason, session_id)) = result.expect("capture join failed") else {
        panic!("capture call failed");
    };
    eprintln!(
        "CAPTURE done: {} events, stop_reason={stop_reason}, session_id={session_id:?}",
        captured.len()
    );
    eprintln!(
        "CAPTURE report tail: {}",
        &report[report.len().saturating_sub(200)..]
    );
    let out = serde_json::json!({ "events": captured, "report": report });
    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_path = format!(
        "{manifest}/../../src/tui/routes/session/right_panel/testdata/subagent_stream_capture.json"
    );
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&out).expect("serialize capture"),
    )
    .unwrap_or_else(|e| panic!("write capture to {out_path}: {e}"));
}
