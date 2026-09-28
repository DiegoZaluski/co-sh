//! Bash executor with security pattern validation and async streaming output.
//!
//! Provides [`run`] and [`spawn_bash`] for executing bash commands as child
//! processes, returning output as an async stream of chunks. Commands are
//! validated against a set of dangerous patterns (recursive rm, fork bombs,
//! disk destruction, etc.) before execution.

use async_stream::stream;
#[cfg(any(unix, windows))]
use portable_pty::CommandBuilder;
#[cfg(any(unix, windows))]
use portable_pty::PtySize;
#[cfg(any(unix, windows))]
use portable_pty::native_pty_system;
use regex::Regex;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::LazyLock;

#[cfg(any(unix, windows))]
use std::io::Read;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::io::{Error, ErrorKind};
use tokio_stream::Stream;
use tokio_stream::StreamExt;

#[cfg(any(unix, windows))]
use tokio::sync::mpsc;

/// Returns the list of critical bash patterns that are checked before execution.
///
/// # Panics
///
/// Panics if any of the internal regular expressions fail to compile (they are
/// static and guaranteed valid on first access).
#[allow(clippy::unwrap_used)]
pub fn critical_bash_patterns() -> &'static [Regex] {
    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
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
    });
    &PATTERNS
}
// Constants & lazy statics

const BUFFER_SIZE: usize = 4096;

/// How often the Unix PTY read loop wakes up to run the input-wait watchdog.
#[cfg(unix)]
const WATCHDOG_TICK_MS: libc::c_int = 250;

/// How long a child may sit in raw (non-canonical) terminal mode — the
/// signature of an interactive pager/TUI waiting for keystrokes the harness
/// can never send — before the input-wait watchdog kills it.
#[cfg(unix)]
const WATCHDOG_RAW_MODE_GRACE: Duration = Duration::from_secs(2);

/// Diagnostic emitted on stderr when the input-wait watchdog kills a child.
///
/// Deliberately DESCRIPTIVE ONLY — no suggested commands. This message only
/// appears when the auto-fix layer could not rewrite the command with
/// 100% certainty (otherwise there is nothing to kill), so any specific
/// advice could be wrong for the unknown case; the model infers the fix
/// itself for the retry.
#[cfg(unix)]
const WATCHDOG_MESSAGE: &str =
    "input-wait watchdog: killed a process that entered raw (interactive) \
     terminal mode and waited for keyboard input the harness cannot provide \
     (interactive pager/editor/TUI).";

static ENV_VAR_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());

#[allow(clippy::unwrap_used)]
fn env_var_pattern() -> &'static Regex {
    &ENV_VAR_PATTERN
}

/// Resolve the bash executable to spawn, per platform.
///
/// - Unix: plain `"bash"` (resolved through `PATH`, as before).
/// - Windows: an absolute path to a Git Bash (`bash.exe`) — never the
///   `C:\Windows\System32\bash.exe` WSL launcher. `CreateProcessW` searches
///   System32 **before** `PATH`, so a bare `"bash"` would silently route
///   commands into WSL (ignoring the Windows `cwd` and filesystem). The
///   resolution order is:
///   1. `where.exe bash.exe` — first hit that is not the WSL launcher
///      (respects the user's PATH, e.g. a scoop/custom install);
///   2. the standard Git-for-Windows install locations
///      (`...\Git\bin\bash.exe`, then `...\Git\usr\bin\bash.exe`).
///
/// `bin\bash.exe` is preferred over `usr\bash.exe`: the former is a login
/// wrapper that adds Git's `usr\bin` to `PATH` for the child, so commands
/// like `ls` and `seq` resolve; the latter relies on the inherited `PATH`.
///
/// # Panics
///
/// Panics if no Git Bash can be located — this is a programming/deployment
/// error (the harness requires Git for Windows), not a per-command failure.
#[cfg(windows)]
fn resolve_bash() -> &'static str {
    static RESOLVED: LazyLock<String> = LazyLock::new(|| {
        let is_real_bash = |p: &Path| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("bash.exe"))
                && !p.components().any(|c| {
                    c.as_os_str().to_str().is_some_and(|s| {
                        s.eq_ignore_ascii_case("WindowsApps") || s.eq_ignore_ascii_case("System32")
                    })
                })
        };

        // 1. Respect the user's PATH via `where.exe`.
        if let Ok(output) = std::process::Command::new("where.exe")
            .arg("bash.exe")
            .stdin(Stdio::null())
            .output()
            && output.status.success()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let path = Path::new(line.trim());
                if is_real_bash(path) {
                    return path.to_string_lossy().into_owned();
                }
            }
        }

        // 2. Standard Git-for-Windows locations. `bin\bash.exe` is a login
        //    wrapper that sets up Git's POSIX PATH; `usr\bin` is the raw
        //    Cygwin binary.
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
            r"C:\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\usr\bin\bash.exe",
            r"C:\Git\usr\bin\bash.exe",
        ] {
            let path = Path::new(candidate);
            if path.is_file() {
                return candidate.to_string();
            }
        }

        panic!(
            "bash.exe not found: install Git for Windows (https://git-scm.com/download/win) \
             or ensure a Git Bash `bash.exe` is on PATH"
        );
    });
    RESOLVED.as_str()
}

#[cfg(not(windows))]
fn resolve_bash() -> &'static str {
    "bash"
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
/// Before spawning, the command goes through [`auto_fix_command`]: when the
/// command can be rewritten into a provably non-blocking form with 100%
/// certainty, the rewritten form runs and a `auto-fix: …` note is yielded as
/// the first stderr item. Ambiguous commands run unchanged.
///
/// When `timeout_ms` is `Some(ms)`, the child is killed after `ms`
/// milliseconds and the stream yields a final item with
/// `signal: Some(-1)` and `exit_code: None`.
///
/// # Errors
///
/// Returns [`BashError`] if the command is an absolute path or matches a
/// dangerous security pattern. Both the original and the auto-fixed command
/// are validated, so a rewrite can never smuggle a pattern past the guards.
pub fn run(
    timeout_ms: Option<u64>,
    env: &Option<Vec<(String, String)>>,
    pty: bool,
    command: &str,
    cwd: &str,
) -> Result<Pin<Box<dyn Stream<Item = SpawnOutput> + Send>>, BashError> {
    // Guards
    // On Windows, POSIX-style absolute commands (`/bin/echo hi`) are NOT
    // `Path::is_absolute`, but Git Bash resolves them against its MSYS root
    // — they are absolute from the shell's perspective and are blocked too.
    let is_absolute = Path::new(command).is_absolute() || command.starts_with('/');
    if is_absolute {
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

    // Auto-fix: rewrite paged commands when 100% certain; ambiguous
    // commands pass through unchanged (the input-wait watchdog remains the
    // backstop, so the worst case is the pre-existing behavior). The
    // rewritten command is owned by this frame and moved into the 'static
    // stream below, so the stream never borrows a local.
    #[cfg(any(unix, windows))]
    let (fixed_command, autofix_note) = auto_fix_command(command);
    #[cfg(any(unix, windows))]
    if fixed_command != command
        && let Err(err) = validate_bash_patterns(&fixed_command)
    {
        return Err(BashError {
            text_err: Some(err),
            exec_err: None,
        });
    }
    #[cfg(any(unix, windows))]
    let command: String = fixed_command;
    #[cfg(not(any(unix, windows)))]
    let command: String = command.to_string();
    #[cfg(not(any(unix, windows)))]
    let autofix_note: Option<String> = None;

    let env = (*env).clone();
    let cwd = cwd.to_string();
    let use_pty = pty;

    Ok(Box::pin(stream! {
        // Surface the rewrite first, so consumers see exactly which command
        // is about to run (the model must know it did not run verbatim).
        if let Some(note) = autofix_note {
            yield SpawnOutput {
                stdout: vec![],
                stderr: note.into_bytes(),
                exit_code: None,
                signal: None,
                truncated: false,
            };
        }

        let mut stream: Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> = if use_pty {
            #[cfg(any(unix, windows))]
            {
                spawn_bash_pty(env, &cwd, &command, timeout_ms)
            }
            #[cfg(not(any(unix, windows)))]
            {
                spawn_bash(env, cwd.as_str(), command.as_str(), timeout_ms)
            }
        } else {
            spawn_bash(env, cwd.as_str(), command.as_str(), timeout_ms)
        };
        while let Some(item) = stream.next().await {
            match item {
                Ok(output) => yield output,
                Err(e) => {
                    // Surface stream/spawn errors instead of dropping them:
                    // a failed spawn (e.g. an invalid cwd) or a mid-stream
                    // read error used to end the stream SILENTLY with zero
                    // items, leaving callers with no output and no
                    // explanation. Convert the error into a stderr-bearing
                    // item so consumers always see what went wrong.
                    yield SpawnOutput {
                        stdout: vec![],
                        stderr: format!("bash stream error: {e}").into_bytes(),
                        exit_code: None,
                        signal: None,
                        truncated: false,
                    };
                }
            }
        }
    }))
}

/// Default environment variables injected into every spawned bash to keep the
/// child process non-interactive.
///
/// Interactive pagers (`less`, `more`) and editors are the most common cause
/// of a command "hanging": when stdout is a PTY (or the pager is inherited
/// from the harness's own environment), tools like `git` launch `less` on
/// `git log` / `git diff`, which then blocks forever waiting for keystrokes
/// that never arrive — the run only ends when the timeout fires.
const NON_INTERACTIVE_DEFAULTS: &[(&str, &str)] = &[
    // Plain-text pagers: never launch an interactive pager.
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
    // If a pager still runs (e.g. an explicit `| less`), environment flags
    // CANNOT make it non-interactive: less reads keystrokes directly from
    // /dev/tty, so it blocks forever inside a PTY no matter what LESS/PAGER
    // vars say. That case is handled by the input-wait watchdog in
    // spawn_bash_pty, not by environment variables.
    // Editors must never open an interactive UI inside a spawned command.
    ("GIT_EDITOR", "true"),
    ("EDITOR", "true"),
    ("VISUAL", "true"),
];

/// Compute which non-interactive defaults still need to be injected.
///
/// Any variable **explicitly** provided by the caller wins over the default:
/// only the defaults whose keys are absent from `caller_env` are returned.
/// Note that an inherited value from the parent process's environment does
/// NOT win — the parent may itself run under `PAGER=less`, which is exactly
/// the hang this mechanism prevents.
fn non_interactive_defaults(
    caller_env: &Option<Vec<(String, String)>>,
) -> Vec<(&'static str, &'static str)> {
    let caller_keys: Vec<&str> = caller_env
        .as_ref()
        .map(|pairs| pairs.iter().map(|(k, _)| k.as_str()).collect())
        .unwrap_or_default();
    NON_INTERACTIVE_DEFAULTS
        .iter()
        .copied()
        .filter(|(key, _)| !caller_keys.contains(key))
        .collect()
}

// --- Auto-fix: deterministic rewrites of commands that would block on an ---
// --- interactive pager, applied before spawn. Fallback = current behavior. ---
//
// The rewrite layer ONLY fires when it can be 100% certain the rewrite is
// safe (no quoting, no shell operators it doesn't understand). When it
// cannot be certain, it returns the command untouched and the input-wait
// watchdog remains the backstop — so the worst case is exactly the
// pre-existing behavior.

/// Regex: `git [env-prefixes] [global-flags...] <paged-subcommand>` at the
/// start of the command line. The `git` token is captured so the rewrite can
/// insert `--no-pager` at its exact position even with env prefixes present.
/// Global flags that consume a separate value are enumerated explicitly
/// (`-c`, `-C`, `--git-dir`, `--work-tree`, `--namespace`,
/// `--super-prefix` — git's fixed global-option list); anything else stays
/// unmatched → no rewrite.
#[cfg(any(unix, windows))]
static AUTO_FIX_GIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        concat!(
            r"^(?:\s*[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(git)",
            r"(?:\s+(?:",
            r"-[cC]\s+\S+",                                                   // -c <name>=<value> / -C <path>
            r"|--(?:git-dir|work-tree|namespace|super-prefix)\s+\S+",         // --opt <value>
            r"|-{1,2}[\w-]+\S*",                                              // valueless global flags
            r"))*",
            r"(?:\s+(log|diff|show|blame|shortlog|reflog|whatchanged)\b)",
        ),
    )
    .unwrap()
});

/// Regex: a final pipeline segment that is ONLY a pager plus flag tokens:
/// `... | less`, `| less -X`. Anything else in the tail makes the match
/// fail → no rewrite (fallback stays the watchdog): a filename argument
/// would make the pager ignore stdin, and a `>`/`<` redirect must never be
/// swallowed by the replacement.
#[cfg(any(unix, windows))]
static AUTO_FIX_PIPE_PAGER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\|\s*(less|more|most)\b(?:\s+-{1,2}[\w][\w-]*)*\s*$").unwrap()
});

/// Regex: `less`/`more` invoked as the command itself with plain filename
/// arguments (no flags — flags would be invalid for `cat`).
#[cfg(any(unix, windows))]
static AUTO_FIX_PAGER_CMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(less|more)\s+([^;|&]+)$").unwrap());

/// Rewrite `command` so it cannot block on an interactive pager, when doing
/// so is 100% certain; otherwise return the command unchanged.
///
/// Returns `(possibly-rewritten-command, note)` where `note` — when present —
/// describes the rewrite and MUST be surfaced to the caller (the model) as
/// stderr, so it knows exactly which command actually ran.
///
/// Certainty rules shared by every pattern below:
/// - no quotes of any kind in the command (a quoted string containing e.g.
///   `git log` must not be rewritten);
/// - anything ambiguous → no rewrite (the watchdog stays the backstop).
///
/// Rules are applied repeatedly until the command stabilizes (bounded), so
/// composite forms like `git log | less` are resolved by a single applicable
/// rule — the pipeline rule — instead of stacking rewrites.
#[cfg(any(unix, windows))]
pub(crate) fn auto_fix_command(command: &str) -> (String, Option<String>) {
    use regex::Captures;

    // Applies at most one rule. Returns `Some((rewritten, note))` or `None`.
    fn apply_one(command: &str) -> Option<(String, String)> {
        // Already explicitly non-paged via git's own flag → nothing to do.
        // Also stops the fix loop right after a git rewrite.
        if command.contains("--no-pager") {
            return None;
        }

        // Rule A: final pipeline segment is a pager → replace it with `cat`
        // (same bytes, no pagination). The regex guarantees the segment is
        // the tail of the command and contains no `|`, `;` or `&`.
        if AUTO_FIX_PIPE_PAGER_RE.is_match(command) {
            let fixed = AUTO_FIX_PIPE_PAGER_RE.replace(command, |_: &Captures| "| cat");
            return Some((
                fixed.into_owned(),
                "auto-fix: replaced the trailing pager in the pipeline with `cat` to keep the output complete".to_string(),
            ));
        }

        // Rule B: `git [env-prefixes] [global-flags] <paged-subcommand>` →
        // insert `--no-pager` right after `git`. Only when NOT piped: with
        // stdout attached to a pipe git never launches its pager anyway,
        // and the flag would be pure noise. Compound commands are also
        // refused: in `git log && less file`, fixing only the git segment
        // would leave a blocking pager behind — a partial rewrite is not
        // 100% certain, so anything with `;` or `&` passes through.
        if !command.contains('|')
            && !command.contains(';')
            && !command.contains('&')
            && let Some(cap) = AUTO_FIX_GIT_RE.captures(command)
        {
            let git_token = cap.get(1)?;
            let sub = cap.get(2)?.as_str();
            // Insert right after the captured `git` token — not at the
            // match start, which can be preceded by env assignments.
            let insert_at = git_token.end();
            let mut fixed = String::with_capacity(command.len() + "--no-pager ".len());
            fixed.push_str(&command[..insert_at]);
            fixed.push_str(" --no-pager");
            fixed.push_str(&command[insert_at..]);
            return Some((
                fixed,
                format!("auto-fix: inserted `--no-pager` into `git {sub}` to prevent an interactive pager"),
            ));
        }

        // Rule C: `less/more <plain filenames>` → `cat <filenames>`. The
        // args must not start a token with `-` (less flags are not valid
        // cat flags) and the regex excludes `;`, `|`, `&`.
        if let Some(cap) = AUTO_FIX_PAGER_CMD_RE.captures(command) {
            let args = cap.get(2)?.as_str();
            if args.split_whitespace().all(|tok| !tok.starts_with('-')) {
                let pager = cap.get(1)?.as_str();
                return Some((
                    format!("cat {args}"),
                    format!("auto-fix: replaced `{pager}` with `cat` to print the file(s) directly"),
                ));
            }
            // `less -X file` etc. → not certain → no rewrite.
        }

        None
    }

    // Global certainty gate: any quoting anywhere in the line disqualifies
    // every rule. A quoted literal could contain the patterns below without
    // being a command (`echo "git log"`); regex cannot prove intent, so we
    // refuse to touch it.
    if command.contains('"') || command.contains('\'') || command.contains('`') {
        return (command.to_string(), None);
    }

    // Multi-line commands are out of scope for every rule: a `\n` can hide
    // a second command or a heredoc body that a single-line-oriented
    // rewrite would corrupt or silently delete.
    if command.contains('\n') {
        return (command.to_string(), None);
    }

    // An explicit git pager opt-in (`--paginate`) must win over our
    // insertion: git's --paginate/--no-pager are last-wins, so inserting
    // --no-pager BEFORE it would re-enable the pager and the note would
    // claim a fix that does not happen. Refuse → watchdog fallback.
    if command.contains("--paginate") {
        return (command.to_string(), None);
    }

    // Fix loop: apply one rule at a time until nothing applies. The bound
    // is defensive — every rule is convergent, but never risk unbounded
    // rewriting on a pathological input.
    let mut current = command.to_string();
    let mut notes: Vec<String> = Vec::new();
    for _ in 0..4 {
        match apply_one(&current) {
            Some((fixed, note)) => {
                notes.push(note);
                current = fixed;
            }
            None => break,
        }
    }

    let note = (!notes.is_empty()).then(|| notes.join("; "));
    (current, note)
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
pub(crate) fn spawn_bash(
    env: Option<Vec<(String, String)>>,
    cwd: impl Into<String>,
    command: impl Into<String>,
    timeout_ms: Option<u64>,
) -> Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> {
    // Own `cwd`/`command` up front so the returned stream is 'static — it
    // must never borrow a caller's string, because `run()` passes an
    // auto-fixed command that is a local of its own frame.
    let cwd = cwd.into();
    let command = command.into();
    let mut buffer_stdout = [0u8; BUFFER_SIZE];
    let mut buffer_stderr = [0u8; BUFFER_SIZE];
    let mut cmd = tokio::process::Command::new(resolve_bash());

    Box::pin(stream! {
        if let Some(ref env) = env {
            let valid_pattern = env_var_pattern();
            for (key, value) in env {
                if !valid_pattern.is_match(key) {
                    yield Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("invalid env variable name: {key}"),
                    ));
                    return;
                }
                cmd.env(key, value);
            }
        }

        // Non-interactive defaults: caller-provided env wins, inherited env
        // does not. Prevents pagers/editors from hanging on the piped path
        // when the parent process itself runs under e.g. PAGER=less.
        for (key, value) in non_interactive_defaults(&env) {
            cmd.env(key, value);
        }

        // Empty cwd means "inherit the parent process's working directory".
        // `current_dir("")` would fail the spawn and (before the stream-error
        // surfacing fix) produced a silently EMPTY stream.
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
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

        #[allow(clippy::expect_used)]
        let mut stream_stdout = child.stdout.take().expect("stdout pipe should be configured");
        #[allow(clippy::expect_used)]
        let mut stream_stderr = child.stderr.take().expect("stderr pipe should be configured");
        let mut stdout_done = false;
        let mut stderr_done = false;

        // Zero timeout: the command must not run at all. Kill it immediately
        // without reading any output, so the result is deterministic — the
        // child never gets a chance to write before the 0ms timer fires.
        if timeout_ms == Some(0) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            yield Ok(SpawnOutput {
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
                signal: Some(-1_i32),
                truncated: false,
            });
            return;
        }

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
                        signal: Some(-1_i32),
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
/// The `unsafe` blocks in this function are:
///
/// | Location | Call | Invariant |
/// |---|---|---|
/// | Stream setup | `pipe2` | `pipe_fds` is a valid pointer to 2 `i32`s |
/// | Timeout handler | `write` + `close` on `pipe_tx` | `pipe_tx` is a valid fd, not used after |
/// | Blocking task | `poll`, `read`, `close` on `pipe_rx` and `pty_fd` | Both fds are valid and open for the lifetime of `poll_fds`; `pipe_rx` is closed once after use |
/// | Watchdog check | `mem::zeroed` + `tcgetattr` on `pty_fd` | `termios` is a fully-owned stack out-buffer, zeroed before the call; `pty_fd` is valid and open for the loop's lifetime (POSIX: `tcgetattr` on a PTY master reads the attached slave's line discipline) |
///
/// These are trivially verified by inspection — the pipe fds are created
/// together, one is consumed per side, and each is closed exactly once.
///
/// [`poll(2)`]: https://man7.org/linux/man-pages/man2/poll.2.html
#[cfg(unix)]
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
        // Zero timeout: the command must not run at all. Yield the timeout
        // item immediately without spawning the process (deterministic).
        if timeout_ms == Some(0) {
            yield Ok(SpawnOutput {
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
                signal: Some(-1_i32),
                truncated: false,
            });
            return;
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let kill = kill_flag.clone();

        // Create a self-pipe before spawn_blocking so the write end
        // (pipe_tx) is accessible from the async timeout handler.
        let mut pipe_fds: [libc::c_int; 2] = [0; 2];
        // SAFETY: pipe2 is safe per POSIX; fds provides a valid array pointer.
        let pipe_result = unsafe {
            libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC)
        };
        if pipe_result != 0_i32 {
            yield Err(Error::other("failed to create self-pipe for timeout"));
            return;
        }
        let [pipe_rx, pipe_tx] = pipe_fds;

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
            // Empty cwd → inherit the parent's working directory (same
            // semantics as the piped path; an empty cwd fails the spawn).
            if !cwd.is_empty() {
                cmd_builder.cwd(&cwd);
            }

            // Non-interactive defaults: caller-provided env wins, inherited
            // env does not. The PTY makes stdout look like an interactive
            // terminal, so git etc. would otherwise launch a full-screen
            // pager (less) that blocks forever waiting for keystrokes.
            let non_interactive = non_interactive_defaults(&env);

            if let Some(env) = env {
                for (key, value) in env {
                    cmd_builder.env(key, value);
                }
            }
            for (key, value) in non_interactive {
                cmd_builder.env(key, value);
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
            // Input-wait watchdog state: timestamp of the first consecutive
            // observation of raw (non-canonical) terminal mode, if any.
            let mut watchdog_raw_since: Option<Instant> = None;
            loop {
                // Watchdog tick or I/O readiness: poll's return value is
                // not needed — revents below drive every event handler.
                let _n_ready = loop {

                    // Bounded timeout so the loop wakes up periodically to
                    // run the input-wait watchdog even with no I/O.
                    let res = unsafe { libc::poll(poll_fds.as_mut_ptr(), 2, WATCHDOG_TICK_MS) };
                    if res < 0_i32 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        let _ = tx.send(Err(Error::other(format!("pty poll error: {err}"))));
                        return;
                    }
                    break res;
                };

                // n_ready == 0 (watchdog tick, nothing readable): fall
                // through — all revents are zero, so every event handler
                // below no-ops and control reaches the input-wait check at
                // the bottom of the loop.

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

                // Input-wait watchdog: a live child that keeps the terminal
                // in non-canonical (raw) mode is an interactive pager/editor/
                // TUI waiting for keystrokes. Those keystrokes would come
                // from /dev/tty, which the harness never writes to, so the
                // child can only be freed by killing it. Environment vars
                // cannot prevent this hang (the pager reads the TTY
                // directly). A brief raw-mode flip is normal (line editing,
                // `read -n`), hence the sustained-raw-mode grace window.
                let child_alive = matches!(child.try_wait(), Ok(None));
                if !child_alive {
                    watchdog_raw_since = None;
                } else {
                    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
                    let is_raw = unsafe { libc::tcgetattr(pty_fd, &mut termios) } == 0
                        && termios.c_lflag & libc::ICANON == 0;
                    if !is_raw {
                        watchdog_raw_since = None;
                    } else {
                        let since =
                            *watchdog_raw_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= WATCHDOG_RAW_MODE_GRACE {
                            let _ = child.kill();
                            let _ = tx.send(Ok(SpawnOutput {
                                stdout: vec![],
                                stderr: WATCHDOG_MESSAGE.as_bytes().to_vec(),
                                exit_code: None,
                                signal: None,
                                truncated: false,
                            }));
                            break;
                        }
                    }
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
                        signal: Some(-1_i32),
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

/// Spawn a bash process into a Windows ConPTY and return its output as an
/// async stream.
///
/// Windows counterpart of the Unix [`spawn_bash_pty`]: uses `portable_pty`'s
/// ConPTY backend (Windows 10 1809+) so the child runs attached to a pseudo
/// terminal; stdout and stderr are multiplexed and [`SpawnOutput::stderr`] is
/// always empty.
///
/// # Windows ConPTY specifics
///
/// Unlike a Unix PTY, the ConPTY **output pipe never reaches EOF when the
/// child exits** — the conhost side keeps the pipe handle open, so a blocking
/// reader would hang forever. Two protocol quirks are handled here (both
/// verified experimentally against Git Bash under ConPTY):
///
/// 1. **DSR cursor query.** bash emits `ESC[6n` (cursor position request) at
///    startup and blocks until the terminal answers. The stream watches for
///    the sequence in the output and replies `ESC[1;1R` through the PTY
///    writer.
/// 2. **No EOF.** Stream termination is driven by the child's exit status:
///    once the watcher thread reports the reaped child, the reader is given a
///    short drain window for in-flight chunks, then the final status item is
///    emitted and the (still blocked) reader thread is detached.
///
/// The blocking reader runs in `tokio::task::spawn_blocking` and forwards
/// chunks through an unbounded `mpsc` channel; `child.wait()` runs in its own
/// thread because it may block longer than the stream lives.
///
/// # Timeout mechanism
///
/// The async deadline arms `tokio::select!` against the channel receiver. On
/// timeout the child is killed via `portable_pty::ChildKiller::kill`
/// (`TerminateProcess`) — callable from the async side through a pre-cloned
/// killer — in-flight chunks are drained, and the final `signal: Some(-1)`
/// item is yielded. The watcher's exit status is discarded in that case,
/// matching the Unix path (timeout item wins).
#[cfg(windows)]
use portable_pty::ChildKiller;

#[cfg(windows)]
pub(crate) fn spawn_bash_pty(
    env: Option<Vec<(String, String)>>,
    cwd: &str,
    command: &str,
    timeout_ms: Option<u64>,
) -> Pin<Box<dyn Stream<Item = Result<SpawnOutput, Error>> + Send>> {
    let cwd = cwd.to_string();
    let command = command.to_string();

    Box::pin(stream! {
        // Zero timeout: the command must not run at all. Yield the timeout
        // item immediately without spawning the process (deterministic).
        if timeout_ms == Some(0) {
            yield Ok(SpawnOutput {
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
                signal: Some(-1_i32),
                truncated: false,
            });
            return;
        }

        // Validate environment variables (same rules as spawn_bash) before
        // spawning anything.
        if let Some(env) = &env {
            let valid_pattern = env_var_pattern();
            for (key, _) in env {
                if !valid_pattern.is_match(key) {
                    yield Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("invalid env variable name: {key}"),
                    ));
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
                yield Err(Error::other(format!("failed to open pty: {e}")));
                return;
            }
        };

        let mut cmd_builder = CommandBuilder::new(resolve_bash());
        cmd_builder.arg("-c");
        cmd_builder.arg(&command);
        // Empty cwd → inherit the parent's working directory (same
        // semantics as the piped path; an empty cwd fails the spawn).
        if !cwd.is_empty() {
            cmd_builder.cwd(&cwd);
        }

        if let Some(env) = env {
            for (key, value) in env {
                cmd_builder.env(key, value);
            }
        }

        let mut child = match pair.slave.spawn_command(cmd_builder) {
            Ok(c) => c,
            Err(e) => {
                yield Err(Error::other(format!("failed to spawn command in pty: {e}")));
                return;
            }
        };

        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                yield Err(Error::other(format!("failed to clone pty reader: {e}")));
                return;
            }
        };
        // Take the writer so we can answer DSR cursor queries.
        let mut writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                yield Err(Error::other(format!("failed to take pty writer: {e}")));
                return;
            }
        };
        // Pre-clone the killer so the async side can terminate the child on
        // timeout while the watcher thread holds the blocking `wait()`.
        let mut killer = ChildKiller::clone_killer(&*child);

        // Reader thread: forwards chunks through an unbounded channel. An
        // empty Vec signals EOF/read-error. The thread may stay blocked in
        // read() forever (ConPTY never EOFs) — it is detached when the
        // stream ends; the OS reclaims it when the process exits.
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; BUFFER_SIZE];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Vec::new());
        });

        // Watcher thread: reaps the child and reports its status as
        // (exit_code, signal). ConPTY has no signal concept — a killed child
        // surfaces as a non-zero exit code.
        let (stx, mut srx) = mpsc::unbounded_channel::<(Option<i32>, Option<i32>)>();
        std::thread::spawn(move || match child.wait() {
            Ok(status) => {
                #[allow(clippy::cast_possible_wrap)]
                let _ = stx.send((Some(status.exit_code() as i32), None));
            }
            Err(e) => {
                let _ = stx.send((None, None));
                let _ = e;
            }
        });

        const DSR_QUERY: &[u8] = b"\x1b[6n";
        const DSR_REPLY: &[u8] = b"\x1b[1;1R";
        const DRAIN_WINDOW: Duration = Duration::from_millis(300);

        let deadline =
            timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        let mut timed_out = false;
        let mut answered_dsr = false;
        let mut status: Option<(Option<i32>, Option<i32>)> = None;
        // Chunks received during the post-exit drain window.
        let mut drained: Vec<Vec<u8>> = Vec::new();

        loop {
            tokio::select! {
                biased;

                () = async {
                    match deadline {
                        Some(dl) => tokio::time::sleep_until(dl).await,
                        None => std::future::pending::<()>().await,
                    }
                }, if deadline.is_some() && !timed_out => {
                    timed_out = true;
                    killer.kill().ok();
                    break;
                }

                status_msg = srx.recv() => {
                    match status_msg {
                        Some(s) => {
                            status = Some(s);
                            // Child reaped: give in-flight chunks a short
                            // drain window, then end the stream (no EOF).
                            let _ = tokio::time::timeout(DRAIN_WINDOW, async {
                                while let Some(chunk) = rx.recv().await {
                                    if chunk.is_empty() {
                                        break;
                                    }
                                    drained.push(chunk);
                                }
                            })
                            .await;
                            break;
                        }
                        None => break,
                    }
                }

                chunk = rx.recv() => {
                    match chunk {
                        Some(data) => {
                            if data.is_empty() {
                                // Reader EOF (unusual on ConPTY): keep
                                // waiting for the watcher's status.
                                continue;
                            }
                            if !answered_dsr
                                && data.windows(DSR_QUERY.len()).any(|w| w == DSR_QUERY)
                            {
                                answered_dsr = true;
                                let _ = writer.write_all(DSR_REPLY);
                                let _ = writer.flush();
                            }
                            yield Ok(SpawnOutput {
                                stdout: data,
                                stderr: vec![],
                                exit_code: None,
                                signal: None,
                                truncated: false,
                            });
                        }
                        None => break,
                    }
                }
            }
        }

        // Emit chunks drained during the post-exit window before the final
        // status item (chunks always precede the status item).
        for chunk in drained {
            yield Ok(SpawnOutput {
                stdout: chunk,
                stderr: vec![],
                exit_code: None,
                signal: None,
                truncated: false,
            });
        }

        if timed_out {
            yield Ok(SpawnOutput {
                stdout: vec![],
                stderr: vec![],
                exit_code: None,
                signal: Some(-1_i32),
                truncated: false,
            });
            return;
        }

        if let Some((exit_code, signal)) = status {
            yield Ok(SpawnOutput {
                stdout: vec![],
                stderr: vec![],
                exit_code,
                signal,
                truncated: false,
            });
        }
        // Watcher died without a status (should not happen) — end the
        // stream silently, matching the Unix path's behavior on error.
    })
}
