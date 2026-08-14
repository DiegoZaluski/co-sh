# The `recall` module: semantic search over stored knowledge

`recall` is a **read-only vector search** tool backed by `LanceDB`: it looks
up entries in a vector database that are semantically similar to a query
vector and returns them with their content. It is the module that lets the
agent retrieve relevant context from a stored knowledge base.

| Tool | What it does |
|---|---|
| [`recall_search`](search.md) | Search a vector DB for entries similar to a query vector. |

> **Feature-gated.** The `recall` module only compiles with the `embed`
> feature (`cosh-tools/embed`, which enables the `lancedb` backend in
> `cosh-recall`). Without it the module does not exist — this is also why
> the harness hides it behind the same feature flag.

---

## A low-level building block

The most important design fact about `recall` is that it is **low-level by
design**:

- It accepts a **pre-computed embedding vector** (`query_vector`) directly —
  the module has no embedding-model dependency and never embeds text.
- It requires the full database coordinates: `db_uri`, `table_name`,
  `vector_dim`, plus the query text and vector.
- It is **read-only** — it only performs similarity searches, never writes.

The embedding step lives in the **harness**: the agent calls `recall_search`
with just `{ db_name, query, limit }`, and the harness resolves the database
from its registry, embeds the query with that database's configured
embedder, and feeds the complete parameters to `Recall::search`. As a
library user you are on the low-level side: you supply the vector.

```
LLM:  recall_search { db_name, query, limit }      ← what the model sees
        ↓  harness: resolve DB, embed query
crate: Recall::search(RecallSearchInput { db_uri, table_name, vector_dim,
                                          query, query_vector, limit })  ← what the crate does
```

---

## The `Recall` wrapper

```rust,ignore
use cosh_tools::recall::{Recall, RecallSearchInput};

let recall = Recall::new();
let output = recall.search(&RecallSearchInput {
    db_uri: "/tmp/my_db".into(),
    table_name: "docs".into(),
    vector_dim: 384,
    query: "What is RAG?".into(),
    query_vector: vec![0.1, 0.2, /* … */],
    limit: Some(5),
})
.await?;
```

`Recall::new()` pre-configures `description_search` — the ready-to-serve MCP
tool description for `recall_search`. The wrapper is **stateless**: every
call passes the full `RecallSearchInput`. `Recall::default()` is
`Recall::new()`.

### Customizing the tool description

The tool description is where the agent learns *which* knowledge base to
use, so the wrapper offers two ways to inject context:

```rust,ignore
// Consuming builder: returns a new Recall with the suffix appended.
let with_db = Recall::new().with_description("Active databases: docs (vectors)");

// In-place: replaces whatever suffix was set before (or reverts to the
// default when given an empty string).
let mut recall = Recall::new();
recall.rebuild_description("This knowledge base covers Rust and WebAssembly.");
```

Both append the suffix after the default description, separated by a blank
line — `db_name` values must match what the description lists, so the model
knows what it can search. An empty suffix keeps (or reverts to) the default.

### The tool schema

The LLM-facing schema requires only `db_name` and `query`; `limit` is
optional (default 5). The internal fields (`db_uri`, `table_name`,
`vector_dim`, `query_vector`) are deliberately **not** in the schema — the
model never sees them, the harness fills them in.

---

## The search contract

`Recall::search` (and the free `search` function) performs:

1. **Connect** — open the `LanceDB` table at `db_uri` read-only
   (`connect_readonly`). Missing table → error.
2. **Dimension check** — the input's `vector_dim` must equal the dimension
   of the table's schema; a mismatch is a hard error
   (`"vector dimension mismatch: input has 5, but table schema has 3"`).
3. **ANN search** — an approximate-nearest-neighbour query with the
   pre-computed `query_vector`, returning up to `limit` entries
   (`limit.unwrap_or(5)`).
4. **Map** — each result becomes a `RecallEntry { id, content }`; the output
   echoes the query text back for the agent's context.

Results come back sorted by similarity, most similar first.

---

## The harness and Ask mode

- The harness registers one or more `RecallDb` entries (name, LanceDB URI,
  table name, embedder config — local fastembed or a cloud provider) and
  injects their names into the tool description via `rebuild_description`.
- `recall_search` is read-only, so it is available in **Ask mode** as well
  as Build/Yolo — searching stored knowledge is an inspection operation.
- The embedding step requires one of the embedding features
  (`fastembed`/`cloud`) at build time; without them the harness returns a
  build-time hint error instead.

---

## Summary

- One read-only vector-search tool, backed by LanceDB.
- Low-level: you supply the embedding vector and full DB coordinates; the
  harness wraps it into a `{ db_name, query, limit }` interface for the LLM.
- `Recall` is stateless; customize the tool description with
  `with_description` / `rebuild_description`.
- Searches verify the vector dimension against the table schema before
  querying, and return `{ id, content }` entries ordered by similarity.

Next: the [data types](types.md), then the [search pipeline](search.md).
