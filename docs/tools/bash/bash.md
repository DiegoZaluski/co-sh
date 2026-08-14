# The `bash` module: executing shell commands

This module runs a bash command in a subprocess and streams its output back to
the agent:

| Tool | What it does |
|---|---|
| [`bash_run`](bsh.md) | Execute a command via `bash -c`, streaming stdout/stderr as they are produced, with a configurable timeout, environment, PTY mode, and working directory. |

It mirrors the `fs` / `find` design: a builder-style wrapper — [`Bash`](#the-bash-wrapper)
— holds the execution configuration, and the operation is a thin method on it.
The heavy lifting — spawning the child process, streaming output, enforcing
timeouts, and the security validation — lives in the [`bsh`](bsh.md) executor
engine. The [type reference](types.md) documents the input and output data
structures.

This page covers the wrapper and the output contract. The
[executor reference](bsh.md) covers the streaming mechanics and the full
security pattern list.

---

## The `Bash` wrapper

`Bash` is a builder-style struct holding the configuration for a single,
reusable execution context:

```rust,ignore
use cosh_tools::bash::Bash;

let bash = Bash::new()
    .cwd("/home/user/project")     // working directory
    .env(Some(vec![("RUST_BACKTRACE".into(), "1".into())]))
    .timeout(10_000);              // ms; None = no timeout

let mut stream = bash.run("cargo test")?;   // returns an async stream
```

`Bash::run` is **asynchronous**: it returns a `Pin<Box<dyn Stream<Item =
SpawnOutput>>>` that yields output chunks as the child produces them, so the
caller can render them live (the harness streams each chunk to the TUI before
the command finishes). The stream borrows from the `Bash` instance and must
not outlive it.

The builder methods, and their defaults from `Bash::new()`:

| Method | Meaning | Default |
|---|---|---|
| `cwd(path)` | Working directory for the child. | `""` — inherits the parent process's directory. |
| `env(Some(vec![(k, v)]))` | Environment variables for the child. | `None` — inherit the parent environment. |
| `timeout(ms)` | Kill the child after `ms` milliseconds. | `None` — no timeout. |
| `pty(true)` | Run the child in a pseudo-terminal (see below). | `false` — separate piped streams. |

All four consume `self` and return it, so they chain; `Bash` implements
`Default` and `Bash::default()` is `Bash::new()`.

### `pty` — the two execution modes

- **`pty: false` (default)** — stdout and stderr are captured as two separate
  piped streams. The output is exactly the program's bytes.
- **`pty: true`** — the child is attached to a pseudo-terminal. stdout and
  stderr are **multiplexed into one stream** (colored output, interactive
  prompts, progress bars all behave as if the child were on a real terminal).
  `SpawnOutput::stderr` is always empty in this mode, and the terminal line
  discipline rewrites `\n` to `\r\n`, so the bytes differ from the piped mode.

`Bash` also carries a ready-to-serve MCP tool description in the public field
`description_run` — a `ToolDescription` with `name: "bash_run"`, a description
covering the security validation and the truncation behavior, and an
`inputSchema` requiring a single `command` string.

> **The tool is configured once, per-call it takes only `command`.** The
> harness constructs a single `Bash` scoped to the project root and the
> `bash_run` tool's input is just `{ "command": "…" }` (see
> [`BashRunInput`](types.md)). Timeout, environment, and PTY mode are harness
> configuration, not per-call arguments.

---

## The output contract

`Bash::run` yields a stream of [`SpawnOutput`](types.md) items. Each item is
one of three kinds, and the model can tell which by the combination of fields:

| Item kind | Fields set | Meaning |
|---|---|---|
| stdout chunk | `stdout` (≤ 4096 bytes), `signal`/`exit_code` `None` | Part of the program's stdout. |
| stderr chunk | `stderr` (≤ 4096 bytes), `signal`/`exit_code` `None` | Part of the program's stderr. |
| exit status | empty `stdout`/`stderr`, one of `exit_code` or `signal` | The command finished. |

Two invariants matter for reading the stream:

- **`exit_code` and `signal` are mutually exclusive.** A normal exit sets
  `exit_code` and leaves `signal` `None`. A process killed by a signal sets
  `signal` (the Unix signal number) and leaves `exit_code` `None`. The harness
  only prints a non-zero `exit_code`; a zero exit is indistinguishable from
  "no status item yet" by design — success is silent.
- **The status item is always last, and chunks always precede it.** Chunks
  emitted before the final status item carry no status fields, so a partial
  result is easy to assemble: concatenate every chunk, then read the final
  status item.

### Timeout

When `timeout(ms)` is set and the child outlives it:

- the child is killed, and
- the stream yields a **final** item with `signal: Some(-1)` and
  `exit_code: None`, regardless of what the child printed before.

Two special cases:

- **`timeout(0)`** kills the child immediately without reading any output, so
  the result is deterministic: exactly one item, `signal: Some(-1)`, empty
  streams. The command never gets a chance to write.
- **Partial output is preserved.** If the command printed something before the
  deadline (e.g. `echo hello && sleep 10` with a short timeout), the output
  chunks arrive first and the timeout item last. An agent can use the retained
  prefix to diagnose *why* the command hung.

> In contrast to `find`, a bash timeout is **not** partial-but-successful data:
> the item is an explicit kill signal (`-1`). Treat any command that ends with
> `signal: -1` as incomplete — if the output before it is meaningful, use it,
> but never assume the command finished.

### Large output

Output is streamed in fixed 4096-byte chunks. A chunk whose length *equals*
4096 is flagged `truncated: true` — a hint that more data is in flight (the
flag is a per-chunk byte-queue signal, not an aggregate indicator). The
caller's aggregate length is the only reliable total.

At the harness level, a `bash_run` output larger than the **3000-token model
budget** is head/tail-truncated with the middle saved to a scratch log file
and replaced by a notice pointing at it — read the middle back with `fs_read`
or `find_grep`, never re-execute. The TUI always receives the full output as
a live stream before truncation runs, so the user is unaffected.

---

## Security validation

Every command is checked **before** spawning. `Bash::run` fails fast — it
returns a `BashError` with the reason in `text_err`, and nothing is executed:

1. **Absolute command paths are rejected.** `bash.run("/bin/ls")` errors with
   `"absolute command not allowed, use relative path"`. Use a relative command
   (`bash.run("ls")`) — the `cwd` already scopes where the command runs.
2. **Dangerous patterns are rejected.** A fixed list of critical patterns is
   matched against the command; the first hit errors with the pattern that
   matched. The list blocks recursive destruction (`rm -rf /`, `sudo rm`,
   `chmod -R … /`, `chown -R … /`), fork bombs (`:(){ :|:`), disk
   destruction (`> /dev/sdX`, `mkfs`, `dd if=… of=/dev/…`, `shred /dev/…`,
   `cryptsetup`), system-config writes (`> /etc/passwd`, `tee … /etc/shadow`,
   etc.), remote-fetch-then-execute (`curl … | bash`, `bash <(curl …)`,
   `eval $(curl …)`), process/host control (`kill -9 1`, `shutdown`,
   `poweroff`, `reboot`, `halt`, `init 0`), and network-shell exfil
   (`nc … -e`). The complete list is on the [executor reference](bsh.md).

The harness additionally gates `bash_run` behind the permission system:
`bash_run` is **not available in Ask mode**, always needs approval in Build
mode (regardless of path), and only Yolo mode runs it without asking.

> **Blocked patterns are a tripwire, not a sandbox.** The list stops the
> well-known foot-guns that would be catastrophic if a command were misparsed
> or a variable expanded wrong. It is not a general command sandbox — a
> sufficiently creative command can always evade a fixed pattern list. The
> permission system (approval before dispatch) is the real boundary.

---

## Summary

- Configure one `Bash`, call `run` — the tool input is just a `command`.
- The stream is chunks then one final status item; `exit_code` and `signal`
  are mutually exclusive; a timeout ends the stream with `signal: -1`.
- `pty: true` multiplexes stdout/stderr through a pseudo-terminal (stderr
  always empty, `\n` → `\r\n`).
- Commands are validated before spawning: absolute paths and a fixed list of
  dangerous patterns are rejected with a `BashError`.

Next: the [data types](types.md), then the
[executor reference](bsh.md) — the full security list and the streaming
mechanics.