//! Stream-action extraction: turning raw LLM text into tool calls, harness
//! tool interception and tool-name resolution.

use cosh_sdk::extract_action::{ExtractAction, Item, ToolCallData, ToolSchema};

use super::{Harness, Mode, background};

impl Harness {
    /// Build an extractor with all registered MCP, cosh, and internal tools.
    pub(super) fn build_extractor(&self) -> ExtractAction {
        use crate::mcp::tool_to_schema;

        log::debug!("build_extractor: mcp_tools={}", self.mcp.all_tools().len());
        let mut extractor = ExtractAction::new();
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if self.disabled_tools.contains(tool.name.as_ref()) {
                    continue;
                }
                extractor.add_tool(tool_to_schema(tool));
            }
        }
        if let Some(ref cosh) = self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build | Mode::Yolo | Mode::Command => {
                    cosh.schemas_enabled(&self.disabled_tools)
                }
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
                // Built-in harness tools carry no curated example; tool
                // crate examples flow through `extract_schema` instead.
                example_args: None,
            });
        }
        #[cfg(test)]
        for ts in &self.test_tools {
            extractor.add_tool(ts.clone());
        }
        extractor
    }

    /// The sorted, deduplicated names of every tool the model is allowed to
    /// call in the current mode: harness + cosh + MCP, minus disabled, with
    /// MCP hidden in Ask mode. Mirrors [`Self::build_extractor`] so the two
    /// stay in lockstep.
    pub(crate) fn effective_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for tool in &self.harness_tools {
            if !self.disabled_tools.contains(&tool.name) {
                names.push(tool.name.clone());
            }
        }
        if let Some(ref cosh) = self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build | Mode::Yolo | Mode::Command => {
                    cosh.schemas_enabled(&self.disabled_tools)
                }
                Mode::Ask => cosh.schemas_filtered(&self.disabled_tools),
            };
            for schema in schemas {
                names.push(schema.name);
            }
        }
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if !self.disabled_tools.contains(tool.name.as_ref()) {
                    names.push(tool.name.to_string());
                }
            }
        }
        #[cfg(test)]
        for ts in &self.test_tools {
            names.push(ts.name.clone());
        }
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Handle a harness tool call immediately, returning `Some` when the call
    /// was consumed by the harness, or `None` when it must be dispatched
    /// externally.
    pub(crate) fn handle_harness_tool(&mut self, tc: &ToolCallData) -> Option<String> {
        let _tool = self.harness_tools.iter().find(|t| t.name == tc.name)?;
        match tc.name.as_str() {
            "stop_agent_loop" => {
                self.stop = true;
                Some(String::new())
            }
            "mask_tool_result" => Some(match self.context_manager.mask_newest_tool_result() {
                Some(id) => serde_json::json!({ "masked": true, "source_item": id }).to_string(),
                None => serde_json::json!({
                    "masked": false,
                    "error": "no unmasked tool result in the visible timeline"
                })
                .to_string(),
            }),
            "subagent_status" => Some(background::status_report(
                tc.arguments.get("task_id").and_then(|v| v.as_str()),
            )),
            // `agents_call` is an ASYNC harness tool: its `wait` mode awaits
            // the pane's report (and `dispatch_next` races it against the
            // stop signal), so it must NOT resolve here — returning `None`
            // queues it into the tool issuer for the async dispatch arm.
            // The explicit arm also keeps the catch-all below from
            // swallowing the call with an empty result.
            "agents_call" => None,
            // `agents_worktree` is async the same way: the tool result is
            // only the parking receipt — the binding outcome arrives later
            // as a Worktree-kind delivery, so the call must reach the
            // dispatch arm, never resolve inline here.
            "agents_worktree" => None,
            _ => Some(String::new()),
        }
    }

    /// Process extracted items into a text response, routing tool calls.
    pub(super) fn process_extraction(
        &mut self,
        raw: &str,
        extractor: &mut ExtractAction,
    ) -> String {
        let result = extractor.extract_batch(raw);
        let mut output = String::new();
        for item in result.items {
            match item {
                Item::Text(t) => output.push_str(&t),
                Item::ToolCall(tc) => match self.handle_harness_tool(&tc) {
                    None => {
                        self.tool_issuer.push_back(tc);
                    }
                    Some(result) if !result.is_empty() => {
                        self.push_tool_history(
                            &tc.id,
                            &tc.name,
                            &tc.arguments,
                            &tc.thought_signature,
                            &result,
                        );
                    }
                    _ => {}
                },
            }
        }
        let extraction_failures = extractor.take_tool_failures();
        self.tool_extraction_failure_count += extraction_failures;
        output
    }
}
