# `subagent` data types

The module has exactly two data types: the call's input and its output. Both
derive `Serialize` + `Deserialize`, so they round-trip through the tool-call
JSON directly.

---

## `SubAgentCallInput` — the `subagent_call` arguments

```rust
pub struct SubAgentCallInput {
    /// The agent harness to call (e.g. "gemini"). Must be one of the
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
| `"gemini"` | `"…"` | External ACP harness `gemini`, with the message. |
| `"gemini"` | omitted / `""` | External ACP harness `gemini`, reusing the last message. |
| omitted / `""` | `"…"` | **Internal** agent, with the message. |
| omitted / `""` | omitted / `""` | **Internal** agent, reusing the last message. |

The `agent` value must be one of the names in the
[agent registry](acp.md#the-agent-registry) (`ACP_AGENTS`); anything else is
rejected by [`validate_agent`](acp.md#validate_agent) with a
`"Unsupported agent …"` error.

---

## `SubAgentCallOutput` — the result

```rust
pub struct SubAgentCallOutput {
    /// Accumulated output from the sub-agent.
    pub output: String,
    /// Why the prompt turn ended: a snake_case ACP stop reason (e.g.
    /// `end_turn`, `cancelled` — the spec-mandated answer to a
    /// `session/cancel`), or the client-side terminal marker `error` when
    /// the harness failed mid-turn. There is no timeout: the turn runs
    /// until the agent ends it or the user stops it.
    pub stop_reason: String,
}
```

The harness returns this as JSON after the sub-agent finishes. The two fields
tell the caller everything needed to judge the outcome:

- `output` — the full accumulated agent message chunks of the external ACP
  harness, or the internal agent's final report.
- `stop_reason` — the snake_case ACP stop reason of the completed turn (e.g.
  `end_turn`, `cancelled` when the user stopped it via `session/cancel`,
  `refusal`); or `"error"` when the ACP turn failed mid-flight with partial
  output already produced.

For the external path, a **hard failure** (validation error, or a turn
failure with no output at all) is returned as an `Err` string instead
of an output struct — see the [acp page](acp.md#errors) for the exact cases.
