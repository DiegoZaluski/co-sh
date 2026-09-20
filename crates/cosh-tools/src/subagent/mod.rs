//! Tool for calling sub-agents.
//!
//! The model sees a single `subagent_call` tool, but the harness dispatches
//! it to two implementations (see [`SubAgent`]): an external agent harness —
//! driven through the [Agent Client Protocol (ACP)](https://agentclientprotocol.com/)
//! as a full client turn (initialize → session/new → session/prompt), with
//! agent message chunks streamed in real time and accumulated until the turn
//! ends — or, when `agent` is omitted/empty, an internal agent (a nested
//! harness that reports only its final answer).
//!
//! # Supported agents
//!
//! Each agent must speak ACP over stdio — the protocol contract replaces the
//! fragile per-CLI flag scraping. See [`acp::ACP_AGENTS`] for the registry.
//! Agents without ACP support are not registered.
//!
//! | Name | ACP invocation |
//! |------|----------------|
//! | `gemini` | `gemini --experimental-acp` |
//! | `goose` | `goose acp` |
//! | `opencode` | `opencode acp` |
//! | `kilo` | `kilo acp` |
//! | `cline` | `cline --acp` |
//! | `devin` | `devin acp` |
//! | `claude` | `npx -y @agentclientprotocol/claude-agent-acp@latest` (official adapter) |
//! | `codex` | `npx -y @agentclientprotocol/codex-acp@latest` (official adapter) |

pub mod acp;
pub mod types;

use std::sync::Mutex;

use crate::ToolDescription;
pub use types::{SubAgentCallInput, SubAgentCallOutput};

/// Tool for calling sub-agents.
///
/// A SINGLE visible tool dispatches to two implementations — the model
/// sees only one: an external ACP agent harness when `agent` is provided,
/// or an internal agent (a nested harness: fresh empty context,
/// auto-approve, no persistence, final report only) when `agent` is omitted
/// or empty. The harness routes the call; this struct owns the visible
/// schema, the optional description `note` (see [`set_note`](Self::set_note))
/// and the last-input storage.
///
/// Each instance keeps the last input message sent to a sub-agent, so a
/// retry after a failed call does not require re-writing the whole prompt.
/// The harness creates one instance per agent loop, so the stored message
/// never leaks across sessions and no explicit `clean()` is needed.
pub struct SubAgent {
    /// MCP Tool description for `call`.
    pub description_call: ToolDescription,
    /// Optional informational chunk interpolated naturally into the tool
    /// description (see [`set_note`](Self::set_note)).
    note: String,
    /// Last input message sent to a sub-agent in this session, reused when
    /// a call omits `input`.
    last_input: Mutex<Option<String>>,
}

impl Default for SubAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SubAgent {
    /// Create a new `SubAgent` with a tool description tailored to
    /// only the ACP agent harnesses that are actually installed in PATH.
    /// Detection runs once per process (cached by `detect_installed()`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            description_call: Self::build_tool_description(""),
            note: String::new(),
            last_input: Mutex::new(None),
        }
    }

    /// Set an informational chunk that is interpolated NATURALLY into the
    /// tool description (inside the description prose — not prepended as a
    /// notice). The harness uses this to tell the model that omitting
    /// `agent` routes the call to an internal agent instead of an external
    /// ACP harness.
    ///
    /// Rebuilds [`Self::description_call`] with the new chunk; an empty
    /// chunk keeps the original description byte-for-byte.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = note.into().trim().to_string();
        self.description_call = Self::build_tool_description(&self.note);
    }

    /// Build the full tool description (prose + input schema), tailoring the
    /// agent list to the ACP harnesses actually installed in PATH, and
    /// interpolating the given `note` into the description's natural flow.
    ///
    /// # Panics
    ///
    /// Panics if an agent returned by `detect_installed()` is not present
    /// in [`ACP_AGENTS`](acp::ACP_AGENTS). This is a logic invariant —
    /// detection only returns names that exist in the table.
    #[allow(clippy::expect_used, clippy::format_collect)]
    fn build_tool_description(note: &str) -> ToolDescription {
        let installed = acp::detect_installed();

        // The note lands as the tail of the FIRST paragraph, so it reads as
        // part of the instructions ("...return its output. When the `agent`
        // argument is omitted or empty, ...") instead of a prepended notice.
        // An empty note reproduces the original text exactly.
        let note = if note.is_empty() {
            String::new()
        } else {
            format!(" {note}")
        };
        let common = format!(
            "Call a supported agent ACP harness with the given input message and \
             return its output.{note}\n\
             `input` is optional: if omitted, the last message sent to a \
             sub-agent in this session is reused automatically, so a failed \
             call can be retried without re-writing the prompt. If no \
             sub-agent has been called yet, an error is returned."
        );
        let usage = "## When to use\n\
                     - Use `subagent_call` with `agent` (and optionally `input`) \
                     to delegate a task to another agent ACP harness.\n\
                     - Omit `input` to reuse the last message sent to a sub-agent.\n\
                     - Omit `agent` (or pass an empty string) to call the internal \
                     agent instead of an external ACP harness.\n\
                     - Use `bash_run` for regular shell commands. \
                     These are separate tools with different purposes.";

        let description = if installed.is_empty() {
            // No agents installed — the LLM will see this and likely
            // avoid calling the tool, but the error message is helpful.
            format!(
                "{common}\n\
                 ## Supported agents\n\
                 (None detected — install one of the ACP-capable agents \
                  (gemini, goose, opencode, kilo, claude, codex) and restart cosh.)\n\
                 {usage}"
            )
        } else {
            let agents_desc = installed
                .iter()
                .map(|name| {
                    // SAFETY: `name` comes from detect_installed() which only
                    // returns entries present in ACP_AGENTS.
                    let entry = acp::ACP_AGENTS
                        .iter()
                        .find(|a| a.name == *name)
                        .expect("installed agent must be in ACP_AGENTS");
                    let invocation = entry.invocation();
                    format!("- `{name}` → `{invocation}`\n")
                })
                .collect::<String>();

            format!(
                "{common}\n\
                 ## Supported agents\n\
                 {agents_desc}\n\
                 {usage}"
            )
        };

        let enum_values: Vec<serde_json::Value> = if installed.is_empty() {
            // Even with no agents detected, keep the full enum so the
            // LLM can still attempt the tool if we missed one.
            acp::ACP_AGENTS
                .iter()
                .map(|a| serde_json::Value::String(a.name.to_string()))
                .collect()
        } else {
            installed
                .iter()
                .map(|n| serde_json::Value::String(n.to_string()))
                .collect()
        };

        serde_json::json!({
            "name": "subagent_call",
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "The agent ACP harness to call. Optional: if omitted (or empty), an internal agent with an empty context runs the task instead and returns only its final report.",
                        "enum": enum_values,
                    },
                    "input": {
                        "type": "string",
                        "description": "The message to send to the sub-agent as input. Optional: if omitted (or empty), the last message sent to a sub-agent in this session is reused automatically, so a failed call can be retried without re-writing the prompt. If no sub-agent has been called yet, an error is returned.",
                    },
                },
                "required": [],
            },
        })
    }

    /// Resolve the effective input message for a sub-agent call.
    ///
    /// When `input` is provided (and non-empty), it is stored as the last
    /// message sent to a sub-agent in this session and returned. When
    /// omitted, the stored message is reused so the calling agent does not
    /// have to re-write a long prompt after a failed call.
    ///
    /// # Errors
    ///
    /// Returns an error if `input` is omitted and no message is stored yet
    /// (i.e. no sub-agent has been called in this session).
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (only possible if another
    /// thread panicked while holding the lock).
    #[allow(clippy::unwrap_used)]
    pub fn resolve_input(&self, input: Option<String>) -> Result<String, String> {
        match input {
            Some(text) => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    self.stored_input()
                } else {
                    *self.last_input.lock().unwrap() = Some(text.clone());
                    Ok(text)
                }
            }
            None => self.stored_input(),
        }
    }

    /// Return the stored last input message, or an error explaining that
    /// there is no message in sub-agent storage yet.
    fn stored_input(&self) -> Result<String, String> {
        self.last_input.lock().unwrap().clone().ok_or_else(|| {
            "subagent_call was called without an 'input' argument, but there is \
                 no stored sub-agent message in this session yet. Provide an 'input' \
                 argument to send the first message."
                .to_string()
        })
    }
}

#[cfg(test)]
mod test;
