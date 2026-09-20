use super::super::events::HarnessEvent;
use super::super::tools::{CoshTools, Tools};
use serde_json::json;
use std::time::Duration;

fn make_tools() -> CoshTools {
    CoshTools::new(".")
}

#[tokio::test]
async fn bash_run_returns_stdout() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "echo hello world"}))
        .await
        .unwrap();
    assert_eq!(result, "hello world\n");
}

#[tokio::test]
async fn bash_run_includes_stderr() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "echo error_msg >&2"}))
        .await
        .unwrap();
    assert!(
        result.contains("error_msg"),
        "expected stderr in output, got: {result:?}"
    );
}

#[tokio::test]
async fn bash_run_exit_code_non_zero() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "exit 42"}))
        .await
        .unwrap();
    assert!(
        result.contains("exit code: 42"),
        "expected 'exit code: 42', got: {result:?}"
    );
}

#[tokio::test]
async fn bash_run_multiple_chunks_accumulated() {
    let tools = make_tools();
    // Generate output larger than BUFFER_SIZE (4096) to trigger multiple chunks
    let n = 10_000;
    let cmd = format!("printf 'a%.0s' $(seq 1 {n})");
    let result = tools
        .dispatch("bash_run", json!({"command": cmd}))
        .await
        .unwrap();
    assert_eq!(
        result.len(),
        n,
        "expected {n} chars of output, got {} chars",
        result.len()
    );
    assert!(result.chars().all(|c| c == 'a'), "expected all 'a's");
}

#[tokio::test]
async fn bash_run_exit_code_zero_not_included() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "echo ok"}))
        .await
        .unwrap();
    // Zero exit code should not appear in output
    assert!(
        !result.contains("exit code"),
        "expected no exit code for success, got: {result:?}"
    );
}

#[tokio::test]
async fn bash_run_command_not_found() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "nonexistent_command_xyz123"}))
        .await;
    assert!(
        result.is_ok(),
        "command not found should still return ok with stderr: {result:?}"
    );
    let output = result.unwrap();
    assert!(
        !output.is_empty(),
        "expected stderr output for missing command"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_run_signal_killed() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "kill -KILL $$"}))
        .await
        .unwrap();
    assert!(
        result.contains("signal: 9"),
        "expected signal 9, got: {result:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bash_run_absolute_path_rejected() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "/bin/ls"}))
        .await;
    assert!(
        result.is_err(),
        "absolute path should be rejected: {result:?}"
    );
}

#[tokio::test]
async fn bash_run_dangerous_pattern_rejected() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "rm -rf /"}))
        .await;
    assert!(
        result.is_err(),
        "dangerous pattern should be rejected: {result:?}"
    );
}

#[tokio::test]
async fn bash_run_ls_output_is_complete() {
    let tools = make_tools();
    let result = tools
        .dispatch("bash_run", json!({"command": "ls"}))
        .await
        .unwrap();
    assert!(!result.is_empty(), "ls should produce output");
    assert!(
        result.contains("Cargo.toml"),
        "expected Cargo.toml in ls output, got: {result:?}"
    );
    assert!(
        result.lines().count() >= 5,
        "expected at least 5 lines from ls, got {}: {:?}",
        result.lines().count(),
        result,
    );
}

/// The harness runs bash through a PTY so line-buffered programs (python,
/// make, …) stream incrementally instead of block-buffering into a single
/// dump at exit. This test asserts the PTY contract end-to-end through
/// `dispatch`: the full output arrives, cleaned of ANSI escapes / CRLF.
#[tokio::test]
async fn bash_run_pty_output_is_clean_text() {
    let tools = make_tools();
    // `printf` with an explicit escape sequence proves ANSI stripping; a
    // piped child would deliver it raw.
    let result = tools
        .dispatch(
            "bash_run",
            json!({"command": "printf 'plain\\033[31mred\\033[0mplain\\n'"}),
        )
        .await
        .unwrap();
    assert_eq!(
        result, "plainredplain\n",
        "ANSI escapes must be stripped from PTY output, got: {result:?}"
    );
}

/// Streaming regression guard: a long-running command that emits one line
/// per second must reach the TUI line by line (via `event_tx`), not as a
/// single block when the process exits. Requires a PTY (the piped path
/// block-buffers in the child).
#[cfg(unix)]
#[tokio::test]
async fn bash_run_streams_lines_incrementally() {
    use std::time::Instant;

    let tools = make_tools();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tools_mut = tools;
    tools_mut.set_event_tx(tx);

    let started = Instant::now();
    // Run the dispatch on a task so we can consume streamed events while it
    // is still running.
    let handle = tokio::spawn(async move {
        tools_mut
            .dispatch(
                "bash_run",
                json!({"command": "for i in 1 2 3; do echo line-$i; sleep 1; done"}),
            )
            .await
    });

    let mut first_line_at: Option<Duration> = None;
    let mut streamed_lines = 0usize;
    while streamed_lines < 3 {
        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("streamed output should arrive well before 10s per line")
            .expect("event channel open while dispatch runs");
        let HarnessEvent::ToolOutput { output, .. } = event else {
            continue;
        };
        if first_line_at.is_none() {
            first_line_at = Some(started.elapsed());
        }
        streamed_lines += output.lines().filter(|l| l.contains("line-")).count();
    }

    let first = first_line_at.expect("at least one streamed line");
    // If the child were block-buffered (piped mode), all 3 lines would only
    // arrive at ~3s. Line-by-line streaming puts the FIRST line at ~0-1s.
    assert!(
        first < Duration::from_millis(2000),
        "first streamed line arrived at {first:?}; expected incremental PTY streaming"
    );
    assert!(
        handle.await.unwrap().is_ok(),
        "dispatch must complete after streaming"
    );
}

/// Chunk-boundary regression: PTY reads are raw 4096-byte slices, so escape
/// sequences and multi-byte UTF-8 chars can split across chunk boundaries.
/// The carry-buffer loop must reassemble them: the accumulated output must
/// contain zero escape residue (`\x1b` fragments like a bare "1m") and zero
/// U+FFFD replacement chars.
#[cfg(unix)]
#[tokio::test]
async fn bash_run_chunk_boundaries_do_not_corrupt_output() {
    let tools = make_tools();
    // ~12 KB of ANSI-colored non-ASCII lines: 3 full BUFFER_SIZE chunks and
    // change, guaranteeing boundaries land mid-escape and mid-char somewhere.
    // The model-facing result is head/tail-truncated (see `truncate`), so the
    // corruption assertions run on whatever whole lines survived.
    let result = tools
        .dispatch(
            "bash_run",
            json!({"command": "awk 'BEGIN{for(i=0;i<300;i++) printf \"\\033[32mlinha-%d-çãо汉字\\033[0m\\n\", i}'"}),
        )
        .await
        .unwrap();

    assert!(
        !result.contains('\u{FFFD}'),
        "multi-byte chars split across chunks must not corrupt (found U+FFFD)"
    );
    assert!(
        !result.contains('\x1b'),
        "escape sequences must be fully stripped, not split into residue"
    );
    assert!(
        result.contains("linha-0-"),
        "the first line must survive truncation, got: {result:?}"
    );
    assert!(
        result.contains("linha-299-"),
        "the last line must survive truncation, got: {result:?}"
    );
    // Corruption invariants (the truncation notice's own framing lines are
    // legitimate formatting): no escape residue, no U+FFFD, and the kept
    // payload lines must be whole.
}

/// stdin regression: in PTY mode the child's stdin is the PTY slave and
/// nothing feeds it, so a command that reads stdin (`cat`, `ssh`, a prompt)
/// blocks until reaped — the piped path got EOF from `Stdio::null()`. The
/// harness guards this with a default 600s timeout (the mechanism is covered
/// by `test_spawn_bash_pty_timeout*` at the stream level); here we assert a
/// bounded stdin-reader still terminates promptly instead of wedging.
#[cfg(unix)]
#[tokio::test]
async fn bash_run_stdin_reader_terminates() {
    use std::time::Duration;
    use std::time::Instant;

    let tools = make_tools();
    let started = Instant::now();
    // `timeout 2` kills the reading `cat` after 2s; the invariant is that
    // the dispatch COMPLETES (the PTY does not deadlock the kill/reap path).
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        tools.dispatch("bash_run", json!({"command": "timeout 2 cat"})),
    )
    .await
    .expect("a bounded stdin-reading command must not wedge the dispatch");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "expected termination at ~2s (the kill path works on a PTY), took {:?}",
        started.elapsed()
    );
    let _ = result;
}
