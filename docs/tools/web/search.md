# `search` — searching the web

`search` runs a text query against the Exa search engine and returns the
results as compact markdown — titles, URLs, and highlight snippets:

```rust,ignore
pub async fn search(search: &WebSearch) -> Result<String, String>
```

```rust,ignore
use cosh_tools::web::WebSearch;

let results = cosh_tools::web::search(&WebSearch {
    num_results: 5,
    query: "rust async programming".into(),
})
.await?;
```

The wrapper method is `Web::search(query)` — it builds the `WebSearch` from
the wrapper's configured result count. The config struct:

```rust
pub struct WebSearch {
    pub num_results: u32,   // silently capped at 10
    pub query: String,
}
```

---

## Validation first

Before anything touches the network, the input is checked:

| Input | Error |
|---|---|
| Empty query | `"query required"` |
| `num_results: 0` | `"num_results must be >= 1"` |

A value above 10 is **silently capped** — `num_results: 25` requests 10.

---

## The two Exa paths

Search has no local engine — it always talks to Exa, via one of two
transports:

1. **REST API** — when `EXA_API_KEY` is set, `api.exa.ai/search` is called
   with the key, requesting `numResults` results with text highlights. If the
   request *succeeds*, its results are formatted and returned.
2. **Free MCP endpoint** — without a key (or when the REST call fails), a
   JSON-RPC `tools/call` request for Exa's `web_search_exa` tool is sent to
   `mcp.exa.ai/mcp`; the SSE response is decoded and cleaned with `strip_na`.

So a search works with no configuration at all (the free MCP endpoint), and
a `EXA_API_KEY` upgrades it to the paid REST API — with the REST path tried
first.

---

## Output format

The result is one block per result, separated by blank lines and `---`:

```text
Title: Rust (programming language) — Wikipedia
URL: https://en.wikipedia.org/wiki/Rust_(programming_language)
Highlights:
  - Rust is a multi-paradigm, general-purpose programming language…

---
```

- `Title:` — the page title (omitted if the engine returned none).
- `URL:` — always present; this is the link to fetch with `web_fetch`.
- `Highlights:` — up to a few snippet lines (omitted when empty).

This block format is produced by the REST path. The free MCP endpoint
returns Exa's own rendered results (which follow the same `Title:`/`URL:`
shape) cleaned of `Published: N/A` / `Author: N/A` lines by `strip_na`.

The whole output is a single `String`, sized for the model's context — not a
structured list. To read a promising result in full, fetch its `URL:` with
[`web_fetch`](fetch.md).

---

## Errors

| Failure | Error looks like |
|---|---|
| Empty query / zero results requested | validation errors above (no network) |
| REST non-success status | `"api 429: …"` |
| REST returned no results | `"no results"` (falls through to MCP) |
| MCP failure | `"mcp err: …"` |

---

## Example

```rust,ignore
use cosh_tools::web::{Web, WebSearch};

let web = Web::new().num_results(3);
match web.search("rust web frameworks").await {
    Ok(results) => println!("{results}"),
    Err(e) => println!("search failed: {e}"),
}

// The free functions validate the same way:
assert!(cosh_tools::web::search(&WebSearch::default()).await.is_err());
```
