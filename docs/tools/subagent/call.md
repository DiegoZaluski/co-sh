# `call` — the external agent CLI engine

`call` is the part of the module that actually talks to other agent CLIs: it
resolves the agent name, spawns the binary as a child process, streams the
output in real time, and accumulates the result until exit.

```rust,ignore
pub fn call(
    agent: &str,
    input: &str,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<(String, i32), String>
```

The `chunk_tx` channel receives every output line as it is produced (for
live TUI display); the returned tuple is the accumulated output plus the
exit code. **The function blocks** — the harness runs it inside
`tokio::task::spawn_blocking` so the async runtime is not blocked.

---

## The agent registry (`AGENTS`)

`AGENTS` is the static table of supported agents, modeled by the [`Agent`]
struct:

```rust,ignore
pub struct Agent {
    pub name: &'static str,        // public name the model passes as `agent`
    pub binary: &'static str,      // executable to spawn
    pub args: &'static [&'static str], // static args placed before the input
    pub input_flag: Option<&'static str>, // how the user `input` is passed
}
```

The `input_flag` field is the single abstraction for how each CLI receives the
prompt:

- `input_flag: None` — the `input` string is appended as a **final positional
  argument** (the common case): `binary <args> <input>`.
- `input_flag: Some(flag)` — the `input` is passed as the **value of `flag`**:
  `binary <args> <flag> <input>` (for CLIs such as `aider` whose prompt is a
  named flag, `--message`, rather than a positional argument).

`Agent::args_for(input)` builds the complete argv for either case, and
`Agent::invocation()` renders the human-readable form used in the tool
description and docs.

| Name | Binary | Invocation |
|---|---|---|
| `opencode` | `opencode` | `opencode run --auto "<input>"` |
| `claude` | `claude` | `claude -p --permission-mode bypassPermissions "<input>"` |
| `codex` | `codex` | `codex exec --sandbox workspace-write "<input>"` |
| `cursor` | `agent` | `agent -p --force "<input>"` |
| `aider` | `aider` | `aider --yes --no-auto-commits --message "<input>"` |
| `goose` | `goose` | `goose run -t "<input>"` |
| `kilo` | `kilo` | `kilo run --auto "<input>"` |
| `gemini` | `gemini` | `gemini -p "<input>"` |
| `interpreter` | `interpreter` | `interpreter exec --ask-for-approval auto "<input>"` |

Two design points behind the table:

- **Only headless-capable agents are listed.** Every entry runs in a
  non-interactive mode (`-p`, `--message`, `run`, `exec`, …) — purely
  interactive TUIs cannot be driven as a sub-process and are excluded.
- **The static args are tuned for automation**: auto-approval flags prevent
  blocking on prompts (e.g. `--permission-mode bypassPermissions` for claude),
  sandbox/permission modes keep the automation safe, and no-auto-commit flags
  preserve git control (e.g. `aider --no-auto-commits`).

To add an agent, add one [`Agent`] entry to `AGENTS` — that is the whole
integration.

---

## `detect_installed` — which CLIs exist

```rust,ignore
pub fn detect_installed() -> &'static Vec<&'static str>
```

Scans `PATH` for each registered agent's binary and returns the names found.
The result is cached in a `OnceLock`, so detection runs **once per process**
— repeated calls return the cached list. `SubAgent::new()` uses this to
tailor the tool description's agent enum to what is actually installed.

---

## `validate_agent` — name checking

```rust,ignore
pub fn validate_agent(agent: &str) -> Result<(), String>
```

Returns `Ok` when the name is in `AGENTS`, else an error listing every
supported agent:

```
Unsupported agent 'nope'. Supported agents: opencode, claude, codex, ….
Use bash_run for shell commands.
```

This runs first inside `call`, so an unknown name fails before anything is
spawned.

---

## The call pipeline

1. **Validate** the agent name (`validate_agent`).
2. **Resolve** the entry and build the command via `entry.args_for(input)`,
   with stdout and stderr piped. The `input` becomes a positional argument or
a named flag's value depending on `input_flag`.
3. **Spawn.** A `NotFound` spawn error is turned into a helpful message with
   the agent-specific install command (e.g. `npm install -g
   @anthropic-ai/claude-code` for claude); other spawn errors surface as
   `"Failed to spawn '<binary>': …"`.
4. **Stream.** A reader thread reads stdout **and** stderr line by line. Each
   line has ANSI escape sequences stripped (so progress bars and colors do not
   pollute the result), is appended to a shared accumulator, and is sent
   through `chunk_tx` for live display.
5. **Wait.** The main thread polls the reader every 100 ms until it finishes,
   bounded by a timeout (2 minutes by default; override with the
   `COSH_SUBAGENT_TIMEOUT_SECS` environment variable). When the reader is
done, `child.wait()` gives the exit code and the call returns
   `(accumulated_output, code)`.
6. **On timeout:** the child is killed. If nothing was produced, the call
   fails with a message naming the timeout and, per agent, a hint about what
   may be blocking it (e.g. `" claude may be waiting for permission
   approval. Use --permission-mode bypassPermissions."`). If partial output
exists, it is returned with exit code `-1` and a warning is logged — the
partial result is real data, not an error.

---

## Errors

| Situation | Result |
|---|---|---|
| Unknown agent name | `Err("Unsupported agent …")` — before any spawn |
| Binary not in `PATH` | `Err` with the install command for that agent |
| Other spawn failure | `Err("Failed to spawn '<binary>': …")` |
| Timeout, no output | `Err` naming the timeout + a per-agent blocking hint |
| Timeout, partial output | `Ok((partial_output, -1))` — with a warning log |
| Normal exit | `Ok((output, exit_code))` |
