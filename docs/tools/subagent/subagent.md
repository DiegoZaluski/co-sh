# The `subagent` module: calling sub-agents

`subagent` lets the agent delegate a task to another agent — either an
**external agent CLI** (opencode, claude, aider, …) spawned as a child
process, or the **internal sub-agent** (a nested harness that runs the task
with a fresh, empty context). Both are reached through a *single* visible
tool, `subagent_call`; the model never sees two tools.

| Tool | What it does |
|---|---|
| [`subagent_call`](call.md) | Delegate a task to a sub-agent. `agent` names the CLI to run; omitting it routes to the internal agent. |

The module has three pieces:

- [`SubAgent`](#the-subagent-wrapper) — the wrapper. Owns the tool
  description (tailored to the CLIs actually installed) and the last-input
  storage.
- [`call`](call.md) — the external-CLI engine: the agent registry, PATH
  detection, and the spawn/stream/wait pipeline.
- [`types`](types.md) — the input and output structs.

---

## The `SubAgent` wrapper

```rust,ignore
use cosh_tools::subagent::SubAgent;

let sub = SubAgent::new();
sub.resolve_input(Some("review this PR".into()))?;   // stores + returns
sub.resolve_input(None)?;                            // reuses the stored message
```

`SubAgent::new()` builds a `description_call` — the ready-to-serve MCP tool
description for `subagent_call` — and an empty last-input store.
`SubAgent::default()` is `SubAgent::new()`.

The wrapper's public surface:

| Member | Purpose |
|---|---|
| `description_call` | The `subagent_call` tool description (name, prose, schema). |
| `set_note(text)` | Rebuild the description with `text` interpolated into the prose flow (used by the harness to teach the internal-agent path). An empty note keeps the original description byte-for-byte. |
| `resolve_input(input)` | Resolve the effective input message, storing/reusing it (below). |

### Input reuse: retries without re-typing

`resolve_input` implements the "last message" convention:

- `Some(text)` with non-empty text → the text is stored as the last message
  and returned.
- `Some("")` or `None` → the **stored** message is returned, so a failed call
  can be retried without re-writing the whole prompt.
- Nothing stored yet → an `Err` explaining that no sub-agent message exists
  yet and asking for an `input` argument.

The store is per-instance, and the harness creates one `SubAgent` per agent
loop — so stored input never leaks across sessions, and no `clean()` is
needed.

### The tool schema

`description_call`'s `inputSchema` is deliberately permissive — **neither
argument is required**:

- `agent` — optional string, restricted to an `enum` of the agent names.
  When omitted (or empty), the call routes to the internal agent.
- `input` — optional string; falls back to the stored last message.

The `enum` is computed from [`detect_installed()`](call.md#detect_installed):
only CLIs found in `PATH` are listed. If none are installed, the enum keeps
the full agent list anyway (so the model can still attempt a call) and the
description says to install one.

---

## Two implementations, one tool

### External CLI (when `agent` is provided)

The named agent's binary is spawned with its static arguments plus the input
message as the final argument (e.g. `opencode run --auto "<input>"`), and
its output is streamed back. The engine is documented on the
[call page](call.md). Only agents with a **non-interactive / headless mode**
are supported — purely interactive TUIs cannot be driven this way.

### Internal agent (when `agent` is omitted or empty)

The harness intercepts the call and runs a **nested harness** instead:

- starts with an **empty context** (no conversation history, no parent
  state);
- runs in **auto-approve (Yolo) mode** — no permission dialogs;
- **persists nothing** — the sub-agent's session state does not survive;
- **returns only its final report** as the tool result; its streaming text is
  forwarded live to the TUI;
- cannot ask the user questions or stop the loop: `ask_questions` and
  `stop_agent_loop` are removed from its tool set so it never even sees
  them (it must end with a written answer).

The internal sub-agent uses its own prompt, so it never inherits the main
agent's review-loop mandate (it would otherwise nest review sub-agents
indefinitely).

---

## The harness and permission model

The harness wires the module together:

- It calls `set_note(SUBAGENT_INTERNAL_NOTE)` at startup so the description
  naturally explains the internal-agent routing.
- `subagent_call` is gated by the permission system: **not available in Ask
  mode**, **always needs approval in Build mode** (regardless of which
  agent), and only Yolo mode runs it without asking.

---

## Summary

- One tool, two targets: an external agent CLI (`agent` set) or the internal
  sub-agent (agent omitted/empty).
- `agent` and `input` are both optional; `input` falls back to the last
  message sent in this session.
- The tool description adapts to what is installed on `PATH`.
- The internal sub-agent is a clean-room nested harness: empty context,
  auto-approve, no persistence, final report only.

Next: the [data types](types.md), then the [call engine](call.md) — the
agent registry and the spawn/stream/wait pipeline.
