# The `web` module: fetching and searching the web

`web` gives the agent two network tools: fetch a URL's content as clean,
readable markdown, and search the web for current information. It is the
module's only source of *external* knowledge — everything else works on the
local filesystem.

| Tool | What it does |
|---|---|
| [`web_fetch`](fetch.md) | Fetch a URL and extract its content as LLM-friendly markdown (boilerplate stripped). |
| [`web_search`](search.md) | Search the web for a query and return results with titles, snippets, and URLs. |

The design follows the module pattern: a builder-style wrapper —
[`Web`](#the-web-wrapper) — holds the configuration (result count), and the
operations are thin async methods on it. Both operations are **asynchronous**
and return `Result<String, String>` — the output is a ready-to-read markdown
string, not a structured payload.

---

## The `Web` wrapper

```rust,ignore
use cosh_tools::web::{Web, WebFetch};

let web = Web::new().num_results(5);   // 5 results for searches

let content = web.fetch(WebFetch { url: "https://example.com".into() }).await?;
let results = web.search("rust async programming").await?;
```

`Web::new()` creates a wrapper with a default of **10 search results**;
`num_results(n)` overrides it (the underlying API caps at 10). The public
fields `description_fetch` and `description_search` carry the ready-to-serve
MCP tool descriptions for `web_fetch` and `web_search`, exactly like the
other modules. `Web::default()` is `Web::new()`.

Each operation also exists as a free async function — `fetch(&WebFetch)` and
`search(&WebSearch)` — that the wrapper methods delegate to.

---

## Two backends behind both tools

Both tools run on the **Exa** search/contents API, with a **local
extraction** fallback for fetching. Which path is used depends on two knobs:

- **`EXA_API_KEY` environment variable.** When set, the module calls Exa's
  REST API (`api.exa.ai/…`) with the key. When unset, it falls back to Exa's
  free MCP endpoint (`mcp.exa.ai/mcp`) via a JSON-RPC `tools/call` request.
- **`EXA_FIRST` (fetch only).** A compile-time constant controlling whether
  the Exa backend is tried before the local extractor or after it. It is
  currently `false`, so **fetch tries the local extractor first**.

The practical consequence: `web_fetch` works entirely locally (reqwest +
`rs_trafilatura` extraction) when Exa is unreachable or unconfigured, while
`web_search` always needs Exa (REST or MCP) — there is no local search
engine. See [fetch](fetch.md) and [search](search.md) for each pipeline.

---

## Errors and the shape of results

Both tools return `Result<String, String>`, and the error strings are meant
to be read by the model: e.g. `"query required"`,
`"fetch: error sending request …"`, or `"contents 401: …"`. Failures are
ordinary `Err`s — there is no silent fallback beyond the documented
backends, and no retrying inside the module.

Outputs are deliberately compact markdown:

- **fetch** — the extracted article/content text.
- **search** — one block per result (`Title:`, `URL:`, `Highlights:` lines),
  separated by `---`.

Both are sized for the model's context, not for a browser.

---

## The harness and Ask mode

`web_fetch` and `web_search` are **read-only**, so both are exposed in Ask
mode alongside Build/Yolo. They are not gated by the permission system in
Build mode (no paths to approve) — only the tools that execute or write
things require approval.

---

## Summary

- Two async tools on one wrapper: `web.fetch(url)` and `web.search(query)`.
- Search result count defaults to 10 (API cap); set it with `num_results`.
- Fetch tries a local extractor first, then Exa; search uses Exa (REST with
  `EXA_API_KEY`, free MCP endpoint otherwise).
- Everything returns a markdown string or an explanatory error.

Next: the [data types](types.md), then the [fetch](fetch.md) and
[search](search.md) pipelines.
