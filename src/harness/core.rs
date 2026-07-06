use super::tools::{CoshTools, Tools};
use cosh_sdk::connector::Connector;
use cosh_sdk::extract_action::{ExtractAction, Item, StreamAction, ToolCallData, ToolSchema};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct ServerSession {
    pub name_server: String,
    pub tools: Vec<Tool>,
    pub client: RunningService<RoleClient, ()>,
}

pub const INSTRUCTIONS: &str = concat!(
    "You are an expert software engineering agent. ",
    "You solve problems step by step using the tools below.\n\n",

    "## Workflow\n",
    "1. **Understand** — read files, search code, explore the project.\n",
    "2. **Plan** — think before you act.\n",
    "3. **Execute** — call the right tools.\n",
    "4. **Verify** — check compilation and tests pass.\n\n",

    "## Tool calls\n",
    "Respond with a JSON object:\n",
    "{\"name\": \"tool_name\", \"arguments\": { ... }}\n\n",
    "Tool schemas are listed below with name, description, and input schema. ",
    "The description tells you what the tool does and when to use it.\n\n",

    "## Rules\n",
    "- **`fs_edit` over `fs_write`**: targeted edits are safer than full rewrites.\n",
    "- **External calls last**: only use `web_*` tools when the answer is not in the codebase.\n",
    "- **Batch questions**: one `ask_questions` call, never split.\n",
    "- **No unnecessary calls**: don't call a tool if you already have the answer.\n",
    "- **Stop when done**: call `stop_agent_loop` when the task is complete.\n\n",

    "## Output\n",
    "- Explain what you're doing before each step, and what happened after.\n",
    "- Be concise. Skip narration of obvious actions.\n",
    "- When finished, summarize what was done.\n"
);

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
    /// To stop the agent loop.
    pub(crate) stop: bool,
    tool_issuer: VecDeque<ToolCallData>,
    server_response: Vec<String>,

    #[cfg(test)]
    pub(crate) mock_chat_response: Option<Result<String, String>>,
    #[cfg(test)]
    pub(crate) mock_stream_response: Option<Result<Vec<String>, String>>,
    #[cfg(test)]
    pub(crate) test_tools: Vec<ToolSchema>,
}

impl Harness {
    #[must_use]
    pub fn new(connector: Connector, cwd: &str) -> Self {
        Self {
            connector,
            sessions: Vec::new(),
            protocol: None,
            header_context: String::new(),
            system_prompts: Vec::new(),
            harness_tools: default_harness_tools(),
            cosh_tools: Some(CoshTools::new(cwd)),
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

    /// Signal the agent loop to stop at the next safe opportunity.
    pub fn request_stop(&mut self) {
        self.stop = true;
    }

    /// Check whether a stop has been requested.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stop
    }

    /// Reset the stop flag so the harness can be reused for a new cycle.
    pub fn reset_stop(&mut self) {
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
    /// Concatenates system prompts, harness tools, system tools, and MCP server tools
    /// — all rendered inline with full name, description, and input schema.
    pub fn format_header_context(&mut self) -> &str {
        let mut out = String::new();

        let _ = write!(out, "{}", INSTRUCTIONS);

        for prompt in &self.system_prompts {
            let _ = write!(out, "## System: {}\n{}\n\n", prompt.title, prompt.text);
        }

        let _ = write!(out, "## Harness Tools\n\n");
        for tool in &self.harness_tools {
            let schema = serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default();
            let _ = write!(
                out,
                "### {}\n{}\nSchema: {}\n\n",
                tool.name, tool.description, schema,
            );
        }

        if let Some(ref cosh) = self.cosh_tools {
            let _ = write!(out, "## System Tools\n\n");
            cosh.write_tool_descriptions(&mut out);
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
            for schema in cosh.schemas() {
                extractor.add_tool(schema);
            }
        }
        for tool in &self.harness_tools {
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
    fn process_extraction(&mut self, raw: &str, extractor: &ExtractAction) -> String {
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
            StreamAction::ToolCall(tc) if !self.handle_harness_tool(&tc) => {
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
        if let Some(response) = self.mock_stream_response.take() {
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
    pub async fn run_agent_loop(
        &mut self,
        input: &str,
        tx: tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
        mut answer_rx: tokio::sync::mpsc::UnboundedReceiver<
            Result<Vec<cosh_tools::question::types::AnswerItem>, String>
        >,
        stop_signal: Arc<AtomicBool>,
    ) {
        use super::events::HarnessEvent;

        let mut current_input = input.to_string();

        macro_rules! check_stop {
            () => {
                if self.stop || stop_signal.load(Ordering::Relaxed) {
                    let _ = tx.send(HarnessEvent::Stopped);
                    true
                } else {
                    false
                }
            };
        }

        loop {
            if check_stop!() {
                break;
            }

            // Phase 1: stream the LLM response
            let result = self
                .stream_chat(&current_input, |token| {
                    let _ = tx.send(HarnessEvent::Token {
                        text: token.to_string(),
                    });
                })
                .await;

            if let Err(e) = result {
                let _ = tx.send(HarnessEvent::Error(e));
                break;
            }

            if check_stop!() {
                break;
            }

            // Phase 2: dispatch all pending tool calls
            let had_tools = self.has_pending_tools();

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
                    let _ = tx.send(HarnessEvent::ToolCall {
                        tool: name.clone(),
                        input: args.clone(),
                    });
                }

                // Intercept `ask_questions` — send to TUI, wait for user answer
                if info.as_ref().is_some_and(|(n, _)| n == "ask_questions") {
                    use cosh_tools::question::types::{QuestionInput, QuestionOutput};

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

                            // Wait for the TUI to send back answers
                            let answer = answer_rx.recv().await;
                            match answer {
                                Some(Ok(answers)) => {
                                    let output = QuestionOutput {
                                        questions: q_input.questions,
                                        answers,
                                    };
                                    let json = serde_json::to_string(&output)
                                        .unwrap_or_else(|_| "{}".to_string());
                                    let ts = chrono::Local::now().format("%H:%M:%S");
                                    self.server_response
                                        .push(format!("[{ts}] {json}"));
                                    let _ = tx
                                        .send(HarnessEvent::ToolResult { output: json });
                                }
                                Some(Err(e)) => {
                                    let _ = tx
                                        .send(HarnessEvent::ToolError { error: e });
                                }
                                None => {
                                    let _ = tx.send(HarnessEvent::ToolError {
                                        error:
                                            "Internal error: question channel closed"
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
                    match self.dispatch_next().await {
                        Ok(output) => {
                            let _ = tx.send(HarnessEvent::ToolResult { output });
                        }
                        Err(e) => {
                            let _ = tx.send(HarnessEvent::ToolError { error: e });
                        }
                    }
                }
            }

            if check_stop!() {
                break;
            }

            if !had_tools {
                // No tool calls were made — the conversation is complete
                let _ = tx.send(HarnessEvent::Done);
                break;
            }

            // Tool results were accumulated in server_response
            // Next iteration sends an empty prompt with results in context
            current_input.clear();
        }
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
                Err(err) => return Err(err),
            }
        }

        // Tier 2: MCP sessions
        let idx = self
            .sessions
            .iter()
            .position(|s| s.tools.iter().any(|t| t.name == tool_name));

        let idx = match idx {
            Some(i) => i,
            None => {
                self.tool_issuer.pop_front();
                return Err(format!("no server found for tool '{tool_name}'"));
            }
        };

        let params = CallToolRequestParams::new(tool_name).with_arguments(args_map);

        let result = self.sessions[idx]
            .client
            .call_tool(params)
            .await
            .map_err(|e| e.to_string())?;

        self.tool_issuer.pop_front();

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

    pub(crate) fn push_tool_call(&mut self, tc: ToolCallData) {
        self.tool_issuer.push_back(tc);
    }
}
