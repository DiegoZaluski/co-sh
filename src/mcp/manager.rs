use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, Prompt,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceTemplate, Tool,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService,
};
use rmcp::transport::{
    IntoTransport, StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use super::bridge::{
    is_tool_error, match_resource_template, prompt_to_info, resource_to_info, result_to_text,
    template_to_info, PromptInfo, ResourceInfo,
};
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
    /// Resources and RFC 6570 templates advertised at connect time, in the
    /// server's own order. Snapshots only: re-listing on every read would
    /// double the round trips, and change notifications are a follow-up.
    /// Empty when the server lacks the resources capability (lists are
    /// skipped for it — calling `resources/list` unrequested is a spec
    /// violation on our side).
    resources: Vec<Resource>,
    resource_templates: Vec<ResourceTemplate>,
    /// Same contract as `resources`, for the prompts capability.
    prompts: Vec<Prompt>,
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
        // no orphan serves tools invisible to snapshots and routing. Their
        // cached eras are evicted too — a reused name with a different
        // URL/transport must re-probe instead of inheriting a stale era.
        // (Plain `disconnect` keeps the era: `connect_one` calls it before
        // every reconnect, where the cache is load-bearing.)
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
            self.eras.remove(&name);
            self.failures.remove(&name);
        }
        // Evict eras for names that vanished from the config entirely (never
        // connected, only cached/failed).
        for name in self
            .eras
            .keys()
            .filter(|name| !wanted.contains(name.as_str()))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.eras.remove(&name);
            self.failures.remove(&name);
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
    /// The cached era is KEPT: `connect_one` disconnects before every
    /// reconnect, where re-probing each time would defeat the cache. Use
    /// [`Self::evict`] (or config reconciliation in `connect_all`) to drop
    /// a server entirely.
    pub async fn disconnect(&mut self, name: &str) {
        if let Some(server) = self.running.remove(name) {
            let _ = tokio::time::timeout(Duration::from_secs(5), server.client.cancel()).await;
        }
        self.failures.remove(name);
    }

    /// Drop a server entirely: client + failure + cached era. Use when the
    /// server is removed from config or its transport identity changed.
    pub async fn evict(&mut self, name: &str) {
        self.disconnect(name).await;
        self.eras.remove(name);
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
    /// A duplicate logs a `warn!` (once per call — call sites are infrequent)
    /// because shadowing is almost always a config mistake.
    pub fn owner_of(&self, tool: &str) -> Option<&str> {
        self.owner_name("tool", "provided by", tool, |server| {
            server.tools.iter().any(|candidate| candidate.name == tool)
        })
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
            Err(_) => Err(McpError::Timeout(name.to_string(), millis_u64(timeout))),
            Ok(Err(err)) => Err(McpError::Call(tool.to_string(), err.to_string())),
            Ok(Ok(result)) if is_tool_error(&result) => {
                Err(McpError::Call(tool.to_string(), result_to_text(&result)))
            }
            Ok(Ok(result)) => Ok(result),
        }
    }

    /// Every resource and template of every connected server, in config
    /// order (concretes before templates within each server).
    /// Templates carry their raw RFC 6570 `uri` with `is_template: true` —
    /// expand before reading; they are listed uniformly so the TUI can
    /// offer completion, not because they are directly readable.
    pub fn all_resources(&self) -> Vec<ResourceInfo> {
        self.entries
            .iter()
            .filter_map(|entry| self.running.get(&entry.name))
            .flat_map(|server| {
                server
                    .resources
                    .iter()
                    .map(resource_to_info)
                    .chain(server.resource_templates.iter().map(template_to_info))
            })
            .collect()
    }

    /// Registered name of the server advertising `uri`, if any. First match
    /// in config order wins (duplicates warn, like [`Self::owner_of`]:
    /// URIs are global identifiers, so shadowing is even more likely a
    /// config mistake). Concrete URIs match exactly; expanded URIs
    /// (e.g. `file:///docs/42`) match a server whose template
    /// (`file:///docs/{id}`) covers them via [`match_resource_template`].
    /// Raw templates match only themselves — expand with
    /// [`super::bridge::expand_resource_template`] before reading.
    pub fn resource_owner_of(&self, uri: &str) -> Option<&str> {
        self.owner_name("resource", "advertised by", uri, |server| {
            server.resources.iter().any(|r| r.uri == uri)
                || server
                    .resource_templates
                    .iter()
                    .any(|t| t.uri_template == uri || match_resource_template(&t.uri_template, uri))
        })
    }

    /// Every prompt of every connected server, in config order.
    pub fn all_prompts(&self) -> Vec<PromptInfo> {
        self.entries
            .iter()
            .filter_map(|entry| self.running.get(&entry.name))
            .flat_map(|server| server.prompts.iter().map(prompt_to_info))
            .collect()
    }

    /// Registered name of the server offering `name`, if any. First match
    /// in config order wins (duplicates warn, like [`Self::owner_of`]).
    pub fn prompt_owner_of(&self, name: &str) -> Option<&str> {
        self.owner_name("prompt", "offered by", name, |server| {
            server.prompts.iter().any(|p| p.name == name)
        })
    }

    /// First registered server satisfying `serves`, in config order.
    ///
    /// Shared by the `*_owner_of` lookups: duplicates resolve
    /// deterministically (first wins) and log a `warn`, since shadowing is
    /// almost always a config mistake. `kind`/`verb`/`id` phrase the warning
    /// (e.g. `tool` / `provided by` / the tool name).
    fn owner_name<'a>(
        &'a self,
        kind: &str,
        verb: &str,
        id: &str,
        serves: impl Fn(&RunningServer) -> bool,
    ) -> Option<&'a str> {
        let mut owners = self.entries.iter().filter(|entry| match self.running.get(&entry.name) {
            Some(server) => serves(server),
            None => false,
        });
        let first = owners.next()?;
        if let Some(second) = owners.next() {
            log::warn!(
                "mcp: {kind} '{id}' {verb} multiple servers; '{}' wins (config order), '{}' shadowed",
                first.name.as_str(),
                second.name.as_str()
            );
        }
        Some(first.name.as_str())
    }

    /// Read one resource on its owning server. `uri` must be concrete:
    /// expand templates with [`super::bridge::expand_resource_template`]
    /// first — a raw `{var}` URI is rejected as `UnknownResource` without
    /// an RPC (no server resolves the literal placeholder). Expanded URIs
    /// route via template match (see [`Self::resource_owner_of`]).
    /// Snapshots are connect-time only (no live re-list); a resource added
    /// after connect is unreadable until reconnect. Unknown resources and
    /// timeouts are typed errors, mirroring [`Self::call_tool`]; binary
    /// (blob) contents are caller-visible via [`bridge::resource_to_text`]
    /// filtering.
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        let owner = self.resource_owner_of(uri);
        let server = owner.and_then(|name| self.running.get(name));
        let (name, server) = match (owner, server) {
            (Some(name), Some(server)) => (name, server),
            _ => return Err(McpError::UnknownResource(uri.to_string())),
        };
        let params = ReadResourceRequestParams::new(uri);
        let timeout = server.call_timeout;
        let result = tokio::time::timeout(timeout, server.client.read_resource(params)).await;
        match result {
            Err(_) => Err(McpError::Timeout(name.to_string(), millis_u64(timeout))),
            Ok(Err(err)) => Err(McpError::ReadResource(uri.to_string(), err.to_string())),
            Ok(Ok(result)) => Ok(result),
        }
    }

    /// Fetch one prompt on its owning server with `arguments` as the fill-in
    /// values. Unknown prompts and timeouts are typed errors, mirroring
    /// [`Self::call_tool`]. Required arguments are NOT validated client-side
    /// (the `required` flags in [`PromptInfo`] are hints for the TUI form);
    /// the server rejects missing values.
    pub async fn get_prompt(
        &self,
        prompt: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<GetPromptResult, McpError> {
        let owner = self.prompt_owner_of(prompt);
        let server = owner.and_then(|name| self.running.get(name));
        let (name, server) = match (owner, server) {
            (Some(name), Some(server)) => (name, server),
            _ => return Err(McpError::UnknownPrompt(prompt.to_string())),
        };
        let params = GetPromptRequestParams::new(prompt.to_string()).with_arguments(arguments);
        let timeout = server.call_timeout;
        let result = tokio::time::timeout(timeout, server.client.get_prompt(params)).await;
        match result {
            Err(_) => Err(McpError::Timeout(name.to_string(), millis_u64(timeout))),
            Ok(Err(err)) => Err(McpError::GetPrompt(prompt.to_string(), err.to_string())),
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

        // Phase 1: handshake/discover only, under the probe budget. A slow
        // `tools/list` must never be misclassified as probe silence (B1).
        let client = tokio::time::timeout(budget, async {
            ().serve_with_lifecycle(transport, lifecycle)
                .await
                .map_err(map_initialize_error(entry_name, era))
        })
        .await
        .map_err(|_| {
            // A modern-mode dial that gets no answer in time is a typed
            // outcome: the caller decides per transport policy whether
            // silence means "legacy" or "unreachable". A legacy dial
            // timing out is a plain failure — a legacy server answers
            // `initialize` or it is down.
            if era == era::Era::Modern {
                McpError::ProbeTimedOut(entry_name.to_string(), millis_u64(budget))
            } else {
                McpError::Connect(entry_name.to_string(), "handshake timed out".to_string())
            }
        })??;
        // Phase 2: catalog lists under the per-call budget (not the probe
        // budget): cold-booted servers already answered the probe, and large
        // catalogs must not flip eras or report probe timeouts.
        let catalog = async {
            let tools = client
                .list_all_tools()
                .await
                .map_err(|err| McpError::ListTools(entry_name.to_string(), err.to_string()))?;
            // Optional capabilities are listed only when declared: calling
            // resources/list on a legacy tool-only server is a protocol
            // violation, and a method-not-found error would be pure noise.
            let (has_resources, has_prompts) = match client.peer_info().as_deref() {
                Some(info) => (
                    info.capabilities.resources.is_some(),
                    info.capabilities.prompts.is_some(),
                ),
                // No peer info ⇒ legacy dial that settled without one;
                // the old protocol advertised nothing readable here.
                None => (false, false),
            };
            let (resources, resource_templates) = if has_resources {
                match client.list_all_resources().await {
                    Ok(resources) => match client.list_all_resource_templates().await {
                        Ok(templates) => (resources, templates),
                        // Same policy as a failed resources/list: a template
                        // failure is a server bug, not worth losing tools
                        // over. Keep the concrete resources and surface an
                        // empty template catalog.
                        Err(err) => {
                            log::warn!(
                                "mcp: '{entry_name}' declares resources but listing templates failed: {err}"
                            );
                            (resources, Vec::new())
                        }
                    },
                    // A capability flag without a working list is a server
                    // bug, but not worth losing the connection over: keep
                    // the tools and surface the empty catalog.
                    Err(err) => {
                        log::warn!(
                            "mcp: '{entry_name}' declares resources but listing failed: {err}"
                        );
                        (Vec::new(), Vec::new())
                    }
                }
            } else {
                (Vec::new(), Vec::new())
            };
            let prompts = if has_prompts {
                match client.list_all_prompts().await {
                    Ok(prompts) => prompts,
                    Err(err) => {
                        log::warn!(
                            "mcp: '{entry_name}' declares prompts but listing failed: {err}"
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            Ok::<_, McpError>((tools, resources, resource_templates, prompts))
        };
        let (tools, resources, resource_templates, prompts) =
            tokio::time::timeout(call_timeout, catalog)
                .await
                .map_err(|_| {
                    McpError::Connect(
                        entry_name.to_string(),
                        "catalog listing timed out".to_string(),
                    )
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
                resources,
                resource_templates,
                prompts,
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
            "mcp: probing '{entry_name}' ({}): era={}, probe_budget={}ms, flip_on_timeout={} (cache: {})",
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
        if remaining().is_zero() {
            // The first dial consumed the whole shared budget: a second
            // spawn would instantly time out. Report the first failure and
            // leave the cache alone (the refuted assumption was never
            // disproven by a second dial).
            self.failures.insert(entry_name.to_string(), first_err.to_string());
            return Err(first_err);
        }
        // Build the retry transport BEFORE publishing the speculative era:
        // a factory failure (missing binary, consumed test transport) must
        // not leak an unproven cache entry.
        let second_transport = match transport() {
            Ok(t) => t,
            Err(err) => {
                self.failures.insert(entry_name.to_string(), err.to_string());
                return Err(err);
            }
        };
        self.eras.insert(entry_name.to_string(), next);

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
                    Err(era_loop_failed(entry_name, next, &first_err, second))
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

/// Milliseconds as `u64` for error payloads. Saturates instead of wrapping:
/// real budgets are seconds, far below `u64::MAX`, but a debug `as` cast
/// would silently truncate absurd durations.
fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
            // NOTE (MCP-B6, accepted): this is deliberately over-broad —
            // a permanently-broken legacy server rejecting `initialize`
            // with e.g. InvalidParams costs one extra modern dial before
            // the cache clears. Narrowing risks missing real upgrades.
            era::Era::Legacy => {
                McpError::EraStale(entry_name.to_string(), assumed.label(), data.to_string())
            }
        },
        _ => McpError::Connect(entry_name.to_string(), err.to_string()),
    }
}

/// A retry in the freshly assumed era also failed: both eras were rejected
/// in one connection cycle. Report both causes so the user sees why neither
/// dialect connected (previously only the second was shown).
/// Visible to the scoped `test` module for message assertions.
pub(crate) fn era_loop_failed(
    entry_name: &str,
    retried: era::Era,
    first: &McpError,
    second: McpError,
) -> McpError {
    McpError::Connect(
        entry_name.to_string(),
        format!(
            "neither protocol era is accepted: {} was refused first ({first}), and the {} retry also failed ({second})",
            retried.other().label(),
            retried.label()
        ),
    )
}

/// Open the stderr log for a stdio server: `<temp>/cosh/log/mcp_server_<label>_<ts>.log`.
///
/// The child's stderr would otherwise inherit the terminal and corrupt the
/// TUI's alternate-screen buffer. The label is sanitized so a server name
/// can never escape the log directory. Visible to the scoped `test` module
/// for path assertions.
pub(crate) fn stderr_log_path(label: &str) -> std::path::PathBuf {
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

/// Visible to the scoped `test` module for header assertions.
pub(crate) fn http_config(
    http: &HttpTransport,
) -> Result<StreamableHttpClientTransportConfig, InvalidHeader> {
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

/// A header name or value that HTTP rejects. Crate-visible only for the
/// `http_config` signature: callers only need the entry name plus this
/// reason inside `McpError::Connect`.
#[derive(Debug)]
pub(crate) struct InvalidHeader(String);

impl std::fmt::Display for InvalidHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid header '{}'", self.0)
    }
}

impl std::error::Error for InvalidHeader {}
