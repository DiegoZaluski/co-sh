#[allow(unused_imports)]
use super::bsh::spawn_bash;
#[cfg(any(unix, windows))]
#[allow(unused_imports)]
use super::bsh::spawn_bash_pty;
#[allow(unused_imports)]
use tokio_stream::StreamExt;
#[allow(dead_code)]
const BUFFER_SIZE: usize = 4096;

/// Count of payload bytes (`a`) after stripping ANSI escape sequences —
/// used by PTY tests on Windows, where ConPTY interleaves escape sequences
/// and viewport re-render artifacts with the payload.
#[allow(dead_code)]
fn payload_len(bytes: &[u8]) -> usize {
    let stripped = strip_ansi_escapes::strip(bytes.as_ref() as &[_]);
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
        assert!(payload_len(&raw) >= n, "expected at least {n} payload bytes");
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
        assert!(raw.len() > 0, "expected some data before PTY timeout");
        assert_eq!(raw.len(), n, "expected all pre-sleep output");
    }
}
