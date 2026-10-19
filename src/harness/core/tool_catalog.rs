//! The harness-side tool catalog: prompt system and built-in tool specs.

use std::sync::Arc;

use cosh_tools::lsp::Lsp;

pub struct PromptSystem {
    pub title: String,
    pub text: String,
}

pub struct HarnessTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Build the language-server wrapper bound to `cwd`.
///
/// Returns `None` when `COSH_LSP=off|0|false`, the config-driven LSP switch is
/// off, no tokio runtime is active, or under `cfg(test)` (unit tests must not
/// spawn real language servers; LSP behavior is covered by the SDK/tools
/// suites). The engine itself is the process-wide singleton in
/// [`crate::harness::lsp::global_lsp`].
pub(super) fn build_lsp(cwd: &str) -> Option<Arc<Lsp>> {
    #[cfg(test)]
    {
        let _ = cwd;
        None
    }
    #[cfg(not(test))]
    {
        if !crate::harness::lsp::lsp_enabled()
            || matches!(
                std::env::var("COSH_LSP").as_deref(),
                Ok("off" | "0" | "false")
            )
        {
            return None;
        }
        crate::harness::lsp::global_lsp(cwd)
    }
}

pub(super) fn default_harness_tools() -> Vec<HarnessTool> {
    vec![
        HarnessTool {
            name: "stop_agent_loop".into(),
            description: "Stop running the agent loop".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        // Model-driven tail masking: the ONLY context-relief mechanism that
        // never mutates the middle of the history, so it never invalidates
        // the providers' exact-prefix prompt caches. The description ties the
        // call to an EVENT (a tool result just turned out to be irrelevant),
        // not to a judgment, and encourages batching it with the next tool
        // call to avoid extra round-trips. No reminder block in front of the
        // context: forgetting to mask is harmless (fail-open — the LLM
        // compaction stays the structural fallback), while a persistent
        // salient block would be re-paid full-price on every request.
        HarnessTool {
            name: "mask_tool_result".into(),
            description: "Drop the most recent tool result from the conversation when it just \
                turned out to be irrelevant to the task (empty or wrong file, superseded data, \
                dead end): its payload is replaced by a short reference in the context. Takes \
                no arguments and always targets the newest tool result. Call it as a reflex in \
                the same response as your next tool call; forgetting it is harmless."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        // Polling half of the background sub-agent feature: reads the
        // process-wide registry spawned tasks report through (`push` is the
        // automated completion notification the loop injects). Cheap,
        // read-only, and available to nested agents too.
        HarnessTool {
            name: "subagent_status".into(),
            description: "Check the status of background sub-agent tasks (spawned with \
                subagent_call using run_in_background: true). Pass task_id to query one task \
                (status + final report when completed); omit it to list ALL background tasks \
                with their ids, agents, and statuses. A completed task's report is included \
                in the result; completed tasks are ALSO pushed to you automatically as an \
                [automated notification] in a later turn, so polling is only needed when you \
                want a progress update before the notification arrives."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The background task to query (e.g. \"bg-1\"). Optional: omit to list all background tasks.",
                    },
                },
                "required": [],
            }),
        },
    ]
}
