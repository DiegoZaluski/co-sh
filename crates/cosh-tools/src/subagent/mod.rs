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

use crate::ToolDescription;
pub use types::{SubAgentCallInput, SubAgentCallOutput};

/// Tool for calling sub-agent CLIs.
pub struct SubAgent {
    /// MCP Tool description for `call`.
    pub description_call: ToolDescription,
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
                          auto-approval flags for headless operation.",
                usage = "## When to use\n\
                         - Use `subagent_call` with `agent` and `input` to delegate \
                         a task to another agent CLI.\n\
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
                          auto-approval flags for headless operation.",
                usage = "## When to use\n\
                         - Use `subagent_call` with `agent` and `input` to delegate \
                         a task to another agent CLI.\n\
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
                        "description": "The message to send to the agent CLI as input.",
                    },
                },
                "required": ["agent", "input"],
            },
        });

        Self { description_call }
    }
}
