//! Tool for calling sub-agent CLIs.
//!
//! Each call spawns the agent CLI as a child process with the input passed
//! as a command-line argument, streams output in real time, and returns
//! the accumulated result when the process exits.
//!
//! # Supported agents
//!
//! Each agent must support a **non-interactive / headless mode** (e.g. `-p`,
//! `--message`, `run`, `exec`) — purely interactive TUIs cannot be driven.
//!
//! | Name | Binary | Invocation |
//! |------|--------|------------|
//! | `opencode` | `opencode` | `opencode run --auto "<input>"` |
//! | `kilo` | `kilo` | `kilo run --auto "<input>"` |
//! | `claude` | `claude` | `claude -p --permission-mode dontAsk --bare "<input>"` |
//! | `devin` | `devin` | `devin -p --permission-mode dangerous "<input>"` |
//! | `codex` | `codex` | `codex exec --sandbox workspace-write "<input>"` |
//! | `cline` | `cline` | `cline -y "<input>"` |
//! | `cursor` | `agent` | `agent -p --force --trust "<input>"` |
//! | `crush` | `crush` | `crush run --yolo --quiet "<input>"` |
//! | `hermes` | `hermes` | `hermes -z "<input>"` |
//! | `openhands` | `openhands` | `openhands --headless -t "<input>"` |
//! | `pi` | `pi` | `pi -p "<input>"` |
//! | `interpreter` | `interpreter` | `interpreter exec --ask-for-approval auto "<input>"` |
//! | `letta` | `letta` | `letta -p "<input>"` |
//! | `vibe` | `vibe` | `vibe --prompt --agent auto-approve "<input>"` |
//! | `aider` | `aider` | `aider --message --yes --no-auto-commits "<input>"` |
//! | `omp` | `omp` | `omp -p "<input>"` |
//! | `goose` | `goose` | `goose run -t "<input>"` |
//! | `gemini` | `gemini` | `gemini -p "<input>"` |
//! | `forge` | `forge` | `forge -p "<input>"` |
//!
//! See [`AGENTS`](call::AGENTS) for the full list.

pub mod call;
pub mod types;

use std::sync::Mutex;

use crate::ToolDescription;
pub use types::{SubAgentCallInput, SubAgentCallOutput};

/// Tool for calling sub-agent CLIs.
///
/// Each instance keeps the last input message sent to a sub-agent, so a
/// retry after a failed call does not require re-writing the whole prompt.
/// The harness creates one instance per agent loop, so the stored message
/// never leaks across sessions and no explicit `clean()` is needed.
pub struct SubAgent {
    /// MCP Tool description for `call`.
    pub description_call: ToolDescription,
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
    ///
    /// # Panics
    ///
    /// Panics if an agent returned by `detect_installed()` is not present
    /// in [`AGENTS`](call::AGENTS). This is a logic invariant — detection
    /// only returns names that exist in the table.
    #[must_use]
    #[allow(clippy::expect_used, clippy::format_collect)]
    pub fn new() -> Self {
        let installed = call::detect_installed();

        let description = if installed.is_empty() {
            // No agents installed — the LLM will see this and likely
            // avoid calling the tool, but the error message is helpful.
            format!(
                "{common}\n\
                 ## Supported agents\n\
                 (None detected — install one of: opencode, claude, aider, etc.\n\
                  and restart cosh.)\n\
                 {usage}",
                common = "Call a supported agent CLI with the given input message and \
                          return its output. The agent runs as a child process; output \
                          is streamed in real time. All agents are configured with \
                          auto-approval flags for headless operation.\n\
                          `input` is optional: if omitted, the last message sent \
                          to a sub-agent in this session is reused automatically, \
                          so a failed call can be retried without re-writing the \
                          prompt. If no sub-agent has been called yet, an error \
                          is returned.",
                usage = "## When to use\n\
                         - Use `subagent_call` with `agent` (and optionally `input`) \
                         to delegate a task to another agent CLI.\n\
                         - Omit `input` to reuse the last message sent to a sub-agent.\n\
                         - Use `bash_run` for regular shell commands. \
                         These are separate tools with different purposes.",
            )
        } else {
            let agents_desc = installed
                .iter()
                .map(|name| {
                    // SAFETY: `name` comes from detect_installed() which only
                    // returns entries present in AGENTS.
                    let entry = call::AGENTS
                        .iter()
                        .find(|(n, _, _)| *n == *name)
                        .expect("installed agent must be in AGENTS");
                    let (_, binary, static_args) = entry;
                    let args = static_args.join(" ");
                    format!("- `{name}` → `{binary} {args} \"<input>\"`\n")
                })
                .collect::<String>();

            format!(
                "{common}\n\
                 ## Supported agents\n\
                 {agents_desc}\n\
                 {usage}",
                common = "Call a supported agent CLI with the given input message and \
                          return its output. The agent runs as a child process; output \
                          is streamed in real time. All agents are configured with \
                          auto-approval flags for headless operation.\n\
                          `input` is optional: if omitted, the last message sent \
                          to a sub-agent in this session is reused automatically, \
                          so a failed call can be retried without re-writing the \
                          prompt. If no sub-agent has been called yet, an error \
                          is returned.",
                usage = "## When to use\n\
                         - Use `subagent_call` with `agent` (and optionally `input`) \
                         to delegate a task to another agent CLI.\n\
                         - Omit `input` to reuse the last message sent to a sub-agent.\n\
                         - Use `bash_run` for regular shell commands. \
                         These are separate tools with different purposes.",
            )
        };

        let enum_values: Vec<serde_json::Value> = if installed.is_empty() {
            // Even with no agents detected, keep the full enum so the
            // LLM can still attempt the tool if we missed one.
            call::AGENTS
                .iter()
                .map(|(n, _, _)| serde_json::Value::String(n.to_string()))
                .collect()
        } else {
            installed
                .iter()
                .map(|n| serde_json::Value::String(n.to_string()))
                .collect()
        };

        let description_call: ToolDescription = serde_json::json!({
            "name": "subagent_call",
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "The agent CLI to call.",
                        "enum": enum_values,
                    },
                    "input": {
                        "type": "string",
                        "description": "The message to send to the agent CLI as input. Optional: if omitted (or empty), the last message sent to a sub-agent in this session is reused automatically, so a failed call can be retried without re-writing the prompt. If no sub-agent has been called yet, an error is returned.",
                    },
                },
                "required": ["agent"],
            },
        });

        Self {
            description_call,
            last_input: Mutex::new(None),
        }
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
}
