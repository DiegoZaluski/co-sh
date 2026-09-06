use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{
    IntoTransport, StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use super::bridge::{is_tool_error, result_to_text};
use super::config::{HttpTransport, McpConfig, McpServerEntry, McpTransport, StdioTransport};
use super::error::McpError;
use super::types::ServerSnapshot;
use super::{validate_mcp_config, validate_mcp_entry};

/// Handshake budget (spawn + `initialize` + first `tools/list`). Cold `npx`
/// downloads can be slow, so this is generous on purpose.
const HANDSHAKE_TIMEOUT_MS: u64 = 60_000;

/// Per-call budget for stdio servers. Local tools often shell out to builds
/// and searches, so this is a backstop against hangs, not a latency target.
const STDIO_CALL_TIMEOUT_MS: u64 = 120_000;

/// One connected server: its tools plus the client scoped to them.
struct RunningServer {
    tools: Vec<Tool>,
    client: RunningService<RoleClient, ()>,
    call_timeout: Duration,
}

/// Owns every MCP connection of the session: at most one client per
/// registered name. A failing server degrades to a `Failed` snapshot and
/// never aborts the others.
///
/// Stays on the agent thread: `RunningService` is not `Send`, so the manager
/// is not either. The harness already runs the agent loop thread-locally.
#[derive(Default)]
pub struct McpManager {
    entries: Vec<McpServerEntry>,
    running: HashMap<String, RunningServer>,
    failures: HashMap<String, String>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect every enabled server in `config`. Validation failures abort
    /// before touching state; per-server connection failures are recorded
    /// and skipped so one bad server never blocks the rest. Connections run
    /// sequentially in config order: predictable snapshots matter more than
    /// boot latency for a handful of servers.
    pub async fn connect_all(&mut self, config: &McpConfig) -> Result<(), McpError> {
        validate_mcp_config(config)?;
        // Reconcile first: clients absent from the new config are dropped so
        // no orphan serves tools invisible to snapshots and routing.
        let wanted: std::collections::HashSet<&str> =
            config.servers.iter().map(|e| e.name.as_str()).collect();
        for name in self
            .running
            .keys()
            .filter(|name| !wanted.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.disconnect(&name).await;
        }
        self.entries = config.servers.clone();
        for entry in &self.entries.clone() {
            if !entry.enabled {
                // Bounded graceful cancel, like any other disconnect: a bare
                // `running.remove` would silently drop a live client.
                self.disconnect(&entry.name).await;
                continue;
            }
            // Failures are recorded inside `connect_one`; one bad server
            // never blocks the rest.
            let _ = self.connect_one(entry).await;
        }
        Ok(())
    }

    /// Connect (or reconnect) a single server, replacing any live client.
    /// Disabled entries disconnect instead and report success.
    pub async fn connect_one(&mut self, entry: &McpServerEntry) -> Result<(), McpError> {
        validate_mcp_entry(entry)?;
        self.upsert_entry(entry);
        if !entry.enabled {
            self.disconnect(&entry.name).await;
            return Ok(());
        }
        // Drop a stale client before dialing so a half-open transport can
        // never serve tools after a reconnect.
        self.disconnect(&entry.name).await;
        let call_timeout = call_timeout_for(entry);
        let outcome = match &entry.transport {
            McpTransport::Stdio(stdio) => {
                let transport = match spawn_stdio(&entry.name, stdio) {
                    Ok(transport) => transport,
                    Err(err) => {
                        let err = McpError::Connect(entry.name.clone(), err.to_string());
                        self.failures.insert(entry.name.clone(), err.to_string());
                        return Err(err);
                    }
                };
                self.register(&entry.name, transport, call_timeout).await
            }
            McpTransport::Http(http) => {
                let config = match http_config(http) {
                    Ok(config) => config,
                    Err(err) => {
                        let err = McpError::Connect(entry.name.clone(), err.to_string());
                        self.failures.insert(entry.name.clone(), err.to_string());
                        return Err(err);
                    }
                };
                let transport = StreamableHttpClientTransport::from_config(config);
                self.register(&entry.name, transport, call_timeout).await
            }
        };
        // A standalone `connect_one` must leave a `Failed` snapshot behind,
        // not a perpetual `Connecting` one.
        if let Err(err) = &outcome {
            self.failures.insert(entry.name.clone(), err.to_string());
            self.running.remove(&entry.name);
        }
        outcome
    }

    /// Drop a server's client and clear its failure, if any. Cancellation is
    /// bounded so a hung transport cannot stall reconnects or shutdown.
    pub async fn disconnect(&mut self, name: &str) {
        if let Some(server) = self.running.remove(name) {
            let _ = tokio::time::timeout(Duration::from_secs(5), server.client.cancel()).await;
        }
        self.failures.remove(name);
    }

    /// Drop every client. No async `Drop` exists, so shutdown is explicit:
    /// the harness calls this at agent-loop end (see `Harness::drain_mcp`).
    pub async fn disconnect_all(&mut self) {
        for name in self.running.keys().cloned().collect::<Vec<_>>() {
            self.disconnect(&name).await;
        }
        self.failures.clear();
    }

    /// Footer view: one snapshot per registered server, in config order.
    pub fn status_snapshots(&self) -> Vec<ServerSnapshot> {
        self.entries
            .iter()
            .map(|entry| {
                if !entry.enabled {
                    return ServerSnapshot::disabled(entry.name.clone());
                }
                if let Some(server) = self.running.get(&entry.name) {
                    return ServerSnapshot::ready(entry.name.clone(), server.tools.len());
                }
                if let Some(error) = self.failures.get(&entry.name) {
                    return ServerSnapshot::failed(entry.name.clone(), error.clone());
                }
                ServerSnapshot::connecting(entry.name.clone())
            })
            .collect()
    }

    /// Every tool of every connected server, for headers and extractors.
    /// Config order, so output is deterministic across runs.
    pub fn all_tools(&self) -> Vec<&Tool> {
        self.entries
            .iter()
            .filter_map(|entry| self.running.get(&entry.name))
            .flat_map(|server| &server.tools)
            .collect()
    }

    /// Tools grouped by registered server name, in config order. Powers the
    /// prompt header, which renders one section per server.
    pub fn tools_by_server(&self) -> Vec<(&str, Vec<&Tool>)> {
        self.entries
            .iter()
            .filter_map(|entry| {
                self.running
                    .get(&entry.name)
                    .map(|server| (entry.name.as_str(), server.tools.iter().collect()))
            })
            .collect()
    }

    /// Registered name of the server providing `tool`, if any. First match
    /// in config order wins, so duplicate tool names resolve deterministically.
    pub fn owner_of(&self, tool: &str) -> Option<&str> {
        self.entries
            .iter()
            .filter(|entry| {
                self.running.get(&entry.name).is_some_and(|server| {
                    server.tools.iter().any(|candidate| candidate.name == tool)
                })
            })
            .map(|entry| entry.name.as_str())
            .next()
    }

    /// Call one tool on its owning server. Unknown tools and timeouts are
    /// typed errors; transport failures carry the server name. A server-side
    /// error flag becomes `Err`: tool failures must feed the harness
    /// correction loop, never render as success. Cancellation is client-side
    /// only — the server may keep running the timed-out call.
    pub async fn call_tool(
        &self,
        tool: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let owner = self.owner_of(tool);
        let server = owner.and_then(|name| self.running.get(name));
        let (name, server) = match (owner, server) {
            (Some(name), Some(server)) => (name, server),
            _ => return Err(McpError::UnknownTool(tool.to_string())),
        };
        let params = CallToolRequestParams::new(tool.to_string()).with_arguments(args);
        let timeout = server.call_timeout;
        let result = tokio::time::timeout(timeout, server.client.call_tool(params)).await;
        match result {
            Err(_) => Err(McpError::Timeout(
                name.to_string(),
                timeout.as_millis() as u64,
            )),
            Ok(Err(err)) => Err(McpError::Call(tool.to_string(), err.to_string())),
            Ok(Ok(result)) if is_tool_error(&result) => {
                Err(McpError::Call(tool.to_string(), result_to_text(&result)))
            }
            Ok(Ok(result)) => Ok(result),
        }
    }

    /// Handshake a transport and publish its tools under `entry_name`.
    /// Shared by [`Self::connect_one`] and the harness test hook; generic
    /// over transports so tests can inject in-memory duplex pairs.
    pub(crate) async fn register<T, E, A>(
        &mut self,
        entry_name: &str,
        transport: T,
        call_timeout: Duration,
    ) -> Result<(), McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let connect = async {
            let client = ().serve(transport).await.map_err(|err| {
                McpError::Connect(entry_name.to_string(), err.to_string())
            })?;
            let tools = client
                .list_all_tools()
                .await
                .map_err(|err| McpError::ListTools(entry_name.to_string(), err.to_string()))?;
            Ok::<_, McpError>((client, tools))
        };
        // The configured per-call budget also bounds the handshake from
        // below: a server allowed slow calls gets a slow handshake too.
        // Transport-internal knobs (rmcp control/session timeouts) stay at
        // their defaults — only the outer budget is ours.
        let handshake = call_timeout.max(Duration::from_millis(HANDSHAKE_TIMEOUT_MS));
        let (client, tools) = tokio::time::timeout(handshake, connect)
            .await
            .map_err(|_| {
                McpError::Connect(entry_name.to_string(), "handshake timed out".to_string())
            })??;
        self.failures.remove(entry_name);
        self.running.insert(
            entry_name.to_string(),
            RunningServer {
                tools,
                client,
                call_timeout,
            },
        );
        Ok(())
    }

    fn upsert_entry(&mut self, entry: &McpServerEntry) {
        if let Some(slot) = self.entries.iter_mut().find(|e| e.name == entry.name) {
            *slot = entry.clone();
        } else {
            self.entries.push(entry.clone());
        }
    }

    /// Register a placeholder entry for a test-injected transport. The entry
    /// only keys snapshots and routing order; the tools come from the live
    /// handshake in [`Self::register`].
    #[cfg(test)]
    pub(crate) fn insert_test_entry(&mut self, entry: McpServerEntry) {
        self.upsert_entry(&entry);
    }
}

fn call_timeout_for(entry: &McpServerEntry) -> Duration {
    match &entry.transport {
        McpTransport::Stdio(_) => Duration::from_millis(STDIO_CALL_TIMEOUT_MS),
        McpTransport::Http(http) => Duration::from_millis(http.timeout_ms),
    }
}

/// Open the stderr log for a stdio server: `<temp>/cosh/log/mcp_server_<label>_<ts>.log`.
///
/// The child's stderr would otherwise inherit the terminal and corrupt the
/// TUI's alternate-screen buffer. The label is sanitized so a server name
/// can never escape the log directory.
fn stderr_log_path(label: &str) -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let safe_label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    crate::harness::truncate::scratch_log_dir()
        .join("log")
        .join(format!("mcp_server_{safe_label}_{timestamp}.log"))
}

fn spawn_stdio(label: &str, stdio: &StdioTransport) -> std::io::Result<TokioChildProcess> {
    let mut cmd = tokio::process::Command::new(&stdio.command);
    cmd.args(&stdio.args);
    for (key, value) in &stdio.env {
        cmd.env(key, value);
    }
    if let Some(cwd) = &stdio.cwd {
        cmd.current_dir(cwd);
    }
    let mut builder = TokioChildProcess::builder(cmd);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log_path(label))
    {
        Ok(file) => builder = builder.stderr(Stdio::from(file)),
        Err(err) => {
            log::warn!("mcp: cannot open stderr log for '{label}': {err}");
            builder = builder.stderr(Stdio::null());
        }
    }
    let (transport, _captured_stderr) = builder.spawn()?;
    Ok(transport)
}

fn http_config(http: &HttpTransport) -> Result<StreamableHttpClientTransportConfig, InvalidHeader> {
    let mut headers = HashMap::new();
    for (key, value) in &http.headers {
        let name: http::HeaderName = key.parse().map_err(|_| InvalidHeader(key.clone()))?;
        let header_value: http::HeaderValue = value
            .parse()
            .map_err(|_| InvalidHeader(format!("{key}={value}")))?;
        headers.insert(name, header_value);
    }
    Ok(StreamableHttpClientTransportConfig::with_uri(http.url.trim()).custom_headers(headers))
}

/// A header name or value that HTTP rejects. Kept private: callers only need
/// the entry name plus this reason inside `McpError::Connect`.
#[derive(Debug)]
struct InvalidHeader(String);

impl std::fmt::Display for InvalidHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid header '{}'", self.0)
    }
}

impl std::error::Error for InvalidHeader {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn stderr_log_path_is_sanitized_and_inside_temp_cosh_log() {
        let path = stderr_log_path("my server/v2");
        let expected_dir = crate::harness::truncate::scratch_log_dir().join("log");
        assert_eq!(path.parent(), Some(expected_dir.as_path()));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("mcp_server_my_server_v2_") && name.ends_with(".log"),
            "unexpected file name: {name}"
        );
        assert!(!name.contains('/'));
    }

    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        CallToolResponse, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo,
    };
    use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
    use rmcp::{ErrorData as McpErrorData, ServiceExt};

    #[derive(Clone)]
    struct OkServer {
        tool: String,
        delay_ms: u64,
        fail: bool,
    }

    impl ServerHandler for OkServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
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
        ) -> impl std::future::Future<Output = Result<ListToolsResult, McpErrorData>>
        + MaybeSendFuture
        + '_ {
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

    async fn attach(manager: &mut McpManager, name: &str, tool: &str, delay_ms: u64) {
        attach_failing(manager, name, tool, delay_ms, false).await;
    }

    async fn attach_failing(
        manager: &mut McpManager,
        name: &str,
        tool: &str,
        delay_ms: u64,
        fail: bool,
    ) {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let handler = OkServer {
            tool: tool.into(),
            delay_ms,
            fail,
        };
        tokio::spawn(async move {
            let _ = handler.serve(server_io).await.unwrap().waiting().await;
        });
        // Mirror `connect_one` hygiene: one entry per name, stale client
        // dropped before the new one registers.
        manager.disconnect(name).await;
        if let Some(slot) = manager.entries.iter_mut().find(|e| e.name == name) {
            slot.enabled = true;
        } else {
            manager.entries.push(McpServerEntry {
                name: name.into(),
                transport: McpTransport::Stdio(StdioTransport {
                    command: "test".into(),
                    args: vec![],
                    env: HashMap::new(),
                    cwd: None,
                }),
                enabled: true,
            });
        }
        manager
            .register(name, client_io, Duration::from_millis(200))
            .await
            .unwrap();
    }

    fn http_entry(name: &str, url: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.into(),
            transport: McpTransport::Http(HttpTransport {
                url: url.into(),
                headers: HashMap::new(),
                timeout_ms: 1000,
            }),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn routes_calls_to_owning_server() {
        let mut manager = McpManager::new();
        attach(&mut manager, "a", "alpha.tool", 0).await;
        attach(&mut manager, "b", "beta.tool", 0).await;

        assert_eq!(manager.owner_of("alpha.tool"), Some("a"));
        assert_eq!(manager.owner_of("beta.tool"), Some("b"));
        assert_eq!(manager.owner_of("missing.tool"), None);
        assert_eq!(manager.all_tools().len(), 2);

        let result = manager
            .call_tool("alpha.tool", serde_json::Map::new())
            .await
            .unwrap();
        assert_eq!(super::super::result_to_text(&result), "ok");
    }

    #[tokio::test]
    async fn unknown_tool_is_typed() {
        let mut manager = McpManager::new();
        attach(&mut manager, "a", "alpha.tool", 0).await;

        let err = manager
            .call_tool("ghost.tool", serde_json::Map::new())
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::UnknownTool(_)));
    }

    #[tokio::test]
    async fn slow_server_times_out() {
        let mut manager = McpManager::new();
        attach(&mut manager, "slow", "slow.tool", 5000).await;

        let err = manager
            .call_tool("slow.tool", serde_json::Map::new())
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Timeout(name, ms) if name == "slow" && ms == 200));
    }

    #[tokio::test]
    async fn error_flag_propagates_as_call_failure() {
        let mut manager = McpManager::new();
        attach_failing(&mut manager, "flaky", "flaky.tool", 0, true).await;

        let err = manager
            .call_tool("flaky.tool", serde_json::Map::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, McpError::Call(tool, text) if tool == "flaky.tool" && text == "nope")
        );
    }

    #[tokio::test]
    async fn duplicate_tool_names_resolve_in_config_order() {
        let mut manager = McpManager::new();
        attach(&mut manager, "first", "dup.tool", 0).await;
        attach(&mut manager, "second", "dup.tool", 0).await;

        assert_eq!(manager.owner_of("dup.tool"), Some("first"));
    }

    #[tokio::test]
    async fn reconnect_replaces_stale_tools() {
        let mut manager = McpManager::new();
        attach(&mut manager, "srv", "old.tool", 0).await;
        assert_eq!(manager.owner_of("old.tool"), Some("srv"));

        attach(&mut manager, "srv", "new.tool", 0).await;
        assert_eq!(manager.owner_of("old.tool"), None);
        assert_eq!(manager.owner_of("new.tool"), Some("srv"));
    }

    #[tokio::test]
    async fn disconnect_clears_running_and_failures() {
        let mut manager = McpManager::new();
        attach(&mut manager, "srv", "srv.tool", 0).await;
        manager.disconnect("srv").await;

        assert_eq!(manager.owner_of("srv.tool"), None);
        assert!(manager.all_tools().is_empty());
        let snapshots = manager.status_snapshots();
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(
            snapshots[0].status,
            super::super::ServerStatus::Connecting
        ));
    }

    #[tokio::test]
    async fn connect_all_isolates_failures() {
        let mut manager = McpManager::new();
        let config = McpConfig {
            servers: vec![http_entry("dead", "http://127.0.0.1:1/mcp")],
        };
        manager.connect_all(&config).await.unwrap();

        let snapshots = manager.status_snapshots();
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(
            snapshots[0].status,
            super::super::ServerStatus::Failed
        ));
        assert!(snapshots[0].last_error.is_some());
    }

    #[tokio::test]
    async fn connect_one_failure_leaves_failed_snapshot() {
        let mut manager = McpManager::new();
        let entry = http_entry("dead", "http://127.0.0.1:1/mcp");
        assert!(manager.connect_one(&entry).await.is_err());

        let snapshots = manager.status_snapshots();
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(
            snapshots[0].status,
            super::super::ServerStatus::Failed
        ));
        assert!(snapshots[0].last_error.is_some());
    }

    #[tokio::test]
    async fn connect_all_reconciles_removed_servers() {
        let mut manager = McpManager::new();
        attach(&mut manager, "good", "good.tool", 0).await;

        let config = McpConfig {
            servers: vec![http_entry("dead", "http://127.0.0.1:1/mcp")],
        };
        manager.connect_all(&config).await.unwrap();

        assert_eq!(manager.owner_of("good.tool"), None);
        let snapshots = manager.status_snapshots();
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(
            snapshots[0].status,
            super::super::ServerStatus::Failed
        ));
    }

    #[tokio::test]
    async fn connect_all_skips_disabled_without_dialing() {
        let mut manager = McpManager::new();
        let mut entry = http_entry("off", "http://127.0.0.1:1/mcp");
        entry.enabled = false;
        let config = McpConfig {
            servers: vec![entry],
        };
        manager.connect_all(&config).await.unwrap();

        let snapshots = manager.status_snapshots();
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(
            snapshots[0].status,
            super::super::ServerStatus::Disabled
        ));
    }

    #[tokio::test]
    async fn invalid_config_aborts_before_state_changes() {
        let mut manager = McpManager::new();
        let entry = McpServerEntry {
            name: "bad".into(),
            transport: McpTransport::Http(HttpTransport {
                url: "not a url".into(),
                headers: HashMap::new(),
                timeout_ms: 0,
            }),
            enabled: true,
        };
        assert!(manager.connect_one(&entry).await.is_err());
        assert!(manager.status_snapshots().is_empty());
    }

    #[test]
    fn http_headers_reject_garbage() {
        let mut headers = HashMap::new();
        headers.insert("X-Ok".to_string(), "yes".to_string());
        let entry = McpServerEntry {
            name: "h".into(),
            transport: McpTransport::Http(HttpTransport {
                url: "https://example.com/mcp".into(),
                headers,
                timeout_ms: 1000,
            }),
            enabled: true,
        };
        let McpTransport::Http(http) = &entry.transport else {
            panic!("expected http");
        };
        assert!(http_config(http).is_ok());

        let bad_name = HttpTransport {
            url: "https://example.com/mcp".into(),
            headers: HashMap::from([("not a header".to_string(), "x".to_string())]),
            timeout_ms: 1000,
        };
        assert!(http_config(&bad_name).is_err());

        let bad_value = HttpTransport {
            url: "https://example.com/mcp".into(),
            headers: HashMap::from([("x-ok".to_string(), "bad\nvalue".to_string())]),
            timeout_ms: 1000,
        };
        assert!(http_config(&bad_value).is_err());
    }
}
