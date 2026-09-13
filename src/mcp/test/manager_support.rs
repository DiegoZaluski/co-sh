//! Shared doubles for the `manager_*` test scopes.
//!
//! In-memory MCP servers over duplex transports, registered through the
//! production retry path. Kept in one place so every manager scope dials the
//! same fakes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, DiscoverResult, ErrorCode,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, ListPromptsResult,
    ListResourcesResult, ListResourceTemplatesResult, ListToolsResult, PaginatedRequestParams,
    Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ResourceTemplate, Role, ServerCapabilities,
    ServerInfo, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use rmcp::{ErrorData as McpErrorData, ServiceExt};

use super::super::config::{HttpTransport, McpServerEntry, McpTransport, StdioTransport};
use super::super::era;
use super::super::error::McpError;
use super::super::manager::{McpManager, ProbePolicy};

#[derive(Clone)]
pub(crate) struct OkServer {
    pub(crate) tool: String,
    pub(crate) delay_ms: u64,
    pub(crate) fail: bool,
    /// Counts `server/discover` probes so tests can assert era behavior.
    pub(crate) probe_requests: Arc<AtomicUsize>,
}

pub(crate) fn ok_server(tool: &str, delay_ms: u64, fail: bool) -> (OkServer, Arc<AtomicUsize>) {
    let probe_requests = Arc::new(AtomicUsize::new(0));
    let server = OkServer {
        tool: tool.into(),
        delay_ms,
        fail,
        probe_requests: Arc::clone(&probe_requests),
    };
    (server, probe_requests)
}

impl ServerHandler for OkServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>> + MaybeSendFuture + '_
    {
        self.probe_requests.fetch_add(1, Ordering::Relaxed);
        std::future::ready(Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        )))
    }

    async fn call_tool(
        &self,
        _request: rmcp::model::CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpErrorData> {
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        if self.fail {
            return Ok(CallToolResult::error(vec![ContentBlock::text("nope")]).into());
        }
        Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        async move {
            Ok(ListToolsResult {
                tools: vec![tool],
                ..Default::default()
            })
        }
    }
}

pub(crate) async fn attach(manager: &mut McpManager, name: &str, tool: &str, delay_ms: u64) {
    attach_failing(manager, name, tool, delay_ms, false).await;
}

pub(crate) async fn attach_failing(
    manager: &mut McpManager,
    name: &str,
    tool: &str,
    delay_ms: u64,
    fail: bool,
) {
    let (server, _probes) = ok_server(tool, delay_ms, fail);
    attach_server(manager, name, server).await;
}

/// Spawn `handler` on a duplex pair and register the client end under
/// `name` through the production retry path.
pub(crate) async fn attach_server<H>(manager: &mut McpManager, name: &str, handler: H)
where
    H: ServerHandler + Clone + Send + Sync + 'static,
{
    attach_server_full(manager, name, handler, ProbePolicy::STDIO, era::PROBE_TIMEOUT)
        .await
        .unwrap();
}

/// [`attach_server`] with an explicit probe policy and budget, for the
/// silent-server and version-mismatch matrix tests (a real 10 s probe
/// budget would slow the suite for nothing). Returns the connect
/// outcome so failure-path tests can assert on it.
pub(crate) async fn attach_server_full<H>(
    manager: &mut McpManager,
    name: &str,
    handler: H,
    policy: ProbePolicy,
    probe_timeout: Duration,
) -> Result<(), McpError>
where
    H: ServerHandler + Clone + Send + Sync + 'static,
{
    // Mirror `connect_one` hygiene: one entry per name, stale client
    // dropped before the new one registers. The placeholder transport is
    // never dialed here (the duplex factory below supplies it); only the
    // name and its config order matter for snapshots and routing.
    manager.disconnect(name).await;
    manager.insert_test_entry(McpServerEntry {
        name: name.into(),
        transport: McpTransport::Stdio(StdioTransport {
            command: "test".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        }),
        enabled: true,
    });
    // A duplex transport is consumed by a connection attempt, so the
    // factory dials a fresh pair (and a fresh server instance) each time
    // — exactly what an era retry needs.
    let factory = move || {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let handler = handler.clone();
        tokio::spawn(async move {
            let _ = handler.serve(server_io).await.unwrap().waiting().await;
        });
        Ok::<_, McpError>(client_io)
    };
    manager
        .register_with_retry(
            name,
            factory,
            Duration::from_millis(200),
            ProbePolicy {
                timeout: probe_timeout,
                ..policy
            },
        )
        .await
}

pub(crate) fn http_entry(name: &str, url: &str) -> McpServerEntry {
    McpServerEntry {
        name: name.into(),
        transport: McpTransport::Http(HttpTransport {
            url: url.into(),
            headers: HashMap::new(),
            api_key_env: None,
            timeout_ms: 1000,
        }),
        enabled: true,
    }
}

/// A server with resources and prompts capabilities: one text resource,
/// one template, one prompt with a required argument.
#[derive(Clone)]
pub(crate) struct CatalogServer {
    pub(crate) tool: String,
}

impl ServerHandler for CatalogServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(Ok(ListResourcesResult {
            resources: vec![
                Resource::new("file:///notes.txt", "notes")
                    .with_description("Text notes")
                    .with_mime_type("text/plain"),
            ],
            ..Default::default()
        }))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourceTemplatesResult, McpErrorData>>
           + MaybeSendFuture
           + '_ {
        std::future::ready(Ok(ListResourceTemplatesResult {
            resource_templates: vec![ResourceTemplate::new(
                "file:///docs/{id}",
                "doc-by-id",
            )],
            ..Default::default()
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResponse, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(if request.uri == "file:///notes.txt" {
            Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
                vec![ResourceContents::text("hello notes", &request.uri)],
            )))
        } else {
            Err(McpErrorData::new(
                ErrorCode::RESOURCE_NOT_FOUND,
                "no such resource",
                None,
            ))
        })
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListPromptsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(Ok(ListPromptsResult {
            prompts: vec![Prompt::new(
                "greet",
                Some("Greets someone"),
                Some(vec![PromptArgument::new("who").with_required(true)]),
            )],
            ..Default::default()
        }))
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<GetPromptResponse, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(if request.name == "greet" {
            let who = request
                .arguments
                .and_then(|a| a.get("who").and_then(|v| v.as_str()).map(str::to_owned))
                .unwrap_or_default();
            let mut result = GetPromptResult::default();
            result.messages = vec![PromptMessage::new_text(
                Role::User,
                format!("Say hello to {who}"),
            )];
            Ok(GetPromptResponse::Complete(result))
        } else {
            Err(McpErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "no such prompt",
                None,
            ))
        })
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        std::future::ready(Ok(ListToolsResult {
            tools: vec![tool],
            ..Default::default()
        }))
    }
}

/// Legacy-era server: `initialize` works, but `server/discover` is
/// rejected with an implementation-defined method error — a correlated
/// JSON-RPC rejection, per the era tests' probe expectations.
#[derive(Clone)]
pub(crate) struct LegacyOnlyServer {
    pub(crate) tool: String,
}

impl ServerHandler for LegacyOnlyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(Err(McpErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            "unknown method: server/discover",
            None,
        )))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        async move {
            Ok(ListToolsResult {
                tools: vec![tool],
                ..Default::default()
            })
        }
    }
}

/// Modern-era server: answers `server/discover` but rejects the legacy
/// `initialize` handshake with a correlated JSON-RPC error.
#[derive(Clone)]
pub(crate) struct ModernOnlyServer {
    pub(crate) tool: String,
}

impl ServerHandler for ModernOnlyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(&[rmcp::model::ProtocolVersion::V_2026_07_28])
    }

    fn initialize(
        &self,
        _request: rmcp::model::InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::InitializeResult, McpErrorData>>
           + MaybeSendFuture
           + '_ {
        std::future::ready(Err(McpErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            "unknown method: initialize",
            None,
        )))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        async move {
            Ok(ListToolsResult {
                tools: vec![tool],
                ..Default::default()
            })
        }
    }
}

/// A pre-discover-era stdio server: it never answers
/// `server/discover` (the method postdates it), but handshakes and
/// serves tools over the legacy protocol just fine.
#[derive(Clone)]
pub(crate) struct SilentServer {
    pub(crate) tool: String,
}

impl ServerHandler for SilentServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, McpErrorData> {
        std::future::pending().await
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        async move {
            Ok(ListToolsResult {
                tools: vec![tool],
                ..Default::default()
            })
        }
    }
}

/// A modern server that supports a protocol revision we do not offer:
/// discovery succeeds, but every version it lists is foreign.
#[derive(Clone)]
pub(crate) struct OldVersionServer {
    pub(crate) tool: String,
}

impl ServerHandler for OldVersionServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>> + MaybeSendFuture + '_
    {
        std::future::ready(Ok(DiscoverResult::new(
            vec![rmcp::model::ProtocolVersion::V_2025_11_25],
            ServerCapabilities::default(),
        )))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>> + MaybeSendFuture + '_
    {
        let mut tool = Tool::default();
        tool.name = self.tool.clone().into();
        tool.input_schema = Arc::new(serde_json::Map::new());
        async move {
            Ok(ListToolsResult {
                tools: vec![tool],
                ..Default::default()
            })
        }
    }
}
