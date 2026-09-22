//! Tests for the `subagent` module: the [`SubAgent`] wrapper (schema,
//! input reuse, note interpolation) and the ACP engine (agent registry,
//! name validation, launcher building, timeout parsing, and the full
//! in-memory ACP client turn driven through `Channel::duplex()`).

// Items from the sibling engine module: some are `pub(crate)` precisely
// because they are exercised here.
use super::SubAgent;
use super::acp::{
    ACP_AGENTS, Agent, DEFAULT_CALL_TIMEOUT, acp_error, agent_launcher, install_hint, run_session,
    timeout_from_secs, validate_agent,
};

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, ReadTextFileRequest,
    SessionNotification, SessionUpdate, TextContent, WriteTextFileRequest,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    let accumulated = Arc::new(Mutex::new(String::new()));

    let (stop_reason, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "hello from acp".to_string(),
            std::env::temp_dir(),
            "",
            accumulated.clone(),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    assert_eq!(stop_reason.unwrap(), "EndTurn");
    assert_eq!(accumulated.lock().unwrap().as_str(), "hello from acp");
    assert_eq!(streamed.unwrap(), "hello from acp");
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
    let accumulated = Arc::new(Mutex::new(String::new()));

    let (stop_reason, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "exercise handlers".to_string(),
            scratch.path().to_path_buf(),
            "",
            accumulated.clone(),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    assert_eq!(stop_reason.unwrap(), "EndTurn");
    assert_eq!(
        accumulated.lock().unwrap().as_str(),
        "read: l2\nperm: allow-yes\nperm-empty: Cancelled\nwrite: written-by-client"
    );
    assert_eq!(streamed.unwrap(), accumulated.lock().unwrap().as_str());
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
    let accumulated = Arc::new(Mutex::new(String::new()));

    let (stop_reason, _, streamed) = tokio::join!(
        run_session(
            client_side,
            "check model".to_string(),
            std::env::temp_dir(),
            PREFERRED,
            accumulated.clone(),
            chunk_tx,
        ),
        agent_task,
        async { chunk_rx.recv().await },
    );

    assert_eq!(stop_reason.unwrap(), "EndTurn");
    assert_eq!(accumulated.lock().unwrap().as_str(), "model-ok");
    assert_eq!(streamed.unwrap(), "model-ok");
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

#[test]
fn timeout_parsing_falls_back_on_garbage_zero_and_missing() {
    // Pure resolver: no process-global env mutation (set_var/remove_var
    // in a parallel test suite is a data race on getenv/setenv).
    assert_eq!(timeout_from_secs(Some("5")), Duration::from_secs(5));
    assert_eq!(timeout_from_secs(Some(" 30 ")), Duration::from_secs(30));
    assert_eq!(timeout_from_secs(Some("0")), DEFAULT_CALL_TIMEOUT);
    assert_eq!(timeout_from_secs(Some("garbage")), DEFAULT_CALL_TIMEOUT);
    assert_eq!(timeout_from_secs(Some("-3")), DEFAULT_CALL_TIMEOUT);
    assert_eq!(timeout_from_secs(Some("")), DEFAULT_CALL_TIMEOUT);
    assert_eq!(timeout_from_secs(None), DEFAULT_CALL_TIMEOUT);
}
