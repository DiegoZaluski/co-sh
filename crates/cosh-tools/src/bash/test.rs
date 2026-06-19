#[allow(unused_imports)]
use super::bsh::spawn_bash;
#[allow(unused_imports)]
use tokio_stream::StreamExt;
#[allow(dead_code)]
const BUFFER_SIZE: usize = 4096;

#[tokio::test]
async fn test_spawn_bash_simple_echo() {
    let stream = spawn_bash(None, ".", "echo hello");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "echo error >&2");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "echo out; echo err >&2");
    tokio::pin!(stream);

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
    let stream = spawn_bash(
        Some(vec![("MY_VAR".to_string(), "hello".to_string())]),
        ".",
        "echo $MY_VAR",
    );
    tokio::pin!(stream);

    let mut output = vec![];
    while let Some(result) = stream.next().await {
        let item = result.unwrap();
        output.extend_from_slice(&item.stdout);
    }
    assert_eq!(output, b"hello\n");
}

#[tokio::test]
async fn test_spawn_bash_invalid_env_var_name() {
    let stream = spawn_bash(
        Some(vec![("INVALID-KEY".to_string(), "value".to_string())]),
        ".",
        "echo hello",
    );
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, "/nonexistent-path-12345", "echo hello");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", &cmd);
    tokio::pin!(stream);

    let mut total = 0usize;
    while let Some(result) = stream.next().await {
        let output = result.unwrap();
        total += output.stdout.len();
    }
    assert_eq!(total, n);
}

#[tokio::test]
async fn test_spawn_bash_exit_code() {
    let stream = spawn_bash(None, ".", "exit 42");
    tokio::pin!(stream);

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
    let stream = spawn_bash(
        Some(vec![("BAD-KEY!".to_string(), "x".to_string())]),
        ".",
        "echo hi",
    );
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "exit 0");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "kill -KILL $$");
    tokio::pin!(stream);

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
    let stream = spawn_bash(None, ".", "echo hello && echo world && exit 10");
    tokio::pin!(stream);

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
