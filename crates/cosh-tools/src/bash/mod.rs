//! Shared-state wrapper for bash execution.
//!
//! [`Bash`] holds execution configuration — environment variables,
//! PTY mode, and working directory — so callers don't have to
//! reconstruct options on every invocation.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::bash::Bash;
//!
//! let bash = Bash::new().cwd("/project");
//! let mut stream = bash.run("ls -la")?;
//! ```

pub mod bsh;
#[cfg(test)]
pub mod test;
pub mod types;

use std::pin::Pin;
use tokio_stream::Stream;

pub use types::BashRunInput;

use crate::ToolDescription;
use crate::bash::bsh::BashError;
use crate::bash::bsh::SpawnOutput;

/// Shared-state wrapper for bash execution.
///
/// Use the builder methods after [`new`](Self::new) to configure the
/// environment, then call [`run`](Self::run) to execute a command.
pub struct Bash {
    timeout: Option<u64>,
    env: Option<Vec<(String, String)>>,
    pty: bool,
    cwd: String,

    /// MCP Tool description for `run`.
    pub description_run: ToolDescription,
}

impl Default for Bash {
    fn default() -> Self {
        Self::new()
    }
}

impl Bash {
    /// Create a new `Bash` with default settings.
    ///
    /// - `timeout`: `None` (no timeout)
    /// - `env`: `None` (inherit parent process)
    /// - `pty`: `false` (piped stdout/stderr, not pseudo-terminal)
    /// - `cwd`: `""` (inherited from the parent process)
    #[must_use]
    pub fn new() -> Self {
        Self {
            timeout: None,
            env: None,
            pty: false,
            cwd: String::new(),
            description_run: serde_json::json!({
                "name": "bash_run",
                "description": concat!(
                    "Execute a bash command and return its output as an async stream. ",
                    "Supports configurable timeout, environment variables, and ",
                    "pseudo-terminal (PTY) mode. PTY mode multiplexes stdout and ",
                    "stderr for colored output and interactive prompts. ",
                    "Security validation blocks dangerous patterns like rm -rf /, ",
                    "fork bombs, and remote execution. Outputs above the token ",
                    "budget are head/tail-truncated: the middle is saved to a log ",
                    "file (path given in the truncation notice) that you can read ",
                    "back in parts with fs_read (line_range) or find_grep."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": concat!(
                                "The bash command to execute. Must not be an absolute path ",
                                "and must not match dangerous security patterns."
                            )
                        }
                    },
                    "required": ["command"]
                }
            }),
        }
    }

    /// Set the execution timeout in milliseconds.
    ///
    /// When the timeout elapses, the child process is killed and the stream
    /// yields a final item with `signal: Some(-1)` and `exit_code: None`.
    /// Leave unset for no timeout.
    #[must_use]
    pub const fn timeout(mut self, ms: u64) -> Self {
        self.timeout = Some(ms);
        self
    }

    /// Set environment variables for the child process.
    ///
    /// Pass `None` to inherit the parent's environment.
    #[must_use]
    pub fn env(mut self, env: Option<Vec<(String, String)>>) -> Self {
        self.env = env;
        self
    }

    /// Use a pseudo-terminal for the child process.
    ///
    /// When `true`, stdout and stderr are multiplexed into the PTY
    /// (colored output, prompts, etc.). When `false` (default), they
    /// are captured as separate piped streams.
    #[must_use]
    pub const fn pty(mut self, v: bool) -> Self {
        self.pty = v;
        self
    }

    /// Set the working directory for the command.
    #[must_use]
    pub fn cwd(mut self, path: impl Into<String>) -> Self {
        self.cwd = path.into();
        self
    }

    /// Execute a bash command and return its output as an async stream.
    ///
    /// The returned stream borrows from `self` — it must not outlive the
    /// [`Bash`] instance.
    ///
    /// # Errors
    ///
    /// Returns [`BashError`] if the command is an absolute path or matches
    /// a dangerous security pattern.
    pub fn run<'a>(
        &'a self,
        command: &'a str,
    ) -> Result<Pin<Box<dyn Stream<Item = SpawnOutput> + Send + 'a>>, BashError> {
        bsh::run(self.timeout, &self.env, self.pty, command, &self.cwd)
    }
}
