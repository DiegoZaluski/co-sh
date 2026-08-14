# The `extract_action` module: tool-call extraction

`extract_action` turns **raw LLM output into structured tool calls**. A model
generates text; somewhere in that text there may be a JSON object describing a
tool to invoke (`{"name": "fs.read", "arguments": {...}}`). This module finds
those objects, **validates them against the registered tool schemas**, and
yields clean, typed call data — in one shot over a full response
([`extract_batch`](extract.md)) or incrementally as the stream arrives
([`extract_stream`](extract.md)).

```
LLM text ──▶ ExtractAction ──▶ Vec<Item>            (batch)
              with registered      ├─ Text(...)        text to show the user
              ToolSchemas          └─ ToolCall(ToolCallData)  validated call

LLM stream ─▶ extract_stream ──▶ StreamAction        (streaming)
              per token            ├─ Text(...)
                                   ├─ ToolCall(...)
                                   └─ Pending          keep buffering
```

## Why extraction, not direct parsing?

Models do not reliably emit well-formed JSON inline. They wrap it in
markdown fences, prefix it with prose, split it across stream chunks, quote
their own strings, and invent envelope shapes. `extract_action` is built for
that reality:

- **Tolerant JSON** — through the vendored [`jsonish`](jsonish.md) parser,
  malformed JSON gets repaired rather than rejected.
- **Envelope flexibility** — `name`/`tool`/`function` are accepted as the
  tool-name key; `arguments`/`input`/`args`/`parameters` as the arguments key.
- **Schema validation** — a candidate must match the registered tool's input
  schema (`required` fields, `type`/`const`/`oneOf` checks) before it is
  surfaced as a call.
- **Fence awareness** — JSON inside a *real* markdown code fence (opening
  backticks at a line start) is display text, never a tool call. A `` ``` ``
  merely *quoted* mid-line does not toggle fence state.

## The vocabulary

| Type | Meaning |
|---|---|
| [`ToolSchema`](extract.md) | A registered tool: `name` + JSON input schema |
| [`ToolCallData`](extract.md) | A validated call: `id`, `name`, `arguments`, `thought_signature` |
| [`Item`](extract.md) | One batch element: `Text` or `ToolCall` |
| [`StreamAction`](extract.md) | One stream element: `Text`, `ToolCall`, or `Pending` |
| [`Value`](jsonish.md) | The tolerant JSON value produced by the jsonish parser |

## Failure handling

A JSON object that **looks** like a tool call but fails validation (unknown
tool name, missing required field, wrong argument type) is **not** surfaced
as a call and **not** shown as raw JSON. In batch mode it becomes the
`tool_failure_message` (default `"\n\n> ⚠ Tool call failure\n\n"`); in
streaming mode the same warning is flushed as text. The failure is countable:
`take_tool_failures()` drains the count, and `take_last_failed_raw()` returns
the raw JSON of the most recent failure so you can feed it back to the model
as correction feedback.

## Where it comes from

The `jsonish` submodule is a direct port of BoundaryML's
[baml](https://github.com/BoundaryML/baml) jsonish parser (Apache 2.0);
`extract_action` is the thin tool-call layer built on top of it. The module
is self-contained: it exposes everything through the `extract_action` re-exports
(`ExtractAction`, `Item`, `StreamAction`, `ToolCallData`, `ToolSchema`,
`BatchResult`, `find_json_objects`) plus the `jsonish` re-exports (`parse`,
`ParseOptions`, `Value`, `CompletionState`, `Fixes`, `JsonishError`).

---

## Example

A complete runnable walkthrough lives at
[`examples/extract_action/extract.rs`](../../../crates/cosh-sdk/examples/extract_action/extract.rs):
it registers schemas, runs batch and streaming extractions over realistic
model output, shows validation failures, and drives the jsonish parser
directly.

Next: [extract — the extractor](extract.md), then [jsonish — the tolerant
parser](jsonish.md).
