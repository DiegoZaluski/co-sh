# `extract_action::jsonish` — the tolerant JSON parser

`jsonish` parses JSON that is **not quite JSON**. It is a direct port of the
[jsonish parser from BoundaryML/baml](https://github.com/BoundaryML/baml)
(Apache 2.0), and it exists so LLM output — which routinely arrives with
trailing commas, unquoted keys, markdown fences, truncated tails, and stray
prose — can still be turned into a structured value.

Its public surface is small:

```rust,ignore
pub fn parse(str: &str, options: ParseOptions, is_done: bool) -> Result<Value, JsonishError>

pub struct ParseOptions { /* all four flags default to true, depth starts at 0 */ }

pub enum Value {
    String(String, CompletionState),
    Number(serde_json::Number, CompletionState),
    Boolean(bool),
    Null,
    Object(Vec<(String, Value)>, CompletionState),
    Array(Vec<Value>, CompletionState),
    Markdown(String, Box<Value>, CompletionState),
    FixedJson(Box<Value>, Vec<Fixes>),
    AnyOf(Vec<Value>, String),
}

pub enum CompletionState { Complete, Incomplete }
pub enum Fixes { GreppedForJSON, InferredArray }
pub struct JsonishError(pub String);
```

## `parse` — one input, every trick

`parse` tries strategies **in order** and returns the first that succeeds:

> **Why multiple strategies:** LLM output is notoriously messy. Models might wrap JSON in markdown fences, split it across stream chunks, forget trailing commas, use unquoted keys, or mix JSON with explanatory prose. Rather than failing on the first sign of malformed input, we try progressively more lenient parsing strategies. This cascading approach maximizes the chance of extracting useful structured data from imperfect model output while still preferring clean JSON when available.

1. **Strict JSON** (`serde_json`). Top-level numbers are marked `Incomplete`
   (a bare number may be a truncated prefix); strings, objects, and arrays
   are necessarily complete.
2. **Markdown-aware parsing** — a fenced code block becomes
   `Value::Markdown(language, inner, state)`; a block with multiple JSON
   objects becomes an `AnyOf` of the individual objects, the whole list as an
   `Array`, and the raw strings.
3. **Multiple-object extraction** — scans for every balanced `{...}` in the
   text; one object → `FixedJson(value, [GreppedForJSON])`, several → an
   `AnyOf` with the array form included.
4. **Fixing parser** — repairs malformed JSON (trailing commas, missing
   quotes/brackets) and reports what it fixed via `Fixes`.

A plain string with no JSON anywhere falls through to the fixing parser,
which may return it as a `String` — or fail with `JsonishError`.

## `Value` — the result shape

- **`AnyOf(items, original)`** is the *uncertainty wrapper*: several plausible
  readings of the same input. Callers pick — `extract` converts each to
  `serde_json::Value` and validates, taking the first that fits.
- **`Markdown(tag, inner, _)`** — the tag is the fence language
  (`json`, `rust`, …); `inner` is the parsed block body.
- **`FixedJson(inner, fixes)`** — a value that required repairs; `fixes`
  records which (`GreppedForJSON` = object(s) pulled out of surrounding
  prose; `InferredArray` = a list was assumed).
- **`CompletionState`** — every container carries it: `Incomplete` means the
  input looked cut off (useful for streaming callers). `complete_deeply()`
  forces a whole tree to `Complete`.
- `Value` implements `Display` (renders compactly) and `serde::Deserialize`
  (via an internal visitor that builds `Value` directly, avoiding a
  `serde_json::Value` round-trip).

## `ParseOptions`

```rust,ignore
pub struct ParseOptions {
    all_finding_all_json_objects: bool,  // try the multi-object strategy
    allow_markdown_json: bool,           // try markdown-fence parsing
    allow_fixes: bool,                   // try the repairing parser
    allow_as_string: bool,               // tolerate a plain-string result
    depth: usize,                        // recursion guard (internal)
}
```

All flags default to `true`. The recursion depth limit is 100; exceeding it
returns `JsonishError("Depth limit reached. Likely a circular reference.")`.

## Using it directly

```rust,ignore
use cosh_sdk::extract_action::{jsonish, ParseOptions};

let v = jsonish::parse(
    r#"Here is the tool call:
    ```json
    {"name": "fs.read", "arguments": {"path": "/tmp"}}
    ```"#,
    ParseOptions::default(),
    true,   // is_done: is this the final input or a stream prefix?
).unwrap();
println!("{v}");  // Markdown("json", Object([...]), ...)
```

Inside `extract_action`, `parse_and_validate` calls
`jsonish::parse(candidate, ParseOptions::default(), true)` and converts the
resulting `Value` to `serde_json::Value` before schema validation — so the
tolerance you get through `ExtractAction` is exactly what `jsonish` provides.

---

Back to the [module overview](extract_action.md).

---

## Summary

- `jsonish` parses JSON that is "not quite JSON" — a direct port of BoundaryML/baml's jsonish parser (Apache 2.0) for handling LLM output with trailing commas, unquoted keys, markdown fences, truncated tails, and stray prose.
- `parse(str, options, is_done)` tries strategies in order: strict JSON → markdown-aware parsing → multiple-object extraction → fixing parser.
- `Value` enum includes `AnyOf` (uncertainty wrapper with multiple plausible readings), `Markdown` (fenced block with language tag), `FixedJson` (repaired value with fixes recorded), and standard types with `CompletionState` (Complete/Incomplete).
- `ParseOptions` controls four flags (all default to true): `all_finding_all_json_objects`, `allow_markdown_json`, `allow_fixes`, `allow_as_string`, plus recursion depth limit (100).
- Inside `extract_action`, `parse_and_validate` calls `jsonish::parse` with default options and converts the result to `serde_json::Value` before schema validation.
- The parser is designed for streaming callers; `is_done` indicates whether the input is final or a stream prefix, and `CompletionState` tracks whether containers look cut off.
