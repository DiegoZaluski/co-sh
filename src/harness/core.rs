use super::correction_memory::CorrectionMemory;
use super::tools::{CoshTools, Tools};
use cosh_sdk::connector::{
    ChatMessage, Connector, ToolCallFunctionMsg, ToolCallMsg, ToolDefinition,
    assistant_tool_call_message, tool_result_message, user_message,
};
use cosh_sdk::extract_action::{ExtractAction, Item, StreamAction, ToolCallData, ToolSchema};
use cosh_tools::TOOL_FORMAT;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Ask,
}

pub struct ServerSession {
    pub name_server: String,
    pub tools: Vec<Tool>,
    pub client: RunningService<RoleClient, ()>,
}

pub const INSTRUCTIONS_BUILD: &str = concat!(
    "You are Cosh, an expert software engineering agent with access to external tools.\n\n",
    "## Behaviour\n",
    "- Respond directly to the user's request.\n",
    "- Do not introduce yourself unless the user explicitly asks who you are.\n",
    "- Do not volunteer information about your internal capabilities or available tools.\n",
    "- Use a tool only when it is required to produce or verify the requested result.\n",
    "- If the request can be completed correctly without using any tool, answer directly.\n",
    "- Avoid unnecessary narration or descriptions of obvious actions.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- When the current user request has been fully completed and no further action is required, invoke `stop_agent_loop`.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your response — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n\n",
    "Tool invocation is defined entirely by each tool specification.\n\n",
    "The available tools and their specifications are listed below.\n"
);

pub const INSTRUCTIONS_ASK: &str = concat!(
    "You are Cosh, a technical discussion and planning agent with access to read-only tools.\n\n",
    "## Behaviour\n",
    "- Respond directly to the user's request.\n",
    "- Do not introduce yourself unless the user explicitly asks who you are.\n",
    "- Your purpose is to discuss, explore, and plan technical work.\n",
    "- Help the user understand the codebase, clarify requirements, evaluate alternatives, and outline implementation strategies.\n",
    "- Do not modify code or perform write operations.\n",
    "- Ask clarifying questions whenever the user's intent is ambiguous or required information is missing.\n",
    "- Use a tool only when it is required to inspect code, documentation, or other relevant information needed to answer correctly.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- When the discussion has naturally concluded and no further exploration is required, invoke `stop_agent_loop`.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your discussion — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n\n",
    "Tool invocation is defined entirely by each tool specification.\n\n",
    "The available tools and their specifications are listed below.\n"
);

/// Default token budget for the context window.
const MAX_TOKENS: usize = 10_000;

/// Maximum consecutive tool-call failures before aborting the agent loop.
const MAX_TOOL_RETRIES: usize = 3;

/// Maximum total agent-loop iterations (tool calls + responses) before
/// the harness stops the loop as a safety net against runaway tool-calling.
const MAX_ITERATIONS: u64 = 20;

pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct HarnessTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A single turn in the structured conversation history with native
/// tool-call roles (`user`, `assistant`, `tool`).
#[derive(Debug, Clone)]
struct HistoryEntry {
    role: String,
    content: String,
    tool_calls: Option<Vec<ToolCallMsg>>,
    tool_call_id: Option<String>,
    token_count: usize,
}

fn default_harness_tools() -> Vec<HarnessTool> {
    vec![HarnessTool {
        name: "stop_agent_loop".into(),
        description: "Stop running the agent loop".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }]
}

#[allow(clippy::struct_field_names)]
pub struct Harness {
    connector: Connector,
    sessions: Vec<ServerSession>,
    protocol: Option<String>,
    header_context: String,
    system_prompts: Vec<PromptSystem>,
    harness_tools: Vec<HarnessTool>,
    cosh_tools: Option<CoshTools>,
    mode: Mode,
    /// Shared stop signal from the TUI, checked during streaming.
    stop_signal: Option<Arc<AtomicBool>>,
    /// To stop the agent loop.
    pub(crate) stop: bool,
    tool_issuer: VecDeque<ToolCallData>,
    /// Structured conversation history with native tool-call roles.
    /// This replaces the text-based history approach — each entry has
    /// a proper role (user, assistant, tool) so the model sees the
    /// native tool-call format it was trained on.
    history: Vec<HistoryEntry>,
    /// Total estimated tokens across all history entries, for budget management.
    total_history_tokens: usize,

    /// Tools explicitly disabled by the user via the Internal Tools screen.
    /// These are excluded from both the prompt header and the extractor.
    disabled_tools: HashSet<String>,

    /// How many consecutive tool calls have failed so far.
    /// Reset to 0 on the first successful dispatch.
    tool_failure_count: usize,

    /// How many tool calls failed extraction (invalid schema) in the current stream.
    /// Reset at the start of each [`stream_chat`](Self::stream_chat).
    tool_extraction_failure_count: usize,

    /// Raw JSON of the last failed tool call attempt, for correction feedback.
    last_failed_raw: String,

    /// Bounded, deduplicated memory of tool correction errors.
    /// Persists across agent loop iterations so the model never repeats the
    /// same mistake blindly.
    correction_memory: CorrectionMemory,

    #[cfg(test)]
    pub(crate) mock_chat_response: Option<Result<String, String>>,
    #[cfg(test)]
    pub(crate) mock_stream_response: Option<Result<Vec<String>, String>>,
    #[cfg(test)]
    pub(crate) test_tools: Vec<ToolSchema>,
}

impl Harness {
    #[must_use]
    pub fn new(connector: Connector, cwd: &str, disabled_tools: HashSet<String>) -> Self {
        Self {
            connector,
            sessions: Vec::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: Some(CoshTools::new(cwd)),
            mode: Mode::Build,
            stop_signal: None,
            stop: false,
            tool_issuer: VecDeque::new(),
            history: Vec::new(),
            total_history_tokens: 0,
            disabled_tools,
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            correction_memory: CorrectionMemory::new(5),
            #[cfg(test)]
            mock_chat_response: None,
            #[cfg(test)]
            mock_stream_response: None,
            #[cfg(test)]
            test_tools: Vec::new(),
        }
    }

    /// Load previous conversation turns into the history so the
    /// assistant sees context when it starts.
    #[must_use]
    pub fn with_history(mut self, turns: &[(String, String)]) -> Self {
        for (role, text) in turns {
            let count = crate::util::token_counter::estimate_tokens(text);
            let role_str = role.as_str();
            self.history.push(HistoryEntry {
                role: role_str.to_string(),
                content: text.clone(),
                tool_calls: None,
                tool_call_id: None,
                token_count: count,
            });
            self.total_history_tokens += count;
        }
        self
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

    #[must_use]
    pub const fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Inject the RAG database context into the `recall_search` tool description.
    ///
    /// The `suffix` describes which knowledge bases are available so the
    /// agent sees a tailored description. Must be called before
    /// [`format_header_context`](Self::format_header_context).
    #[cfg(feature = "embed")]
    pub fn set_recall_context(&mut self, suffix: String) {
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_recall_context(suffix);
        }
    }

    /// Set the RAG database registry for the `recall_search` dispatch.
    ///
    /// Each entry holds the connection URI, table name, and embedder config
    /// needed to embed a query and search the vector DB. Must be called
    /// before [`format_header_context`](Self::format_header_context).
    #[cfg(feature = "embed")]
    pub fn set_recall_dbs(&mut self, dbs: Vec<super::tools::RecallDb>) {
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_recall_dbs(dbs);
        }
    }

    /// Signal the agent loop to stop at the next safe opportunity.
    pub const fn request_stop(&mut self) {
        self.stop = true;
    }

    /// Check whether a stop has been requested.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stop
    }

    /// Reset the stop flag so the harness can be reused for a new cycle.
    pub const fn reset_stop(&mut self) {
        self.stop = false;
    }

    /// Check whether there are pending tool calls awaiting dispatch.
    #[must_use]
    pub fn has_pending_tools(&self) -> bool {
        !self.tool_issuer.is_empty()
    }

    /// How many tool calls are currently queued.
    #[must_use]
    pub fn pending_tool_count(&self) -> usize {
        self.tool_issuer.len()
    }

    /// Builds the system header for the LLM.
    ///
    /// Concatenates mode-specific instructions, system prompts, harness tools, system tools,
    /// and MCP server tools — all rendered inline with full name, description, and input schema.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        let instructions = match self.mode {
            Mode::Build => INSTRUCTIONS_BUILD,
            Mode::Ask => INSTRUCTIONS_ASK,
        };
        let _ = write!(out, "{instructions}");
        let _ = write!(out, "{TOOL_FORMAT}");
        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
        }

        let _ = write!(out, "## Harness Tools\n\n");
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
            let _ = write!(
                out,
                "### {}\n{}\nSchema: {}\n\n",
                tool.name, tool.description, schema,
            );
        }
        if let Some(ref cosh) = self.cosh_tools {
            let _ = write!(out, "## System Tools\n\n");
            match self.mode {
                Mode::Build => cosh.write_tool_descriptions_enabled(&mut out, &self.disabled_tools),
                Mode::Ask => cosh.write_tool_descriptions_filtered(&mut out, &self.disabled_tools),
            }
        }

        for session in &self.sessions {
            let _ = write!(out, "## MCP Server: {}\n\n", session.name_server);
            for tool in &session.tools {
                let desc = tool.description.as_deref().unwrap_or_default();
                let schema = serde_json::to_string_pretty(&*tool.input_schema).unwrap_or_default();
                let _ = write!(
                    out,
                    "- **{name}**: {desc}\n  Schema: {schema}\n",
                    name = tool.name
                );
            }
        }

        self.header_context = out;
        &self.header_context
    }

    /// Build an extractor with all registered MCP, cosh, and internal tools.
    fn build_extractor(&self) -> ExtractAction {
        log::debug!("build_extractor: sessions={}", self.sessions.len());
        let mut extractor = ExtractAction::new();
        for session in &self.sessions {
            for tool in &session.tools {
                extractor.add_tool(ToolSchema {
                    name: tool.name.to_string(),
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
                });
            }
        }
        if let Some(ref cosh) = self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build => cosh.schemas_enabled(&self.disabled_tools),
                Mode::Ask => cosh.schemas_filtered(&self.disabled_tools),
            };
            log::debug!(
                "build_extractor: cosh.schemas() returned {} tools",
                schemas.len()
            );
            for schema in schemas {
                extractor.add_tool(schema);
            }
        }
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
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

    /// Handle a harness tool call immediately, returning `true` if consumed.
    pub(crate) fn handle_harness_tool(&mut self, tc: &ToolCallData) -> bool {
        let Some(tool) = self.harness_tools.iter().find(|t| t.name == tc.name) else {
            return false;
        };
        if tool.name == "stop_agent_loop" {
            self.stop = true;
        }
        true
    }

    /// Process extracted items into a text response, routing tool calls.
    fn process_extraction(&mut self, raw: &str, extractor: &mut ExtractAction) -> String {
        let result = extractor.extract_batch(raw);
        let mut output = String::new();
        for item in result.items {
            match item {
                Item::Text(t) => output.push_str(&t),
                Item::ToolCall(tc) if !self.handle_harness_tool(&tc) => {
                    self.tool_issuer.push_back(tc);
                }
                Item::ToolCall(_) => {}
            }
        }
        let extraction_failures = extractor.take_tool_failures();
        self.tool_extraction_failure_count += extraction_failures;
        output
    }

    /// Build the system context for the LLM.
    ///
    /// Returns only the header context (instructions + tool definitions)
    /// and correction memory. Conversation history is NOT included here —
    /// it is sent as separate messages with proper roles (user, assistant
    /// with tool_calls, tool with tool_call_id) via
    /// [`stream_chat_with_messages`](Self::stream_chat_with_messages).
    fn build_chat_context(&mut self) -> String {
        let mut out = self.header_context.clone();
        let correction = self.correction_memory.format();
        out.push_str(&correction);
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
            let mut extractor = self.build_extractor();
            return Ok(self.process_extraction(&raw, &mut extractor));
        }

        let context = self.build_chat_context();

        let out = self
            .connector
            .chat_with_system(input, &context)
            .await
            .map_err(|e| e.to_string())?;

        let mut extractor = self.build_extractor();
        let result = self.process_extraction(out.message(), &mut extractor);
        self.last_failed_raw = extractor.take_last_failed_raw();
        Ok(result)
    }

    /// Stream a chat completion using a proper messages array (native tool-call
    /// format). This is the replacement for the old text-based
    /// [`stream_chat`](Self::stream_chat) when using the native tool API.
    ///
    /// The `system` parameter is the system context (instructions + tool
    /// definitions, no history). The `messages` array contains the
    /// conversation history with roles (user, assistant with `tool_calls`,
    /// tool with `tool_call_id`).
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start.
    pub async fn stream_chat_with_messages(
        &mut self,
        system: &str,
        messages: &[ChatMessage],
        mut on_token: impl FnMut(&str),
    ) -> Result<String, String> {
        #[cfg(test)]
        if let Some(response) = self.mock_stream_response.take() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.process_stream_chunk(token, &mut extractor, &mut on_token);
                    }
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        log::debug!("stream_chat_with_messages messages={}", messages.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_messages(system, messages) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_chat_with_messages CONNECTOR_ERR={e}");
                        return Err(e.to_string());
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                log::debug!("stream_chat_with_messages STOPPED during connect");
                return Err("Interrupted by user".to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    log::debug!("stream_chat_with_messages STOPPED by signal");
                    break;
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        log::debug!("stream_chat_with_messages STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            if token_count <= 5
                || token_count.is_multiple_of(100)
                || !token.is_empty()
                || fr.is_some()
            {
                log::debug!(
                    "stream_chat_with_messages token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            self.process_stream_chunk(token, &mut extractor, &mut on_token);
        }

        self.last_failed_raw = extractor.take_last_failed_raw();

        log::debug!("stream_chat_with_messages DONE total_tokens={token_count}");
        Ok("done".into())
    }

    /// Process a stream chunk through the extractor, routing tool calls and
    /// yielding text via the callback.
    fn process_stream_chunk(
        &mut self,
        token: &str,
        extractor: &mut ExtractAction,
        on_token: &mut dyn FnMut(&str),
    ) {
        let action = extractor.extract_stream(token);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_token(&text);
            }
            StreamAction::ToolCall(tc) if !self.handle_harness_tool(&tc) => {
                self.tool_issuer.push_back(tc);
            }
            StreamAction::ToolCall(_) | StreamAction::Pending => {
                // harness tool consumed
            }
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
        if let Some(response) = self.mock_stream_response.take() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.process_stream_chunk(token, &mut extractor, &mut on_token);
                    }
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        let context = self.build_chat_context();
        log::debug!("stream_chat context_len={}", context.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_system(input, &context) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_chat CONNECTOR_ERR={e}");
                        return Err(e.to_string());
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                log::debug!("stream_chat STOPPED during connect");
                return Err("Interrupted by user".to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    log::debug!("stream_chat STOPPED by signal");
                    break;
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        log::debug!("stream_chat STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            if token_count <= 5
                || token_count.is_multiple_of(100)
                || !token.is_empty()
                || fr.is_some()
            {
                log::debug!(
                    "stream_chat token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            self.process_stream_chunk(token, &mut extractor, &mut on_token);
        }

        // Capture the last failed tool call raw JSON for correction feedback
        self.last_failed_raw = extractor.take_last_failed_raw();

        log::debug!("stream_chat DONE total_tokens={token_count}");
        Ok("done".into())
    }

    // Populate the connector's native tool definitions once.
    fn init_native_tools(&mut self) {
        use cosh_sdk::connector::ToolFunction as TFunc;

        let mut defs: Vec<ToolDefinition> = Vec::new();

        // Harness tools (e.g. stop_agent_loop)
        for tool in &self.harness_tools {
            if self.disabled_tools.contains(&tool.name) {
                continue;
            }
            defs.push(ToolDefinition::new(
                TFunc::new(&tool.name)
                    .with_description(&tool.description)
                    .with_parameters(tool.input_schema.clone()),
            ));
        }

        // Cosh tools (bash_run, fs_read, etc.)
        if let Some(ref cosh) = self.cosh_tools {
            for desc in cosh.tool_descriptions() {
                let name = desc["name"].as_str().unwrap_or_default();
                if self.disabled_tools.contains(name) {
                    continue;
                }
                let description = desc["description"].as_str().unwrap_or_default();
                if let Some(input_schema) = desc.get("inputSchema").cloned() {
                    defs.push(ToolDefinition::new(
                        TFunc::new(name)
                            .with_description(description)
                            .with_parameters(input_schema),
                    ));
                }
            }
        }

        // MCP server tools
        for session in &self.sessions {
            for tool in &session.tools {
                let name: &str = tool.name.as_ref();
                if self.disabled_tools.contains(name) {
                    continue;
                }
                let description = tool.description.as_deref().unwrap_or_default();
                let input_schema = (*tool.input_schema).clone();
                defs.push(ToolDefinition::new(
                    TFunc::new(name)
                        .with_description(description)
                        .with_parameters(serde_json::Value::Object(input_schema)),
                ));
            }
        }

        self.connector.set_tools(defs);
    }

    /// Build the conversation messages array for the current iteration.
    ///
    /// The array includes all history entries with their proper roles
    /// (user, assistant with tool_calls, tool with tool_call_id) plus
    /// the current user input as the final message.
    ///
    /// Leading `tool` messages (which can be left behind after history
    /// eviction) are skipped because some providers (notably Mistral)
    /// reject `tool` right after `system`.
    fn build_conversation_messages(&self, current_input: &str) -> Vec<ChatMessage> {
        let mut messages: Vec<ChatMessage> = Vec::new();

        // Skip leading tool messages — they have no preceding assistant
        // message, and some providers (Mistral) reject the sequence
        // system → tool → ... with HTTP 400.
        let history = self.history.iter().skip_while(|entry| entry.role == "tool");

        for entry in history {
            match entry.role.as_str() {
                "assistant" if entry.tool_calls.is_some() => {
                    messages.push(assistant_tool_call_message(
                        entry.tool_calls.clone().unwrap(),
                    ));
                }
                "tool" => {
                    messages.push(tool_result_message(
                        &entry.tool_call_id.clone().unwrap_or_default(),
                        &entry.content,
                    ));
                }
                _ => {
                    messages.push(ChatMessage {
                        role: entry.role.clone(),
                        content: Some(entry.content.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
        }

        if !current_input.is_empty() {
            messages.push(user_message(current_input));
        }

        messages
    }

    /// Add a tool call + its result to the history.
    fn push_tool_history(&mut self, id: &str, name: &str, args: &serde_json::Value, result: &str) {
        let args_str = serde_json::to_string(args).unwrap_or_default();

        // Estimate tokens for the pair (before moving args_str)
        let assistant_tokens =
            crate::util::token_counter::estimate_tokens(&format!("{name} {args_str}"));

        let tc_msg = ToolCallMsg {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ToolCallFunctionMsg {
                name: name.to_string(),
                arguments: args_str,
            },
        };
        let result_tokens = crate::util::token_counter::estimate_tokens(result);

        self.history.push(HistoryEntry {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(vec![tc_msg]),
            tool_call_id: None,
            token_count: assistant_tokens,
        });
        self.total_history_tokens += assistant_tokens;

        self.history.push(HistoryEntry {
            role: "tool".to_string(),
            content: result.to_string(),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
            token_count: result_tokens,
        });
        self.total_history_tokens += result_tokens;

        self.evict_history_if_needed();
    }

    /// Evict the oldest history entries when the total token budget is exceeded.
    fn evict_history_if_needed(&mut self) {
        while self.total_history_tokens >= MAX_TOKENS && self.history.len() > 1 {
            if let Some(evicted) = self.history.first() {
                self.total_history_tokens = self
                    .total_history_tokens
                    .saturating_sub(evicted.token_count);
            }
            self.history.remove(0);
        }
    }

    /// Run the full agent loop: stream LLM response, dispatch tool calls,
    /// feed results back to the LLM, and repeat — until the model finishes
    /// without requesting tools, [`request_stop`](Self::request_stop) is called,
    /// or `stop_signal` is set to `true`.
    ///
    /// Events are sent through `tx` so the caller (typically the TUI) can
    /// render tokens, tool calls, and results in real time.
    ///
    /// When the LLM calls `ask_questions`, the tool is intercepted: the
    /// questions are sent to the TUI via `QuestionRequest` and the loop
    /// waits for an answer on `answer_rx`. The TUI must send back the
    /// user's answers (or an error) before the loop continues.
    ///
    /// The `stop_signal` is an external flag (usually an `Arc<AtomicBool>`)
    /// that allows the caller to interrupt the loop from another thread.
    #[allow(clippy::too_many_lines)]
    pub async fn run_agent_loop(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        mut answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>,
        >,
        stop_signal: Arc<AtomicBool>,
    ) {
        use super::events::HarnessEvent;
        use cosh_tools::question::types::{QuestionInput, QuestionOutput};

        let mut current_input = input.to_string();
        let mut iteration = 0u64;

        // Store the stop signal so stream_chat can check it mid-stream.
        self.stop_signal = Some(stop_signal.clone());

        // Pass the event tx to CoshTools for streaming tool output (e.g. bash)
        if let Some(ref mut cosh) = self.cosh_tools {
            cosh.set_event_tx(tx.clone());
        }

        log::debug!(
            "run_agent_loop ENTER input={:?}",
            &input[..input.floor_char_boundary(input.len().min(80))]
        );

        // Populate native tool definitions for the API so the model
        // uses the native tool-calling mechanism instead of inline JSON.
        self.init_native_tools();

        // Add the initial user input to structured history so the model
        // sees it as a proper `role: "user"` message in the conversation.
        let input_tokens = crate::util::token_counter::estimate_tokens(input);
        self.history.push(HistoryEntry {
            role: "user".to_string(),
            content: input.to_string(),
            tool_calls: None,
            tool_call_id: None,
            token_count: input_tokens,
        });
        self.total_history_tokens = input_tokens;

        macro_rules! check_stop {
            () => {
                if self.stop || stop_signal.load(Ordering::Relaxed) {
                    log::debug!("run_agent_loop STOPPED");
                    let _ = tx.send(HarnessEvent::Stopped);
                    true
                } else {
                    false
                }
            };
        }

        loop {
            iteration += 1;
            log::debug!(
                "run_agent_loop ITERATION={} input_len={} pending_tools={}",
                iteration,
                current_input.len(),
                self.tool_issuer.len()
            );

            if check_stop!() {
                break;
            }

            // Phase 1: stream the LLM response using native tool-call format
            log::debug!("run_agent_loop PHASE1_START iteration={iteration}");
            let messages = self.build_conversation_messages(&current_input);
            let system_context = self.build_chat_context();
            let result = self
                .stream_chat_with_messages(&system_context, &messages, |token| {
                    let _ = tx.send(HarnessEvent::Token {
                        text: token.to_string(),
                    });
                })
                .await;
            log::debug!(
                "run_agent_loop PHASE1_END iteration={} result_ok={}",
                iteration,
                result.is_ok()
            );

            // Capture extraction failures from this stream and reset for next.
            let extraction_failures = self.tool_extraction_failure_count;
            self.tool_extraction_failure_count = 0;

            if let Err(e) = result {
                log::debug!("run_agent_loop PHASE1_ERR={e}");
                let _ = tx.send(HarnessEvent::Error(e));
                break;
            }

            // Record extraction failures in the correction memory so the model
            // sees the pattern even when no specific dispatch error is available.
            if extraction_failures > 0 {
                let msg = if self.last_failed_raw.is_empty() {
                    "Invalid JSON tool call — no registered schema matched".to_string()
                } else {
                    format!(
                        "Tool call failed — you sent: {}\nThe JSON did not match any registered tool schema.\nFollow the Tool format and Schema definition above exactly.",
                        self.last_failed_raw
                    )
                };
                self.correction_memory.push(&msg);
            }

            if check_stop!() {
                break;
            }

            // Phase 2: dispatch all pending tool calls
            let had_tools = self.has_pending_tools();
            log::debug!("run_agent_loop PHASE2 had_tools={had_tools}");

            while self.has_pending_tools() {
                // Safety: stop the loop if we've exceeded the maximum
                // number of iterations. This prevents runaway tool-calling
                // when the model fails to recognise that its request is
                // complete.
                if iteration >= MAX_ITERATIONS {
                    log::debug!("run_agent_loop MAX_ITERATIONS={MAX_ITERATIONS} reached");
                    let _ = tx.send(HarnessEvent::Done);
                    self.stop = true;
                    break;
                }

                if check_stop!() {
                    break;
                }

                // Peek at tool info before consuming the item
                let info = self
                    .tool_issuer
                    .front()
                    .map(|tc| (tc.id.clone(), tc.name.clone(), tc.arguments.clone()));

                if let Some((ref _call_id, ref name, ref args)) = info {
                    log::debug!("run_agent_loop DISPATCH tool={name}");
                    let _ = tx.send(HarnessEvent::ToolCall {
                        tool: name.clone(),
                        input: args.clone(),
                    });
                }

                // Intercept `ask_questions` — send to TUI, wait for user answer
                if info.as_ref().is_some_and(|(_, n, _)| n == "ask_questions") {
                    log::debug!("run_agent_loop ASK_QUESTIONS intercepted");

                    // Clone info before consuming in .map() below
                    let info_clone = info.clone();
                    let input: Result<QuestionInput, String> = info_clone
                        .map(|(_, _, args)| args)
                        .ok_or_else(|| "missing tool arguments".to_string())
                        .and_then(|args| serde_json::from_value(args).map_err(|e| e.to_string()));

                    self.tool_issuer.pop_front();

                    match input {
                        Ok(q_input) => {
                            let _ = tx.send(HarnessEvent::QuestionRequest {
                                questions: q_input.questions.clone(),
                            });

                            log::debug!("run_agent_loop WAITING for answer_rx");
                            // Wait for the TUI to send back answers
                            let answer = answer_rx.recv().await;
                            log::debug!("run_agent_loop GOT answer={:?}", answer.is_some());
                            match answer {
                                Some(Ok(answers)) => {
                                    let output = QuestionOutput {
                                        questions: q_input.questions,
                                        answers,
                                    };
                                    let json = serde_json::to_string(&output)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    // Save to structured history (native tool format)
                                    if let Some((ref call_id, ref name, ref args)) = info {
                                        let tool_id = if call_id.is_empty() {
                                            format!("call_{:016x}", iteration)
                                        } else {
                                            call_id.clone()
                                        };
                                        self.push_tool_history(&tool_id, name, args, &json);
                                    }
                                    let _ = tx.send(HarnessEvent::ToolResult { output: json });
                                }
                                Some(Err(e)) => {
                                    let _ = tx.send(HarnessEvent::ToolError { error: e });
                                }
                                None => {
                                    log::debug!("run_agent_loop answer_rx CLOSED");
                                    let _ = tx.send(HarnessEvent::ToolError {
                                        error: "Internal error: question channel closed"
                                            .to_string(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                        }
                    }
                } else {
                    // Normal dispatch for all other tools
                    log::debug!("run_agent_loop dispatch_next start");

                    /// Outcome of a tool dispatch, possibly interrupted by stop.
                    #[derive(Debug)]
                    enum DispatchOut {
                        Ok(String),
                        Err(String),
                        Stopped,
                    }

                    // Scope dispatch_fut tightly so its &mut self borrow is
                    // released BEFORE we match on the result below.
                    let dispatch_out = {
                        let dispatch_fut = self.dispatch_next();
                        tokio::pin!(dispatch_fut);

                        tokio::select! {
                            result = &mut dispatch_fut => match result {
                                Ok(output) => DispatchOut::Ok(output),
                                Err(e) => DispatchOut::Err(e),
                            },
                            _ = async {
                                loop {
                                    if stop_signal.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(
                                        50,
                                    ))
                                    .await;
                                }
                            } => DispatchOut::Stopped,
                        }
                        // dispatch_fut dropped here → &mut self released
                    };

                    match dispatch_out {
                        DispatchOut::Ok(output) => {
                            self.tool_failure_count = 0;
                            log::debug!("run_agent_loop dispatch_next OK len={}", output.len());
                            // Record the tool call + result in native history
                            if let Some((ref call_id, ref name, ref args)) = info {
                                let tool_id = if call_id.is_empty() {
                                    // Generate a synthetic ID for inline tool calls
                                    format!("call_{:016x}", iteration)
                                } else {
                                    call_id.clone()
                                };
                                self.push_tool_history(&tool_id, name, args, &output);
                            }
                            let _ = tx.send(HarnessEvent::ToolResult { output });
                        }
                        DispatchOut::Err(e) => {
                            self.tool_failure_count += 1;
                            self.correction_memory.push(&format!(
                                "Tool `{}` failed: {}",
                                info.as_ref().map_or("?", |(_, n, _)| n),
                                e,
                            ));
                            log::debug!(
                                "run_agent_loop dispatch_next ERR={e} \
                                 (failure #{}/{MAX_TOOL_RETRIES})",
                                self.tool_failure_count,
                            );
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call \
                                     failures. Agent loop interrupted."
                                );
                                let _ = tx.send(HarnessEvent::Error(msg));
                                self.stop = true;
                                break;
                            }
                        }
                        DispatchOut::Stopped => {
                            log::debug!("run_agent_loop dispatch_next STOPPED by user");
                            self.stop = true;
                            let _ = tx.send(HarnessEvent::Stopped);
                            return; // Exit run_agent_loop entirely
                        }
                    }
                }
            }

            if check_stop!() {
                break;
            }

            if !had_tools {
                if extraction_failures > 0 {
                    // Model tried but all tool calls failed validation.
                    // Give it another chance with the correction prompt.
                    log::debug!("run_agent_loop RETRY (extraction failures)");
                    current_input.clear();
                    continue;
                }
                // No tools and no extraction failures — conversation is complete
                log::debug!("run_agent_loop DONE (no tools)");
                let _ = tx.send(HarnessEvent::Done);
                break;
            }

            log::debug!("run_agent_loop RESTARTING with tool results");
            // The tool results have already been recorded in `self.history`
            // via `push_tool_history` during dispatch, so we don't need to
            // summarise them here. Just set the continuation prompt for the
            // next iteration.
            current_input =
                "Please continue with your response based on the information above.".to_string();
        }
        log::debug!("run_agent_loop EXIT");
    }

    /// Dequeue and dispatch the next pending tool call through the
    /// three-tier dispatch: cosh tools → MCP servers.
    ///
    /// Returns the text content of the tool response on success.
    /// Returns an error if the queue is empty, the tool is not found,
    /// arguments are malformed, or the execution fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue is empty, the tool is unknown,
    /// arguments are not a JSON object, or the execution fails.
    pub async fn dispatch_next(&mut self) -> Result<String, String> {
        let (tool_name, args_map) = {
            let tc = self
                .tool_issuer
                .front()
                .ok_or_else(|| "no pending tool calls".to_string())?;

            let serde_json::Value::Object(ref args_map) = tc.arguments else {
                self.tool_issuer.pop_front();
                return Err("tool arguments must be a JSON object".to_string());
            };

            (tc.name.clone(), args_map.clone())
        };

        // Tier 1: cosh tools
        if let Some(ref cosh) = self.cosh_tools {
            let args = serde_json::Value::Object(args_map.clone());
            match cosh.dispatch(&tool_name, args).await {
                Ok(result) => {
                    self.tool_issuer.pop_front();
                    return Ok(result);
                }
                Err(err) if err.starts_with("unknown cosh tool") => {}
                Err(err) => {
                    self.tool_issuer.pop_front();
                    return Err(err);
                }
            }
        }

        // Tier 2: MCP sessions
        let idx = self
            .sessions
            .iter()
            .position(|s| s.tools.iter().any(|t| t.name == tool_name));

        let Some(idx) = idx else {
            self.tool_issuer.pop_front();
            return Err(format!("no server found for tool '{tool_name}'"));
        };

        self.tool_issuer.pop_front();
        let params = CallToolRequestParams::new(tool_name).with_arguments(args_map);

        let result = self.sessions[idx]
            .client
            .call_tool(params)
            .await
            .map_err(|e| e.to_string())?;

        let text: Vec<String> = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect();

        let text = text.join("\n");

        Ok(text)
    }
}

#[cfg(test)]
impl Harness {
    /// Create a harness for testing without a real connector.
    pub(crate) fn new_test() -> Self {
        Self {
            connector: Connector::new("openai").unwrap(),
            sessions: Vec::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: None,
            mode: Mode::Build,
            stop_signal: None,
            stop: false,
            tool_issuer: VecDeque::new(),
            history: Vec::new(),
            total_history_tokens: 0,
            disabled_tools: HashSet::new(),
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            last_failed_raw: String::new(),
            correction_memory: CorrectionMemory::new(5),
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

    pub(crate) fn push_tool_call(&mut self, tc: ToolCallData) {
        self.tool_issuer.push_back(tc);
    }
}
