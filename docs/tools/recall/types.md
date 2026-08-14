# `recall` data types

The module has three types: the search input, the entry, and the output.
The input derives `Deserialize` (the harness deserializes its arguments);
the output types derive `Serialize` + `JsonSchema`.

---

## `RecallSearchInput` — the search parameters

```rust
pub struct RecallSearchInput {
    /// LanceDB database URI (local directory or cloud URI).
    pub db_uri: String,
    /// Name of the table inside the database to search.
    pub table_name: String,
    /// Dimension of the embedding vectors stored in the table.
    pub vector_dim: usize,
    /// The original query text (echoed back for agent context).
    pub query: String,
    /// Pre-computed embedding vector for the query.
    pub query_vector: Vec<f32>,
    /// Maximum number of results to return (default: 5).
    pub limit: Option<usize>,
}
```

Field-by-field:

- **`db_uri` / `table_name`** — where the data lives. `db_uri` is a local
  directory (LanceDB's on-disk format) or a cloud URI; `table_name` is the
  table inside it.
- **`vector_dim`** — the expected embedding dimension. It is checked against
  the table's actual schema before searching (a mismatch errors), and it
  doubles as metadata telling the caller which embedding model to use.
- **`query`** — the original natural-language text. The tool does not use it
  for matching (the vector does that); it echoes it back in the output so
  the agent can correlate the results with what it asked.
- **`query_vector`** — the pre-computed embedding of the query. This is the
  actual search key.
- **`limit`** — result cap; `None` means 5 (the default applied by the
  search, and the default the tool schema advertises).

Note that the input has **no `total` or pagination** — a fixed top-N by
design.

## `RecallEntry` — one hit

```rust
pub struct RecallEntry {
    pub id: String,       // unique identifier for the entry
    pub content: String,  // text content of the entry
}
```

The two fields the agent actually needs: what the entry is called and what
it says. There are no vectors or scores in the output — the caller asked for
similar entries, and gets the text.

## `RecallOutput` — the search result

```rust
pub struct RecallOutput {
    pub query: String,             // the original query text, echoed back
    pub results: Vec<RecallEntry>, // matching entries, most similar first
}
```

A thin wrapper: the echoed query plus the ordered hits. The serialized JSON
is exactly `{ "query": …, "results": [ { "id": …, "content": … }, … ] }`.

---

## JSON round-trip

`RecallSearchInput` deserializes from the harness's JSON, so the wire form
matches the struct:

```json
{
  "db_uri": "/tmp/my_db",
  "table_name": "docs",
  "vector_dim": 384,
  "query": "What is RAG?",
  "query_vector": [0.1, 0.2, 0.3],
  "limit": 5
}
```

`limit` may be omitted (it defaults to `None`, then to 5 inside the search).
