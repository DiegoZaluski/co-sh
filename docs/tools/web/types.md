# `web` data types

The web module's visible data types are the two tool-call inputs. The config
structs the free functions take (`WebFetch`, `WebSearch`) live in
[`fetch.rs`](fetch.md) / [`search.rs`](search.md) and are documented on
their pages; the types here are the wire types the harness deserializes.

---

## `WebFetchInput` — the `web_fetch` arguments

```rust
pub struct WebFetchInput {
    /// The full HTTP or HTTPS URL to fetch.
    pub url: String,
}
```

Exactly mirrors [`WebFetch`](fetch.md): one required field, `url`. It derives
`Deserialize` + `JsonSchema`, so the same struct both parses the tool call
arguments and describes the schema.

## `WebSearchInput` — the `web_search` arguments

```rust
pub struct WebSearchInput {
    /// The search query string.
    pub query: String,
}
```

Exactly mirrors the `query` field of [`WebSearch`](search.md). Note that the
result count is **not** part of the tool call — it comes from the `Web`
wrapper's configured `num_results`, so the harness never needs the model to
pass it.

---

## Why two pairs of types?

There is a deliberate split:

- **`WebFetchInput` / `WebSearchInput`** — the per-call JSON arguments for
  the MCP tool surface. One field each, `required`.
- **`WebFetch` / `WebSearch`** — the configuration structs consumed by the
  free functions and the wrapper. `WebSearch` additionally carries the
  result count, which is wrapper configuration rather than a per-call
  argument.

The harness parses the tool call into the input types, then builds the
config (filling `num_results` from the wrapper) and calls the free
functions. As a library user you interact with the config structs directly.

---

## Output

Both tools return `Result<String, String>` — a markdown string on success,
an explanatory error string on failure. There is no dedicated output struct:
the module's outputs are text by design, sized for the model's context (see
the [module page](web.md#errors-and-the-shape-of-results)).
