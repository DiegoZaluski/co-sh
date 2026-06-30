use super::namespace_cache::{CacheData, NamespaceCache, Verification};
use super::summarizer;

use cosh_sdk::connector::Connector;
use cosh_sdk::extract_action::{ExtractAction, Item, StreamAction, ToolCallData, ToolSchema};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
const CACHE_FILE: &str = "cache-namespace-tools.toml";

pub struct NamespaceTool {
    pub name: String,
    pub description: Vec<String>,
    pub tools: Vec<Tool>,
}

pub struct ServerSession {
    pub name_server: String,
    pub tools: Vec<Tool>,
    pub client: RunningService<RoleClient, ()>,
}

pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct InternalTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

fn default_internal_tools() -> Vec<InternalTool> {
    vec![
        InternalTool {
            name: "expand_namespace".into(),
            description: "Expand a namespace to see every available MCP tool inside it with their original descriptions and schemas. Call this when the summary alone is not enough to understand what the namespace offers.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "server": {
                        "type": "string",
                        "description": "Server name containing the namespace"
                    },
                    "namespace": {
                        "type": "string",
                        "description": "Namespace to expand"
                    }
                },
                "required": ["server", "namespace"]
            }),
        },
        InternalTool {
            name: "stop_agent_loop".into(),
            description: "Stop running the agent loop".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}

pub struct Harness {
    connector: Connector,
    sessions: Vec<ServerSession>,
    live_cache: HashMap<String, HashMap<String, CacheData>>,
    protocol: Option<String>, // add fallback ?
    header_context: String,
    system_prompts: Vec<PromptSystem>,
    expanded_namespaces: HashSet<(String, String)>,
    internal_tools: Vec<InternalTool>,
    /// To stop the agent loop.
    stop: bool,
    tool_issuer: VecDeque<ToolCallData>,
    server_response: Vec<String>,

    #[cfg(test)]
    pub(crate) mock_chat_response: Option<Result<String, String>>,
    #[cfg(test)]
    pub(crate) mock_stream_response: Option<Result<Vec<String>, String>>,
    #[cfg(test)]
    pub(crate) test_tools: Vec<ToolSchema>,
}

/// Extract the namespace portion of a tool name (everything before the first `.`).
///
/// Returns `""` when the name has no dot — no namespace to extract.
fn namespace_of(name: &str) -> &str {
    name.split_once('.').map_or("", |(ns, _)| ns)
}

impl Harness {
    #[must_use]
    pub fn new(connector: Connector) -> Self {
        Self {
            connector,
            sessions: Vec::new(),
            live_cache: HashMap::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            expanded_namespaces: HashSet::new(),
            internal_tools: default_internal_tools(),
            stop: false,
            tool_issuer: VecDeque::new(),
            server_response: Vec::new(),
            #[cfg(test)]
            mock_chat_response: None,
            #[cfg(test)]
            mock_stream_response: None,
            #[cfg(test)]
            test_tools: Vec::new(),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the transport cannot be created,
    /// the MCP handshake fails, or the protocol is unsupported.
    pub async fn connect(
        &mut self,
        server: &str,
        protocol: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = match protocol {
            "stdio" => {
                let mut cmd = tokio::process::Command::new("npx");
                cmd.arg("-y").arg(server);
                let transport = TokioChildProcess::new(cmd)?;
                ().serve(transport).await?
            }

            "http" => {
                let transport = StreamableHttpClientTransport::from_uri(server);
                ().serve(transport).await?
            }

            other => return Err(format!("unsupported protocol: {other}").into()),
        };

        let name_server = client
            .peer_info()
            .map(|i| i.server_info.name.clone())
            .unwrap_or_default();

        let tools = client.list_all_tools().await?;

        self.sessions.push(ServerSession {
            name_server,
            tools,
            client,
        });
        Ok(())
    }

    /// Builds the full content for each (server, namespace) by concatenating
    /// every tool's description + `input_schema`. The string is used as the hash
    /// seed — any change to any tool in the group invalidates the namespace.
    ///
    /// Tools whose name does not contain a `.` (no namespace) are skipped —
    /// they are rendered inline instead of being summarized.
    #[must_use]
    pub fn build_cache_map(&self) -> HashMap<(String, String), String> {
        let mut out = HashMap::new();
        for session in &self.sessions {
            // Group tool parts by namespace within this server
            let mut ns_parts: HashMap<String, Vec<String>> = HashMap::new();
            for tool in &session.tools {
                let ns = namespace_of(&tool.name);
                if ns.is_empty() {
                    continue;
                }
                let desc = tool.description.as_deref().unwrap_or_default();
                // input_schema is Arc<JsonObject> — serialize to include in hash
                let schema = serde_json::to_string(&*tool.input_schema).unwrap_or_default();
                ns_parts
                    .entry(ns.to_string())
                    .or_default()
                    .push(format!("{desc}\n{schema}"));
            }

            // Flatten each namespace group into one content blob
            for (ns, parts) in ns_parts {
                out.insert((session.name_server.clone(), ns), parts.join("\n---\n"));
            }
        }
        out
    }

    /// # Panics
    ///
    /// Panics if the cache file cannot be written to disk.
    pub fn resolve_cache(&mut self) -> &HashMap<String, HashMap<String, CacheData>> {
        let cache_map = self.build_cache_map();
        let mut cache = NamespaceCache::new().with_cache_file(CACHE_FILE);

        for ((server, ns), content) in &cache_map {
            cache.set_context(server, ns, content);
            if cache.verify() == Verification::Modified {
                // send content to LLM to summarize
                let summary = summarizer::summarize_namespace(&self.connector, server, ns, content);
                cache.mark_modified(summary);
            }
        }

        cache.flush().expect("failed to flush cache");
        self.live_cache.clone_from(cache.cached());
        &self.live_cache
    }

    pub fn set_protocol(&mut self, protocol: impl Into<Option<String>>) -> &mut Self {
        self.protocol = protocol.into();
        self
    }

    #[must_use]
    pub fn with_system_prompt(mut self, title: impl Into<String>, text: impl Into<String>) -> Self {
        self.system_prompts.push(PromptSystem {
            title: title.into(),
            text: text.into(),
        });
        self
    }

    /// Builds the system header for the LLM.
    ///
    /// Concatenates system prompts, internal harness tools (always expanded),
    /// and MCP namespace summaries from the live cache. Namespaces that were
    /// previously marked as expanded via `expand_namespace` show the original
    /// tool descriptions from the MCP sessions instead of the cached summary.
    ///
    /// Tools whose name does not contain a `.` (no namespace) are rendered
    /// inline with full schema — they are not cached or summarized.
    ///
    /// Clears the expanded set after formatting so the next loop starts fresh.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
        }

        let _ = write!(out, "## Internal Tools\n\n");
        for tool in &self.internal_tools {
            let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
            let _ = write!(
                out,
                "### {}\n{}\nSchema: {}\n\n",
                tool.name, tool.description, schema,
            );
        }

        // Index tools by (server, namespace) for O(1) expanded lookup.
        // Dotless tools are excluded — they render in a separate section.
        let mut tools_by_ns: HashMap<(&str, &str), Vec<&Tool>> = HashMap::new();
        let mut dotless_tools: Vec<(&str, &Tool)> = Vec::new();
        for session in &self.sessions {
            for tool in &session.tools {
                let ns = namespace_of(&tool.name);
                if ns.is_empty() {
                    dotless_tools.push((session.name_server.as_str(), tool));
                } else {
                    tools_by_ns
                        .entry((session.name_server.as_str(), ns))
                        .or_default()
                        .push(tool);
                }
            }
        }

        let _ = write!(out, "## Connected Servers\n\n");
        for (server, namespaces) in &self.live_cache {
            for (ns, data) in namespaces {
                if self
                    .expanded_namespaces
                    .contains(&(server.clone(), ns.clone()))
                {
                    if let Some(tools) = tools_by_ns.get(&(server.as_str(), ns.as_str())) {
                        for tool in tools {
                            let desc = tool.description.as_deref().unwrap_or_default();
                            let schema = serde_json::to_string_pretty(&tool.input_schema)
                                .unwrap_or_default();
                            let _ = write!(
                                out,
                                "- **{}**: {}\n  Schema: {}\n",
                                tool.name, desc, schema,
                            );
                        }
                    }
                } else {
                    let _ = writeln!(out, "- **{}.{}**: {}", server, ns, data.description);
                }
            }
        }

        // Render tools with no namespace individually (always expanded).
        if !dotless_tools.is_empty() {
            let _ = write!(out, "## Other Tools\n\n");
            for (server, tool) in &dotless_tools {
                let desc = tool.description.as_deref().unwrap_or_default();
                let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
                let _ = write!(
                    out,
                    "- **{}** (on `{server}`): {}\n  Schema: {}\n",
                    tool.name, desc, schema,
                );
            }
        }

        // Intentional: expanded state is consumed after render.
        //
        // Caller must call expand_namespace() again for next format.
        self.expanded_namespaces.clear();
        self.header_context = out;
        &self.header_context
    }

    /// Build an extractor with all registered MCP and internal tools.
    fn build_extractor(&self) -> ExtractAction {
        let mut extractor = ExtractAction::new();
        for session in &self.sessions {
            for tool in &session.tools {
                extractor.add_tool(ToolSchema {
                    name: tool.name.to_string(),
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
                });
            }
        }
        for tool in &self.internal_tools {
            extractor.add_tool(ToolSchema {
                name: tool.name.clone(),
                input_schema: tool.input_schema.clone(),
            });
        }
        #[cfg(test)]
        for ts in &self.test_tools {
            extractor.add_tool(ts.clone());
        }
        extractor
    }

    /// Handle an internal tool call immediately, returning `true` if consumed.
    pub(crate) fn handle_internal_tool(&mut self, tc: &ToolCallData) -> bool {
        let Some(internal) = self.internal_tools.iter().find(|t| t.name == tc.name) else {
            return false;
        };
        match internal.name.as_str() {
            "expand_namespace" => {
                if let (Some(server), Some(ns)) = (
                    tc.arguments.get("server").and_then(|v| v.as_str()),
                    tc.arguments.get("namespace").and_then(|v| v.as_str()),
                ) {
                    self.expanded_namespaces
                        .insert((server.to_string(), ns.to_string()));
                }
                true
            }
            "stop_agent_loop" => {
                self.stop = true;
                true
            }
            _ => true,
        }
    }

    /// Process extracted items into a text response, routing tool calls.
    fn process_extraction(&mut self, raw: &str, extractor: &ExtractAction) -> String {
        let result = extractor.extract_batch(raw);
        let mut output = String::new();
        for item in result.items {
            match item {
                Item::Text(t) => output.push_str(&t),
                Item::ToolCall(tc) if !self.handle_internal_tool(&tc) => {
                    self.tool_issuer.push_back(tc);
                }
                _ => {}
            }
        }
        output
    }

    /// Build the final context by appending pending server responses to the header.
    fn build_chat_context(&mut self) -> String {
        let mut out = self.header_context.clone();
        if !self.server_response.is_empty() {
            let _ = write!(out, "\n## Tool Results\n\n");
            for (i, resp) in self.server_response.iter().enumerate() {
                let _ = writeln!(out, "### Result {}\n{}\n", i + 1, resp);
            }
            self.server_response.clear();
        }
        out
    }

    /// Send a chat completion and return the full response as a single string.
    ///
    /// Use this when streaming is not enabled — the model's reply is collected
    /// entirely and returned as `Result<String, String>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the connector call fails.
    pub async fn chat(&mut self, input: &str) -> Result<String, String> {
        #[cfg(test)]
        if let Some(ref response) = self.mock_chat_response.clone() {
            let raw = response.clone()?;
            return Ok(self.process_extraction(&raw, &self.build_extractor()));
        }

        let context = self.build_chat_context();

        let out = self
            .connector
            .chat_with_system(input, &context)
            .await
            .map_err(|e| e.to_string())?;

        Ok(self.process_extraction(out.message(), &self.build_extractor()))
    }

    /// Process a stream chunk through the extractor, routing tool calls and
    /// yielding text via the callback.
    fn process_stream_chunk(
        &mut self,
        token: &str,
        extractor: &mut ExtractAction,
        on_token: &mut dyn FnMut(&str),
    ) {
        match extractor.extract_stream(token) {
            StreamAction::Text(text) => on_token(&text),
            StreamAction::ToolCall(tc) if !self.handle_internal_tool(&tc) => {
                self.tool_issuer.push_back(tc);
            }
            _ => {}
        }
    }

    /// Stream a chat completion, calling `on_token` with each text delta.
    ///
    /// Use this when streaming is enabled — tokens are delivered in real time
    /// via the callback. Returns `Ok("done".into())` when the stream finishes.
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start or a chunk is malformed.
    pub async fn stream_chat(
        &mut self,
        input: &str,
        mut on_token: impl FnMut(&str),
    ) -> Result<String, String> {
        #[cfg(test)]
        if let Some(response) = self.mock_stream_response.clone() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.process_stream_chunk(token, &mut extractor, &mut on_token);
                    }
                    return Ok("done".into());
                }
                Err(msg) => return Err(msg),
            }
        }

        use tokio_stream::StreamExt;

        let context = self.build_chat_context();

        let mut stream = self
            .connector
            .stream_chat_with_system(input, &context)
            .await
            .map_err(|e| e.to_string())?;

        let mut extractor = self.build_extractor();

        while let Some(content) = stream.next().await {
            let chunk = content.map_err(|err| err.to_string())?;
            self.process_stream_chunk(chunk.token(), &mut extractor, &mut on_token);
        }

        Ok("done".into())
    }

    /// Dequeue and dispatch the next pending tool call to its MCP server.
    ///
    /// Returns the text content of the tool response on success.
    /// Returns an error if the queue is empty, the tool is not found,
    /// arguments are malformed, or the server call fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue is empty, the tool is unknown,
    /// arguments are not a JSON object, or the MCP server call fails.
    pub async fn dispatch_next(&mut self) -> Result<String, String> {
        // Peek at the front without consuming — item stays on failure.
        let (tool_name, args_map) = {
            let tc = self
                .tool_issuer
                .front()
                .ok_or_else(|| "no pending tool calls".to_string())?;

            let serde_json::Value::Object(ref args_map) = tc.arguments else {
                return Err("tool arguments must be a JSON object".to_string());
            };

            (tc.name.clone(), args_map.clone())
        };

        let idx = self
            .sessions
            .iter()
            .position(|s| s.tools.iter().any(|t| t.name == tool_name))
            .ok_or_else(|| format!("no server found for tool '{tool_name}'"))?;

        let params = CallToolRequestParams::new(tool_name).with_arguments(args_map);

        let result = self.sessions[idx]
            .client
            .call_tool(params)
            .await
            .map_err(|e| e.to_string())?;

        // Only remove on success — item stays queued for retry on error.
        self.tool_issuer.pop_front();

        let text: Vec<String> = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect();

        let text = text.join("\n");

        let ts = chrono::Local::now().format("%H:%M:%S");
        self.server_response.push(format!("[{}] {}", ts, text));

        Ok(text) // !?!
    }
}

#[cfg(test)]
impl Harness {
    /// Create a harness for testing without a real connector.
    pub(crate) fn new_test() -> Self {
        Self {
            connector: Connector::new("openai").unwrap(),
            sessions: Vec::new(),
            live_cache: HashMap::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            expanded_namespaces: HashSet::new(),
            internal_tools: default_internal_tools(),
            stop: false,
            server_response: Vec::new(),
            tool_issuer: VecDeque::new(),
            mock_chat_response: None,
            mock_stream_response: None,
            test_tools: Vec::new(),
        }
    }

    /// Register a tool schema for testing extraction without needing a real MCP session.
    pub(crate) fn with_test_tool(mut self, name: &str, schema: serde_json::Value) -> Self {
        self.test_tools.push(ToolSchema {
            name: name.to_string(),
            input_schema: schema,
        });
        self
    }

    /// Set a mock response for `chat()`. `Ok(text)` simulates a successful reply;
    /// `Err(msg)` simulates a connector failure.
    pub(crate) fn with_mock_chat(mut self, response: Result<&str, &str>) -> Self {
        self.mock_chat_response = Some(response.map(|s| s.to_string()).map_err(|s| s.to_string()));
        self
    }

    /// Set mock tokens for `stream_chat()`. `Ok(tokens)` simulates successful
    /// streaming; `Err(msg)` simulates a stream start failure.
    pub(crate) fn with_mock_stream(mut self, response: Result<Vec<&str>, &str>) -> Self {
        self.mock_stream_response = Some(
            response
                .map(|v| v.into_iter().map(|s| s.to_string()).collect())
                .map_err(|s| s.to_string()),
        );
        self
    }

    pub(crate) fn push_session(&mut self, session: ServerSession) {
        self.sessions.push(session);
    }

    pub(crate) fn set_live_cache(&mut self, cache: HashMap<String, HashMap<String, CacheData>>) {
        self.live_cache = cache;
    }

    pub(crate) fn expand_namespace(&mut self, server: &str, ns: &str) {
        self.expanded_namespaces
            .insert((server.to_string(), ns.to_string()));
    }

    pub(crate) fn push_tool_call(&mut self, tc: ToolCallData) {
        self.tool_issuer.push_back(tc);
    }
}
