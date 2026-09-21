# `bash` data types: inputs and outputs

The bash module has one request type and a small set of stream / error types.

All stream/error types below live in `cosh_tools::bash::bsh` — only
`BashRunInput` is re-exported at the `bash` root. `Bash::run`'s signature
uses them, so import from `bash::bsh::…` when you need to name a concrete
type (the [example](../../../crates/cosh-tools/examples/bash/bash.rs) does).

---

## Request type

### `BashRunInput` — the `bash_run` arguments

```rust
pub struct BashRunInput {
    /// The bash command to execute. Must not be an absolute path
    /// and must not match dangerous security patterns.
    pub command: String,
    /// Optional per-call timeout override in milliseconds. May only RAISE
    /// the harness-configured timeout: it must be an integer strictly
    /// greater than the default (interpolated into the tool description);
    /// values equal to or below the default are rejected. It never
    /// changes the configured default.
    pub timeout_ms: Option<u64>,
}
```

`command` is the only required field; `timeout_ms` is optional and may be
omitted (the call then runs with the wrapper's configured timeout). All other
execution configuration — `cwd`, `env`, the timeout default, `pty` — lives on
the [`Bash`](bash.md#the-bash-wrapper) wrapper and is fixed when the harness
builds its tool set. It derives
`Deserialize` and `JsonSchema`, so the same struct both parses the tool
arguments and can generate their schema (the `inputSchema` on
`description_run` mirrors it by hand).

---

## Stream item

### `SpawnOutput` — one item in the output stream

```rust
pub struct SpawnOutput {
    pub stdout: Vec<u8>,     // a stdout chunk (≤ 4096 bytes), or empty
    pub stderr: Vec<u8>,     // a stderr chunk (≤ 4096 bytes), or empty
    pub exit_code: Option<i32>, // set exactly once: the process exited normally
    pub signal: Option<i32>,    // set exactly once: killed by a signal, or timeout
    pub truncated: bool,        // this chunk filled the whole 4096-byte buffer
}
```

`Bash::run` yields a stream of these. The three item kinds and their
invariants are described on the [module page](bash.md#the-output-contract);
the essentials:

- **Chunks** have non-empty `stdout` *or* `stderr` and both status fields
  `None`.
- **The status item** has empty `stdout`/`stderr` and exactly one of
  `exit_code` / `signal` set. `exit_code` and `signal` are **mutually
  exclusive**: a normal exit sets `exit_code` (0 or not); a signal death sets
  `signal` to the Unix signal number; a timeout sets `signal` to `-1`.
- **`truncated`** is `true` when a chunk exactly filled the 4096-byte read
  buffer — a per-chunk "more may be coming" hint, not an aggregate total.
- Both `stdout` and `stderr` are raw bytes, not text: the program may emit
  arbitrary bytes, and PTY mode rewrites `\n` to `\r\n`. Decode with
  `String::from_utf8_lossy` when displaying.

---

## Error and helper types

### `BashError` — the error from `Bash::run`

```rust
pub struct BashError {
    pub text_err: Option<String>,   // validation or human-readable reason
    pub exec_err: Option<ExecError>, // present only for execution-stage failures
}
```

`Bash::run` returns `Result<_, BashError>` and only *fails* on validation:
an absolute command path, or a command matching a dangerous security pattern.
In both cases `text_err` holds the reason (e.g. `"absolute command not
allowed, use relative path"` or `"Pattern match found: …"`) and `exec_err` is
`None`.

The harness renders an error from `text_err`, falling back to a signal summary
from `exec_err` when `text_err` is absent.

### `ExecError`

```rust
pub struct ExecError {
    pub stderr: Option<String>,
    pub signal: Option<i32>,
}
```

The execution-stage error payload carried by `BashError::exec_err`.

### `BashOutput`

```rust
pub struct BashOutput {
    pub stdout: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}
```

A legacy convenience aggregate (no `stderr`, no chunk granularity). Prefer
consuming the [`SpawnOutput`](#spawnoutput--one-item-in-the-output-stream)
stream directly.

> **Stream errors are dropped by `Bash::run`.** The underlying executor emits
> *stream* errors (`Err(io::Error)`) for runtime failures — an invalid
> environment-variable name (`ErrorKind::InvalidInput`), a nonexistent `cwd`,
> a spawn failure. The wrapper's stream only yields `Ok` items, so those
> surface as a **silently truncated/empty stream**, not as an `Err` from
> `Bash::run`. Only the pre-spawn validation produces a real error. This is
> intentional at the engine level (see the [executor reference](bsh.md)); when
> you need these failures, consume the engine's raw `Result`-carrying stream
> or validate inputs yourself.

---

## The `ToolDescription` on `Bash`

`Bash::description_run` is a ready-to-serve MCP tool description
(`{ "name", "description", "inputSchema" }`) for the `bash_run` tool. Its
`inputSchema` requires exactly one property — `command` — matching
[`BashRunInput`](#bashruninput--the-bash_run-arguments) above. The prose
description covers the security validation, the streaming semantics, and the
harness truncation behavior (output over the token budget is head/tail-cut
with the middle recoverable via `fs_read` / `find_grep`).