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
//! | `opencode` | `opencode` | `opencode run "<input>"` |
//! | `kilo` | `kilo` | `kilo run "<input>"` |
//! | `claude` | `claude` | `claude -p "<input>"` |
//! | `devin` | `devin` | `devin -p "<input>"` |
//! | `codex` | `codex` | `codex exec "<input>"` |
//! | `letta` | `letta` | `letta -p "<input>"` |
//! | `vibe` | `vibe` | `vibe --prompt "<input>"` |
//! | `aider` | `aider` | `aider --message "<input>"` (needs `AIDER_YES=true`) |
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
    /// Create a new `SubAgent` with the tool description pre-configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            description_call: serde_json::json!({
                "name": "subagent_call",
                "description": concat!(
                    "Call a supported agent CLI with the given input message and ",
                    "return its output. The agent runs as a child process; output ",
                    "is streamed in real time.\n\n",
                    "## Supported agents\n",
                    "- `opencode` → `opencode run \"<input>\"`\n",
                    "- `kilo` → `kilo run \"<input>\"`\n",
                    "- `claude` → `claude -p \"<input>\"`\n",
                    "- `devin` → `devin -p \"<input>\"`\n",
                    "- `codex` → `codex exec \"<input>\"`\n",
                    "- `letta` → `letta -p \"<input>\"`\n",
                    "- `vibe` → `vibe --prompt \"<input>\"`\n",
                    "- `aider` → `aider --message \"<input>\"` (env `AIDER_YES=true` for headless)\n",
                    "- `omp` → `omp -p \"<input>\"`\n",
                    "- `goose` → `goose run -t \"<input>\"`\n",
                    "- `gemini` → `gemini -p \"<input>\"`\n",
                    "- `forge` → `forge -p \"<input>\"`\n",
                    "## When to use\n",
                    "- Use `subagent_call` with `agent` and `input` to delegate ",
                    "a task to another agent CLI.\n",
                    "- Use `bash_run` for regular shell commands. ",
                    "These are separate tools with different purposes.",
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "string",
                            "description": "The agent CLI to call. See the list of supported agents above.",
                            "enum": call::AGENTS.iter().map(|(n, _, _)| serde_json::Value::String(n.to_string())).collect::<Vec<_>>(),
                        },
                        "input": {
                            "type": "string",
                            "description": "The message to send to the agent CLI as input.",
                        },
                    },
                    "required": ["agent", "input"],
                },
            }),
        }
    }
}
