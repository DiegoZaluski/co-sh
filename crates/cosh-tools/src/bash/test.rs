#[allow(unused_imports)]
use super::bsh::spawn_bash;
#[allow(unused_imports)]
use super::bsh::spawn_bash_pty;
#[allow(unused_imports)]
use tokio_stream::StreamExt;
#[allow(dead_code)]
const BUFFER_SIZE: usize = 4096;

#[tokio::test]
async fn test_spawn_bash_simple_echo() {
    let mut stream = spawn_bash(None, ".", "echo hello");

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
    let mut stream = spawn_bash(None, ".", "echo error >&2");
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
    let mut stream = spawn_bash(None, ".", "echo out; echo err >&2");
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
    let mut stream = spawn_bash(None, "/nonexistent-path-12345", "echo hello");
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
    let mut stream = spawn_bash(None, ".", &cmd);
    let mut total = 0usize;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        total += output.stdout.len();
    }
    assert_eq!(total, n);
}

#[tokio::test]
async fn test_spawn_bash_exit_code() {
    let mut stream = spawn_bash(None, ".", "exit 42");
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
    );
    let mut got = false;
    while let Some(result) = stream.next().await {
        match result {
            Err(err) => {
                assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
                got = true;
                break;
            }
            Ok(_) => {}
        }
    }
    assert!(got);
}

#[tokio::test]
async fn test_spawn_bash_empty_command() {
    let mut stream = spawn_bash(None, ".", "");
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
    let mut stream = spawn_bash(None, ".", "exit 0");
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
    let mut stream = spawn_bash(None, ".", "kill -KILL $$");
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
    let mut stream = spawn_bash(None, ".", "echo hello && echo world && exit 10");
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
// output bytes differ from the non-PTY path.  We trim whitespace before
// comparing text content.

#[tokio::test]
async fn test_spawn_bash_pty_simple_echo() {
    let mut stream = spawn_bash_pty(None, ".", "echo hello");
    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    assert!(!output.is_empty());
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("hello"));
}

#[tokio::test]
async fn test_spawn_bash_pty_exit_code() {
    let mut stream = spawn_bash_pty(None, ".", "exit 42");
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
async fn test_spawn_bash_pty_env() {
    let mut stream = spawn_bash_pty(
        Some(vec![("MY_VAR".to_string(), "world".to_string())]),
        ".",
        "echo hello $MY_VAR",
    );
    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("hello world"));
}

#[tokio::test]
async fn test_spawn_bash_pty_invalid_env_var_name() {
    let mut stream = spawn_bash_pty(
        Some(vec![("INVALID-KEY".to_string(), "value".to_string())]),
        ".",
        "echo hello",
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
    let mut stream = spawn_bash_pty(None, ".", "kill -KILL $$");
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

#[tokio::test]
async fn test_spawn_bash_pty_large_output() {
    let n = BUFFER_SIZE * 2 + 100;
    let cmd = format!("printf 'a%.0s' $(seq 1 {n})", n = n);
    let mut stream = spawn_bash_pty(None, ".", &cmd);
    let mut total = 0usize;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        total += output.stdout.len();
    }
    assert_eq!(total, n);
}
