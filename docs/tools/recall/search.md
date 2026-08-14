# `search` — the vector search pipeline

`search` turns a `RecallSearchInput` into a ranked list of matching entries.
It is a thin, well-defined pipeline over the `cosh-recall` `VecDb` wrapper:

```rust,ignore
pub async fn search(input: &RecallSearchInput) -> Result<RecallOutput, String>
```

The wrapper method is `Recall::search(input)`; both delegate to the same
code.

---

## The pipeline

1. **Connect (read-only).** `VecDb::connect_readonly(db_uri, table_name)`
   opens the table without ever creating or writing anything. A missing
   table is an error (`"failed to connect to vector db: …"`).
2. **Dimension check.** The input's `vector_dim` is compared against the
   dimension inferred from the table's schema:

   ```
   vector dimension mismatch: input has 5, but table schema has 3
   ```

   This is a hard error, not a truncation — a wrong dimension means the
   caller used the wrong embedding model, and the results would be
   meaningless. The input's `vector_dim` is metadata (it tells the caller
   which model to use); the table schema is the source of truth.
3. **ANN search.** An approximate-nearest-neighbour query runs with the
   pre-computed `query_vector`, capped at `limit` (defaulting to 5 when
   `None`). LanceDB returns the closest entries first.
4. **Map.** Each hit becomes `RecallEntry { id, content }`, and the output
   echoes the original `query` text back:

   ```rust
   RecallOutput { query: input.query.clone(), results }
   ```

---

## Deterministic error paths

| Situation | Error |
|---|---|
| Table missing / connection failure | `"failed to connect to vector db: …"` |
| `vector_dim` ≠ table schema dimension | `"vector dimension mismatch: input has X, but table schema has Y"` |
| Search failure (bad vector length, etc.) | `"vector search failed: …"` |

A `query_vector` whose length does not match the table dimension is handled
by the underlying `VecDb::get`, which returns an empty result set rather
than an error — the dimension check on `vector_dim` is the guard that
catches the mismatch up front.

---

## Example

```rust,ignore
use cosh_tools::recall::{Recall, RecallSearchInput};

let recall = Recall::new();
let output = recall.search(&RecallSearchInput {
    db_uri: "/tmp/my_db".into(),
    table_name: "docs".into(),
    vector_dim: 3,
    query: "rust async".into(),
    query_vector: vec![0.8, 0.1, 0.0],   // pre-computed embedding
    limit: Some(3),
})
.await?;

println!("query: {}", output.query);
for entry in &output.results {
    println!("  {}: {}", entry.id, entry.content);
}
```
