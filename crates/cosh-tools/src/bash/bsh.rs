//! Bash executor with security pattern validation and async streaming output.
//!
//! Provides [`run`] and [`spawn_bash`] for executing bash commands as child
//! processes, returning output as an async stream of chunks. Commands are
//! validated against a set of dangerous patterns (recursive rm, fork bombs,
//! disk destruction, etc.) before execution.

use async_stream::stream;
use regex::Regex;
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;
use tokio::io::{Error, ErrorKind};
use tokio_stream::StreamExt;

pub fn critical_bash_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Recursive destruction.
            Regex::new(r"(?i)\brm\s+-[a-z]*[rRfF][a-z]*\s+\/").unwrap(),
            Regex::new(r"(?i)\bsudo\s+rm\b").unwrap(),
            Regex::new(r"(?i)\bchmod\s+-R\s+[0-7]+\s+\/").unwrap(),
            Regex::new(r"\bchmod\s+-R\s+[ugoa+\-=rwxXst,]+\s+\/").unwrap(),
            Regex::new(r"(?i)\bchown\s+-R\s+\S+\s+\/").unwrap(),
            // Fork bomb.
            Regex::new(r"(?i):\(\)\s*\{\s*:\s*\|\s*:").unwrap(),
            // Disk / filesystem destruction.
            Regex::new(r"(?i)>\s*\/dev\/sd[a-z]").unwrap(),
            Regex::new(r"(?i)\bmkfs(\.|\b)").unwrap(),
            Regex::new(r"(?i)\bdd\s+if=.+of=\/dev\/").unwrap(),
            Regex::new(r"(?i)\bshred\s+\/dev\/").unwrap(),
            Regex::new(r"(?i)\bcryptsetup\b").unwrap(),
            // System-config destruction.
            Regex::new(r"(?i)>\s*\/etc\/(?:passwd|shadow|sudoers)\b").unwrap(),
            Regex::new(r"(?i)\btee\s+(?:-a\s+)?\/etc\/(?:passwd|shadow|sudoers)\b").unwrap(),
            // Remote-fetch-then-execute.
            Regex::new(r"(?i)\b(?:curl|wget|fetch)\b[^|]*\|\s*(?:bash|sh|zsh|fish)\b").unwrap(),
            Regex::new(r"(?i)(?:^|[\s;&|(])(?:bash|sh|zsh|source|\.)\s+<\(\s*(?:curl|wget|fetch)\b").unwrap(),
            Regex::new(r#"(?i)\beval\s+["'`]?\$\(\s*(?:curl|wget|fetch)\b|\beval\s+`\s*(?:curl|wget|fetch)\b"#).unwrap(),
            // Process/host control.
            Regex::new(r"\bkill\s+-9\s+1\b").unwrap(),
            Regex::new(r"(?i)(?:^|[\s;&|(])(?:shutdown|poweroff|reboot|halt)(?:\s|$|[;|&])").unwrap(),
            Regex::new(r"(?i)(?:^|[\s;&|(])init\s+0\b").unwrap(),
            // Network-shell exfil.
            Regex::new(r"(?i)\bnc\b[^|;]*\s-[a-zA-Z]*[ec][a-zA-Z]*\s").unwrap(),
        ]
    })
}
// Constants & lazy statics

const BUFFER_SIZE: usize = 4096;

static ENV_VAR_PATTERN: OnceLock<Regex> = OnceLock::new();

fn env_var_pattern() -> &'static Regex {
    ENV_VAR_PATTERN.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap())
}

// Types

pub struct BashInput {
    pub command: Option<String>,
    pub timeout: Option<u64>,
    pub env: Option<Vec<(String, String)>>,
    pub cwd: Option<String>,
    pub pty: bool,
}

pub struct BashOutput {
    pub stdout: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

pub struct SpawnOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

pub struct ExecError {
    pub stderr: Option<String>,
    pub signal: Option<i32>,
}

pub struct BashError {
    pub text_err: Option<String>,
    pub exec_err: Option<ExecError>,
}
// active stream state
#[derive(PartialEq)]
enum StreamState {
    On,
    Off,
}
// Public API

impl BashInput {
    pub fn new(command: String) -> Self {
        Self {
            command: Some(command),
            timeout: None,
            env: None,
            cwd: None,
            pty: false,
        }
    }

    pub(super) fn validate_patterns(&self) -> Result<(), String> {
        let patterns = critical_bash_patterns();
        for pattern in patterns {
            if pattern.is_match(self.command.as_deref().unwrap_or("")) {
                return Err(format!("Pattern match found: {}", pattern.as_str()));
            }
        }
        Ok(())
    }
}

/// Execute a bash command and return its output as an async stream.
///
/// Validates the command against dangerous patterns, ensures a working
/// directory is provided, and spawns `bash -c <command>` as a child
/// process. Returns a stream of [`SpawnOutput`] items.
pub async fn run(
    bash_input: BashInput,
) -> Result<impl tokio_stream::Stream<Item = SpawnOutput>, BashError> {
    // Guards
    let cwd = bash_input.cwd.clone().ok_or_else(|| BashError {
        text_err: Some("cwd is required".to_string()),
        exec_err: None,
    })?;

    if bash_input
        .command
        .as_deref()
        .is_some_and(|c| Path::new(c).is_absolute())
    {
        return Err(BashError {
            text_err: Some("absolute command not allowed, use relative path".to_string()),
            exec_err: None,
        });
    }

    if let Err(err) = bash_input.validate_patterns() {
        return Err(BashError {
            text_err: Some(err),
            exec_err: None,
        });
    }

    let command = bash_input.command.unwrap();

    Ok(stream! {
        // Execute
        let stream = spawn_bash(bash_input.env, &cwd, &command);
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            if let Ok(output) = item {
                yield output;
            }
        }
    })
}

pub fn spawn_bash(
    env: Option<Vec<(String, String)>>,
    cwd: &str,
    command: &str,
) -> impl tokio_stream::Stream<Item = Result<SpawnOutput, Error>> {
    let mut _buffer_stdout = [0u8; BUFFER_SIZE];
    let mut _buffer_stderr = [0u8; BUFFER_SIZE];
    let mut cmd = tokio::process::Command::new("bash");

    let mut state = StreamState::Off;

    stream! {
        if state == StreamState::Off {state = StreamState::On;}
        if state == StreamState::On {
            _buffer_stdout.fill(0);
            _buffer_stderr.fill(0);
        }
        if let Some(env) = env {
            let valid_pattern = env_var_pattern();
            for (key, value) in env {
                if !valid_pattern.is_match(&key) {
                    yield Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("invalid env variable name: {key}"),
                    ));
                    return;
                }
                cmd.env(key, value);
            }
        }

        cmd.current_dir(cwd);
        cmd.arg("-c").arg(command);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                yield Err(Error::other(format!("failed to spawn bash process: {e}")));
                return;
            }
        };

        let mut stream_stdout = child.stdout.take().unwrap();
        let mut stream_stderr = child.stderr.take().unwrap();
        loop {
            tokio::select! {
                result_stdout = stream_stdout.read(&mut _buffer_stdout) => {
                    let stdout = match result_stdout {
                        Ok(0) => break,
                        Ok(n) => _buffer_stdout[..n].to_vec(),
                        Err(e) => {
                            yield Err(Error::other(format!("stdout read error: {e}")));
                            break;
                        }
                    };

                    yield Ok(SpawnOutput {
                        stdout,
                        stderr: vec![],
                        exit_code: None,
                        truncated: false,
                    });
                }

                result_stderr = stream_stderr.read(&mut _buffer_stderr) => {
                    let stderr = match result_stderr {
                        Ok(0) => break,
                        Ok(n) => _buffer_stderr[..n].to_vec(),
                        Err(e) => {
                            yield Err(Error::other(format!("stderr read error: {e}")));
                            break;
                        }
                    };

                    yield Ok(SpawnOutput {
                        stdout: vec![],
                        stderr,
                        exit_code: None,
                        truncated: false,
                    });
                }
            }
        }
    }
}
