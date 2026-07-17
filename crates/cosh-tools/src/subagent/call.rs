//! Sub-agent call implementation.
//!
//! Spawns the agent CLI as a child process via `std::process::Command`,
//! streams stdout/stderr in real time through a channel, and returns the
//! accumulated output when the process exits.
//!
//! # Adding new agents
//!
//! Add an entry to [`AGENTS`] — a tuple of (name, binary, \[args\]).
//! The `input` string is appended as the final argument.
//!
//! Example: `("claude", "claude", &["run"])` → `claude run "the message"`

use std::io::BufRead;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
    Mutex,
};
use std::time::{Duration, Instant};

/// Registered agents: `(api_name, binary, [static_args, ...])`.
/// The user-provided `input` is appended as the final argument.
pub const AGENTS: &[(&str, &str, &[&str])] = &[
    ("opencode", "opencode", &["run"]),
];

/// Strip ANSI escape sequences from `s`, returning the clean string.
#[allow(clippy::expect_used)]
fn strip_ansi(s: &str) -> String {
    // strip_ansi_escapes preserves all non-ANSI bytes; if input is valid UTF-8
    // (always true here since we read from BufReader::lines()), output is also valid UTF-8.
    String::from_utf8(strip_ansi_escapes::strip(s))
        .expect("strip_ansi_escapes output is always valid UTF-8")
}

/// Max time to wait for the child process to exit.
const CALL_TIMEOUT: Duration = Duration::from_mins(2);
/// Interval at which we poll whether the reader thread has finished.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Validate that `agent` is registered in [`AGENTS`].
///
/// # Errors
///
/// Returns an error if the agent name is not found.
pub fn validate_agent(agent: &str) -> Result<(), String> {
    if AGENTS.iter().any(|(name, _, _)| *name == agent) {
        Ok(())
    } else {
        let supported: Vec<&str> = AGENTS.iter().map(|(n, _, _)| *n).collect();
        Err(format!(
            "Unsupported agent '{agent}'. Supported agents: {}. Use bash_run for shell commands.",
            supported.join(", "),
        ))
    }
}

/// Call a sub-agent CLI and return its output.
///
/// 1. Looks up `agent` in [`AGENTS`] to get the binary and static args.
/// 2. Spawns the process with `input` as the final argument.
/// 3. Spawns a reader thread that reads stdout line-by-line, sends each
///    chunk through `chunk_tx` for real-time TUI display, and accumulates
///    the full output in a shared buffer.
/// 4. Waits for the process to exit (or [`CALL_TIMEOUT`]).
/// 5. Returns the accumulated output and exit code.
///
/// > **Note:** This function blocks the calling thread. The harness dispatch
/// > runs it in [`tokio::task::spawn_blocking`] so the async runtime is not
/// > blocked.
///
/// # Errors
///
/// Returns an error if the agent is unsupported, spawning fails, or the
/// process does not exit within [`CALL_TIMEOUT`].
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

    // ── Resolve agent config ──────────────────────
    let (_, binary, static_args) = AGENTS
        .iter()
        .find(|(name, _, _)| *name == agent)
        .ok_or_else(|| format!("unknown agent '{agent}'"))?;

    // ── Spawn process ─────────────────────────────
    let mut cmd = std::process::Command::new(binary);
    cmd.args(*static_args);
    cmd.arg(input);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn '{agent}': {e}"))?;

    // ── Shared state ──────────────────────────────
    let output: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let output_clone = output.clone();

    let reader_done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let reader_done_clone = reader_done.clone();

    // Clone chunk_tx for the reader thread so the original stays in scope
    // for sending the final exit-code message after the reader finishes.
    let chunk_tx_reader = chunk_tx.clone();

    // ── Reader thread ─────────────────────────────
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
                        let chunk = format!("{text}\n");
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

    // ── Wait for completion with timeout ──────────
    let start = Instant::now();
    while start.elapsed() < CALL_TIMEOUT {
        if reader_done.load(Ordering::Acquire) {
            let code = child
                .wait()
                .ok()
                .map(|s| {
                    #[allow(clippy::cast_possible_wrap)]
                    { s.code().unwrap_or(-1_i32) }
                })
                .unwrap_or(-1_i32);

            let result = output.lock().unwrap().clone();
            let result = strip_ansi(&result);
            let _ = chunk_tx.send(format!("\n[exit code: {code}]"));
            return Ok((result, code));
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    // ── Timeout ───────────────────────────────────
    let _ = child.kill();
    let result = output.lock().unwrap().clone();
    let result = strip_ansi(&result);

    if result.is_empty() {
        Err(format!(
            "sub-agent '{agent}' did not respond within {timeout} seconds. \
             The agent CLI may not have been installed or configured correctly.",
            agent = agent,
            timeout = CALL_TIMEOUT.as_secs(),
        ))
    } else {
        log::warn!(
            "sub-agent '{agent}' timed out after {timeout}s, returning partial output",
            agent = agent,
            timeout = CALL_TIMEOUT.as_secs(),
        );
        Ok((result, -1_i32))
    }
}
