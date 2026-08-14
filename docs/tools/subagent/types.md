# `subagent` data types

The module has exactly two data types: the call's input and its output. Both
derive `Serialize` + `Deserialize`, so they round-trip through the tool-call
JSON directly.

---

## `SubAgentCallInput` — the `subagent_call` arguments

```rust
pub struct SubAgentCallInput {
    /// The agent CLI to call (e.g. "opencode"). Must be one of the
    /// supported agents listed in the tool description.
    ///
    /// Optional: if omitted (or empty), an internal agent runs the task
    /// instead — a fresh nested harness with an empty context, in
    /// auto-approve mode, that persists nothing and returns only its final
    /// report.
    pub agent: Option<String>,

    /// The message to send to the sub-agent.
    ///
    /// Optional: if omitted (or empty), the last message sent to a
    /// sub-agent in this session is reused automatically. If no sub-agent
    /// has been called yet, an error is returned telling the caller to
    /// provide one.
    pub input: Option<String>,
}
```

Both fields are optional — deliberately. The combination of what is present
and what is missing selects the behavior:

| `agent` | `input` | Behavior |
|---|---|---|
| `"opencode"` | `"…"` | External CLI `opencode`, with the message. |
| `"opencode"` | omitted / `""` | External CLI `opencode`, reusing the last message. |
| omitted / `""` | `"…"` | **Internal** agent, with the message. |
| omitted / `""` | omitted / `""` | **Internal** agent, reusing the last message. |

The `agent` value must be one of the names in the
[agent registry](call.md#the-agent-registry) (`AGENTS`); anything else is
rejected by [`validate_agent`](call.md#validate_agent) with a
`"Unsupported agent …"` error.

---

## `SubAgentCallOutput` — the result

```rust
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Exit code of the process.
    pub exit_code: i32,
}
```

The harness returns this as JSON after the sub-agent finishes. The two fields
tell the caller everything needed to judge the outcome:

- `output` — the full accumulated stdout/stderr of the external CLI, or the
  internal agent's final report.
- `exit_code` — `0` on success; a non-zero process exit code on failure; or
  `-1` when the process was killed by a timeout (partial output may still be
  present, and the harness logs a warning in that case).

For the external path, a **hard failure** (spawn error, or a timeout with no
output at all) is returned as an `Err` string instead of an output struct —
see the [call page](call.md#errors) for the exact cases.
