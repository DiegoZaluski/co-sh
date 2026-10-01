#[cfg(any(unix, windows))]
#[allow(unused_imports)]
use super::bsh::auto_fix_command;
#[allow(unused_imports)]
use super::bsh::spawn_bash;
#[cfg(any(unix, windows))]
#[allow(unused_imports)]
use super::bsh::spawn_bash_pty;
#[cfg(unix)]
#[allow(unused_imports)]
use std::time::{Duration, Instant};
#[allow(unused_imports)]
use tokio_stream::StreamExt;
#[allow(dead_code)]
const BUFFER_SIZE: usize = 4096;

/// Count of payload bytes (`a`) after stripping ANSI escape sequences —
/// used by PTY tests on Windows, where ConPTY interleaves escape sequences
/// and viewport re-render artifacts with the payload.
#[allow(dead_code)]
fn payload_len(bytes: &[u8]) -> usize {
    let stripped = strip_ansi_escapes::strip(bytes);
    stripped.iter().filter(|b| **b == b'a').count()
}

#[tokio::test]
async fn test_spawn_bash_simple_echo() {
    let mut stream = spawn_bash(None, ".", "echo hello", None);

    let mut got_output = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if !output.stdout.is_empty() {
            assert_eq!(output.stdout, b"hello\n");
            got_output = true;
        }
    }
    assert!(got_output);
}

#[tokio::test]
async fn test_spawn_bash_stderr() {
    let mut stream = spawn_bash(None, ".", "echo error >&2", None);
    let mut got_stderr = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if !output.stderr.is_empty() {
            assert_eq!(output.stderr, b"error\n");
            got_stderr = true;
        }
    }
    assert!(got_stderr);
}

#[tokio::test]
async fn test_spawn_bash_stdout_and_stderr() {
    let mut stream = spawn_bash(None, ".", "echo out; echo err >&2", None);
    let mut out = vec![];
    let mut err = vec![];
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        out.extend_from_slice(&output.stdout);
        err.extend_from_slice(&output.stderr);
    }
    assert_eq!(out, b"out\n");
    assert_eq!(err, b"err\n");
}

#[tokio::test]
async fn test_spawn_bash_with_env() {
    let mut stream = spawn_bash(
        Some(vec![("MY_VAR".to_string(), "hello".to_string())]),
        ".",
        "echo $MY_VAR",
        None,
    );
    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    assert_eq!(output, b"hello\n");
}

#[tokio::test]
async fn test_spawn_bash_invalid_env_var_name() {
    let mut stream = spawn_bash(
        Some(vec![("INVALID-KEY".to_string(), "value".to_string())]),
        ".",
        "echo hello",
        None,
    );
    let mut got_error = false;
    while let Some(result) = stream.next().await {
        if result.is_err() {
            got_error = true;
            break;
        }
    }
    assert!(got_error);
}

#[tokio::test]
async fn test_spawn_bash_nonexistent_cwd() {
    let mut stream = spawn_bash(None, "/nonexistent-path-12345", "echo hello", None);
    let mut got_error = false;
    while let Some(result) = stream.next().await {
        if result.is_err() {
            got_error = true;
            break;
        }
    }
    assert!(got_error);
}

/// Regression: `Bash::new()` (default cwd "") used to produce a SILENTLY
/// EMPTY stream — the spawn failed on `current_dir("")` and the error was
/// dropped. The empty cwd must mean "inherit the parent's working
/// directory" and the command must run.
#[tokio::test]
async fn test_spawn_bash_empty_cwd_inherits_parent() {
    let mut stream = spawn_bash(None, "", "echo empty-cwd-works", None);
    let mut got_output = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if !output.stdout.is_empty() {
            assert_eq!(output.stdout, b"empty-cwd-works\n");
            got_output = true;
        }
    }
    assert!(
        got_output,
        "empty cwd must inherit the parent's cwd, not fail"
    );
}

/// Regression: stream/spawn errors (invalid env var, read failures) used to
/// be swallowed by the outer `run` wrapper — the stream just ended with zero
/// items. The inner PTY stream yields the `Err` and the public `run` wrapper
/// converts it into a stderr-bearing item so callers always see why nothing
/// was produced. (A bad cwd is NOT an error in the PTY path: portable_pty
/// spawns the child anyway and it runs elsewhere.)
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_error_is_surfaced() {
    let mut stream = spawn_bash_pty(
        Some(vec![("BAD-KEY!".to_string(), "x".to_string())]),
        ".",
        "echo hello",
        None,
    );
    let mut got_error = false;
    while let Some(result) = stream.next().await {
        if let Err(e) = result {
            let text = e.to_string();
            assert!(
                text.contains("invalid env variable name"),
                "expected the env validation error, got: {text:?}"
            );
            got_error = true;
        }
    }
    assert!(
        got_error,
        "a failed spawn must surface an error item, not end silently"
    );
}

/// The public `run` wrapper must convert inner stream errors into a
/// stderr-bearing item (never swallow them into a silent empty stream).
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_run_surfaces_stream_error_as_stderr_item() {
    use super::bsh::run;
    use tokio_stream::StreamExt;

    let mut stream = match run(
        None,
        &Some(vec![("BAD-KEY!".to_string(), "x".to_string())]),
        true,
        "echo hello",
        ".",
    ) {
        Ok(s) => s,
        Err(e) => panic!(
            "validation passed, spawn error expected later: {:?}",
            e.text_err.as_deref()
        ),
    };

    let mut got_error_text = false;
    while let Some(output) = stream.next().await {
        if !output.stderr.is_empty() {
            let text = String::from_utf8_lossy(&output.stderr);
            assert!(
                text.contains("bash stream error") && text.contains("invalid env variable name"),
                "expected the surfaced stream error in stderr, got: {text:?}"
            );
            got_error_text = true;
        }
    }
    assert!(
        got_error_text,
        "run() must surface the inner stream error as a stderr item"
    );
}

#[tokio::test]
async fn test_spawn_bash_large_output() {
    let n = BUFFER_SIZE * 2 + 100;
    let cmd = format!("printf 'a%.0s' $(seq 1 {n})", n = n);
    let mut stream = spawn_bash(None, ".", &cmd, None);
    let mut total = 0usize;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        total += output.stdout.len();
    }
    assert_eq!(total, n);
}

#[tokio::test]
async fn test_spawn_bash_exit_code() {
    let mut stream = spawn_bash(None, ".", "exit 42", None);
    let mut exit_code = None;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if output.exit_code.is_some() {
            exit_code = output.exit_code;
        }
    }
    assert_eq!(exit_code, Some(42));
}

#[tokio::test]
async fn test_spawn_bash_invalid_env_var_error_kind() {
    let mut stream = spawn_bash(
        Some(vec![("BAD-KEY!".to_string(), "x".to_string())]),
        ".",
        "echo hi",
        None,
    );
    let mut got = false;
    while let Some(result) = stream.next().await {
        if let Err(err) = result {
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            got = true;
            break;
        }
    }
    assert!(got);
}

#[tokio::test]
async fn test_spawn_bash_empty_command() {
    let mut stream = spawn_bash(None, ".", "", None);
    let mut got_output = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if !output.stdout.is_empty() || !output.stderr.is_empty() {
            got_output = true;
        }
    }
    // bash with empty -c produces no output
    assert!(!got_output);
}

#[tokio::test]
async fn test_spawn_bash_exit_code_zero() {
    let mut stream = spawn_bash(None, ".", "exit 0", None);
    let mut exit_code = None;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if output.exit_code.is_some() {
            assert_eq!(output.signal, None);
            exit_code = output.exit_code;
        }
    }
    assert_eq!(exit_code, Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn test_spawn_bash_signal() {
    let mut stream = spawn_bash(None, ".", "kill -KILL $$", None);
    let mut got_signal = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if output.signal.is_some() {
            assert_eq!(output.exit_code, None);
            assert_eq!(output.signal, Some(9));
            got_signal = true;
        }
    }
    assert!(got_signal);
}

#[tokio::test]
async fn test_spawn_bash_exit_code_after_output() {
    let mut stream = spawn_bash(None, ".", "echo hello && echo world && exit 10", None);
    let mut saw_data = false;
    let mut exit_code = None;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if !output.stdout.is_empty() {
            assert_eq!(output.exit_code, None);
            assert_eq!(output.signal, None);
            saw_data = true;
        }
        if output.exit_code.is_some() || output.signal.is_some() {
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
            exit_code = output.exit_code;
        }
    }
    assert!(saw_data);
    assert_eq!(exit_code, Some(10));
}

// ── PTY path tests ────────────────────────────────────────────────────
// With a PTY the terminal line discipline translates `\n` to `\r\n`, so
// output bytes differ from the non-PTY path.  We use `text.contains()`
// instead of exact byte comparison.

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_simple_echo() {
    let mut stream = spawn_bash_pty(None, ".", "echo hello", None);
    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    assert!(!output.is_empty());
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("hello"));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_exit_code() {
    let mut stream = spawn_bash_pty(None, ".", "exit 42", None);
    let mut exit_code = None;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if output.exit_code.is_some() {
            exit_code = output.exit_code;
        }
    }
    assert_eq!(exit_code, Some(42));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_env() {
    let mut stream = spawn_bash_pty(
        Some(vec![("MY_VAR".to_string(), "world".to_string())]),
        ".",
        "echo hello $MY_VAR",
        None,
    );
    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("hello world"));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_invalid_env_var_name() {
    let mut stream = spawn_bash_pty(
        Some(vec![("INVALID-KEY".to_string(), "value".to_string())]),
        ".",
        "echo hello",
        None,
    );
    let mut got_error = false;
    while let Some(result) = stream.next().await {
        if result.is_err() {
            got_error = true;
            break;
        }
    }
    assert!(got_error);
}

#[cfg(unix)]
#[tokio::test]
async fn test_spawn_bash_pty_signal() {
    let mut stream = spawn_bash_pty(None, ".", "kill -KILL $$", None);
    let mut got_signal = false;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        if output.signal.is_some() {
            assert_eq!(output.exit_code, None);
            assert_eq!(output.signal, Some(9)); // SIGKILL
            got_signal = true;
        }
    }
    assert!(got_signal);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_large_output() {
    let n = BUFFER_SIZE * 2 + 100;
    let cmd = format!("printf 'a%.0s' $(seq 1 {n})", n = n);
    let mut stream = spawn_bash_pty(None, ".", &cmd, None);
    let mut raw: Vec<u8> = Vec::new();
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        raw.extend_from_slice(&output.stdout);
    }
    // ConPTY (Windows) interleaves escape sequences and, when a line wraps
    // past the viewport width, re-renders the wrapped segment — so the
    // payload count can slightly exceed `n` but never falls short. Unix
    // PTYs pass the payload through untouched; the count is exact.
    #[cfg(windows)]
    {
        assert!(
            payload_len(&raw) >= n,
            "expected at least {n} payload bytes"
        );
    }
    #[cfg(unix)]
    assert_eq!(raw.len(), n);
}

#[tokio::test]
async fn test_spawn_bash_timeout() {
    let mut stream = spawn_bash(None, ".", "sleep 10", Some(10));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].signal, Some(-1));
    assert_eq!(items[0].exit_code, None);
    assert!(items[0].stdout.is_empty());
    assert!(items[0].stderr.is_empty());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_timeout() {
    let mut stream = spawn_bash_pty(None, ".", "sleep 10", Some(10));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].signal, Some(-1));
    assert_eq!(items[0].exit_code, None);
    assert!(items[0].stdout.is_empty());
    assert!(items[0].stderr.is_empty());
}

#[tokio::test]
async fn test_spawn_bash_timeout_zero() {
    let mut stream = spawn_bash(None, ".", "echo should-not-appear", Some(0));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].signal, Some(-1));
    assert!(items[0].stdout.is_empty());
    assert!(items[0].stderr.is_empty());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_timeout_zero() {
    let mut stream = spawn_bash_pty(None, ".", "echo should-not-appear", Some(0));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].signal, Some(-1));
    assert!(items[0].stdout.is_empty());
    assert!(items[0].stderr.is_empty());
}

#[tokio::test]
async fn test_spawn_bash_timeout_partial_output() {
    let mut stream = spawn_bash(None, ".", "echo hello && sleep 10", Some(200));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert!(!items.is_empty(), "expected at least the timeout item");
    assert_eq!(items.last().unwrap().signal, Some(-1));
    assert_eq!(items.last().unwrap().exit_code, None);
    assert!(items.last().unwrap().stdout.is_empty());
    let all_stdout: Vec<u8> = items.iter().flat_map(|i| i.stdout.clone()).collect();
    assert!(
        all_stdout.starts_with(b"hello\n"),
        "expected 'hello\\n' before timeout, got: {:?}",
        String::from_utf8_lossy(&all_stdout),
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_timeout_partial_output() {
    let mut stream = spawn_bash_pty(None, ".", "echo hello && sleep 10", Some(200));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert!(!items.is_empty(), "expected at least the timeout item");
    assert_eq!(items.last().unwrap().signal, Some(-1));
    assert_eq!(items.last().unwrap().exit_code, None);
    assert!(items.last().unwrap().stdout.is_empty());
    let all_stdout: Vec<u8> = items.iter().flat_map(|i| i.stdout.clone()).collect();
    let text = String::from_utf8_lossy(&all_stdout);
    assert!(
        text.contains("hello"),
        "expected 'hello' before timeout, got: {text:?}",
    );
}

#[tokio::test]
async fn test_spawn_bash_no_timeout_completes_normally() {
    let mut stream = spawn_bash(None, ".", "echo hi", Some(10_000));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert!(!items.is_empty(), "expected output items");
    let last = items.last().unwrap();
    assert_eq!(last.signal, None, "no timeout expected");
    assert_eq!(last.exit_code, Some(0));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_no_timeout_completes_normally() {
    let mut stream = spawn_bash_pty(None, ".", "echo hi", Some(10_000));

    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }

    assert!(!items.is_empty(), "expected output items");
    let last = items.last().unwrap();
    assert_eq!(last.signal, None, "no timeout expected");
    assert_eq!(last.exit_code, Some(0));
}

#[tokio::test]
async fn test_spawn_bash_timeout_signal_exit_code_invariant() {
    let mut stream = spawn_bash(None, ".", "sleep 10", Some(10));

    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        if item.signal == Some(-1) {
            assert_eq!(item.exit_code, None);
        }
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_timeout_signal_exit_code_invariant() {
    let mut stream = spawn_bash_pty(None, ".", "sleep 10", Some(10));

    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        if item.signal == Some(-1) {
            assert_eq!(item.exit_code, None);
        }
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_timeout_large_output_before_timeout() {
    let n = BUFFER_SIZE * 2 + 50;
    let cmd = format!("printf 'a%.0s' $(seq 1 {n}) && sleep 10", n = n,);
    let mut stream = spawn_bash_pty(None, ".", &cmd, Some(200));

    let mut raw: Vec<u8> = Vec::new();
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        if item.signal == Some(-1) {
            assert_eq!(item.exit_code, None);
        } else {
            raw.extend_from_slice(&item.stdout);
        }
    }

    // ConPTY (Windows) interleaves escape sequences and, when a line wraps
    // past the viewport width, re-renders the wrapped segment — so the
    // payload count can slightly exceed `n` but never falls short. Unix
    // PTYs pass the payload through untouched; the count is exact.
    #[cfg(windows)]
    {
        assert!(payload_len(&raw) >= n, "expected all pre-sleep output");
    }
    #[cfg(unix)]
    {
        assert!(!raw.is_empty(), "expected some data before PTY timeout");
        assert_eq!(raw.len(), n, "expected all pre-sleep output");
    }
}

// ---------------------------------------------------------------------------
// Non-interactive environment defaults (pager/editor hang prevention)
// ---------------------------------------------------------------------------
//
// A command like `git log` can hang forever when it launches an interactive
// pager (`less`) — most commonly on the PTY path, where stdout is a real
// terminal. These tests pin the default env injection in both spawn paths:
// defaults must be present, and caller-provided env must win over them.

async fn collect_all<S>(stream: &mut S) -> Vec<crate::bash::SpawnOutput>
where
    S: tokio_stream::Stream<Item = Result<crate::bash::SpawnOutput, std::io::Error>> + Unpin,
{
    let mut items = vec![];
    while let Some(result) = stream.next().await {
        items.push(result.unwrap());
    }
    items
}

fn joined_stdout(items: &[crate::bash::SpawnOutput]) -> String {
    String::from_utf8_lossy(
        &items
            .iter()
            .flat_map(|i| i.stdout.clone())
            .collect::<Vec<u8>>(),
    )
    .to_string()
}

#[tokio::test]
async fn test_spawn_bash_injects_pager_defaults() {
    // If PAGER/GIT_PAGER/MANPAGER were unset, `git log` inside a repo could
    // launch an interactive pager and hang until the timeout. The injected
    // defaults must be visible to the child on the piped path.
    let mut stream = spawn_bash(
        None,
        ".",
        "env | grep -E '^(PAGER|GIT_PAGER|MANPAGER)='",
        None,
    );
    let items = collect_all(&mut stream).await;
    let stdout = joined_stdout(&items);

    assert!(stdout.contains("PAGER=cat"), "stdout was: {stdout:?}");
    assert!(stdout.contains("GIT_PAGER=cat"), "stdout was: {stdout:?}");
    assert!(stdout.contains("MANPAGER=cat"), "stdout was: {stdout:?}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_injects_pager_defaults() {
    // Same guarantee on the PTY path — this is the path where `git log`
    // actually launched `less` and blocked until the 10-minute timeout.
    let mut stream = spawn_bash_pty(
        None,
        ".",
        "env | grep -E '^(PAGER|GIT_PAGER|MANPAGER)='",
        None,
    );
    let items = collect_all(&mut stream).await;
    let stdout = joined_stdout(&items);

    assert!(stdout.contains("PAGER=cat"), "stdout was: {stdout:?}");
    assert!(stdout.contains("GIT_PAGER=cat"), "stdout was: {stdout:?}");
    assert!(stdout.contains("MANPAGER=cat"), "stdout was: {stdout:?}");
}

#[tokio::test]
async fn test_spawn_bash_caller_env_overrides_pager_defaults() {
    // A caller-provided value must win over the injected default.
    let mut stream = spawn_bash(
        Some(vec![("PAGER".to_string(), "my-custom-pager".to_string())]),
        ".",
        "echo \"$PAGER\"",
        None,
    );
    let items = collect_all(&mut stream).await;
    let stdout = joined_stdout(&items);

    assert!(stdout.contains("my-custom-pager"), "stdout was: {stdout:?}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_caller_env_overrides_pager_defaults() {
    let mut stream = spawn_bash_pty(
        Some(vec![(
            "GIT_PAGER".to_string(),
            "my-custom-pager".to_string(),
        )]),
        ".",
        "echo \"$GIT_PAGER\"",
        None,
    );
    let items = collect_all(&mut stream).await;
    let stdout = joined_stdout(&items);

    assert!(stdout.contains("my-custom-pager"), "stdout was: {stdout:?}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_spawn_bash_pty_editor_defaults_no_hang() {
    // Editor defaults must be injected on the PTY path too: GIT_EDITOR=true
    // makes `git commit` and friends exit immediately instead of opening an
    // interactive editor. Pinned via plain `env` output (no git dependency
    // so the test also passes on images without git installed).
    let mut stream = spawn_bash_pty(None, ".", "echo \"$GIT_EDITOR $EDITOR $VISUAL\"", None);
    let items = collect_all(&mut stream).await;
    let stdout = joined_stdout(&items);

    assert!(stdout.contains("true true true"), "stdout was: {stdout:?}");
}

// ---------------------------------------------------------------------------
// Input-wait watchdog (Unix PTY path)
// ---------------------------------------------------------------------------
//
// A live child holding the terminal in raw (non-canonical) mode is waiting
// for keystrokes the harness can never send (interactive pager/editor/TUI;
// bash's `read -n` has the same signature). Environment variables cannot
// fix that — the pager reads /dev/tty directly — so the watchdog kills the
// child after a sustained raw-mode grace window and emits a diagnostic.

/// Collect stdout/stderr and record total stream duration.
#[cfg(unix)]
async fn collect_watchdog_run(command: &str) -> (String, String, Duration) {
    let started = Instant::now();
    let mut stream = spawn_bash_pty(None, ".", command, None);
    let mut out = String::new();
    let mut err = String::new();
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        out.push_str(&String::from_utf8_lossy(&item.stdout));
        err.push_str(&String::from_utf8_lossy(&item.stderr));
    }
    (out, err, started.elapsed())
}

#[cfg(unix)]
#[tokio::test]
async fn test_watchdog_kills_interactive_pager_hang() {
    // bash `read -n 1` flips the PTY to raw mode and blocks forever waiting
    // for a keystroke — the same terminal-mode signature as `less`. With
    // the watchdog this run must end shortly after the 2s grace window
    // (well before the 10-minute default timeout) and carry the diagnostic.
    let (_out, err, elapsed) = collect_watchdog_run("read -n 1 x").await;

    assert!(
        err.contains("input-wait watchdog"),
        "expected watchdog diagnostic, stderr was: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "watchdog took too long to release the hang: {elapsed:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_watchdog_kills_explicit_less_hang() {
    // The original incident: an explicit `| less` with long output blocks
    // forever inside the PTY. The watchdog must free the terminal. Guarded
    // on `less` availability so minimal CI images without a pager still
    // pass (the `read -n 1` test covers the same watchdog path without the
    // external dependency).
    if std::process::Command::new("sh")
        .arg("-c")
        .arg("command -v less >/dev/null 2>&1")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: `less` not installed");
        return;
    }

    let (_out, err, elapsed) = collect_watchdog_run("seq 1 1000 | less").await;

    assert!(
        err.contains("input-wait watchdog"),
        "expected watchdog diagnostic, stderr was: {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "watchdog took too long to release the hang: {elapsed:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_watchdog_does_not_kill_canonical_sleep() {
    // A legitimate long-running command stays in canonical mode while
    // alive; the watchdog must NOT kill it. Run `sleep 4` — past the 2s
    // grace window — and require it to survive until natural completion.
    let (out, err, elapsed) = collect_watchdog_run("sleep 4 && echo survived").await;

    assert!(
        out.contains("survived"),
        "sleep must complete normally, stdout was: {out:?}"
    );
    assert!(
        !err.contains("input-wait watchdog"),
        "watchdog must not fire for canonical-mode waits, stderr was: {err:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(4),
        "sleep was cut short: {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Auto-fix: deterministic paged-command rewrites (pre-spawn)
// ---------------------------------------------------------------------------
//
// The rewrite layer only fires when 100% certain; everything ambiguous must
// pass through UNCHANGED (the input-wait watchdog stays the backstop). The
// table below pins both the rewrites and the must-not-touch cases.

#[cfg(any(unix, windows))]
#[test]
fn test_auto_fix_table() {
    let cases: &[(&str, &str, bool)] = &[
        // --- rewrites expected ---
        ("git log", "git --no-pager log", true),
        ("git diff HEAD~1", "git --no-pager diff HEAD~1", true),
        (
            "git -c core.abbrev=8 log --oneline",
            "git --no-pager -c core.abbrev=8 log --oneline",
            true,
        ),
        ("FOO=bar git log", "FOO=bar git --no-pager log", true),
        ("seq 1 1000 | less", "seq 1 1000 | cat", true),
        ("git log | less", "git log | cat", true),
        ("cat big.txt | more", "cat big.txt | cat", true),
        ("seq 1 1000 | most", "seq 1 1000 | cat", true),
        ("less file.txt", "cat file.txt", true),
        ("more a.txt b.txt", "cat a.txt b.txt", true),
        ("seq 1 1000 | less -X", "seq 1 1000 | cat", true),
        // --- must NOT be rewritten (fallback: watchdog) ---
        // Regression (review R1): the pipeline rule must never cross a
        // newline — a second command or a heredoc body would be deleted.
        (
            "echo hi | less\necho done",
            "echo hi | less\necho done",
            false,
        ),
        (
            "cat <<EOF | less\nline\nEOF",
            "cat <<EOF | less\nline\nEOF",
            false,
        ),
        // Regression (review Y1): a trailing redirect must never be
        // swallowed by the `| cat` replacement.
        ("echo a | less > f", "echo a | less > f", false),
        // Regression (review Y2): an explicit git pager opt-in must win —
        // git's --paginate/--no-pager are last-wins, so inserting
        // --no-pager before it would NOT fix anything while the note
        // claimed it did.
        ("git --paginate log", "git --paginate log", false),
        ("cat less", "cat less", false),
        ("grep less file.txt", "grep less file.txt", false),
        (
            "git config core.pager less",
            "git config core.pager less",
            false,
        ),
        ("echo git log", "echo git log", false),
        ("echo \"git log\"", "echo \"git log\"", false),
        ("bash -c 'git log'", "bash -c 'git log'", false),
        ("less -X file.txt", "less -X file.txt", false),
        ("less", "less", false),
        ("top", "top", false),
        ("git status", "git status", false),
        ("git log | head", "git log | head", false),
        ("git --no-pager log", "git --no-pager log", false),
        ("git log && less file", "git log && less file", false),
        ("vim file.txt", "vim file.txt", false),
        ("read -n 1 x", "read -n 1 x", false),
        ("cat file | grep less", "cat file | grep less", false),
        ("echo done; less file", "echo done; less file", false),
        ("less file | grep x", "less file | grep x", false),
    ];

    for (input, expected_cmd, expect_note) in cases {
        let (cmd, note) = auto_fix_command(input);
        assert_eq!(&cmd, expected_cmd, "command rewrite wrong for {input:?}");
        assert_eq!(
            note.is_some(),
            *expect_note,
            "note presence wrong for {input:?} (note was {note:?})"
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn test_auto_fix_composite_git_piped_pager_single_rule() {
    // `git log | less`: git is piped, so only the pipeline rule applies —
    // one rewrite, no `--no-pager` noise.
    let (cmd, note) = auto_fix_command("git log | less");
    assert_eq!(cmd, "git log | cat");
    let note = note.expect("expected a note");
    assert!(
        !note.contains("--no-pager"),
        "pipeline case must not also insert --no-pager: {note}"
    );
    assert!(note.contains("auto-fix:"), "note was: {note}");
}

#[cfg(any(unix, windows))]
#[test]
fn test_auto_fix_adversarial_matrix() {
    // Adversarial battery, permanently persisted (originally run as a
    // one-off review probe): every "false" entry MUST pass through
    // unchanged — these are the inputs a naive regex-based rewriter gets
    // wrong. Each line documents WHY it must not be rewritten.
    let must_not_rewrite: &[(&str, &str)] = &[
        // `less`/`most` as a substring of a longer word (word boundary).
        ("x | lessful", "word boundary: `lessful` is not `less`"),
        (
            "x | guiless",
            "word boundary: suffix `less` is not the pager",
        ),
        ("x | Less", "case-sensitive: `Less` is not the pager"),
        (
            "x | less-X",
            "word boundary: `less-X` is a single token, not pager+flag",
        ),
        // Pager with a positional argument: less would IGNORE stdin, so
        // `| cat` is NOT semantically equivalent — must not rewrite.
        (
            "x | less -S extra-arg",
            "positional arg: less would ignore stdin",
        ),
        ("x | most -N 5", "positional numeric arg"),
        ("x | more --SOME-ARG 7", "positional arg after flags"),
        // Flag-looking token glued to the pager without whitespace.
        ("x | less-X -y", "`less-X` is one token"),
        // Compound tail: rewriting one segment of a compound command is
        // not a full fix — the other segment could still block.
        ("x | less -X;", "compound tail `;`"),
        ("x | less -X &", "compound tail `&`"),
        ("x | less -X | wc", "not the FINAL segment (inner `|`)"),
        // Quoting gate: any quote disqualifies the whole line.
        ("echo \"git log\"", "quoted literal"),
        ("bash -c 'git log'", "single-quoted literal"),
        ("x | less `foo`", "backtick command substitution"),
        // Env-prefix edge cases that must NOT shift the insertion point.
        (
            "git --paginate log",
            "explicit pager opt-in wins (last-wins)",
        ),
        (
            "git -c key=a b.log",
            "value-consumed flag: `b.log` is not a subcommand",
        ),
        (
            "git --exec-path /path log",
            "value flag outside the enumerated list",
        ),
        ("man git log", "`git` is not the invoked command"),
        ("bash git log", "`git` is an argument"),
        ("gti log", "typo'd command is not git"),
        ("git logg", "not a paged subcommand"),
    ];

    for (input, why) in must_not_rewrite {
        let (cmd, note) = auto_fix_command(input);
        assert_eq!(
            &cmd, input,
            "must NOT be rewritten ({why}): {input:?} → {cmd:?}"
        );
        assert!(note.is_none(), "unexpected note for {input:?}: {note:?}");
    }

    // Companion positives pinned right next to the negatives above so the
    // table stays honest about what SHOULD fire.
    let must_rewrite: &[(&str, &str)] = &[
        (
            "git diff --cached --stat",
            "git --no-pager diff --cached --stat",
        ),
        ("x | less --follow-name", "x | cat"),
        ("x | less -X -S", "x | cat"),
        (
            "FOO=bar BAR=a.b git log",
            "FOO=bar BAR=a.b git --no-pager log",
        ),
        ("git -c key=val log", "git --no-pager -c key=val log"),
        ("git -C /repo log", "git --no-pager -C /repo log"),
        (
            "git --git-dir=x --work-tree=y log",
            "git --no-pager --git-dir=x --work-tree=y log",
        ),
        ("git show $(seq 1 3)", "git --no-pager show $(seq 1 3)"),
    ];

    for (input, expected) in must_rewrite {
        let (cmd, note) = auto_fix_command(input);
        assert_eq!(&cmd, expected, "must be rewritten: {input:?}");
        assert!(note.is_some(), "missing note for {input:?}");
    }
}

#[cfg(any(unix, windows))]
#[test]
fn test_auto_fix_note_is_single_line_joined() {
    // If several rules ever apply, the note must be one joined line, not
    // multiple stream items.
    let (_cmd, note) = auto_fix_command("less file.txt");
    let note = note.unwrap();
    assert!(note.starts_with("auto-fix: "));
    assert!(!note.contains('\n'));
}

#[cfg(unix)]
#[tokio::test]
async fn test_run_pty_autofix_emits_note_and_full_output() {
    // Integration through `run`: the rewritten command must actually run
    // (full 1000-line output, no watchdog), and the auto-fix note must be
    // the first stderr item so the model knows the command was rewritten.
    // `run` returns Result but BashError has no Debug impl; discard the
    // error variant via `ok()` and expect on the Option instead.
    let mut stream = crate::bash::bsh::run(None, &None, true, "seq 1 1000 | less", ".")
        .ok()
        .expect("run should accept the command");
    let mut stderr = String::new();
    let mut stdout_len = 0usize;
    let mut saw_note = false;
    while let Some(item) = stream.next().await {
        let item: crate::bash::SpawnOutput = item;
        let err_text = String::from_utf8_lossy(&item.stderr).to_string();
        if !saw_note && err_text.contains("auto-fix:") {
            saw_note = true;
        } else {
            stderr.push_str(&err_text);
        }
        stdout_len += item.stdout.len();
    }

    assert!(saw_note, "missing auto-fix note; stderr was: {stderr:?}");
    assert!(
        !stderr.contains("input-wait watchdog"),
        "watchdog must not fire for an auto-fixed command; stderr was: {stderr:?}"
    );
    // All 1000 numbers × ~4 bytes + newlines: decisively more than the
    // single viewport the watchdog path would have preserved.
    assert!(
        stdout_len > 4000,
        "expected the FULL pipeline output (not a single viewport), got {stdout_len} bytes"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_run_pty_ambiguous_command_falls_back_to_watchdog() {
    // `less` with a flag is not 100% certain → no rewrite → the run must
    // fall back to the current behavior: the watchdog kills it and the
    // diagnostic (description only, no command suggestions) is emitted.
    let mut stream = crate::bash::bsh::run(None, &None, true, "less -X /etc/hostname", ".")
        .ok()
        .expect("run should accept the command");
    let mut stderr = String::new();
    while let Some(item) = stream.next().await {
        stderr.push_str(&String::from_utf8_lossy(&item.stderr));
    }

    assert!(
        !stderr.contains("auto-fix:"),
        "ambiguous command must not be rewritten; stderr was: {stderr:?}"
    );
    assert!(
        stderr.contains("input-wait watchdog"),
        "expected the watchdog diagnostic; stderr was: {stderr:?}"
    );
    assert!(
        !stderr.contains("--no-pager") && !stderr.contains("e.g."),
        "watchdog diagnostic must not suggest commands; stderr was: {stderr:?}"
    );
}
