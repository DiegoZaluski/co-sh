//! Native tool registration, routing and error enrichment: wiring the
//! connector's tool definitions and interpreting dispatch rejections.

use cosh_sdk::connector::ToolDefinition;

use super::{Harness, Mode, looks_like_argument_error};
use crate::harness::tools::{Tools, is_tool_disabled};

impl Harness {
    // Populate the connector's native tool definitions once.
    pub(super) fn init_native_tools(&mut self) {
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

        // Cosh tools — filtered by mode so the native API only exposes
        // tools the model is allowed to call in the current mode.
        if let Some(ref cosh) = self.cosh_tools {
            let descriptions: Vec<serde_json::Value> = match self.mode {
                Mode::Build => {
                    // All tools minus disabled: every mode sees the full
                    // toolset now. The coordinate forms of computer_pointer /
                    // computer_screenshot are gated per-call by the guardrail
                    // (denied in Build with guidance toward the tree-grounded
                    // forms), not by hiding the tool.
                    cosh.tool_descriptions()
                        .into_iter()
                        .filter(|desc| {
                            let name = desc["name"].as_str().unwrap_or_default();
                            !is_tool_disabled(name, &self.disabled_tools)
                        })
                        .collect()
                }
                Mode::Yolo | Mode::Command => {
                    // All tools minus disabled
                    cosh.tool_descriptions()
                        .into_iter()
                        .filter(|desc| {
                            !is_tool_disabled(
                                desc["name"].as_str().unwrap_or_default(),
                                &self.disabled_tools,
                            )
                        })
                        .collect()
                }
                Mode::Ask => {
                    // Only read-only / planning tools
                    cosh.tool_descriptions_filtered(&self.disabled_tools)
                }
            };

            for desc in descriptions {
                let name = desc["name"].as_str().unwrap_or_default();
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

        // MCP server tools (hidden in Ask mode, like the header).
        if !matches!(self.mode, Mode::Ask) {
            for tool in self.mcp.all_tools() {
                if self.disabled_tools.contains(tool.name.as_ref()) {
                    continue;
                }
                defs.push(crate::mcp::tool_to_definition(tool));
            }
        }

        self.connector.set_tools(defs);
    }

    /// Mirror the tools' current TODO list into the dedicated protected TODO
    /// context block (see `context_manager::todo_ctxt`). Called at loop start
    /// and after every `plan_*` dispatch so the block always reflects the
    /// authoritative `Plan` state.
    pub(super) fn sync_todo_context(&mut self) {
        if let Some(cosh) = &self.cosh_tools {
            let list = cosh.todo_list();
            self.context_manager.set_todo_list(list);
        }
    }

    /// Enrich an argument-shaped rejection with the tool's expected argument
    /// shape (a minimal example rendered from its input schema). Runtime
    /// failures (file not found, permission denied, timeouts) pass through
    /// untouched — echoing the schema there would be noise.
    pub(super) fn enrich_with_schema_hint(&self, tool_name: &str, err: String) -> String {
        if !looks_like_argument_error(&err) {
            return err;
        }
        match self.schema_input(tool_name) {
            Some(schema) => format!(
                "{err}\nExpected `{tool_name}` arguments — minimal shape: {}",
                cosh_sdk::extract_action::schema_skeleton(&schema)
            ),
            None => err,
        }
    }

    /// Rejection for a tool nothing serves. The model may be following stale
    /// handoff context (naming a tool that no longer exists), so the reply
    /// echoes the CURRENT available tool set — the same mode-filtered,
    /// deduplicated set the model was registered with — instead of leaving
    /// it to re-derive from memory (which may name removed tools).
    pub(super) fn unknown_tool_message(&self, tool_name: &str) -> String {
        const MAX_LISTED: usize = 15;
        let mut names = self.effective_tool_names();
        if names.is_empty() {
            return format!("no server found for tool '{tool_name}'");
        }
        let more = if names.len() > MAX_LISTED {
            format!(" (+{} more)", names.len() - MAX_LISTED)
        } else {
            String::new()
        };
        names.truncate(MAX_LISTED);
        format!(
            "no server found for tool '{tool_name}'. Available tools: {}{more}",
            names.join(", ")
        )
    }

    /// The tool's input schema, from the same mode-filtered, disabled-aware
    /// set the model was registered with (never a hidden tool's schema).
    pub(super) fn schema_input(&self, tool_name: &str) -> Option<serde_json::Value> {
        if let Some(cosh) = &self.cosh_tools {
            let schemas = match self.mode {
                Mode::Build | Mode::Yolo | Mode::Command => {
                    cosh.schemas_enabled(&self.disabled_tools)
                }
                Mode::Ask => cosh.schemas_filtered(&self.disabled_tools),
            };
            for schema in schemas {
                if schema.name == tool_name {
                    return Some(schema.input_schema);
                }
            }
        }
        if matches!(self.mode, Mode::Ask) {
            return None;
        }
        self.mcp
            .all_tools()
            .into_iter()
            .find(|tool| tool.name.as_ref() == tool_name)
            .filter(|tool| !self.disabled_tools.contains(tool.name.as_ref()))
            .map(|tool| serde_json::Value::Object((*tool.input_schema).clone()))
    }
}
