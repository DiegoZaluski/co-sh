//! Tool for calling sub-agents.
//!
//! The model sees a single `subagent_call` tool, but the harness dispatches
//! it to two implementations (see [`SubAgent`]): an external agent CLI —
//! spawned as a child process with the input passed as a command-line
//! argument, streamed in real time, accumulating the result until exit —
//! or, when `agent` is omitted/empty, an internal agent (a nested harness
//! that reports only its final answer).
//!
//! # Supported agents
//!
//! Each agent must support a **non-interactive / headless mode** (e.g. `-p`,
//! `--message`, `run`, `exec`) — purely interactive TUIs cannot be driven.
//!
//! | Name | Binary | Invocation |
//! |------|--------|------------|
//! | `opencode` | `opencode` | `opencode run --auto "<input>"` |
//! | `claude` | `claude` | `claude -p --permission-mode bypassPermissions "<input>"` |
//! | `codex` | `codex` | `codex exec --sandbox workspace-write "<input>"` |
//! | `cursor` | `agent` | `agent -p --force "<input>"` |
//! | `aider` | `aider` | `aider --yes --no-auto-commits --message "<input>"` |
//! | `goose` | `goose` | `goose run -t "<input>"` |
//! | `kilo` | `kilo` | `kilo run --auto "<input>"` |
//! | `gemini` | `gemini` | `gemini -p "<input>"` |
//! | `interpreter` | `interpreter` | `interpreter exec --ask-for-approval auto "<input>"` |
//!
//! See [`AGENTS`](call::AGENTS) for the full list.

pub mod call;
pub mod types;

use std::sync::Mutex;

use crate::ToolDescription;
pub use types::{SubAgentCallInput, SubAgentCallOutput};

/// Tool for calling sub-agents.
///
/// A SINGLE visible tool dispatches to two implementations — the model
/// sees only one: an external agent CLI when `agent` is provided, or an
/// internal agent (a nested harness: fresh empty context, auto-approve,
/// no persistence, final report only) when `agent` is omitted or empty.
/// The harness routes the call; this struct owns the visible schema, the
/// optional description `note` (see [`set_note`](Self::set_note)) and the
/// last-input storage.
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
    /// only the agent CLIs that are actually installed in PATH.
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
    /// CLI.
    ///
    /// Rebuilds [`Self::description_call`] with the new chunk; an empty
    /// chunk keeps the original description byte-for-byte.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = note.into().trim().to_string();
        self.description_call = Self::build_tool_description(&self.note);
    }

    /// Build the full tool description (prose + input schema), tailoring the
    /// agent list to the CLIs actually installed in PATH, and interpolating
    /// the given `note` into the description's natural flow.
    ///
    /// # Panics
    ///
    /// Panics if an agent returned by `detect_installed()` is not present
    /// in [`AGENTS`](call::AGENTS). This is a logic invariant — detection
    /// only returns names that exist in the table.
    #[allow(clippy::expect_used, clippy::format_collect)]
    fn build_tool_description(note: &str) -> ToolDescription {
        let installed = call::detect_installed();

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
            "Call a supported agent CLI with the given input message and \
             return its output.{note}\n\
             `input` is optional: if omitted, the last message sent to a \
             sub-agent in this session is reused automatically, so a failed \
             call can be retried without re-writing the prompt. If no \
             sub-agent has been called yet, an error is returned."
        );
        let usage = "## When to use\n\
                     - Use `subagent_call` with `agent` (and optionally `input`) \
                     to delegate a task to another agent CLI.\n\
                     - Omit `input` to reuse the last message sent to a sub-agent.\n\
                     - Omit `agent` (or pass an empty string) to call the internal \
                     agent instead of an external CLI.\n\
                     - Use `bash_run` for regular shell commands. \
                     These are separate tools with different purposes.";

        let description = if installed.is_empty() {
            // No agents installed — the LLM will see this and likely
            // avoid calling the tool, but the error message is helpful.
            format!(
                "{common}\n\
                 ## Supported agents\n\
                 (None detected — install one of: opencode, claude, aider, etc.\n\
                  and restart cosh.)\n\
                 {usage}"
            )
        } else {
            let agents_desc = installed
                .iter()
                .map(|name| {
                    // SAFETY: `name` comes from detect_installed() which only
                    // returns entries present in AGENTS.
                    let entry = call::AGENTS
                        .iter()
                        .find(|a| a.name == *name)
                        .expect("installed agent must be in AGENTS");
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
            call::AGENTS
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
                        "description": "The agent CLI to call. Optional: if omitted (or empty), an internal agent with an empty context runs the task instead and returns only its final report.",
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
mod tests {
    use super::SubAgent;

    #[test]
    fn fresh_instance_without_input_errors() {
        let sub = SubAgent::new();
        let err = sub.resolve_input(None).unwrap_err();
        assert!(err.contains("no stored sub-agent message"), "{err}");
    }

    #[test]
    fn fresh_instance_with_empty_input_errors() {
        let sub = SubAgent::new();
        let err = sub.resolve_input(Some(String::new())).unwrap_err();
        assert!(err.contains("no stored sub-agent message"), "{err}");
    }

    #[test]
    fn first_call_stores_input_and_returns_it() {
        let sub = SubAgent::new();
        let resolved = sub
            .resolve_input(Some("review this PR".to_string()))
            .unwrap();
        assert_eq!(resolved, "review this PR");
    }

    #[test]
    fn omitted_input_reuses_last_message() {
        let sub = SubAgent::new();
        let first = sub
            .resolve_input(Some("review this PR".to_string()))
            .unwrap();
        assert_eq!(first, "review this PR");

        let reused = sub.resolve_input(None).unwrap();
        assert_eq!(reused, "review this PR");
    }

    #[test]
    fn empty_input_after_a_call_reuses_last_message() {
        let sub = SubAgent::new();
        sub.resolve_input(Some("review this PR".to_string()))
            .unwrap();

        let reused = sub.resolve_input(Some(String::new())).unwrap();
        assert_eq!(reused, "review this PR");
    }

    #[test]
    fn new_input_overwrites_stored_message() {
        let sub = SubAgent::new();
        sub.resolve_input(Some("first message".to_string()))
            .unwrap();
        sub.resolve_input(Some("second message".to_string()))
            .unwrap();

        let reused = sub.resolve_input(None).unwrap();
        assert_eq!(reused, "second message");
    }

    #[test]
    fn instances_do_not_share_stored_input() {
        // Each session owns its SubAgent, so stored input never leaks
        // across instances (no .clean() needed).
        let first = SubAgent::new();
        let second = SubAgent::new();
        first
            .resolve_input(Some("first session".to_string()))
            .unwrap();

        assert!(second.resolve_input(None).is_err());
    }

    #[test]
    fn empty_note_keeps_original_description() {
        let fresh = SubAgent::new();
        let mut with_note = SubAgent::new();
        with_note.set_note("");
        assert_eq!(with_note.description_call, fresh.description_call);
    }

    #[test]
    fn note_is_interpolated_inside_the_description_flow() {
        let mut sub = SubAgent::new();
        let fresh = sub.description_call.clone();
        let before = fresh["description"].as_str().unwrap();
        assert!(
            !before.contains("internal agent runs the task instead"),
            "the note must be absent by default"
        );

        sub.set_note(
            "When the `agent` argument is omitted or empty, an internal \
             agent runs the task instead.",
        );
        let after = sub.description_call["description"].as_str().unwrap();

        // Interpolated as the tail of the FIRST paragraph: right after the
        // opening sentence and BEFORE the `input` paragraph — not prepended
        // at the top.
        assert!(after.contains(
            "return its output. When the `agent` argument is omitted or \
             empty, an internal agent runs the task instead.\n`input` is \
             optional"
        ));
        assert!(
            after.starts_with("Call a supported agent CLI"),
            "the description must still start with the original prose"
        );
    }

    #[test]
    fn schema_agent_is_optional_and_required_is_empty() {
        let sub = SubAgent::new();
        let schema = &sub.description_call["inputSchema"];
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "neither `agent` nor `input` is required (empty required array)"
        );
        assert!(
            schema["properties"]["agent"]["enum"].is_array(),
            "the agent enum (installed CLIs) must still be present"
        );
    }

    #[test]
    fn input_parses_without_agent_as_none() {
        let input: super::SubAgentCallInput =
            serde_json::from_value(serde_json::json!({ "input": "hi" })).unwrap();
        assert!(input.agent.is_none());
        assert_eq!(input.input.as_deref(), Some("hi"));
    }

    #[test]
    fn input_parses_empty_agent_as_some_empty() {
        let input: super::SubAgentCallInput =
            serde_json::from_value(serde_json::json!({ "agent": "" })).unwrap();
        assert_eq!(input.agent.as_deref(), Some(""));
    }

    #[test]
    fn input_parses_with_agent() {
        let input: super::SubAgentCallInput = serde_json::from_value(serde_json::json!({
            "agent": "opencode",
            "input": "review this"
        }))
        .unwrap();
        assert_eq!(input.agent.as_deref(), Some("opencode"));
        assert_eq!(input.input.as_deref(), Some("review this"));
    }
}
