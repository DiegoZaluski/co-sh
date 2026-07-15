use super::super::tools::{CoshTools, Tools};
use serde_json::json;

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
