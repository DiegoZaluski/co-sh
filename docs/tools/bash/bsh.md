# `bsh` — the bash executor engine

`bsh` is the part of the module that actually talks to the operating system.
While the [`Bash`](bash.md#the-bash-wrapper) wrapper holds configuration and is
the public interface, this page documents the engine underneath: the
spawning functions, the streaming and timeout mechanics, and the complete
security pattern list.

The engine exposes one public function and two internal spawners:

| Function | Role |
|---|---|
| [`run(timeout_ms, env, pty, command, cwd)`](#run--the-free-function) | Validate, then stream the command; dispatches to a spawner. |
| `spawn_bash(env, cwd, command, timeout_ms)` | The piped (non-PTY) spawner. |
| `spawn_bash_pty(env, cwd, command, timeout_ms)` | The PTY spawner (Unix only). |

Both spawners return `Pin<Box<dyn Stream<Item = Result<SpawnOutput,
io::Error>>>>` — note the `Result` per item. `run` wraps whichever spawner
was chosen and **filters out the `Err` items**, so its stream only yields
`Ok(SpawnOutput)`. This is the source of the "stream errors are dropped" note
on the [types page](types.md).

---

## `run` — the free function

```rust,ignore
pub fn run<'a>(
    timeout_ms: Option<u64>,
    env: &Option<Vec<(String, String)>>,
    pty: bool,
    command: &'a str,
    cwd: &'a str,
) -> Result<Pin<Box<dyn Stream<Item = SpawnOutput> + Send + 'a>>, BashError>
```

`run` performs the two **pre-spawn guards** and fails with a `BashError`
(`text_err` set, `exec_err` `None`) without executing anything:

1. `Path::new(command).is_absolute()` → `"absolute command not allowed, use
   relative path"`.
2. `validate_bash_patterns(command)` → `"Pattern match found: <regex>"` for
   the first matching pattern.

It then selects the spawner: `spawn_bash_pty` when `pty` is true **and** the
platform is Unix; `spawn_bash` otherwise (on non-Unix platforms a `pty: true`
request silently degrades to the piped path). The selected stream's `Err`
items are dropped as described above.

---

## Security patterns

`critical_bash_patterns()` returns a lazily-initialized list of compiled
regexes; `validate_bash_patterns` runs a command against all of them and
errors on the first match. The list is case-insensitive where it matters. In
full:

| Category | Pattern (normalized) |
|---|---|
| Recursive destruction | `rm -…r/f/R/F… /`, `sudo rm`, `chmod -R <octal> /`, `chmod -R <symbolic> /`, `chown -R <owner> /` |
| Fork bomb | `:(){ :\|:` (case-insensitive) |
| Disk / filesystem | `> /dev/sd…`, `mkfs…`, `dd if=… of=/dev/…`, `shred /dev/…`, `cryptsetup` |
| System-config writes | `> /etc/passwd`, `> /etc/shadow`, `> /etc/sudoers`, `tee … /etc/passwd`, `tee … /etc/shadow`, `tee … /etc/sudoers` |
| Remote-fetch-then-execute | `curl/wget/fetch … \| bash/sh/zsh/fish`, `bash/sh/zsh/source/. <(curl/wget/fetch …)`, `eval $(curl/wget/fetch …)`, `` eval `curl/wget/fetch …` `` |
| Process / host control | `kill -9 1`, `shutdown`, `poweroff`, `reboot`, `halt`, `init 0` |
| Network-shell exfil | `nc … -e/-c …` |

A few notes on how they are written:

- The `rm`/`chmod`/`chown` patterns demand a literal `/` right after the
  flags (`\b` only anchors the start of the command word), so `rm -rf /` and
  `rm -rf /tmp/x` are caught while `rm -rf ./vendor` passes — `./vendor`
  starts with `.`, not `/`.
- The `> /etc/…` and `> /dev/sd…` patterns are unanchored: they match any
  `>` immediately followed by the path, so **both** the truncating
  `echo x > /etc/passwd` and the appending `echo x >> /etc/passwd` are
  blocked (the second `>` of `>>` satisfies the pattern).
- `curl … | bash` is blocked, but a download-then-run in two separate
  commands (`curl -o /tmp/x && bash /tmp/x`) is **not** matched.
- `sudo rm` is blocked even without `/`.

> These are tripwires against catastrophic typos, not a sandbox. See the
> [security note on the module page](bash.md#security-validation).

---

## `spawn_bash` — the piped path

`spawn_bash` builds a `tokio::process::Command::new("bash")` and runs
`bash -c <command>` with `cwd`, `stdin` from `/dev/null`, and piped
stdout/stderr. It streams with a `tokio::select!` loop reading up to
`BUFFER_SIZE` (4096) bytes from stdout and stderr in parallel.

Ordering and flags:

- Reads are **biased** toward the timeout arm, so a deadline can preempt a
  data read that is already available.
- Each chunk is emitted as a `SpawnOutput` with the corresponding buffer set
  and `truncated: n == 4096`.
- When both pipes hit EOF, the loop breaks, `child.wait()` runs, and the
  **final** item carries the exit status: `exit_code: status.code()` with
  `signal` from `std::os::unix::process::ExitStatusExt::signal` (Unix), or
  `exit_code` only on non-Unix.

### Timeout mechanics

`timeout_ms = Some(ms)` arms a `sleep_until(now + ms)` deadline; when it fires
the child is killed and the stream yields a final `signal: Some(-1)`,
`exit_code: None`, empty-buffers item. The special case `Some(0)` never reads
output — the child is spawned and killed immediately — so the result is
exactly one deterministic timeout item. (The PTY path short-circuits even
earlier, before spawning; see below.)

With a nonzero timeout, output already read before the deadline is preserved —
the timeout item always comes last, after any chunks the child managed to
write.

### Environment validation

Environment variables are validated against `^[A-Za-z_][A-Za-z0-9_]*$`
before the child spawns; an invalid name yields an `Err(io::Error::new(
ErrorKind::InvalidInput, "invalid env variable name: …"))` item and the
stream ends. Because `Bash::run` drops stream errors, this surfaces as an
empty stream through the wrapper.

---

## `spawn_bash_pty` — the PTY path

The PTY path uses `portable_pty` to give the child a real terminal. It differs
from the piped path in several ways:

- **Multiplexing.** stdout and stderr share the terminal, so every emitted
  `SpawnOutput` has `stderr` empty. The terminal line discipline rewrites
  `\n` to `\r\n` and interprets control codes (colors, prompts, progress).
- **Sizing.** The PTY is 24 rows × 80 columns.
- **Bridging.** `portable_pty`'s I/O is synchronous, so the reader runs in a
  `tokio::task::spawn_blocking` task and sends chunks through an unbounded
  `mpsc` channel — one extra buffer copy per chunk.
- **Signal mapping.** `portable_pty::ExitStatus::signal()` returns the libc
  `strsignal(3)` *description* (e.g. `"Killed"`), not the constant name; a
  table maps each description back to the numeric signal so PTY and piped
  paths agree on the `signal` field. Unmapped descriptions fall through to
  `None`. As in the piped path, `exit_code` is `None` when the process died
  by a signal, `Some(n)` otherwise.

### The self-pipe timeout

Because the reader blocks inside a blocking thread, the async timeout cannot
just abort it. Instead, a **self-pipe** unblocks the blocking `poll(2)`:

1. `pipe2` creates a pipe before the blocking task starts; the read end moves
   into the blocking task, the write end stays on the async side.
2. The blocking task's `poll(2)` watches **both** the PTY master fd and the
   pipe read end.
3. When the async timeout fires, it sets a kill flag, writes one byte to the
   pipe write end, and closes it. `poll` wakes, the blocking task sees the
   pipe fd readable, breaks out, kills and reaps the child, then sends the
   final `signal: Some(-1)` item.

The unsafe code is confined to three sites — pipe creation (`pipe2`), the
timeout-side `write` + `close`, and the blocking-side `poll`/`read`/`close` —
and each fd is created once, consumed on one side, and closed exactly once
(verified by inspection in the source).

As in the piped path, `Some(0)` yields the deterministic single timeout
item — but here the check runs before the process is even spawned, so the
PTY is never opened.

---

## Summary

- `run` validates (absolute path, critical patterns) then dispatches to a
  spawner; `Bash::run` hides the spawner selection and drops stream errors.
- The piped path reads stdout/stderr in parallel in 4096-byte chunks and ends
  with one exit-status item; `exit_code` and `signal` are mutually exclusive.
- Timeouts kill the child and emit a final `signal: -1` item; `0` never reads
  output; partial output before the deadline is preserved.
- The PTY path multiplexes through a 24×80 pseudo-terminal with a blocking
  reader bridged via a channel, a self-pipe for timeouts, and
  strsignal→number mapping.