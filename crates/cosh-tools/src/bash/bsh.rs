//! Bash executor with security pattern validation and async streaming output.
//!
//! Provides [`run`] and [`spawn_bash`] for executing bash commands as child
//! processes, returning output as an async stream of chunks. Commands are
//! validated against a set of dangerous patterns (recursive rm, fork bombs,
//! disk destruction, etc.) before execution.

use async_stream::stream;
#[cfg(unix)]
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use regex::Regex;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::OnceLock;

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::{Error, ErrorKind};
use tokio_stream::Stream;
use tokio_stream::StreamExt;

#[cfg(unix)]
use tokio::sync::mpsc;

/// Returns the list of critical bash patterns that are checked before execution.
///
/// # Panics
///
/// Panics if any of the internal regular expressions fail to compile (they are
/// static and guaranteed valid on first access).
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
#[cfg(unix)]
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
/// When `timeout_ms` is `Some(ms)`, the child is killed after `ms`
/// milliseconds and the stream yields a final item with
/// `signal: Some(-1)` and `exit_code: None`.
///
/// # Errors
///
/// Returns [`BashError`] if the command is an absolute path or matches a
/// dangerous security pattern.
pub fn run<'a>(
    timeout_ms: Option<u64>,
    env: &Option<Vec<(String, String)>>,
    pty: bool,
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

    let env = (*env).clone();
    let use_pty = pty;

    Ok(Box::pin(stream! {
        let mut stream: Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> = if use_pty {
            #[cfg(unix)]
            {
                spawn_bash_pty(env, cwd, command, timeout_ms)
            }
            #[cfg(not(unix))]
            {
                spawn_bash(env, cwd, command, timeout_ms)
            }
        } else {
            spawn_bash(env, cwd, command, timeout_ms)
        };
        while let Some(item) = stream.next().await {
            if let Ok(output) = item {
                yield output;
            }
        }
    }))
}

/// Spawn a bash process and return its output as an async stream.
///
/// When `timeout_ms` is `Some(ms)`, the child is killed after `ms`
/// milliseconds and the stream yields a final item with
/// `signal: Some(-1)` and `exit_code: None`.
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
    timeout_ms: Option<u64>,
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
        let deadline = timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));

        loop {
            tokio::select! {
                biased;

                () = async {
                    match deadline {
                        Some(dl) => tokio::time::sleep_until(dl).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    let _ = child.kill().await;
                    yield Ok(SpawnOutput {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: None,
                        signal: Some(-1),
                        truncated: false,
                    });
                    return;
                }

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
///
/// # Timeout mechanism
///
/// A **self-pipe** trick is used to unblock the blocking [`poll(2)`] call
/// from the async timeout handler without requiring a separate monitor
/// thread:
///
/// 1. A [`libc::pipe2`] is created before [`tokio::task::spawn_blocking`]
///    is called, giving one fd for each side of the pipe.
/// 2. The read end (`pipe_rx`) is moved into the blocking task; the write
///    end (`pipe_tx`) stays on the async side.
/// 3. In the blocking task, [`libc::poll`] watches **both** the PTY master
///    file descriptor and `pipe_rx` for readability.
/// 4. When the async timeout fires, it writes a single byte to `pipe_tx`.
///    `poll` wakes immediately, the blocking task detects the byte on the
///    pipe fd, breaks out of the read loop, kills and reaps the child.
///
/// # Safety
///
/// The three `unsafe` blocks in this function are:
///
/// | Location | Call | Invariant |
/// |---|---|---|
/// | Stream setup | `pipe2` | `pipe_fds` is a valid pointer to 2 `i32`s |
/// | Timeout handler | `write` + `close` on `pipe_tx` | `pipe_tx` is a valid fd, not used after |
/// | Blocking task | `poll`, `read`, `close` on `pipe_rx` and `pty_fd` | Both fds are valid and open for the lifetime of `poll_fds`; `pipe_rx` is closed once after use |
///
/// These are trivially verified by inspection — the pipe fds are created
/// together, one is consumed per side, and each is closed exactly once.
///
/// [`poll(2)`]: https://man7.org/linux/man-pages/man2/poll.2.html
#[cfg(unix)]
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_bash_pty(
    env: Option<Vec<(String, String)>>,
    cwd: &str,
    command: &str,
    timeout_ms: Option<u64>,
) -> Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> {
    let cwd = cwd.to_string();
    let command = command.to_string();
    let kill_flag = Arc::new(AtomicBool::new(false));

    Box::pin(stream! {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let kill = kill_flag.clone();

        // Create a self-pipe before spawn_blocking so the write end
        // (pipe_tx) is accessible from the async timeout handler.
        let mut pipe_fds: [libc::c_int; 2] = [0; 2];
        // SAFETY: pipe2 is safe per POSIX; fds provides a valid array pointer.
        let pipe_result = unsafe {
            libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC)
        };
        if pipe_result != 0 {
            yield Err(Error::other("failed to create self-pipe for timeout"));
            return;
        }
        let (pipe_rx, pipe_tx) = (pipe_fds[0], pipe_fds[1]);

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

            // Get the PTY master fd for poll(). Both pair.master and the
            // cloned reader share the same underlying PTY, so polling the
            // master fd for POLLIN and reading from the reader is coherent.
            let Some(pty_fd) = pair.master.as_raw_fd() else {
                let _ = tx.send(Err(Error::other("failed to get PTY fd")));
                return;
            };

            // poll_fds[0] = PTY master, poll_fds[1] = self-pipe read end.
            let mut poll_fds = [
                libc::pollfd { fd: pty_fd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: pipe_rx, events: libc::POLLIN, revents: 0 },
            ];

            let mut buf = [0u8; BUFFER_SIZE];
            loop {
                let n_ready = loop {

                    let res = unsafe { libc::poll(poll_fds.as_mut_ptr(), 2, -1) };
                    if res < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        let _ = tx.send(Err(Error::other(format!("pty poll error: {err}"))));
                        return;
                    }
                    break res;
                };

                if n_ready == 0 {
                    // Spurious wakeup (shouldn't happen with -1 timeout).
                    continue;
                }

                // Self-pipe has data → timeout requested by the async handler.
                if poll_fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                    // Consume the notification byte (ignore errors — we
                    // only care that poll woke up).
                    let _ = unsafe {
                        libc::read(
                            pipe_rx,
                            buf.as_mut_ptr().cast::<libc::c_void>(),
                            buf.len(),
                        )
                    };
                    break;
                }

                // PTY has data available.
                if poll_fds[0].revents & libc::POLLIN != 0 {
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
                                // Receiver dropped (stream cancelled).
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(Error::other(format!("pty read error: {e}"))));
                            return;
                        }
                    }
                }

                // PTY hung up (child exited).
                if poll_fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    // Drain any remaining data before breaking.
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => {}
                        Ok(n) => {
                            let _ = tx.send(Ok(SpawnOutput {
                                stdout: buf[..n].to_vec(),
                                stderr: vec![],
                                exit_code: None,
                                signal: None,
                                truncated: n == BUFFER_SIZE,
                            }));
                        }
                    }
                    break;
                }
            }

            // Close pipe read end (write end is closed by the async handler).
            // SAFETY: pipe_rx is a valid fd not used after this point.
            unsafe { libc::close(pipe_rx); }

            if kill.load(Ordering::Relaxed) {
                let _ = child.kill();
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

        let deadline = timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        loop {
            tokio::select! {
                biased;

                () = async {
                    match deadline {
                        Some(dl) => tokio::time::sleep_until(dl).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    kill_flag.store(true, Ordering::Relaxed);

                    // Write a byte to the self-pipe so the blocking task's
                    // poll() wakes up and detects the timeout.
                    let byte: u8 = 0;
                    // SAFETY: pipe_tx is a valid fd owned by this scope.
                    unsafe {
                        libc::write(
                            pipe_tx,
                            (&raw const byte).cast::<libc::c_void>(),
                            1,
                        );
                    }
                    // SAFETY: pipe_tx is never used after this point.
                    unsafe { libc::close(pipe_tx); }

                    yield Ok(SpawnOutput {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: None,
                        signal: Some(-1),
                        truncated: false,
                    });
                    return;
                }

                item = rx.recv() => {
                    match item {
                        Some(result) => yield result,
                        None => break,
                    }
                }
            }
        }

        // Stream ended normally — close the pipe write end.
        // SAFETY: pipe_tx was not closed by the timeout handler (the
        // handler returned above).
        unsafe { libc::close(pipe_tx); }
    })
}
