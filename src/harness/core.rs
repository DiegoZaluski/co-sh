use super::correction_memory::CorrectionMemory;
use super::tools::{CoshTools, Tools};
use cosh_recall::window::ContextWindow;
use cosh_sdk::connector::Connector;
use cosh_sdk::extract_action::{ExtractAction, Item, StreamAction, ToolCallData, ToolSchema};
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
    "You are an expert software engineering agent with access to tools.\n\n",
    "## Behaviour\n",
    "- **Answer naturally.** If someone says \"hello\", just greet them back. ",
    "Do not list tools or describe your capabilities.\n",
    "- **Use tools only when necessary.** If you already know the answer, answer directly.\n",
    "- **Be concise.** Skip narration of obvious actions.\n",
    "- When done, call `stop_agent_loop`.\n\n",
    "## Tool format\n",
    "To call a tool, respond with a JSON object:\n",
    "{\"name\": \"tool_name\", \"arguments\": { ... }}\n\n",
    "The available tools and their schemas are listed below.\n"
);

pub const INSTRUCTIONS_ASK: &str = concat!(
    "You are a technical discussion and planning agent with access to read-only tools.\n\n",
    "## Behaviour\n",
    "- **Your role is to discuss, explore, and plan.** You help the user understand their ",
    "codebase, clarify requirements, and outline implementation strategies.\n",
    "- **Do not make changes.** You cannot edit, write, or run code.\n",
    "- **Be conversational.** Ask clarifying questions to understand the user's intent.\n",
    "- **Use tools to explore.** Read files, search code, fetch documentation, and research ",
    "before answering.\n",
    "- When the user is satisfied with the plan, call `stop_agent_loop` to end the session.\n\n",
    "## Tool format\n",
    "To call a tool, respond with a JSON object:\n",
    "{\"name\": \"tool_name\", \"arguments\": { ... }}\n\n",
    "The available tools and their schemas are listed below.\n"
);

/// Default token budget for the context window.
const MAX_TOKENS: usize = 10_000;

/// Maximum consecutive tool-call failures before aborting the agent loop.
const MAX_TOOL_RETRIES: usize = 3;

pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct HarnessTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
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
    server_response: Vec<String>,
    /// Context window for iterative agent sessions.
    context_window: ContextWindow,

    /// Tools explicitly disabled by the user via the Internal Tools screen.
    /// These are excluded from both the prompt header and the extractor.
    disabled_tools: HashSet<String>,

    /// How many consecutive tool calls have failed so far.
    /// Reset to 0 on the first successful dispatch.
    tool_failure_count: usize,

    /// How many tool calls failed extraction (invalid schema) in the current stream.
    /// Reset at the start of each [`stream_chat`](Self::stream_chat).
    tool_extraction_failure_count: usize,

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
            server_response: Vec::new(),
            context_window: ContextWindow::new(MAX_TOKENS),
            disabled_tools,
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
            correction_memory: CorrectionMemory::new(5),
            #[cfg(test)]
            mock_chat_response: None,
            #[cfg(test)]
            mock_stream_response: None,
            #[cfg(test)]
            test_tools: Vec::new(),
        }
    }

    /// Load previous conversation turns into the context window so the
    /// assistant sees history when it starts.
    #[must_use]
    pub fn with_history(mut self, turns: &[(String, String)]) -> Self {
        for (role, text) in turns {
            let line = if role == "user" {
                format!("## User\n{text}")
            } else {
                format!("## Assistant\n{text}")
            };
            let count = crate::util::token_counter::estimate_tokens(&line);
            self.context_window.push(line, count);
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

    /// Build the final context by appending pending server responses and
    /// the context window to the header.
    fn build_chat_context(&mut self) -> String {
        let mut out = self.header_context.clone();
        if !self.server_response.is_empty() {
            let _ = write!(out, "\n## Tool Results\n\n");
            for (i, resp) in self.server_response.iter().enumerate() {
                let _ = writeln!(out, "### Result {}\n{}\n", i + 1, resp);
            }
            self.server_response.clear();
        }
        out.push_str(&self.context_window.format_context());
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
        Ok(self.process_extraction(out.message(), &mut extractor))
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

        let mut stream = match self
            .connector
            .stream_chat_with_system(input, &context)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                log::debug!("stream_chat CONNECTOR_ERR={e}");
                return Err(e.to_string());
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

        log::debug!("stream_chat DONE total_tokens={token_count}");
        Ok("done".into())
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

        log::debug!(
            "run_agent_loop ENTER input={:?}",
            &input[..input.floor_char_boundary(input.len().min(80))]
        );

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
                "run_agent_loop ITERATION={} input_len={} pending_tools={} server_responses={}",
                iteration,
                current_input.len(),
                self.tool_issuer.len(),
                self.server_response.len()
            );

            if check_stop!() {
                break;
            }

            // Phase 1: stream the LLM response
            log::debug!("run_agent_loop PHASE1_START iteration={iteration}");
            let result = self
                .stream_chat(&current_input, |token| {
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
                self.correction_memory.push(
                    "Invalid JSON tool call — no registered schema matched",
                );
            }

            if check_stop!() {
                break;
            }

            // Phase 2: dispatch all pending tool calls
            let had_tools = self.has_pending_tools();
            log::debug!("run_agent_loop PHASE2 had_tools={had_tools}");

            while self.has_pending_tools() {
                if check_stop!() {
                    break;
                }

                // Peek at tool info before consuming the item
                let info = self
                    .tool_issuer
                    .front()
                    .map(|tc| (tc.name.clone(), tc.arguments.clone()));

                if let Some((ref name, ref args)) = info {
                    log::debug!("run_agent_loop DISPATCH tool={name}");
                    let _ = tx.send(HarnessEvent::ToolCall {
                        tool: name.clone(),
                        input: args.clone(),
                    });
                }

                // Intercept `ask_questions` — send to TUI, wait for user answer
                if info.as_ref().is_some_and(|(n, _)| n == "ask_questions") {
                    log::debug!("run_agent_loop ASK_QUESTIONS intercepted");

                    let input: Result<QuestionInput, String> = info
                        .map(|(_, args)| args)
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
                                    let ts = chrono::Local::now().format("%H:%M:%S");
                                    self.server_response.push(format!("[{ts}] {json}"));
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
                    match self.dispatch_next().await {
                        Ok(output) => {
                            self.tool_failure_count = 0;
                            log::debug!("run_agent_loop dispatch_next OK len={}", output.len());
                            let _ = tx.send(HarnessEvent::ToolResult { output });
                        }
                        Err(e) => {
                            self.tool_failure_count += 1;
                            self.correction_memory.push(&format!(
                                "Tool `{}` failed: {}",
                                info.as_ref().map_or("?", |(n, _)| n),
                                e,
                            ));
                            log::debug!(
                                "run_agent_loop dispatch_next ERR={e} (failure #{}/{MAX_TOOL_RETRIES})",
                                self.tool_failure_count,
                            );
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                            if self.tool_failure_count >= MAX_TOOL_RETRIES {
                                let msg = format!(
                                    "{MAX_TOOL_RETRIES} consecutive tool call failures. \
                                     Agent loop interrupted."
                                );
                                let _ = tx.send(HarnessEvent::Error(msg));
                                self.stop = true;
                                break;
                            }
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
            // Summarize this iteration into the context window
            let summary = self.server_response.join("; ");
            let token_count = crate::util::token_counter::estimate_tokens(&summary);
            self.context_window.push(summary, token_count);
            // Tool results were accumulated in server_response
            // Next iteration sends an empty prompt with results in context
            current_input.clear();
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
                    let ts = chrono::Local::now().format("%H:%M:%S");
                    self.server_response.push(format!("[{ts}] {result}"));
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

        let ts = chrono::Local::now().format("%H:%M:%S");
        self.server_response.push(format!("[{ts}] {text}"));

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
            server_response: Vec::new(),
            tool_issuer: VecDeque::new(),
            context_window: ContextWindow::new(MAX_TOKENS),
            disabled_tools: HashSet::new(),
            tool_failure_count: 0,
            tool_extraction_failure_count: 0,
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
