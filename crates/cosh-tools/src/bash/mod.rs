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

/// Clean PTY output for display and model consumption.
///
/// PTY chunks carry ANSI escape sequences (colors, cursor moves) and CRLF
/// line endings (the PTY line discipline turns `\n` into `\r\n`). Strip the
/// escapes and normalize the line endings so the text is plain, newline-
/// separated output.
///
/// Only feed this function data ending on a **complete line** (see the
/// carry-buffer loop in the harness dispatch): a byte stream can split an
/// escape sequence, a multi-byte UTF-8 char, or a CRLF pair across read
/// boundaries, and a fresh parser per call would corrupt them.
#[must_use]
pub fn strip_ansi(bytes: &[u8]) -> String {
    let clean = strip_ansi_escapes::strip(bytes);
    let text = String::from_utf8_lossy(&clean);
    // Normalize CRLF, then drop lone `\r` (progress-bar redraw frames:
    // `cargo`, `wget`, …) so redraws merge into one line instead of
    // accumulating invisible carrier returns in the output.
    text.replace("\r\n", "\n").replace('\r', "")
}

/// Default execution timeout in milliseconds: 10 minutes (600_000 ms).
///
/// This is the timeout a `Bash` built with [`Bash::new`] runs with — the
/// hang guard described in [`Bash::timeout`]. The value is interpolated into
/// the `bash_run` tool description so the model knows the baseline it can
/// extend per call via the optional `timeout_ms` argument.
pub const DEFAULT_TIMEOUT_MS: u64 = 600_000;

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
    /// - `timeout`: [`DEFAULT_TIMEOUT_MS`] (10 minutes)
    /// - `env`: `None` (inherit parent process)
    /// - `pty`: `false` (piped stdout/stderr, not pseudo-terminal)
    /// - `cwd`: `""` (inherited from the parent process)
    #[must_use]
    pub fn new() -> Self {
        Self {
            timeout: Some(DEFAULT_TIMEOUT_MS),
            env: None,
            pty: false,
            cwd: String::new(),
            description_run: Self::build_description_run(DEFAULT_TIMEOUT_MS),
        }
    }

    /// Build the model-facing `bash_run` tool description for a configured
    /// timeout.
    ///
    /// The timeout value is interpolated here, from the **instance field** —
    /// not from [`DEFAULT_TIMEOUT_MS`] directly — so the text always states
    /// the threshold that `run_with_timeout` actually enforces, even when a
    /// caller overrides the default via [`Bash::timeout`]. This struct owns
    /// the property: the harness never restates the value.
    fn build_description_run(configured_ms: u64) -> ToolDescription {
        let description = format!(
            "Execute a bash command and return its output. \
             Outputs above the token budget are head/tail-truncated: the \
             middle is saved to a log file (path given in the truncation \
             notice) that you can read back in parts with fs_read \
             (offset/limit) or find_grep. \
             Environment variables and PTY mode are wrapper configuration, \
             not call arguments. \
             The configured default timeout is {configured_ms} \
             milliseconds ({} minutes); to extend it for a single call, pass \
             the optional `timeout_ms` argument with an integer strictly \
             greater than {configured_ms}. Values equal to or below \
             the default are rejected — the argument can only raise the \
             timeout, never lower it.",
            configured_ms / 60_000
        );
        serde_json::json!({
            "name": "bash_run",
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": concat!(
                            "The bash command to execute. Must not be an absolute path ",
                            "and must not match dangerous security patterns."
                        )
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": concat!(
                            "Optional. Extend the execution timeout for this call only, ",
                            "in milliseconds. Must be an integer strictly greater than the ",
                            "configured default (see the tool description); values equal to ",
                            "or below the default are rejected. Does not change the default."
                        )
                    }
                },
                "required": ["command"]
            }
        })
    }

    /// Set the execution timeout in milliseconds.
    ///
    /// When the timeout elapses, the child process is killed and the stream
    /// yields a final item with `signal: Some(-1)` and `exit_code: None`.
    /// The model-facing tool description is regenerated with the new value,
    /// so it always states the threshold `run_with_timeout` enforces.
    #[must_use]
    pub fn timeout(mut self, ms: u64) -> Self {
        self.timeout = Some(ms);
        self.description_run = Self::build_description_run(ms);
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
        self.run_with_timeout(command, None)
    }

    /// Execute a bash command with an optional per-call timeout override.
    ///
    /// `timeout_ms` extends the configured timeout for **this call only** —
    /// it never changes the default held by `self`. It may only *raise* the
    /// timeout: a value that is not strictly greater than the instance's
    /// configured timeout is rejected with a [`BashError`] and nothing is
    /// executed. `None` runs with the configured timeout.
    ///
    /// # Errors
    ///
    /// Returns [`BashError`] if the command is an absolute path, matches a
    /// dangerous security pattern, or `timeout_ms` fails to raise the
    /// configured timeout.
    pub fn run_with_timeout<'a>(
        &'a self,
        command: &'a str,
        timeout_ms: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = SpawnOutput> + Send + 'a>>, BashError> {
        let configured = self.timeout.unwrap_or(0);
        if let Some(ms) = timeout_ms {
            if ms <= configured {
                return Err(BashError {
                    text_err: Some(format!(
                        "timeout_ms must be strictly greater than the configured timeout \
                         ({configured} ms); the per-call argument can only raise the \
                         timeout, never lower it"
                    )),
                    exec_err: None,
                });
            }
            return bsh::run(Some(ms), &self.env, self.pty, command, &self.cwd);
        }
        bsh::run(self.timeout, &self.env, self.pty, command, &self.cwd)
    }
}

#[cfg(test)]
mod timeout_default_tests {
    use super::Bash;
    use super::DEFAULT_TIMEOUT_MS;

    /// `Bash::new()` ships the 10-minute default — the harness relies on it
    /// (it no longer calls `.timeout(...)` itself).
    #[test]
    fn new_uses_default_timeout() {
        let bash = Bash::new();
        assert_eq!(bash.timeout, Some(DEFAULT_TIMEOUT_MS));
        assert_eq!(DEFAULT_TIMEOUT_MS, 600_000);
    }

    /// The default is interpolated into the model-facing description so the
    /// model knows the baseline the `timeout_ms` argument must exceed.
    #[test]
    fn description_interpolates_default_timeout() {
        let desc = Bash::new().description_run;
        let text = desc["description"].as_str().unwrap();
        assert!(
            text.contains("default timeout is 600000 milliseconds (10 minutes)"),
            "description must state the default timeout, got: {text}"
        );
        assert!(
            text.contains("strictly greater than 600000"),
            "description must state the raise-only rule, got: {text}"
        );
        let schema = &desc["inputSchema"];
        assert_eq!(
            schema["properties"]["timeout_ms"]["type"].as_str(),
            Some("integer"),
            "inputSchema must expose the optional timeout_ms argument"
        );
        // `command` remains the only required argument.
        assert_eq!(
            schema["required"].as_array().unwrap(),
            &["command".to_string()],
            "timeout_ms must stay optional"
        );
    }

    /// A dev-level builder override regenerates the description with the new
    /// configured value: the text always states the threshold
    /// `run_with_timeout` enforces — never just the constant's default.
    #[test]
    fn builder_timeout_regenerates_description() {
        let bash = Bash::new().timeout(1_800_000);
        let text = bash.description_run["description"].as_str().unwrap();
        assert!(
            text.contains("default timeout is 1800000 milliseconds (30 minutes)"),
            "description must track the instance's configured timeout, got: {text}"
        );
        assert!(
            !text.contains("600000"),
            "stale default must not remain after the override, got: {text}"
        );
    }

    /// A `timeout_ms` that fails to raise the configured timeout is rejected
    /// before anything is spawned.
    #[test]
    fn run_with_timeout_rejects_lower_or_equal() {
        let bash = Bash::new();
        for ms in [0, DEFAULT_TIMEOUT_MS, DEFAULT_TIMEOUT_MS - 1] {
            let err = match bash.run_with_timeout("echo hi", Some(ms)) {
                Err(err) => err,
                Ok(_) => panic!("must reject timeout_ms = {ms} <= configured"),
            };
            assert!(
                err.text_err
                    .as_deref()
                    .unwrap_or_default()
                    .contains("strictly greater"),
                "expected raise-only error, got: {:?}",
                err.text_err
            );
        }
        // Valid overrides (strictly greater) pass validation and spawn.
        assert!(
            bash.run_with_timeout("echo hi", Some(DEFAULT_TIMEOUT_MS + 1))
                .is_ok(),
            "timeout_ms above the default must be accepted"
        );
        assert!(bash.run_with_timeout("echo hi", None).is_ok());
    }
}

#[cfg(test)]
mod strip_ansi_tests {
    use super::strip_ansi;

    #[test]
    fn strips_escapes_and_normalizes_crlf() {
        assert_eq!(strip_ansi(b"\x1b[32mfoo\x1b[0m\r\nbar\r\n"), "foo\nbar\n");
    }

    #[test]
    fn drops_lone_carriage_returns() {
        // Progress-bar redraw frames merge into one line.
        assert_eq!(strip_ansi(b"10%\r50%\r100%\n"), "10%50%100%\n");
    }

    #[test]
    fn keeps_non_ascii_text() {
        assert_eq!(strip_ansi("café ☕\r\n".as_bytes()), "café ☕\n");
    }
}
