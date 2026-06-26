//! Bash executor with security pattern validation and async streaming output.
//!
//! Provides [`run`] and [`spawn_bash`] for executing bash commands as child
//! processes, returning output as an async stream of chunks. Commands are
//! validated against a set of dangerous patterns (recursive rm, fork bombs,
//! disk destruction, etc.) before execution.

use async_stream::stream;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use regex::Regex;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::AsyncReadExt;
use tokio::io::{Error, ErrorKind};
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

/// Returns the list of critical bash patterns that are checked before execution.
///
/// # Panics
///
/// Panics if any of the internal regular expressions fail to compile (they are
/// static and guaranteed valid at compile time).
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

/// Maps a Unix signal *description* (from `strsignal(3)`) to its numeric value.
///
/// `portable_pty::ExitStatus::signal()` returns the description string
/// produced by the libc `strsignal()` function (e.g. `"Killed"` for
/// SIGKILL), not the `"SIGKILL"` constant name.  We map back to the
/// numeric value to match `SpawnOutput::signal` in the non-PTY path
/// (which uses `std::os::unix::process::ExitStatusExt`).
fn signal_name_to_number(name: &str) -> Option<i32> {
    match name {
        "Hangup" => Some(1),
        "Interrupt" => Some(2),
        "Quit" => Some(3),
        "Illegal instruction" => Some(4),
        "Trace/breakpoint trap" => Some(5),
        "Aborted" => Some(6),
        "Bus error" => Some(7),
        "Arithmetic exception" | "Floating point exception" => Some(8),
        "Killed" => Some(9),
        "User defined signal 1" => Some(10),
        "Segmentation fault" => Some(11),
        "User defined signal 2" => Some(12),
        "Broken pipe" => Some(13),
        "Alarm clock" => Some(14),
        "Terminated" => Some(15),
        "Stack fault" => Some(16),
        "Child exited" => Some(17),
        "Continued" => Some(18),
        "Stopped (signal)" => Some(19),
        "Stopped" => Some(20),
        "Stopped (tty input)" => Some(21),
        "Stopped (tty output)" => Some(22),
        "Urgent I/O condition" => Some(23),
        "CPU time limit exceeded" => Some(24),
        "File size limit exceeded" => Some(25),
        "Virtual timer expired" => Some(26),
        "Profiling timer expired" => Some(27),
        "Window changed" => Some(28),
        "I/O possible" => Some(29),
        "Power failure" => Some(30),
        "Bad system call" => Some(31),
        _ => None,
    }
}

// Types
#[derive(Default)]
pub struct Bash {
    pub timeout: Option<u64>,
    pub env: Option<Vec<(String, String)>>,
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
    pub signal: Option<i32>,
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
// Public API

pub(super) fn validate_bash_patterns(command: &str) -> Result<(), String> {
    let patterns = critical_bash_patterns();
    for pattern in patterns {
        if pattern.is_match(command) {
            return Err(format!("Pattern match found: {}", pattern.as_str()));
        }
    }
    Ok(())
}

/// Execute a bash command and return its output as an async stream.
///
/// Validates the command against dangerous patterns and spawns
/// `bash -c <command>` as a child process in the given `cwd`.
/// Returns a stream of [`SpawnOutput`] items.
///
/// # Errors
///
/// Returns [`BashError`] if the command is an absolute path or matches a
/// dangerous security pattern.
pub fn run<'a>(
    bash: &Bash,
    command: &'a str,
    cwd: &'a str,
) -> Result<Pin<Box<dyn Stream<Item = SpawnOutput> + Send + 'a>>, BashError> {
    // Guards
    if Path::new(command).is_absolute() {
        return Err(BashError {
            text_err: Some("absolute command not allowed, use relative path".to_string()),
            exec_err: None,
        });
    }

    if let Err(err) = validate_bash_patterns(command) {
        return Err(BashError {
            text_err: Some(err),
            exec_err: None,
        });
    }

    let env = bash.env.clone();
    let use_pty = bash.pty;

    Ok(Box::pin(stream! {
        if use_pty {
            let mut stream = spawn_bash_pty(env, cwd, command);
            while let Some(item) = stream.next().await {
                if let Ok(output) = item {
                    yield output;
                }
            }
        } else {
            let mut stream = spawn_bash(env, cwd, command);
            while let Some(item) = stream.next().await {
                if let Ok(output) = item {
                    yield output;
                }
            }
        }
    }))
}

/// Spawn a bash process and return its output as an async stream.
///
/// # Panics
///
/// Panics if the child process stdout or stderr pipe cannot be taken (this
/// only happens if [`std::process::Stdio::piped`] was not set).
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_bash<'a>(
    env: Option<Vec<(String, String)>>,
    cwd: &'a str,
    command: &'a str,
) -> Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send + 'a>> {
    let mut buffer_stdout = [0u8; BUFFER_SIZE];
    let mut buffer_stderr = [0u8; BUFFER_SIZE];
    let mut cmd = tokio::process::Command::new("bash");

    Box::pin(stream! {
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
        cmd.stdin(Stdio::null());
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
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            tokio::select! {
                result_stdout = stream_stdout.read(&mut buffer_stdout), if !stdout_done => {
                    match result_stdout {
                        Ok(0) => stdout_done = true,
                        Ok(n) => {
                            let stdout = buffer_stdout[..n].to_vec();
                            yield Ok(SpawnOutput {
                                stdout,
                                stderr: vec![],
                                exit_code: None,
                                signal: None,
                                truncated: n == BUFFER_SIZE,
                            });
                        }
                        Err(e) => {
                            yield Err(Error::other(format!("stdout read error: {e}")));
                            stdout_done = true;
                        }
                    }
                }

                result_stderr = stream_stderr.read(&mut buffer_stderr), if !stderr_done => {
                    match result_stderr {
                        Ok(0) => stderr_done = true,
                        Ok(n) => {
                            let stderr = buffer_stderr[..n].to_vec();
                            yield Ok(SpawnOutput {
                                stdout: vec![],
                                stderr,
                                exit_code: None,
                                signal: None,
                                truncated: n == BUFFER_SIZE,
                            });
                        }
                        Err(e) => {
                            yield Err(Error::other(format!("stderr read error: {e}")));
                            stderr_done = true;
                        }
                    }
                }
            }
            if stdout_done && stderr_done { break; }
        }

        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => {
                yield Err(Error::other(format!("failed to wait for child: {e}")));
                return;
            }
        };

        let signal = {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                status.signal()
            }
            #[cfg(not(unix))]
            {
                None
            }
        };

        yield Ok(SpawnOutput {
            stdout: vec![],
            stderr: vec![],
            exit_code: status.code(),
            signal,
            truncated: false,
        });
    })
}

/// Spawn a bash process into a PTY and return its output as an async stream.
///
/// Unlike [`spawn_bash`], this uses a pseudo-terminal so the child process
/// behaves as if connected to a terminal (colored output, prompts, etc.).
/// stdout and stderr are multiplexed into the PTY; [`SpawnOutput::stderr`]
/// is always empty in this path.
///
/// I/O is bridged from the synchronous `portable_pty` reader to the async
/// stream via [`tokio::task::spawn_blocking`] and an `mpsc` channel, adding
/// one buffer copy per chunk.
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_bash_pty(
    env: Option<Vec<(String, String)>>,
    cwd: &str,
    command: &str,
) -> Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> {
    let cwd = cwd.to_string();
    let command = command.to_string();

    Box::pin(stream! {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::task::spawn_blocking(move || {
            // Validate environment variables (same rules as spawn_bash).
            if let Some(ref env) = env {
                let valid_pattern = env_var_pattern();
                for (key, _) in env {
                    if !valid_pattern.is_match(key) {
                        let _ = tx.send(Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!("invalid env variable name: {key}"),
                        )));
                        return;
                    }
                }
            }

            let pty_system = native_pty_system();
            let pair = match pty_system.openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.send(Err(Error::other(format!("failed to open pty: {e}"))));
                    return;
                }
            };

            let mut cmd_builder = CommandBuilder::new("bash");
            cmd_builder.arg("-c");
            cmd_builder.arg(&command);
            cmd_builder.cwd(&cwd);

            if let Some(env) = env {
                for (key, value) in env {
                    cmd_builder.env(key, value);
                }
            }

            let mut child = match pair.slave.spawn_command(cmd_builder) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(Error::other(
                        format!("failed to spawn command in pty: {e}"),
                    )));
                    return;
                }
            };

            let mut reader = match pair.master.try_clone_reader() {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(Error::other(
                        format!("failed to clone pty reader: {e}"),
                    )));
                    return;
                }
            };

            // Dropping the slave closes its end of the PTY, signalling
            // EOF to the master reader once the child exits.
            drop(pair.slave);

            let mut buf = [0u8; BUFFER_SIZE];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx
                            .send(Ok(SpawnOutput {
                                stdout: buf[..n].to_vec(),
                                stderr: vec![],
                                exit_code: None,
                                signal: None,
                                truncated: n == BUFFER_SIZE,
                            }))
                            .is_err()
                        {
                            // Receiver dropped (stream consumer cancelled).
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Error::other(format!("pty read error: {e}"))));
                        return;
                    }
                }
            }

            match child.wait() {
                Ok(status) => {
                    let signal = status.signal().and_then(signal_name_to_number);
                    // Match non-PTY semantics: exit_code is None when
                    // killed by a signal; only Some when the process
                    // exited normally.
                    let exit_code = if signal.is_some() {
                        None
                    } else {
                        #[allow(clippy::cast_possible_wrap)]
                        Some(status.exit_code() as i32)
                    };
                    let _ = tx.send(Ok(SpawnOutput {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code,
                        signal,
                        truncated: false,
                    }));
                }
                Err(e) => {
                    let _ = tx.send(Err(Error::other(format!("pty wait error: {e}"))));
                }
            }
        });

        while let Some(item) = rx.recv().await {
            yield item;
        }
    })
}
