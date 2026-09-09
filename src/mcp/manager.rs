use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService,
};
use rmcp::transport::{
    IntoTransport, StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use super::bridge::{is_tool_error, result_to_text};
use super::config::{HttpTransport, McpConfig, McpServerEntry, McpTransport, StdioTransport};
use super::error::McpError;
use super::types::ServerSnapshot;
use super::{era, validate_mcp_config, validate_mcp_entry};

/// Handshake budget (spawn + `initialize` + first `tools/list`). Cold `npx`
/// downloads can be slow, so this is generous on purpose.
const HANDSHAKE_TIMEOUT_MS: u64 = 60_000;

/// How an unknown server's modern probe may fail before the other era is
/// tried, and whether a probe timeout counts as era evidence.
///
/// Per the spec's transport bindings (versioning page, "Backward
/// Compatibility"): on stdio, "the probe returns a non-modern error **or
/// times out**" — a silent legacy server is a legitimate legacy signal. On
/// Streamable HTTP, only the 4xx-body classification identifies legacy; a
/// network timeout says nothing about the era, so HTTP never flips on
/// timeout (the client would otherwise retry legacy against an unreachable
/// or overloaded remote for every request).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbePolicy {
    /// Budget for the open-ended modern probe of an unknown server.
    pub(crate) timeout: Duration,
    /// Whether a probe timeout is era evidence (stdio: yes, HTTP: no).
    pub(crate) flip_on_timeout: bool,
}

impl ProbePolicy {
    /// stdio servers boot slowly (cold `npx`), and a silent server is a
    /// legitimate legacy signal per the spec's stdio binding.
    pub(crate) const STDIO: Self = Self {
        timeout: era::PROBE_TIMEOUT,
        flip_on_timeout: true,
    };

    /// A network timeout is not era evidence on HTTP; the spec classifies
    /// legacy there by response body only.
    pub(crate) const HTTP: Self = Self {
        timeout: era::PROBE_TIMEOUT,
        flip_on_timeout: false,
    };
}

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
    /// Cached protocol era per server (see `era`). The spec says era is a
    /// property of the server (stdio process or HTTP origin), not of a
    /// connection: a reconnect must not re-probe. In-memory only, so an app
    /// restart re-probes once — deliberately trading a probe for never
    /// serving from stale on-disk state.
    eras: HashMap<String, era::Era>,
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
                let name = entry.name.clone();
                let transport = move || {
                    spawn_stdio(&name, stdio)
                        .map_err(|err| McpError::Connect(name.clone(), err.to_string()))
                };
                self.register_with_retry(&entry.name, transport, call_timeout, ProbePolicy::STDIO)
                    .await
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
                let transport =
                    move || Ok(StreamableHttpClientTransport::from_config(config.clone()));
                self.register_with_retry(&entry.name, transport, call_timeout, ProbePolicy::HTTP)
                    .await
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

    /// The era the server last settled in, for tests and diagnostics.
    /// `None` when never connected in this manager's lifetime.
    #[cfg(test)]
    pub(crate) fn era_of(&self, name: &str) -> Option<era::Era> {
        self.eras.get(name).copied()
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

    /// Connect a transport and publish its tools under `entry_name`.
    ///
    /// One connection attempt in a known era.
    ///
    /// - `Modern`: rmcp's `Discover` lifecycle — a `server/discover` round
    ///   trip negotiates the version, then every request carries per-request
    ///   metadata.
    /// - `Legacy`: rmcp's `Initialize` lifecycle — the classic handshake.
    ///
    /// A correlated JSON-RPC rejection while dialing is mapped to
    /// [`McpError::EraStale`] (flip evidence); see `map_initialize_error`.
    /// On success the settled era is read back from `peer_info` and cached.
    async fn connect_in_era<T, E, A>(
        &mut self,
        entry_name: &str,
        transport: T,
        call_timeout: Duration,
        budget: Duration,
        era: era::Era,
    ) -> Result<(), McpError>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let lifecycle = match era {
            era::Era::Modern => ClientLifecycleMode::Discover {
                preferred_versions: era::preferred_versions(),
            },
            era::Era::Legacy => ClientLifecycleMode::Initialize,
        };
        log::debug!("mcp: dialing '{entry_name}' in {}", era.label());

        let connect = async {
            let client = ()
                .serve_with_lifecycle(transport, lifecycle)
                .await
                .map_err(map_initialize_error(entry_name, era))?;
            let tools = client
                .list_all_tools()
                .await
                .map_err(|err| McpError::ListTools(entry_name.to_string(), err.to_string()))?;
            Ok::<_, McpError>((client, tools))
        };
        let (client, tools) = tokio::time::timeout(budget, connect).await.map_err(|_| {
            // A modern-mode dial that gets no answer in time is a typed
            // outcome: the caller decides per transport policy whether
            // silence means "legacy" or "unreachable". A legacy dial
            // timing out is a plain failure — a legacy server answers
            // `initialize` or it is down.
            if era == era::Era::Modern {
                McpError::ProbeTimedOut(entry_name.to_string(), budget.as_millis() as u64)
            } else {
                McpError::Connect(entry_name.to_string(), "handshake timed out".to_string())
            }
        })??;

        // Read the era back rather than trusting the assumption: if the SDK
        // ever settles differently than the lifecycle we dialed, the cache
        // records what the server actually speaks.
        let settled = era::era_of(client.peer_info().as_deref());
        self.failures.remove(entry_name);
        self.running.insert(
            entry_name.to_string(),
            RunningServer {
                tools,
                client,
                call_timeout,
            },
        );
        self.eras.insert(entry_name.to_string(), settled);
        log::debug!("mcp: '{entry_name}' settled in {}", settled.label());
        Ok(())
    }

    /// Connect with era detection and the cached-assumption retry.
    ///
    /// Implements the spec's dual-era mechanics (versioning page,
    /// "Backward Compatibility") with explicit dials instead of rmcp's
    /// `Auto` lifecycle: `Auto` falls back to `initialize` on the *same*
    /// transport session, but rmcp servers stick a modern opener marker on
    /// a session whose first message was `discover`, so a same-transport
    /// fallback gets every later legacy request rejected. Dialing fresh per
    /// attempt sidesteps that and keeps each attempt's session era-pure.
    ///
    /// - Unknown server: dial modern (`server/discover` probe). A correlated
    ///   non-modern rejection — the legacy signature — falls back to one
    ///   legacy dial. On stdio, a silent server (probe timeout) does the
    ///   same, per the spec's stdio binding. A modern-identified rejection
    ///   (reserved -3202x codes, no compatible version) is a hard failure:
    ///   the server *is* modern, there is nothing to fall back to.
    /// - Cached era: dial directly in it. A correlated rejection
    ///   ([`McpError::EraStale`]) refutes the assumption, so retry once in
    ///   the other era — the spec's "re-probe if the cached assumption later
    ///   fails". Transport failures and timeouts carry no era evidence and
    ///   keep the cache.
    ///
    /// `transport` is a factory because a consumed transport (a stdio child
    /// process, a duplex pair) cannot be reused for the second dial. The
    /// whole cycle shares one budget, `max(call_timeout, HANDSHAKE)`.
    pub(crate) async fn register_with_retry<T, E, A, F>(
        &mut self,
        entry_name: &str,
        mut transport: F,
        call_timeout: Duration,
        probe_policy: ProbePolicy,
    ) -> Result<(), McpError>
    where
        F: FnMut() -> Result<T, McpError>,
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let budget = call_timeout.max(Duration::from_millis(HANDSHAKE_TIMEOUT_MS));
        let deadline = tokio::time::Instant::now() + budget;
        let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());

        let first = match self.eras.get(entry_name).copied() {
            Some(cached) => cached,
            None => era::Era::Modern,
        };
        // An unknown server's open-ended probe gets the policy budget; a
        // cached-era dial is a direct negotiation, not a search.
        let probing = !self.eras.contains_key(entry_name);
        let first_budget = if probing {
            probe_policy.timeout
        } else {
            remaining()
        };
        log::debug!(
            "mcp: probing '{entry_name}' ({}): era={}, probe_budget={}ms, flip_on_timeout={} (-{} cache)",
            if probing { "unknown" } else { "cached" },
            first.label(),
            first_budget.as_millis(),
            probe_policy.flip_on_timeout,
            if probing { "no" } else { "yes" }
        );

        let first_transport = transport()?;
        let first_err = match self
            .connect_in_era(entry_name, first_transport, call_timeout, first_budget, first)
            .await
        {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };

        let flip = match (&first_err, first) {
            // Correlated rejection: the assumed era was refused. During a
            // probe this is the legacy signature; on a cached era it is the
            // spec's refuted assumption. Either way, the other era is next.
            (McpError::EraStale(..), assumed) => Some(assumed.other()),
            // Probe timeout on a stdio server: silence is a legitimate
            // legacy signal there (a silent legacy binary ignores discover
            // entirely). It also heals a stale Modern cache pointing at a
            // server that was replaced by a silent legacy one; if the
            // server is merely hung, the legacy dial fails too and the
            // cache is cleared — one extra dial, same outcome. HTTP never
            // flips on a timeout: a network timeout says nothing about
            // which dialect the remote speaks.
            (McpError::ProbeTimedOut(..), era::Era::Modern) if probe_policy.flip_on_timeout => {
                log::debug!(
                    "mcp: '{entry_name}' probe timed out after {}ms; stdio policy treats silence as a legacy signal",
                    probe_policy.timeout.as_millis()
                );
                Some(era::Era::Legacy)
            }
            // Modern-identified rejections (reserved codes, no compatible
            // version) prove the server *is* modern — no fall-back mechanism
            // exists per the spec. HTTP probe timeouts and transport
            // failures carry no era evidence at all.
            _ => None,
        };
        let Some(next) = flip else {
            log::debug!(
                "mcp: '{entry_name}' failed in {} without era evidence: {first_err}",
                first.label()
            );
            self.failures.insert(entry_name.to_string(), first_err.to_string());
            return Err(first_err);
        };
        log::info!(
            "mcp: '{entry_name}' refused {}; falling back to {}",
            first.label(),
            next.label()
        );
        self.eras.insert(entry_name.to_string(), next);

        // Spawn failure on the retry dial: the era evidence stands, so the
        // cache keeps `next`, but there is nothing to report but the cause.
        let second_transport = transport()?;
        match self
            .connect_in_era(entry_name, second_transport, call_timeout, remaining(), next)
            .await
        {
            Ok(()) => Ok(()),
            Err(second) => {
                self.failures
                    .insert(entry_name.to_string(), second.to_string());
                // The cycle ended without a working era and the original
                // assumption was refuted: nothing valid is known anymore,
                // so the next reconnect starts from a fresh probe.
                self.eras.remove(entry_name);
                log::debug!(
                    "mcp: '{entry_name}' failed in {} too; era cache cleared, next reconnect re-probes",
                    next.label()
                );
                if matches!(second, McpError::EraStale(..)) {
                    Err(era_loop_failed(entry_name, next, second))
                } else {
                    Err(second)
                }
            }
        }
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
    /// handshake in [`Self::register_with_retry`].
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

/// Map rmcp's client initialization error to our `McpError`.
///
/// A correlated JSON-RPC rejection while dialing an assumed era carries
/// flip evidence: a non-modern code against a modern assumption is the
/// legacy signature; a modern-reserved code against a legacy assumption
/// proves the server modern. The inverse combinations are plain failures
/// (a modern probe rejected with modern codes means version mismatch, not
/// a legacy server). Transport errors, timeouts, and closing handshakes
/// carry no era evidence and stay [`McpError::Connect`].
fn map_initialize_error(entry_name: &str, assumed: era::Era) -> impl Fn(ClientInitializeError) -> McpError + '_ {
    move |err| match err {
        ClientInitializeError::JsonRpcError(data) => match assumed {
            // Modern dial refused with a modern-reserved code: the server
            // *is* modern but rejected this dial (version mismatch, missing
            // capabilities). Not a legacy signal — there is nothing to
            // fall back to.
            era::Era::Modern if era::is_modern_error(&data) => {
                McpError::Connect(entry_name.to_string(), data.to_string())
            }
            // Modern dial refused with any other correlated error: the
            // legacy signature (spec: "fall back on any error that is not
            // a recognized modern error").
            era::Era::Modern => {
                McpError::EraStale(entry_name.to_string(), assumed.label(), data.to_string())
            }
            // Legacy dial refused at all — with any correlated error: a
            // legacy server accepts `initialize` from a legacy-revision
            // client, so a rejection identifies a modern server. This
            // includes UnsupportedProtocolVersionError, which is how a
            // modern-only server answers an initialize naming an old
            // version (no fall-forward exists, so flip instead).
            era::Era::Legacy => {
                McpError::EraStale(entry_name.to_string(), assumed.label(), data.to_string())
            }
        },
        _ => McpError::Connect(entry_name.to_string(), err.to_string()),
    }
}

/// A retry in the freshly assumed era also failed: both eras were rejected
/// in one connection cycle. Report the retried era and its cause so the
/// user sees why neither dialect connected.
fn era_loop_failed(entry_name: &str, retried: era::Era, second: McpError) -> McpError {
    McpError::Connect(
        entry_name.to_string(),
        format!(
            "neither protocol era is accepted: {} was refused first, and the {} retry also failed ({second})",
            retried.other().label(),
            retried.label()
        ),
    )
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        CallToolResponse, CallToolResult, ContentBlock, DiscoverResult, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    };
    use super::super::ServerStatus;
    use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
    use rmcp::{ErrorData as McpErrorData, ServiceExt};

    #[derive(Clone)]
    struct OkServer {
        tool: String,
        delay_ms: u64,
        fail: bool,
        /// Counts `server/discover` probes so tests can assert era behavior.
        probe_requests: Arc<AtomicUsize>,
    }

    fn ok_server(tool: &str, delay_ms: u64, fail: bool) -> (OkServer, Arc<AtomicUsize>) {
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
        ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>>
        + MaybeSendFuture
        + '_ {
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
        let (server, _probes) = ok_server(tool, delay_ms, fail);
        attach_server(manager, name, server).await;
    }

    /// Spawn `handler` on a duplex pair and register the client end under
    /// `name` through the production retry path.
    async fn attach_server<H>(manager: &mut McpManager, name: &str, handler: H)
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
    async fn attach_server_full<H>(
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

    /// Live end-to-end connect against the official MCP test server over a
    /// real stdio child. Ignored by default — run explicitly with
    /// `cargo test --lib -- --ignored live_connects`: needs `npx` on PATH
    /// and downloads the package on first run. Exercises the exact
    /// production path (`connect_all` → `spawn_stdio` → era probe), which
    /// the duplex-based tests above never touch.
    #[tokio::test]
    #[ignore = "live: spawns npx and downloads @modelcontextprotocol/server-everything"]
    async fn live_connects_server_everything_over_stdio() {
        let mut manager = McpManager::new();
        let config = McpConfig {
            servers: vec![McpServerEntry {
                name: "everything".into(),
                transport: McpTransport::Stdio(StdioTransport {
                    command: "npx".into(),
                    args: vec![
                        "-y".into(),
                        "@modelcontextprotocol/server-everything".into(),
                    ],
                    env: HashMap::new(),
                    cwd: None,
                }),
                enabled: true,
            }],
        };
        manager.connect_all(&config).await.unwrap();
        let snaps = manager.status_snapshots();
        assert_eq!(snaps.len(), 1, "{snaps:?}");
        assert_eq!(snaps[0].status, ServerStatus::Ready, "{snaps:?}");
        assert!(snaps[0].tool_count > 0, "{snaps:?}");
        // The npm-published server (pre-July TS SDK) does not implement
        // `server/discover`, so the probe is rejected and the fallback
        // settles legacy — exactly the dual-era path this client exists
        // for. Pin only that the connect itself is healthy.
        assert_eq!(
            manager.era_of("everything"),
            Some(era::Era::Legacy),
            "{snaps:?}"
        );
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

    /// Legacy-era server: `initialize` works, but `server/discover` is
    /// rejected with an implementation-defined method error — a correlated
    /// JSON-RPC rejection, per the era tests' probe expectations.
    #[derive(Clone)]
    struct LegacyOnlyServer {
        tool: String,
    }

    impl ServerHandler for LegacyOnlyServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        fn discover(
            &self,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>>
        + MaybeSendFuture
        + '_ {
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

    /// Modern-era server: answers `server/discover` but rejects the legacy
    /// `initialize` handshake with a correlated JSON-RPC error.
    #[derive(Clone)]
    struct ModernOnlyServer {
        tool: String,
    }

    impl ServerHandler for ModernOnlyServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
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

    /// A pre-discover-era stdio server: it never answers
    /// `server/discover` (the method postdates it), but handshakes and
    /// serves tools over the legacy protocol just fine.
    #[derive(Clone)]
    struct SilentServer {
        tool: String,
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

    /// A modern server that supports a protocol revision we do not offer:
    /// discovery succeeds, but every version it lists is foreign.
    #[derive(Clone)]
    struct OldVersionServer {
        tool: String,
    }

    impl ServerHandler for OldVersionServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        fn discover(
            &self,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<DiscoverResult, McpErrorData>>
        + MaybeSendFuture
        + '_ {
            std::future::ready(Ok(DiscoverResult::new(
                vec![rmcp::model::ProtocolVersion::V_2025_11_25],
                ServerCapabilities::default(),
            )))
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

    #[tokio::test]
    async fn unknown_server_probes_then_caches_modern_era() {
        let mut manager = McpManager::new();
        let (server, probes) = ok_server("alpha.tool", 0, false);
        attach_server(&mut manager, "a", server).await;

        assert_eq!(manager.era_of("a"), Some(super::super::era::Era::Modern));
        assert_eq!(probes.load(Ordering::Relaxed), 1, "first connect must probe");

        // Reconnect the same name: the cached era dials `Discover` directly,
        // skipping the open-ended probe. The one discover round trip left is
        // the lifecycle's own version negotiation, not era detection.
        let (server, probes) = ok_server("alpha.tool", 0, false);
        attach_server(&mut manager, "a", server).await;
        assert_eq!(
            probes.load(Ordering::Relaxed),
            1,
            "cached era dials Discover directly (single negotiation round)"
        );
    }

    #[tokio::test]
    async fn legacy_server_falls_back_to_initialize() {
        let mut manager = McpManager::new();
        attach_server(&mut manager, "l", LegacyOnlyServer { tool: "legacy.tool".into() }).await;

        assert_eq!(manager.era_of("l"), Some(super::super::era::Era::Legacy));
        assert_eq!(manager.owner_of("legacy.tool"), Some("l"));
    }

    #[tokio::test]
    async fn stale_legacy_cache_recovers_as_modern() {
        let mut manager = McpManager::new();
        attach_server(&mut manager, "s", LegacyOnlyServer { tool: "old.tool".into() }).await;
        assert_eq!(manager.era_of("s"), Some(super::super::era::Era::Legacy));

        // The server was modernized between reconnects: the cached legacy
        // era is rejected (`initialize` is unknown there) and the retry
        // connects modern.
        attach_server(&mut manager, "s", ModernOnlyServer { tool: "new.tool".into() }).await;
        assert_eq!(manager.era_of("s"), Some(super::super::era::Era::Modern));
        assert_eq!(manager.owner_of("new.tool"), Some("s"));
    }

    #[tokio::test]
    async fn stale_modern_cache_recovers_as_legacy() {
        let mut manager = McpManager::new();
        attach_server(&mut manager, "m", ModernOnlyServer { tool: "new.tool".into() }).await;
        assert_eq!(manager.era_of("m"), Some(super::super::era::Era::Modern));

        // The server was downgraded between reconnects: the cached modern
        // era is rejected (`discover` unknown there) and the retry handshakes
        // legacy.
        attach_server(&mut manager, "m", LegacyOnlyServer { tool: "old.tool".into() }).await;
        assert_eq!(manager.era_of("m"), Some(super::super::era::Era::Legacy));
        assert_eq!(manager.owner_of("old.tool"), Some("m"));
    }

    #[tokio::test]
    async fn transport_failure_keeps_cached_era() {
        let mut manager = McpManager::new();
        attach_server(&mut manager, "t", LegacyOnlyServer { tool: "t.tool".into() }).await;
        assert_eq!(manager.era_of("t"), Some(super::super::era::Era::Legacy));

        // A dead transport produces no correlated JSON-RPC rejection, so
        // the era cache must survive the failed connect untouched.
        let entry = http_entry("t", "http://127.0.0.1:1/mcp");
        assert!(manager.connect_one(&entry).await.is_err());
        assert_eq!(manager.era_of("t"), Some(super::super::era::Era::Legacy));
    }

    #[tokio::test]
    async fn silent_stdio_server_falls_back_to_legacy() {
        let mut manager = McpManager::new();
        attach_server_full(
            &mut manager,
            "silent",
            SilentServer {
                tool: "quiet.tool".into(),
            },
            ProbePolicy::STDIO,
            Duration::from_millis(150),
        )
        .await
        .unwrap();

        // Spec, stdio binding: "the probe returns a non-modern error or
        // times out, and the client falls back to initialize".
        assert_eq!(manager.era_of("silent"), Some(era::Era::Legacy));
        assert_eq!(manager.owner_of("quiet.tool"), Some("silent"));
    }

    #[tokio::test]
    async fn silent_probe_never_flips_without_stdio_policy() {
        let mut manager = McpManager::new();
        let outcome = attach_server_full(
            &mut manager,
            "hush",
            SilentServer {
                tool: "quiet.tool".into(),
            },
            // The HTTP policy treats a probe timeout as unreachable, not
            // legacy — a silent stdio binary must not be classified by it.
            ProbePolicy::HTTP,
            Duration::from_millis(150),
        )
        .await;
        assert!(
            matches!(outcome, Err(McpError::ProbeTimedOut(_, 150))),
            "expected a typed probe timeout, got {outcome:?}"
        );

        assert_eq!(manager.era_of("hush"), None);
        assert_eq!(manager.owner_of("quiet.tool"), None);
        let snapshots = manager.status_snapshots();
        let snapshot = snapshots.iter().find(|s| s.name == "hush").unwrap();
        assert!(
            matches!(snapshot.status, ServerStatus::Failed),
            "expected Failed, got {snapshot:?}"
        );
        assert!(snapshot.last_error.is_some());
    }

    #[tokio::test]
    async fn modern_server_without_shared_version_is_typed_failure() {
        let mut manager = McpManager::new();
        let outcome = attach_server_full(
            &mut manager,
            "fut",
            OldVersionServer {
                tool: "old.tool".into(),
            },
            ProbePolicy::STDIO,
            Duration::from_millis(150),
        )
        .await;
        assert!(
            matches!(
                &outcome,
                Err(McpError::Connect(_, text)) if text.contains("compatible protocol version")
            ),
            "expected a version-mismatch diagnostic, got {outcome:?}"
        );

        // Discovery answered, so the server is modern; a version mismatch
        // there is terminal (no fall-forward exists), not a fallback signal.
        assert_eq!(manager.era_of("fut"), None);
        assert_eq!(manager.owner_of("old.tool"), None);
        let snapshots = manager.status_snapshots();
        let snapshot = snapshots.iter().find(|s| s.name == "fut").unwrap();
        assert!(matches!(snapshot.status, ServerStatus::Failed));
        let error = snapshot.last_error.as_deref().unwrap_or_default();
        assert!(
            error.contains("compatible protocol version"),
            "expected a version-mismatch diagnostic, got: {error}"
        );
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
