# `fetch` — fetching a URL as clean markdown

`fetch` downloads a page and extracts its main content, returning it as
clean text ready for the model's context — navigation, scripts, and
boilerplate are stripped:

```rust,ignore
pub async fn fetch(fetch: &WebFetch) -> Result<String, String>
```

```rust,ignore
use cosh_tools::web::WebFetch;

let content = cosh_tools::web::fetch(&WebFetch {
    url: "https://example.com".into(),
})
.await?;
```

The wrapper method is `Web::fetch(WebFetch)`. The input struct has exactly
one field:

```rust
pub struct WebFetch {
    pub url: String,   // any HTTP(S) URL
}
```

---

## The extraction pipeline

`fetch` has two backends, tried in an order controlled by the compile-time
constant `EXA_FIRST` (currently `false`):

### 1. The local extractor (tried first)

1. **Download** — `reqwest` GETs the URL with the `Cosh/0.1` user agent.
2. **Extract** — the HTML goes through `rs_trafilatura`, a readability-style
   extractor, which returns the main content plus an `extraction_quality`
   score (0.0–1.0).
3. **Quality gate** — if `extraction_quality` is below `0.75`, the extractor
   is considered to have failed (it falls back to raw body text for
   non-HTML, e.g. JSON API responses), and the fetch falls through to Exa.

A detail worth knowing: `rs_trafilatura` writes debug noise to stderr, so
the extraction runs with stderr temporarily redirected to the OS null device
to keep the TUI clean.

### 2. The Exa backend (fallback)

- With `EXA_API_KEY` set: the **Exa Contents REST API**
  (`api.exa.ai/contents`) is called with the key and the URL; the first
  result's `text` is returned.
- Without a key: a JSON-RPC `tools/call` request for Exa's `web_fetch_exa`
  tool is sent to the free MCP endpoint (`mcp.exa.ai/mcp`), and the SSE
  response's first `data:` line is decoded.

---

## Output

The result is the extracted main content as plain text. There is no HTML,
no navigation, no comments — the model sees the article. (A `strip_na`
cleanup pass removes useless `Published: N/A` / `Author: N/A` /
`Highlights:` lines that some sources emit, keeping the text lean.)

---

## Errors

Every failure surfaces as a descriptive `Err(String)`:

| Failure | Error looks like |
|---|---|
| Network error downloading | `"fetch: error sending request …"` |
| Response read failure | `"read: …"` |
| Local extraction failure | `"extract: …"` |
| Extraction quality too low | `"low extraction quality"` (falls through to Exa) |
| Exa REST non-success status | `"contents 404: …"` |
| Exa MCP error | `"mcp err: …"` |

When both backends fail, the last error is returned. The error string is
deliberately informative — the model can read it and decide whether to
retry, switch URLs, or give up.

---

## Example

```rust,ignore
use cosh_tools::web::{Web, WebFetch};

let web = Web::new();
match web.fetch(WebFetch { url: "https://example.com".into() }).await {
    Ok(text) => println!("{}", &text[..text.len().min(200)]),
    Err(e) => println!("fetch failed: {e}"),
}
```
