use std::sync::Arc;

use super::super::core::{Harness, ServerSession};
use cosh_sdk::extract_action::ToolCallData;
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
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

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, McpError>> + MaybeSendFuture + '_
    {
        let expected = self.expected_args.clone();
        async move {
            let received = serde_json::to_value(&request.arguments).unwrap_or_default();
            if received == expected {
                Ok(CallToolResult::success(vec![Content::text("ok")]))
            } else {
                Err(McpError::internal_error("argument mismatch", None))
            }
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

    fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, McpError>> + MaybeSendFuture + '_
    {
        async move { Err(McpError::internal_error("server crashed", None)) }
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

/// Create a session from any `ServerHandler`.
async fn session_from_handler(
    server_name: &str,
    handler: impl ServerHandler + Clone + Send + Sync + 'static,
) -> (ServerSession, tokio::task::JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        let _ = handler.serve(server_io).await.unwrap().waiting().await;
    });

    let client = ().serve(client_io).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();

    let session = ServerSession {
        name_server: server_name.to_string(),
        tools,
        client,
    };

    (session, handle)
}

/// Create a session with a basic ok-responding server.
async fn ok_session(
    server_name: &str,
    tool_name: &str,
) -> (ServerSession, tokio::task::JoinHandle<()>) {
    use rmcp::handler::server::ServerHandler;

    #[derive(Clone)]
    struct OkServer {
        name: String,
    }

    impl ServerHandler for OkServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<CallToolResult, McpError>> + MaybeSendFuture + '_
        {
            async move { Ok(CallToolResult::success(vec![Content::text("ok")])) }
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

    session_from_handler(
        server_name,
        OkServer {
            name: tool_name.into(),
        },
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dispatch_next_errors_on_empty_queue() {
    let mut h = make_harness();
    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no pending tool calls"));
}

#[tokio::test]
async fn dispatch_next_errors_on_unknown_tool() {
    let (session, server_handle) = ok_session("test-server", "known.tool").await;

    let mut h = make_harness();
    h.push_session(session);

    h.push_tool_call(ToolCallData {
        name: "unknown.tool".into(),
        arguments: json!({}),
    });

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("no server found for tool"));

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_errors_on_non_object_args() {
    let (session, server_handle) = ok_session("s", "some.tool").await;

    let mut h = make_harness();
    h.push_session(session);

    h.push_tool_call(ToolCallData {
        name: "some.tool".into(),
        arguments: json!("not-an-object"),
    });

    let err = h.dispatch_next().await.unwrap_err();
    assert!(err.contains("must be a JSON object"));

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_success_removes_from_queue() {
    let (session, server_handle) = ok_session("s", "ok.tool").await;

    let mut h = make_harness();
    h.push_session(session);
    h.push_tool_call(ToolCallData {
        name: "ok.tool".into(),
        arguments: json!({}),
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
    let (session, server_handle) = session_from_handler(
        "crash-server",
        CrashOnCall {
            tool_name: "crash.tool".into(),
        },
    )
    .await;

    let mut h = make_harness();
    h.push_session(session);
    h.push_tool_call(ToolCallData {
        name: "crash.tool".into(),
        arguments: json!({"x": 1}),
    });

    // Dispatch fails — server returned error
    let err = h.dispatch_next().await.unwrap_err();
    assert!(
        err.contains("server crashed"),
        "expected server error, got: {err}"
    );

    // Item should STILL be in queue — dispatch_next preserves on failure.
    let err2 = h.dispatch_next().await.unwrap_err();
    assert!(
        err2.contains("server crashed"),
        "item should still be in queue, got: {err2}"
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

    let (session, server_handle) = session_from_handler("integrity-server", checker).await;

    let mut h = make_harness();
    h.push_session(session);
    h.push_tool_call(ToolCallData {
        name: "echo.tool".into(),
        arguments: json!({"msg": "hello", "count": 42}),
    });

    let result = h.dispatch_next().await;
    assert!(result.is_ok(), "arguments should match exactly: {result:?}");

    drop(h);
    let _ = server_handle.await;
}

#[tokio::test]
async fn dispatch_next_respects_order() {
    let (session_a, handle_a) = ok_session("server-a", "alpha.read").await;
    let (session_b, handle_b) = ok_session("server-b", "beta.write").await;

    let mut h = make_harness();
    h.push_session(session_a);
    h.push_session(session_b);

    h.push_tool_call(ToolCallData {
        name: "alpha.read".into(),
        arguments: json!({"seq": 1}),
    });
    h.push_tool_call(ToolCallData {
        name: "beta.write".into(),
        arguments: json!({"seq": 2}),
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
    let (session, server_handle) = ok_session("seq-server", "seq.tool").await;

    let mut h = make_harness();
    h.push_session(session);

    for i in 0..5 {
        h.push_tool_call(ToolCallData {
            name: "seq.tool".into(),
            arguments: json!({"n": i}),
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
