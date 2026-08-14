# `connector::params` — messages, tools, formats

The request vocabulary. These types are what you hand to
[`Connector`](client.md) for tool use and structured output — and the message
builders are the ergonomic way to construct [`ChatMessage`] values.

## `ChatMessage` — the shared conversation format

```rust,ignore
pub struct ChatMessage {
    pub role: String,                            // "system" | "user" | "assistant" | "tool"
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallMsg>>,    // assistant-only
    pub tool_call_id: Option<String>,            // tool-result-only
    pub thinking_blocks: Option<Vec<ClaudeThinkingBlock>>,  // Claude-only transport detail
}
```

The format mirrors OpenAI's Chat Completion message shape, so the model
natively understands tool calls (`role: "assistant"` + `tool_calls`) and tool
results (`role: "tool"` + `tool_call_id`). Each family translates it onto its
own wire format internally.

### The builders

```rust,ignore
pub fn user_message(content: &str) -> ChatMessage              // role: "user"
pub fn system_message(content: &str) -> ChatMessage            // role: "system"
pub fn tool_result_message(tool_call_id: &str, content: &str) -> ChatMessage  // role: "tool"
pub fn assistant_tool_call_message(tool_calls: Vec<ToolCallMsg>) -> ChatMessage // role: "assistant"
```

Prefer these over constructing `ChatMessage` by hand — they set exactly the
fields each role needs (e.g. `tool_result_message` carries the `tool_call_id`
that pairs it with the assistant's call).

### Tool-call payloads

```rust,ignore
pub struct ToolCallMsg {
    pub id: String,
    #[serde(rename = "type")] pub kind: String,   // "function"
    pub function: ToolCallFunctionMsg,
    pub thought_signature: Option<String>,        // Gemini 3.x transport detail
}

pub struct ToolCallFunctionMsg {
    pub name: String,
    pub arguments: String,                        // JSON-encoded string
}
```

The `thought_signature` field is a **Gemini 3.x requirement**: when a
thinking model emits a native `functionCall`, the part travels with a sibling
`thoughtSignature` that MUST be replayed verbatim when the call is re-sent in
conversation history (the API rejects the replay with HTTP 400 otherwise).
The `skip_serializing_if` keeps it off every other provider's wire format.

## Tools: `ToolDefinition` + `ToolFunction`

```rust,ignore
let tool = ToolDefinition::new(
    ToolFunction::new("get_weather")
        .with_description("Get the current weather for a city")
        .with_parameters(serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }))
);
connector.with_tools(vec![tool]);
```

- `ToolFunction::new(name)` → `with_description(...)` → `with_parameters(...)`.
- `ToolDefinition::new(function)` wraps it with `type: "function"`.
- Hand the `Vec<ToolDefinition>` to [`Connector::with_tools`](client.md) or
  `set_tools`, and control selection with `with_tool_choice` (`"auto"`,
  `"none"`, or a specific tool name).

## `ResponseFormat` — structured output

```rust,ignore
pub fn ResponseFormat::json_object() -> Self
connector.with_response_format(ResponseFormat::json_object());
```

Currently the only variant is `json_object` (serializes as
`{"type": "json_object"}`) — request that the model emit valid JSON.

## `ClaudeThinkingBlock` — extended thinking replay

```rust,ignore
pub struct ClaudeThinkingBlock {
    pub thinking: String,
    pub signature: String,
}
```

Captured from a previous Claude response and replayed **verbatim** at the
start of the assistant message when the turn is re-sent — the Anthropic API
validates the signature cryptographically and rejects modified/missing blocks
with HTTP 400. Only the Claude caller sets this; `skip_serializing_if` keeps
it off other providers' wire formats.

---

Next: [output — responses and streams](output.md).
