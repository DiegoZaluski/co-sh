# `extract_action::extract` — the extractor

The core [`ExtractAction`] type plus the data types it produces.

## Types

```rust,ignore
pub struct ToolSchema {
    pub name: String,
    pub input_schema: serde_json::Value,   // JSON Schema subset (see below)
    // Canonical, hand-curated example of valid arguments (optional). When
    // present, schema-rejection hints show it instead of the derived
    // type-only skeleton, so the model can copy a complete, correct payload
    // (optional fields, conditional rules and all) on retry. The harness
    // feeds this from a tool description's `exampleArgs` field.
    pub example_args: Option<serde_json::Value>,
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
- **String-encoded field repair** (`repair_string_encoded`): before schema
  validation, any field the schema types as `array`/`object` whose value is
  a *string containing valid JSON of that shape* is parsed back into
  structured form — recursively, schema-directed (objects via `properties`,
  arrays via `items`). This recovers double-encoded emissions such as
  `"questions": "[{...}]"` without weakening validation: a string is only
  coerced when it parses AND matches the declared type. Applied identically
  on the enveloped path, native calls (which share `validate_tool_call`),
  and the bare-arguments fallback.
- **Bare-arguments fallback**: an object with *no* name field that matches
  **exactly one** registered tool's input schema (after the same repair) is
  treated as that tool's bare arguments. (The `id` and `thought_signature`
  fields are read from the same object when present.)

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

> **Why early exit matters:** Without this behavior, a model that outputs explanatory JSON (like `{"result": "I will use the tool"}`) would cause the streamer to buffer indefinitely, waiting for what it thinks is an incomplete tool call. By detecting keys that aren't part of any registered tool schema, we can immediately reject such explanatory JSON and continue processing, preventing the stream from hanging.

- **Fence tracking across tokens.** The code-block state persists across
  chunks, and a backtick run only toggles it if the run's *first* backtick was
  at a line start — so prose quoting ``` mid-sentence cannot swallow a
  following real tool call.

> **Why fence tracking matters:** Models often use backticks for inline code quoting in prose (e.g., "use the `fs.read` tool"). Without position-aware fence tracking, such inline backticks could incorrectly toggle the code-block state, causing the parser to misinterpret subsequent real tool calls as display text. By only recognizing fences that start at line beginnings, we distinguish between inline code quotes and actual markdown code blocks.

## Failure accounting

```rust,ignore
pub fn take_tool_failures(&mut self) -> usize   // drain the count
pub fn take_last_failed_raw(&mut self) -> String // drain the last failed JSON
```

## Validation rules

Before validation runs, `repair_string_encoded` coerces string-encoded
array/object fields back to structured form (see [Envelope
aliases](#envelope-aliases)); the repaired value is what `validate_against_schema`
sees — and what `ToolCallData.arguments` carries downstream.

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

---

## Summary

- `ExtractAction` is the core extractor type; build with `new()` and register tools with `with_tool(schema)` or mutable `add_tool`.
- Batch mode: `extract_batch` scans whole text, splits into alternating `Text`/`ToolCall` items, and validates candidates against schemas.
- Streaming mode: `extract_stream` processes tokens incrementally, returning `Text` (safe text), `ToolCall` (complete validated call), or `Pending` (still buffering).
- Envelope aliases accept flexible shapes: name/tool/function for tool name, arguments/input/args/parameters for arguments; bare-arguments fallback matches exactly one registered tool's schema.
- Streaming safety behaviors: early exit on unknown top-level keys (prevents explanatory JSON from hanging in Pending limbo) and fence tracking across tokens (backticks only toggle fence state if first backtick was at line start).
- Validation rules implement a JSON Schema subset: oneOf, type checking, required fields, properties with const/type/oneOf; unknown extra fields are allowed (permissive by design). Before validation, string-encoded array/object fields are repaired back to structured form (`repair_string_encoded`) on every call path.
- `ToolSchema.example_args` (optional): a curated example embedded in the tool description (`exampleArgs`); rejection hints prefer it over the derived skeleton, capped at the same 240-char payload limit.
- `find_json_objects` is a standalone helper that scans text for balanced `{...}` spans and returns byte offsets.
- Errors are represented (failure warning text, failure count) rather than thrown; only `jsonish::parse` returns `Result`.
