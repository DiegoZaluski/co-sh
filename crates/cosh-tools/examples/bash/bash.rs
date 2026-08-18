//! Demonstrate `bash`: the streaming output contract — stdout/stderr chunks
//! then a single exit-status item — plus environment variables, working
//! directory, timeouts (with and without partial output), the security
//! validation that rejects absolute paths and dangerous patterns, and PTY
//! mode.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example bash
//! ```

use cosh_tools::bash::Bash;
use cosh_tools::bash::bsh::SpawnOutput;
use std::pin::Pin;
use tokio_stream::{Stream, StreamExt};

// `BashError` has no `Debug`, so unwrap via a helper that reports the reason.
fn expect_ok<'a>(
    bash: &'a Bash,
    command: &'a str,
) -> Pin<Box<dyn Stream<Item = SpawnOutput> + Send + 'a>> {
    match bash.run(command) {
        Ok(stream) => stream,
        Err(e) => panic!("command should pass validation: {:?}", e.text_err),
    }
}

// Accumulate a command's stream into harness-style text: stdout and stderr
// interleaved as they arrive, a trailing "exit code:" for non-zero exits,
// and "signal:" for signal deaths and timeouts.
async fn run_and_render(bash: &Bash, command: &str) -> String {
    let mut stream = expect_ok(bash, command);
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        out.push_str(&String::from_utf8_lossy(&chunk.stdout));
        out.push_str(&String::from_utf8_lossy(&chunk.stderr));
        if let Some(code) = chunk.exit_code
            && code != 0
        {
            out.push_str(&format!("exit code: {code}\n"));
        }
        if let Some(sig) = chunk.signal {
            out.push_str(&format!("signal: {sig}\n"));
        }
    }
    out
}

fn show(label: &str, rendered: &str) {
    println!("== {label} ==");
    for line in rendered.lines() {
        println!("  {line}");
    }
    println!();
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("cosh-bash-example");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello from cwd\n").unwrap();

    let bash = Bash::new().cwd(dir.to_string_lossy());

    // 1. stdout: chunks first, then a final status item. A zero exit code is
    //    silent (the harness only prints non-zero codes).
    show("echo", &run_and_render(&bash, "echo hello world").await);

    // 2. stderr is its own stream: it arrives as stderr chunks, not mixed
    //    into stdout.
    show("stderr", &run_and_render(&bash, "echo boom >&2").await);

    // 3. A non-zero exit code is appended by the harness.
    show("non-zero exit", &run_and_render(&bash, "exit 42").await);

    // 4. A signal death: killed by SIGKILL (9); exit_code is None.
    show(
        "killed by signal",
        &run_and_render(&bash, "kill -KILL $$").await,
    );

    // 5. Environment variables are set on the child.
    let bash_env = Bash::new()
        .cwd(dir.to_string_lossy())
        .env(Some(vec![("GREETING".into(), "bonjour".into())]));
    show(
        "env var",
        &run_and_render(&bash_env, "echo $GREETING").await,
    );

    // 6. The working directory is where the command runs.
    show("cwd", &run_and_render(&bash, "cat notes.txt").await);

    // 7. Timeout with partial output: the printed prefix survives, then a
    //    final `signal: -1` item arrives.
    let bash_timed = Bash::new().cwd(dir.to_string_lossy()).timeout(300);
    show(
        "timeout (partial output kept)",
        &run_and_render(&bash_timed, "echo partial && sleep 10").await,
    );

    // 8. A zero timeout kills before the child can write anything — exactly
    //    one deterministic item, `signal: -1`.
    let bash_zero = Bash::new().cwd(dir.to_string_lossy()).timeout(0);
    show("timeout 0", &run_and_render(&bash_zero, "echo never").await);

    // 9. Security: an absolute command path is rejected before spawning.
    let err = match bash.run("/bin/echo hi") {
        Err(e) => e,
        Ok(_) => panic!("absolute path must fail"),
    };
    println!("== absolute path rejected ==");
    println!("  error: {}", err.text_err.unwrap_or_default());
    println!();

    // 10. Security: a dangerous pattern is rejected with the matched regex.
    let err = match bash.run("rm -rf /") {
        Err(e) => e,
        Ok(_) => panic!("dangerous pattern must fail"),
    };
    println!("== dangerous pattern rejected ==");
    println!("  error: {}", err.text_err.unwrap_or_default());
    println!();

    // 11. Large output streams in 4096-byte chunks; a full buffer is flagged
    //     `truncated: true` as a "more coming" hint.
    let mut stream = expect_ok(&bash, "seq 1 10000");
    let (mut bytes, mut full_chunks) = (0usize, 0usize);
    while let Some(chunk) = stream.next().await {
        bytes += chunk.stdout.len();
        full_chunks += usize::from(chunk.truncated);
    }
    println!("== large output streaming ==");
    println!("  total bytes: {bytes}, chunks flagged truncated: {full_chunks}");
    println!();

    // 12. PTY mode multiplexes stdout/stderr into the terminal; `\n` becomes
    //     `\r\n` and stderr is always empty.
    let bash_pty = Bash::new().cwd(dir.to_string_lossy()).pty(true);
    let mut stream = expect_ok(&bash_pty, "echo hello");
    let mut raw = Vec::new();
    while let Some(chunk) = stream.next().await {
        raw.extend_from_slice(&chunk.stdout);
    }
    println!("== PTY mode ==");
    println!("  raw bytes: {:?}", String::from_utf8_lossy(&raw));
    println!(
        "  contains \"hello\": {}",
        String::from_utf8_lossy(&raw).contains("hello")
    );
    println!();
}
