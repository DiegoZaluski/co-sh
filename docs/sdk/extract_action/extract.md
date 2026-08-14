# `extract_action::extract` — the extractor

The core [`ExtractAction`] type plus the data types it produces.

## Types

```rust,ignore
pub struct ToolSchema {
    pub name: String,
    pub input_schema: serde_json::Value,   // JSON Schema subset (see below)
}

pub struct ToolCallData {
    pub id: String,              // API's native tool-call id, or "" for inline JSON
    pub name: String,
    pub arguments: serde_json::Value,
    pub thought_signature: String,  // Gemini 3.x only; "" otherwise
}

pub enum Item { Text(String), ToolCall(ToolCallData) }
pub struct BatchResult { pub items: Vec<Item> }

pub enum StreamAction {
    Text(String),
    ToolCall(ToolCallData),
    Pending,               // still buffering a potential call — emit nothing
}
```

## Building an extractor

```rust,ignore
ExtractAction::new()                  // no tools registered
    .with_tool(schema)                // builder: registers one tool
    .with_tool_failure_message(msg)   // builder: override the failure warning
```

There are mutable twins for working with a shared instance: `add_tool` /
`set_tool_failure_message`. Registration matters twice: the extractor looks
up candidates by name **and** pre-loads the schema's `properties` keys so the
streaming parser can bail out early on an object that is obviously not a tool
call.

## Batch mode: `extract_batch`

```rust,ignore
pub fn extract_batch(&mut self, text: &str) -> BatchResult
```

Scans the whole text with [`find_json_objects`](#find_json_objects), then
splits it into alternating `Text` / `ToolCall` items **in original order**.
For each candidate object:

1. **Inside a fenced code block?** (opening `` ``` `` at a line start) →
   kept as display `Text`. Fenced JSON is *shown*, never invoked.
2. Otherwise, try `parse_and_validate` — first as strict JSON, then through
   the [`jsonish`](jsonish.md) lenient parser, then against the schemas.
3. **Valid** → `Item::ToolCall`. **Invalid** → the failure warning as
   `Item::Text`, and the failure is counted (see [Failure
   handling](extract_action.md#failure-handling)).

No candidate objects at all → the entire input comes back as one `Text` item.

### Envelope aliases

A candidate is validated against this flexible shape:

```text
{ "name" | "tool" | "function": "<registered name>",
  "arguments" | "input" | "args" | "parameters": { ... } }
```

- **Arguments as a JSON string** (OpenAI-style): a string value is parsed as
  JSON first so validation sees the real object.
- **Bare-arguments fallback**: an object with *no* name field that matches
  **exactly one** registered tool's input schema is treated as that tool's
  bare arguments. (The `id` and `thought_signature` fields are read from the
  same object when present.)

## Streaming mode: `extract_stream`

```rust,ignore
pub fn extract_stream(&mut self, token: &str) -> StreamAction
```

Feed it one token/chunk at a time. It maintains all state internally and
returns `Text` for text that is safely past any potential tool call,
`ToolCall` when a complete validated call closes, and `Pending` while it is
still buffering a possible call. `extract_stream` is *non-reentrant* per
instance — one instance, one stream.

Streaming adds two safety behaviors on top of batch:

- **Early exit on unknown top-level keys.** The streamer tracks the first
  object's top-level keys; a `:` after a key that is neither a known envelope
  key nor a registered schema property aborts the buffered object as a
  failure instead of waiting for the closing brace. This is what keeps a
  model's *explanatory* JSON (`{"result": "I will use the tool"}`) from being
  held in a long `Pending` limbo.
- **Fence tracking across tokens.** The code-block state persists across
  chunks, and a backtick run only toggles it if the run's *first* backtick was
  at a line start — so prose quoting ``` mid-sentence cannot swallow a
  following real tool call.

## Failure accounting

```rust,ignore
pub fn take_tool_failures(&mut self) -> usize   // drain the count
pub fn take_last_failed_raw(&mut self) -> String // drain the last failed JSON
```

## Validation rules

`validate_against_schema` implements the JSON Schema subset the tools
actually use:

- `oneOf` — the value must match at least one sub-schema.
- `type` — an object value must be `"object"` (or absent); a non-object value
  only passes if the schema allows `"null"`. Property types are checked with
  `string` / `number` / `integer` / `boolean` / `object` / `array` / `null`.
- `required` — every listed field must be present.
- `properties` — present fields are checked: `const` values must match
  exactly, and field `type`/`oneOf` must hold. **Unknown extra fields are
  allowed** (no `additionalProperties: false` handling) — validation is
  permissive by design.

## `find_json_objects`

```rust,ignore
pub fn find_json_objects(text: &str) -> Vec<(usize, usize)>
```

A standalone helper: scans text for balanced `{ ... }` spans (string- and
escape-aware) and returns their `(start, end)` byte offsets. Useful on its
own for any "extract the JSON-ish objects from this blob" job.

## Errors

`extract_batch` / `extract_stream` never return `Result` — failures are
*represented* (failure warning text, failure count), not thrown. The only
`Result` in the module surface is `jsonish::parse`'s `Result<Value,
JsonishError>`.

---

Next: [jsonish — the tolerant parser](jsonish.md).
