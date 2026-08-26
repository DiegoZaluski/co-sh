//! Sub-agent call implementation.
//!
//! Spawns the agent CLI as a child process via `std::process::Command`,
//! streams stdout/stderr in real time through a channel, and returns the
//! accumulated output when the process exits.
//!
//! # Adding new agents
//!
//! Add an entry to [`AGENTS`] — an [`Agent`] struct of
//! `(name, binary, [static_args], input_flag)`. The `input` string is passed
//! either as a final positional argument (the default) or, when `input_flag`
//! is set, as the value of that flag (see [`Agent`]).
//!
//! Example: `Agent { name: "claude", .., args: &["run"], input_flag: None }`
//! → `claude run "the message"`

use std::io::BufRead;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// A single registered agent CLI.
///
/// Maps a public `name` (used by the LLM in the tool call) to a `binary` and
/// its static `args`. Each agent declares how the user-supplied `input` string
/// is passed to the process:
///
/// - `input_flag: None` — `input` is appended as a **final positional
///   argument** (the common case): `binary <args> <input>`.
/// - `input_flag: Some(flag)` — `input` is passed as the **value of `flag`**:
///   `binary <args> <flag> <input>` (for CLIs such as `aider` whose prompt is a
///   named flag, `--message`, rather than a positional argument).
///
/// Only agents with a documented non-interactive / headless mode
/// (e.g. `-p`, `--message`, `run`, `exec`) are included — purely
/// interactive TUIs cannot be driven this way.
///
/// Static args are configured for optimal headless operation:
/// - Auto-approval flags to prevent blocking on prompts
/// - Sandbox/permission modes for safe automation
/// - No-auto-commit flags where appropriate to preserve git control
#[derive(Debug, Clone, Copy)]
pub struct Agent {
    /// Public name used by the LLM in the `subagent_call` tool.
    pub name: &'static str,
    /// Executable to spawn.
    pub binary: &'static str,
    /// Static arguments placed before the input.
    pub args: &'static [&'static str],
    /// How `input` is passed; see [`Agent`] docs.
    pub input_flag: Option<&'static str>,
}

impl Agent {
    /// Full argument list for this agent given `input`, per [`Agent`]
    /// semantics. The caller appends nothing else — this is the complete argv.
    #[must_use]
    pub fn args_for(&self, input: &str) -> Vec<String> {
        let mut out: Vec<String> = self.args.iter().map(|s| String::from(*s)).collect();
        match self.input_flag {
            Some(flag) => {
                out.push(String::from(flag));
                out.push(String::from(input));
            }
            None => out.push(String::from(input)),
        }
        out
    }

    /// Human-readable invocation (`binary <args> <input>` or
    /// `binary <args> <flag> "<input>"`) used in the tool description and docs.
    #[must_use]
    pub fn invocation(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(self.args.len() + 2);
        parts.push(self.binary);
        parts.extend_from_slice(self.args);
        if let Some(flag) = self.input_flag {
            parts.push(flag);
        }
        format!("{} \"<input>\"", parts.join(" "))
    }
}

/// Registered agents, each mapping a public name to a binary, static args and
/// how the user `input` is passed (see [`Agent`]). This is the whole
/// integration surface for adding a new sub-agent CLI.
pub const AGENTS: &[Agent] = &[
    Agent {
        name: "opencode",
        binary: "opencode",
        args: &["run", "--auto"],
        input_flag: None,
    },
    Agent {
        name: "claude",
        binary: "claude",
        args: &["-p", "--permission-mode", "bypassPermissions"],
        input_flag: None,
    },
    Agent {
        name: "codex",
        binary: "codex",
        args: &["exec", "--sandbox", "workspace-write"],
        input_flag: None,
    },
    Agent {
        name: "cursor",
        binary: "agent",
        args: &["-p", "--force"],
        input_flag: None,
    },
    Agent {
        name: "aider",
        binary: "aider",
        args: &["--yes", "--no-auto-commits"],
        // `aider` takes the prompt as the value of `--message`, not as a
        // positional argument (positional args are file names). Passing the
        // input as the flag's value is required for a single-shot call.
        input_flag: Some("--message"),
    },
    Agent {
        name: "goose",
        binary: "goose",
        args: &["run", "-t"],
        input_flag: None,
    },
    Agent {
        name: "kilo",
        binary: "kilo",
        args: &["run", "--auto"],
        input_flag: None,
    },
    Agent {
        name: "gemini",
        binary: "gemini",
        args: &["-p"],
        input_flag: None,
    },
    Agent {
        name: "interpreter",
        binary: "interpreter",
        args: &["exec", "--ask-for-approval", "auto"],
        input_flag: None,
    },
];
/// Strip ANSI escape sequences from `s`, returning the clean string.
#[allow(clippy::expect_used)]
fn strip_ansi(s: &str) -> String {
    // strip_ansi_escapes preserves all non-ANSI bytes; if input is valid UTF-8
    // (always true here since we read from BufReader::lines()), output is also valid UTF-8.
    String::from_utf8(strip_ansi_escapes::strip(s))
        .expect("strip_ansi_escapes output is always valid UTF-8")
}

/// Return installation / configuration guidance for a given agent.
fn install_hint(agent: &str) -> &'static str {
    match agent {
        "opencode" => {
            "Install: npm install -g @opencode/cli. More: https://github.com/opencode-ai/opencode"
        }
        "kilo" => {
            "Install: curl -fsSL https://kilo.ai/install.sh | sh. More: https://kilo.ai/docs/code-with-ai/platforms/cli"
        }
        "claude" => {
            "Install: npm install -g @anthropic-ai/claude-code. More: https://code.claude.com/docs"
        }
        "codex" => "Install: npm install -g @openai/codex. More: https://learn.chatgpt.com/docs",
        "cursor" => {
            "Install: curl https://cursor.com/install -fsS | bash. More: https://cursor.com/cli"
        }
        "interpreter" => {
            "Install: pip install open-interpreter. More: https://github.com/OpenInterpreter/open-interpreter"
        }
        "aider" => {
            "Install: pip install aider-chat. For headless mode also use --yes --no-auto-commits. More: https://aider.chat/docs/scripting.html"
        }
        "goose" => {
            "Install: curl -fsSL https://block.github.io/goose/install.sh | bash. More: https://goose-docs.ai"
        }
        "gemini" => {
            "Install: npm install -g @google/gemini-cli. Then: gemini auth login. More: https://github.com/google-gemini/gemini-cli"
        }
        _ => "",
    }
}

/// Check which agent CLIs the user has installed (binary found in PATH).
///
/// Results are cached in a `OnceLock` so detection runs exactly once
/// per process lifetime. Subsequent calls return the cached list.
pub fn detect_installed() -> &'static Vec<&'static str> {
    use std::sync::OnceLock;

    static INSTALLED: OnceLock<Vec<&'static str>> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        AGENTS
            .iter()
            .filter(|agent| {
                std::env::var_os("PATH").is_some_and(|path| {
                    std::env::split_paths(&path).any(|dir| {
                        let full = dir.join(agent.binary);
                        full.is_file() || full.with_extension("exe").is_file()
                    })
                })
            })
            .map(|agent| agent.name)
            .collect()
    })
}

/// Default max time to wait for the child process to exit.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_mins(2);
/// Interval at which we poll whether the reader thread has finished.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Resolve the effective call timeout. Honors the `COSH_SUBAGENT_TIMEOUT_SECS`
/// environment variable (in seconds); falls back to [`DEFAULT_CALL_TIMEOUT`].
///
/// Values that are missing, non-numeric, or less than one second are ignored
/// (they fall back to [`DEFAULT_CALL_TIMEOUT`]) so a `0`/garbage value cannot
/// make calls time out instantly.
fn call_timeout() -> Duration {
    match std::env::var("COSH_SUBAGENT_TIMEOUT_SECS") {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(secs) if secs >= 1 => Duration::from_secs(secs),
            _ => DEFAULT_CALL_TIMEOUT,
        },
        Err(_) => DEFAULT_CALL_TIMEOUT,
    }
}

/// Validate that `agent` is registered in [`AGENTS`].
///
/// # Errors
///
/// Returns an error if the agent name is not found.
pub fn validate_agent(agent: &str) -> Result<(), String> {
    if AGENTS.iter().any(|entry| entry.name == agent) {
        Ok(())
    } else {
        let supported: Vec<&str> = AGENTS.iter().map(|a| a.name).collect();
        Err(format!(
            "Unsupported agent '{agent}'. Supported agents: {}. Use bash_run for shell commands.",
            supported.join(", "),
        ))
    }
}
/// Call a sub-agent CLI and return its output.
///
/// 1. Looks up `agent` in [`AGENTS`] to get the binary and static args.
/// 2. Spawns the process with `input` passed per the agent's `input_flag`
///    (final positional argument by default, or the value of a named flag).
/// 3. Spawns a reader thread that reads stdout/stderr line-by-line, sends each
///    chunk through `chunk_tx` for real-time TUI display, and accumulates
///    the full output in a shared buffer.
/// 4. Waits for the process to exit (or [`call_timeout`]).
/// 5. Returns the accumulated output and exit code.
///
/// > **Note:** This function blocks the calling thread. The harness dispatch
/// > runs it in [`tokio::task::spawn_blocking`] so the async runtime is not
/// > blocked.
///
/// # Errors
///
/// Returns an error if the agent is unsupported, spawning fails, or the
/// process does not exit within [`call_timeout`].
///
/// # Panics
///
/// Panics if the internal mutex protecting the output buffer is poisoned
/// (only possible if the reader thread panics while holding the lock, which
/// does not happen under normal operation).
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::unwrap_used)]
pub fn call(
    agent: &str,
    input: &str,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<(String, i32), String> {
    validate_agent(agent)?;

    // Resolve agent config
    let entry = AGENTS
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("unknown agent '{agent}'"))?;
    let binary = entry.binary;

    // Spawn process
    let mut cmd = std::process::Command::new(binary);
    cmd.args(entry.args_for(input));
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            let install = install_hint(agent);
            if install.is_empty() {
                format!("The '{binary}' CLI is required but was not found in PATH.")
            } else {
                format!("The '{binary}' CLI is required but was not found in PATH. {install}")
            }
        } else {
            format!("Failed to spawn '{binary}': {e}")
        }
    })?;

    // Shared state
    let output: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let output_clone = output.clone();

    let reader_done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let reader_done_clone = reader_done.clone();

    // Clone chunk_tx for the reader thread so the original stays in scope
    // for sending the final exit-code message after the reader finishes.
    let chunk_tx_reader = chunk_tx.clone();

    // Reader thread
    //
    // Reads stdout/stderr of the child process line by line. Each chunk is
    // appended to the shared output buffer and sent through chunk_tx for
    // real-time TUI display. The main thread reads accumulated output from
    // the buffer directly.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    std::thread::spawn(move || {
        // Read stdout
        if let Some(stdout) = stdout {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        let clean = strip_ansi(&text);
                        let chunk = format!("{clean}\n");
                        output_clone.lock().unwrap().push_str(&chunk);
                        let _ = chunk_tx_reader.send(chunk);
                    }
                    Err(e) => {
                        log::error!("subagent stdout read error: {e}");
                        break;
                    }
                }
            }
        }

        // Read stderr
        if let Some(stderr) = stderr {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        let clean = strip_ansi(&text);
                        let chunk = format!("{clean}\n");
                        output_clone.lock().unwrap().push_str(&chunk);
                        let _ = chunk_tx_reader.send(chunk);
                    }
                    Err(e) => {
                        log::error!("subagent stderr read error: {e}");
                        break;
                    }
                }
            }
        }

        reader_done_clone.store(true, Ordering::Release);
    });

    // Wait for completion with timeout
    let timeout = call_timeout();
    let start = Instant::now();
    while start.elapsed() < timeout {
        if reader_done.load(Ordering::Acquire) {
            let code = child.wait().ok().map_or(-1_i32, |s| {
                #[allow(clippy::cast_possible_wrap)]
                {
                    s.code().unwrap_or(-1_i32)
                }
            });

            let result = output.lock().unwrap().clone();
            let result = strip_ansi(&result);
            let _ = chunk_tx.send(format!("\n[exit code: {code}]"));
            return Ok((result, code));
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    // Timeout
    let _ = child.kill();
    // Reap the child so it doesn't linger as a zombie in long-running harnesses.
    // The reader thread already drained the pipes, so this wait() returns fast.
    let _ = child.wait();
    let result = output.lock().unwrap().clone();
    let result = strip_ansi(&result);

    if result.is_empty() {
        let blocking_hint = match agent {
            "aider" => " aider may be waiting for confirmation. Set AIDER_YES=true or pass --yes.",
            "claude" => {
                " claude may be waiting for permission approval. Use --permission-mode bypassPermissions."
            }
            "opencode" => " opencode may be waiting for permission approval. Use --auto.",
            "kilo" => " kilo may be waiting for permission approval. Use --auto.",
            "codex" => {
                " codex may be waiting for permission approval. Use --sandbox workspace-write."
            }
            "cursor" => {
                " cursor may be waiting for permission approval. Use agent -p --force."
            }
            "interpreter" => {
                " interpreter may be waiting for permission approval. Use --ask-for-approval auto."
            }
            _ => "",
        };
        Err(format!(
            "sub-agent '{agent}' did not respond within {timeout_secs} seconds.{blocking_hint} The CLI may not be installed. Run the install command from the error above.",
            agent = agent,
            timeout_secs = timeout.as_secs(),
        ))
    } else {
        log::warn!(
            "sub-agent '{agent}' timed out after {timeout_secs}s, returning partial output",
            agent = agent,
            timeout_secs = timeout.as_secs(),
        );
        Ok((result, -1_i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> &'static Agent {
        AGENTS.iter().find(|a| a.name == name).expect("known agent")
    }

    #[test]
    fn positional_input_is_appended_as_last_arg() {
        let expected: Vec<String> = vec!["run".into(), "--auto".into(), "hello".into()];
        assert_eq!(agent("opencode").args_for("hello"), expected);
    }

    #[test]
    fn flag_input_becomes_the_flag_value() {
        // Regression: aider's prompt is the value of `--message`, never a
        // positional argument (positional args are file names).
        let expected: Vec<String> = vec![
            "--yes".into(),
            "--no-auto-commits".into(),
            "--message".into(),
            "review x".into(),
        ];
        assert_eq!(agent("aider").args_for("review x"), expected);
        assert_eq!(
            agent("aider").invocation(),
            "aider --yes --no-auto-commits --message \"<input>\""
        );
    }

    #[test]
    fn claude_uses_documented_permission_mode() {
        let expected: Vec<String> = vec![
            "-p".into(),
            "--permission-mode".into(),
            "bypassPermissions".into(),
            "hello".into(),
        ];
        assert_eq!(agent("claude").args_for("hello"), expected);
    }

    #[test]
    fn cursor_does_not_use_trust_flag() {
        let expected: Vec<String> = vec!["-p".into(), "--force".into(), "hello".into()];
        assert_eq!(agent("cursor").args_for("hello"), expected);
    }

    #[test]
    fn invocation_matches_documented_commands() {
        let expected: &[(&str, &str)] = &[
            ("opencode", "opencode run --auto \"<input>\""),
            ("claude", "claude -p --permission-mode bypassPermissions \"<input>\""),
            ("codex", "codex exec --sandbox workspace-write \"<input>\""),
            ("cursor", "agent -p --force \"<input>\""),
            ("aider", "aider --yes --no-auto-commits --message \"<input>\""),
            ("goose", "goose run -t \"<input>\""),
            ("kilo", "kilo run --auto \"<input>\""),
            ("gemini", "gemini -p \"<input>\""),
            (
                "interpreter",
                "interpreter exec --ask-for-approval auto \"<input>\"",
            ),
        ];
        assert_eq!(AGENTS.len(), expected.len(), "registry must stay in sync");
        for (name, expected_inv) in expected {
            let agent = AGENTS
                .iter()
                .find(|a| a.name == *name)
                .unwrap_or_else(|| panic!("agent '{name}' missing from registry"));
            assert_eq!(agent.invocation(), *expected_inv, "agent '{name}'");
        }
        // Regression guard: every registered agent is covered above exactly once.
        for a in AGENTS {
            assert!(
                expected.iter().any(|(n, _)| *n == a.name),
                "agent '{}' missing from the expected table",
                a.name
            );
        }
    }

    #[test]
    fn empty_input_is_still_appended() {
        // Empty input must not collapse the argv shape: positional agents still
        // get one final empty arg, and flag-input agents still get `--flag ""`.
        assert_eq!(
            agent("opencode").args_for(""),
            vec!["run".to_string(), "--auto".to_string(), String::new()]
        );
        assert_eq!(
            agent("aider").args_for(""),
            vec![
                "--yes".to_string(),
                "--no-auto-commits".to_string(),
                "--message".to_string(),
                String::new()
            ]
        );
    }
}
