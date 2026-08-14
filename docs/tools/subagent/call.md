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

`AGENTS` is the static table of supported agents: `(api_name, binary,
[static_args])`. The public name is what the model passes as `agent`; the
binary and static arguments are what get executed, with the user's `input`
appended as the final argument.

| Name | Binary | Invocation |
|---|---|---|
| `opencode` | `opencode` | `opencode run --auto "<input>"` |
| `kilo` | `kilo` | `kilo run --auto "<input>"` |
| `claude` | `claude` | `claude -p --permission-mode dontAsk --bare "<input>"` |
| `devin` | `devin` | `devin -p --permission-mode dangerous "<input>"` |
| `codex` | `codex` | `codex exec --sandbox workspace-write "<input>"` |
| `cline` | `cline` | `cline -y "<input>"` |
| `cursor` | `agent` | `agent -p --force --trust "<input>"` |
| `crush` | `crush` | `crush run --yolo --quiet "<input>"` |
| `hermes` | `hermes` | `hermes -z "<input>"` |
| `openhands` | `openhands` | `openhands --headless -t "<input>"` |
| `pi` | `pi` | `pi -p "<input>"` |
| `interpreter` | `interpreter` | `interpreter exec --ask-for-approval auto "<input>"` |
| `letta` | `letta` | `letta -p "<input>"` |
| `vibe` | `vibe` | `vibe --prompt --agent auto-approve "<input>"` |
| `aider` | `aider` | `aider --message --yes --no-auto-commits "<input>"` |
| `omp` | `omp` | `omp -p "<input>"` |
| `goose` | `goose` | `goose run -t "<input>"` |
| `gemini` | `gemini` | `gemini -p "<input>"` |
| `forge` | `forge` | `forge -p "<input>"` |

Two design points behind the table:

- **Only headless-capable agents are listed.** Every entry runs in a
  non-interactive mode (`-p`, `run`, `exec`, `--message`, …) — purely
  interactive TUIs cannot be driven as a sub-process and are excluded.
- **The static args are tuned for automation**: auto-approval flags prevent
  blocking on prompts, sandbox/permission modes keep the automation safe,
  and no-auto-commit flags preserve git control (e.g. `aider`).

To add an agent, add one tuple to `AGENTS` — that is the whole integration.

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
Unsupported agent 'nope'. Supported agents: opencode, kilo, claude, ….
Use bash_run for shell commands.
```

This runs first inside `call`, so an unknown name fails before anything is
spawned.

---

## The call pipeline

1. **Validate** the agent name (`validate_agent`).
2. **Resolve** the entry and build the command: `binary <static args> <input>`,
   with stdout and stderr piped.
3. **Spawn.** A `NotFound` spawn error is turned into a helpful message with
   the agent-specific install command (e.g. `npm install -g
   @anthropic-ai/claude-code` for claude); other spawn errors surface as
   `"Failed to spawn '<binary>': …"`.
4. **Stream.** A reader thread reads stdout and stderr line by line. Each
   stdout line has ANSI escape sequences stripped (so progress bars and
   colors do not pollute the result), is appended to a shared accumulator,
   and is sent through `chunk_tx` for live display. stderr lines are
   forwarded too (without ANSI stripping), keeping the full picture.
5. **Wait.** The main thread polls the reader every 100 ms until it finishes,
   bounded by a **2-minute timeout**. When the reader is done, `child.wait()`
   gives the exit code and the call returns `(accumulated_output, code)`.
6. **On timeout:** the child is killed. If nothing was produced, the call
   fails with a message naming the timeout and, per agent, a hint about what
   may be blocking it (e.g. `" claude may be waiting for permission
   approval. Use --permission-mode dontAsk."`). If partial output exists, it
   is returned with exit code `-1` and a warning is logged — the partial
   result is real data, not an error.

---

## Errors

| Situation | Result |
|---|---|
| Unknown agent name | `Err("Unsupported agent …")` — before any spawn |
| Binary not in `PATH` | `Err` with the install command for that agent |
| Other spawn failure | `Err("Failed to spawn '<binary>': …")` |
| Timeout, no output | `Err` naming the timeout + a per-agent blocking hint |
| Timeout, partial output | `Ok((partial_output, -1))` — with a warning log |
| Normal exit | `Ok((output, exit_code))` |
