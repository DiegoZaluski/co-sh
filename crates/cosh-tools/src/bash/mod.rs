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

mod liveness;

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

/// Default hard runtime cap in milliseconds: 30 minutes (1_800_000 ms).
///
/// The cap is the last line of defense, not the primary hang guard — the
/// liveness watchdog (see `bash/liveness.rs`) kills a command much earlier
/// when it shows no output, no CPU time and no I/O progress for the
/// quiescence window (120s), so legitimate long-running commands are never
/// cut by this cap. A cap is still required because a CPU-burning busy loop
/// is indistinguishable from real work from the outside.
///
/// The value is interpolated into the `bash_run` tool description so the
/// model knows the baseline. It is wrapper configuration, not a call
/// argument — the model cannot change it per call.
pub const DEFAULT_MAX_RUNTIME_MS: u64 = 1_800_000;

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
    /// - `timeout`: [`DEFAULT_MAX_RUNTIME_MS`] (30 minutes, hard cap)
    /// - `env`: `None` (inherit parent process)
    /// - `pty`: `false` (piped stdout/stderr, not pseudo-terminal)
    /// - `cwd`: `""` (inherited from the parent process)
    #[must_use]
    pub fn new() -> Self {
        Self {
            timeout: Some(DEFAULT_MAX_RUNTIME_MS),
            env: None,
            pty: false,
            cwd: String::new(),
            description_run: Self::build_description_run(DEFAULT_MAX_RUNTIME_MS),
        }
    }

    /// Build the model-facing `bash_run` tool description for a configured
    /// hard runtime cap.
    ///
    /// The cap value is interpolated here, from the **instance field** —
    /// not from [`DEFAULT_MAX_RUNTIME_MS`] directly — so the text always
    /// states the cap that `run` actually enforces, even when a caller
    /// overrides the default via [`Bash::timeout`]. This struct owns the
    /// property: the harness never restates the value.
    fn build_description_run(cap_ms: u64) -> ToolDescription {
        let description = format!(
            "Execute a bash command and return its output. \
             Outputs above the token budget are head/tail-truncated: the \
             middle is saved to a log file (path given in the truncation \
             notice) that you can read back in parts with fs_read \
             (offset/limit) or find_grep. \
             Environment variables and PTY mode are wrapper configuration, \
             not call arguments. \
             Commands run until completion; there is no timeout argument. \
             The harness kills a command automatically when it stops making \
             progress: if a command produces no output, no CPU time and no \
             I/O for {} seconds it is presumed hung and killed. Long-running \
             but active commands (builds, downloads) are never cut by that \
             guard. A hard runtime cap of {cap_ms} milliseconds ({} minutes) \
             applies regardless of progress.",
            liveness::LivenessConfig::default_quiescent_secs(),
            cap_ms / 60_000
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
                    }
                },
                "required": ["command"]
            }
        })
    }

    /// Set the hard runtime cap in milliseconds.
    ///
    /// The cap is the last line of defense against commands that keep
    /// making progress forever (e.g. a CPU-burning busy loop) — the primary
    /// hang guard is the liveness watchdog, which kills a command that
    /// shows no output, CPU or I/O progress for the quiescence window long
    /// before this cap. When the cap elapses, the child process is killed
    /// and the stream yields a `runtime cap: …` stderr item followed by a
    /// final item with `signal: Some(-1)` and `exit_code: None`. The
    /// model-facing tool description is regenerated with the new value, so
    /// it always states the cap `run` enforces.
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
        bsh::run(self.timeout, &self.env, self.pty, command, &self.cwd)
    }
}

#[cfg(test)]
mod timeout_default_tests {
    use super::Bash;
    use super::DEFAULT_MAX_RUNTIME_MS;

    /// `Bash::new()` ships the 30-minute hard cap — the harness relies on
    /// it (it no longer calls `.timeout(...)` itself).
    #[test]
    fn new_uses_default_timeout() {
        let bash = Bash::new();
        assert_eq!(bash.timeout, Some(DEFAULT_MAX_RUNTIME_MS));
        assert_eq!(DEFAULT_MAX_RUNTIME_MS, 1_800_000);
    }

    /// The cap is interpolated into the model-facing description, which
    /// must NOT offer a per-call `timeout_ms` argument anymore: the
    /// liveness watchdog handles hangs automatically.
    #[test]
    fn description_interpolates_cap_and_has_no_timeout_arg() {
        let desc = Bash::new().description_run;
        let text = desc["description"].as_str().unwrap();
        assert!(
            text.contains("hard runtime cap of 1800000 milliseconds (30 minutes)"),
            "description must state the hard cap, got: {text}"
        );
        assert!(
            text.contains("no timeout argument") && text.contains("120 seconds"),
            "description must describe the automatic hang guard, got: {text}"
        );
        let schema = &desc["inputSchema"];
        assert!(
            schema["properties"]["timeout_ms"].is_null(),
            "timeout_ms must no longer be exposed to the model"
        );
        // `command` remains the only required argument.
        assert_eq!(
            schema["required"].as_array().unwrap(),
            &["command".to_string()],
        );
    }

    /// A dev-level builder override regenerates the description with the new
    /// configured cap: the text always states the cap `run` enforces —
    /// never just the constant's default.
    #[test]
    fn builder_timeout_regenerates_description() {
        let bash = Bash::new().timeout(600_000);
        let text = bash.description_run["description"].as_str().unwrap();
        assert!(
            text.contains("hard runtime cap of 600000 milliseconds (10 minutes)"),
            "description must track the instance's configured cap, got: {text}"
        );
        assert!(
            !text.contains("1800000"),
            "stale default must not remain after the override, got: {text}"
        );
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
