use std::collections::HashSet;
use std::sync::Arc;

use super::super::core::Harness;
use cosh_sdk::extract_action::ToolCallData;
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use serde_json::json;

fn make_harness() -> Harness {
    Harness::new_test()
}

// ── Mock MCP Servers ──────────────────────────────────────────────

#[derive(Clone)]
struct IntegrityChecker {
    tool_name: String,
    expected_args: serde_json::Value,
}

impl ServerHandler for IntegrityChecker {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let expected = self.expected_args.clone();
        let received = serde_json::to_value(&request.arguments).unwrap_or_default();
        if received == expected {
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
        } else {
            Err(McpError::internal_error("argument mismatch", None))
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool_name.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());

        let tools = vec![tool];
        async move {
            Ok(ListToolsResult {
                tools,
                ..Default::default()
            })
        }
    }
}

/// A server that always fails on `call_tool` — simulates server-side crash.
#[derive(Clone)]
struct CrashOnCall {
    tool_name: String,
}

impl ServerHandler for CrashOnCall {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        Err(McpError::internal_error("server crashed", None))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool_name.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());

        let tools = vec![tool];
        async move {
            Ok(ListToolsResult {
                tools,
                ..Default::default()
            })
        }
    }
}

// ── Session Helpers ───────────────────────────────────────────────

/// Attach any `ServerHandler` to a harness over an in-memory duplex pair.
/// Returns the server task handle; tools come from the live handshake.
async fn attach_handler(
    harness: &mut Harness,
    server_name: &str,
    handler: impl ServerHandler + Clone + 'static,
) -> tokio::task::JoinHandle<()> {
    let (server_io, client_io) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        let _ = handler.serve(server_io).await.unwrap().waiting().await;
    });

    harness
        .attach_test_transport(server_name, client_io)
        .await
        .unwrap();

    handle
}

/// Attach a basic ok-responding server to a harness.
async fn attach_ok(
    harness: &mut Harness,
    server_name: &str,
    tool_name: &str,
) -> tokio::task::JoinHandle<()> {
    use rmcp::handler::server::ServerHandler;

    #[derive(Clone)]
    struct OkServer {
        name: String,
    }

    impl ServerHandler for OkServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, McpError> {
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
        }

        fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_
        {
            let mut tool = Tool::default();
            tool.name = self.name.clone().into();
            tool.input_schema = Arc::new(serde_json::Map::new());

            let tools = vec![tool];
            async move {
                Ok(ListToolsResult {
                    tools,
                    ..Default::default()
                })
            }
        }
    }

    attach_handler(
        harness,
        server_name,
        OkServer {
            name: tool_name.into(),
        },
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────

/// `run_agent_loop` must release every MCP client when it ends (explicit
/// shutdown, no async `Drop`): the in-memory server task terminates while
/// the harness is still alive, not only after `drop(h)`.
#[tokio::test]
async fn agent_loop_end_drains_mcp_clients() {
    use std::sync::atomic::AtomicBool;

    let mut h = make_harness().with_mock_stream(Ok(vec!["done"]));
    let server_handle = attach_ok(&mut h, "s", "loop.tool").await;

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_answer_tx, answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop_signal = Arc::new(AtomicBool::new(false));

    h.run_agent_loop("hi", tx, answer_rx, perm_rx, stop_signal)
        .await;

    tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
        .await
        .expect("MCP client must be disconnected at agent-loop end")
        .expect("server task must not panic");
}

#[tokio::test]
async fn dispatch_next_rejects_disabled_mcp_tool() {
    use cosh_sdk::connector::Connector;

    let mut disabled = HashSet::new();
    disabled.insert("off.tool".to_string());
    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", disabled);
    let server_handle = attach_ok(&mut h, "s", "off.tool").await;

    // Hidden from the header/extractor, and refused at dispatch even when
    // the name reaches the queue.
    assert!(!h.format_header_context().contains("off.tool"));
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "off.tool".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    });
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("is disabled"), "got: {err}");

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_hides_mcp_tools_in_ask_mode() {
    use cosh_sdk::connector::Connector;

    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new())
        .with_mode(super::super::core::Mode::Ask);
    let server_handle = attach_ok(&mut h, "s", "ask.tool").await;

    assert!(!h.format_header_context().contains("ask.tool"));
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "ask.tool".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    });
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found"), "got: {err}");

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_errors_on_empty_queue() {
    let mut h = make_harness();
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no pending tool calls"));
}

#[tokio::test]
async fn dispatch_next_errors_on_unknown_tool() {
    let mut h = make_harness();
    let server_handle = attach_ok(&mut h, "test-server", "known.tool").await;

    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "unknown.tool".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    });

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found for tool"));

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_errors_on_non_object_args() {
    let mut h = make_harness();
    let server_handle = attach_ok(&mut h, "s", "some.tool").await;

    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "some.tool".into(),
        arguments: json!("not-an-object"),
        thought_signature: String::new(),
    });

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("must be a JSON object"));

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_success_removes_from_queue() {
    let mut h = make_harness();
    let server_handle = attach_ok(&mut h, "s", "ok.tool").await;
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "ok.tool".into(),
        arguments: json!({}),
        thought_signature: String::new(),
    });

    assert!(h.dispatch_next().await.is_ok());

    // Queue should now be empty
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no pending tool calls"));

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_preserves_item_on_server_error() {
    let mut h = make_harness();
    let server_handle = attach_handler(
        &mut h,
        "crash-server",
        CrashOnCall {
            tool_name: "crash.tool".into(),
        },
    )
    .await;

    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "crash.tool".into(),
        arguments: json!({"x": 1}),
        thought_signature: String::new(),
    });

    // Dispatch fails — server returned error
    let err = h.dispatch_next().await.unwrap_err();
    assert!(
        err.contains("server crashed"),
        "expected server error, got: {err}"
    );

    // Item should be removed from queue — dispatch_next pops even on failure
    // to avoid infinite loops in Phase 2.
    let err2 = h.dispatch_next().await.unwrap_err();
    assert!(
        err2.contains("no pending"),
        "expected empty queue error, got: {err2}"
    );

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_passes_correct_arguments() {
    // Use IntegrityChecker to verify arguments arrive intact
    let checker = IntegrityChecker {
        tool_name: "echo.tool".into(),
        expected_args: json!({"msg": "hello", "count": 42}),
    };

    let mut h = make_harness();
    let server_handle = attach_handler(&mut h, "integrity-server", checker).await;
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "echo.tool".into(),
        arguments: json!({"msg": "hello", "count": 42}),
        thought_signature: String::new(),
    });

    let result = h.dispatch_next().await;
    assert!(result.is_ok(), "arguments should match exactly: {result:?}");

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_respects_order() {
    let mut h = make_harness();
    let handle_a = attach_ok(&mut h, "server-a", "alpha.read").await;
    let handle_b = attach_ok(&mut h, "server-b", "beta.write").await;

    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "alpha.read".into(),
        arguments: json!({"seq": 1}),
        thought_signature: String::new(),
    });
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "beta.write".into(),
        arguments: json!({"seq": 2}),
        thought_signature: String::new(),
    });

    let r1 = h.dispatch_next().await.unwrap();
    assert_eq!(r1, "ok");

    let r2 = h.dispatch_next().await.unwrap();
    assert_eq!(r2, "ok");

    drop(h);
    let _ = handle_a.await;
    let _ = handle_b.await;
}

#[tokio::test]
async fn dispatch_next_multiple_calls_sequential() {
    let mut h = make_harness();
    let server_handle = attach_ok(&mut h, "seq-server", "seq.tool").await;

    for i in 0..5 {
        h.push_tool_call(ToolCallData {
            id: String::new(),
            name: "seq.tool".into(),
            arguments: json!({"n": i}),
            thought_signature: String::new(),
        });
    }

    for _ in 0..5 {
        let result = h.dispatch_next().await.unwrap();
        assert_eq!(result, "ok");
    }

    assert!(h.dispatch_next().await.is_err());

    drop(h);
    let _ = server_handle.await;
}

// ── Merged subagent_call routing ──────────────────────────────────
//
// `subagent_call` is ONE visible tool with TWO dispatch paths: an external
// agent CLI when `agent` is provided, or an INTERNAL sub-agent (nested
// harness) when `agent` is omitted/empty. Routing happens in
// `dispatch_next` before the cosh-tools tier.

#[tokio::test]
async fn dispatch_next_routes_empty_agent_subagent_call_to_internal_path() {
    let mut h = make_harness();
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "subagent_call".into(),
        // No `agent` → must take the INTERNAL path (new_test has no
        // CoshTools, so the internal path reports it is unavailable).
        arguments: json!({ "input": "review this" }),
        thought_signature: String::new(),
    });
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("internal sub-agent"), "got: {err}");
    // Dispatch contract: the item is consumed even on error, so the caller
    // never re-dispatches the same failed call.
    assert!(
        h.dispatch_next().await.unwrap_err().contains("no pending"),
        "the routed item must be consumed even on error"
    );
}

#[tokio::test]
async fn dispatch_next_routes_empty_string_agent_subagent_call_to_internal_path() {
    let mut h = make_harness();
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "subagent_call".into(),
        // `agent: ""` is treated exactly like omitting it.
        arguments: json!({ "agent": "", "input": "x" }),
        thought_signature: String::new(),
    });
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("internal sub-agent"), "got: {err}");
}

#[tokio::test]
async fn dispatch_next_keeps_external_subagent_path_with_agent() {
    let mut h = make_harness();
    h.push_tool_call(ToolCallData {
        id: String::new(),
        name: "subagent_call".into(),
        arguments: json!({ "agent": "opencode", "input": "x" }),
        thought_signature: String::new(),
    });
    // Agent present → NOT intercepted: falls through to the tiers (new_test
    // has no cosh tools or sessions, so it reaches the MCP tier).
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found"), "got: {err}");
}

#[test]
fn internal_subagent_note_is_interpolated_into_the_tool_description() {
    use std::collections::HashSet;

    use cosh_sdk::connector::Connector;

    // `Harness::new` (not the test constructor) sets the internal-sub-agent
    // note on the `subagent_call` description.
    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new());
    let header = h.format_header_context();
    assert!(
        header.contains("an internal agent runs the task instead"),
        "the internal-sub-agent note must be interpolated into the description"
    );
}

#[test]
fn internal_subagent_header_hides_blocked_tools_and_uses_subagent_prompt() {
    use std::collections::HashSet;

    use super::super::core::{INSTRUCTIONS_SUBAGENT, Mode, SUBAGENT_BLOCKED_TOOLS};
    use cosh_sdk::connector::Connector;

    // Mirror the nested harness construction: parent disabled set + the
    // sub-agent blocklist, Yolo mode, and the sub-agent instructions.
    let mut disabled: HashSet<String> = HashSet::new();
    disabled.extend(SUBAGENT_BLOCKED_TOOLS.iter().map(|t| t.to_string()));
    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", disabled)
        .with_mode(Mode::Yolo)
        .with_instructions(INSTRUCTIONS_SUBAGENT);
    let header = h.format_header_context();
    for blocked in SUBAGENT_BLOCKED_TOOLS {
        assert!(
            !header.contains(blocked),
            "blocked tool `{blocked}` must not appear in the sub-agent header"
        );
    }
    // The sub-agent must never inherit the main agent's review-loop mandate
    // (that is what would make it nest sub-agents indefinitely).
    assert!(!header.contains("Self-Review Loop"));
}

#[test]
fn build_header_teaches_plan_workflow_but_ask_header_does_not() {
    use std::collections::HashSet;

    use super::super::core::Mode;
    use cosh_sdk::connector::Connector;

    // Build mode exposes the plan tools, so the header must teach the
    // Markdown plan workflow (PLAN_WRITE) that lets the model create the
    // full TODO list up front via `plan_load_from_md`.
    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new());
    let header = h.format_header_context();
    assert!(
        header.contains("# Plan: <title>"),
        "Build header must include the PLAN_WRITE workflow"
    );
    assert!(
        header.contains("Save the plan file with `fs_write`"),
        "Build header must instruct the file-based plan workflow"
    );

    // Ask mode is read-only for planning: it exposes only todo_read and
    // load_from_md, so the mutation workflow instructions must stay out.
    let mut h = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new())
        .with_mode(Mode::Ask);
    let header = h.format_header_context();
    assert!(
        !header.contains("# Plan: <title>"),
        "Ask header must not include the PLAN_WRITE workflow"
    );
}

#[test]
fn header_notifies_when_available_tool_set_changes_between_turns() {
    use std::collections::HashSet;

    use super::super::core::Mode;
    use cosh_sdk::connector::Connector;

    // A fresh turn never warns: there is no previous tool set to compare.
    let mut fresh = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new());
    assert!(!fresh.format_header_context().contains("Tool Availability Changed"));

    // First turn in Build mode records the full tool set in the context
    // manager, which travels with the persisted turn state.
    let mut build = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new());
    build.format_header_context();
    let state = build.context_manager.save_state();
    assert!(state.last_tool_set.is_some());

    // Second turn in Ask mode restores that state: the effective tool set
    // shrank to read-only, so the header must tell the model which tools
    // disappeared instead of letting it re-call them and fail.
    let mut ask = Harness::new(Connector::new("openai").unwrap(), ".", HashSet::new())
        .with_mode(Mode::Ask);
    ask.context_manager.restore_state(&state);
    let header = ask.format_header_context();
    assert!(header.contains("Tool Availability Changed"), "header: {header}");
    assert!(header.contains("No longer available"), "header: {header}");
}
