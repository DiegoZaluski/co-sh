# `embed` — the embedding and RAG API

The `embed` module provides a high-level RAG (Retrieval-Augmented Generation) workflow that combines text embedding with vector storage. It offers two embedding backends (local ONNX and cloud providers) and a simple ingest-and-search interface backed by LanceDB.

> **Feature-gated.** This module only compiles with the `lancedb` feature. At least one embedding provider feature (`fastembed` or `cloud`) is also required.

---

## The three public types

| Type | Purpose |
|---|---|
| [`Embedder`](#embedder) | Embedding provider (local ONNX or cloud API) |
| [`Rag`](#rag) | High-level RAG index (embed + store + search) |
| [`VecDb`](#vecdb) | Low-level LanceDB wrapper (vector operations only) |

[`Entry`](#entry) is the data structure for stored entries, and [`LocalEmbedModel`](#localembedmodel) enumerates supported local models.

---

## `Embedder`

`Embedder` is the embedding layer. It converts text into vector representations using either a local ONNX model or a cloud provider API.

```rust
pub enum Embedder {
    #[cfg(feature = "fastembed")]
    Local { model: Box<Mutex<fastembed::TextEmbedding>>, dim: usize },

    #[cfg(feature = "cloud")]
    Cloud { connector: aiface::connector::Connector, dim: usize },
}
```

### Local embedding (fastembed)

Uses ONNX Runtime to run models locally. Models are downloaded from Hugging Face on first use and cached.

```rust,ignore
use cosh_recall::embed::Embedder;
use fastembed::EmbeddingModel;

let embedder = Embedder::try_new_local(EmbeddingModel::BGESmallENV15)?;
let vectors = embedder.embed(&["hello world", "foo bar"]).await?;
assert_eq!(vectors.len(), 2);
assert_eq!(vectors[0].len(), 384); // BGE-small-en-v1.5 dimension
```

Requires the `fastembed` feature. The download progress bar is suppressed automatically to avoid breaking TUI rendering.

### Cloud embedding (aiface)

Delegates to a cloud provider (OpenAI, Gemini, etc.) via the `aiface` connector.

```rust,ignore
use cosh_recall::embed::Embedder;
use aiface::connector::Connector;

let connector = Connector::new("openai")?
    .with_model("text-embedding-3-small")
    .with_api_key("sk-...");
let embedder = Embedder::new_cloud(connector, 1536); // dimension must match model
let vectors = embedder.embed(&["hello world"]).await?;
```

Requires the `cloud` feature. The `dim` parameter must match the output dimension of the chosen model (e.g., 1536 for `text-embedding-3-small`, 768 for `text-embedding-3-large`).

### Methods

| Method | Returns | Purpose |
|---|---|---|
| `try_new_local(model)` | `Result<Self, RagError>` | Create local ONNX embedder (fastembed) |
| `new_cloud(connector, dim)` | `Self` | Create cloud embedder (aiface) |
| `embed(texts)` | `Result<Vec<Vec<f32>>, RagError>` | Embed a batch of texts |

`embed` processes the entire batch and returns one vector per input string. For the local variant, the ONNX session is wrapped in a mutex to enable concurrent use.

---

## `Rag`

`Rag` is the high-level RAG index. It combines an `Embedder` with a `VecDb` (LanceDB wrapper) to provide a simple ingest-and-search workflow.

```rust
pub struct Rag {
    embedder: Embedder,
    db: VecDb,
}
```

### Connecting to a database

```rust,ignore
use cosh_recall::embed::{Rag, Embedder};

let embedder = Embedder::try_new_local(EmbeddingModel::BGESmallENV15)?;
let rag = Rag::connect("data/lancedb", "my_docs", embedder).await?;
```

`connect` opens (or creates) a LanceDB table. If the table already exists, its schema is validated against the embedder's dimension; otherwise a new table is created with columns `id`, `content`, and `vector`.

### Ingesting documents

```rust,ignore
// Single document
let id = rag.ingest("doc-1", "RAG combines retrieval with generation.").await?;

// Batch ingestion
let docs = vec![
    ("doc-2".into(), "LanceDB is a vector database.".into()),
    ("doc-3".into(), "Embeddings capture semantic meaning.".into()),
];
let ids = rag.ingest_batch(&docs).await?;
```

`ingest` embeds the content and stores it. If the same content string already exists in the table, the existing id is returned and no duplicate is inserted (content-based deduplication).

`ingest_batch` processes multiple entries in a single call. The batch is processed sequentially; if any entry fails, the error is returned immediately and earlier entries remain in the database. The function returns the same number of ids as inputs — if content already exists, the existing id is returned for that entry instead of the one you passed.

### Searching

```rust,ignore
let results = rag.search("What is a vector database?", 5).await?;
for entry in &results {
    println!("[{}] {}", entry.id, entry.content);
}
```

`search` embeds the query and performs an approximate-nearest-neighbour (ANN) search in LanceDB, returning the most semantically similar entries sorted by relevance.

### Management

| Method | Returns | Purpose |
|---|---|---|
| `delete(id)` | `Result<(), RagError>` | Remove an entry by id |
| `entry_count()` | `Result<usize, RagError>` | Total number of entries |

---

## `VecDb`

`VecDb` is the low-level LanceDB wrapper. It provides vector-only operations (no embedding) and is used internally by `Rag`. Use it directly when you already have pre-computed vectors.

```rust
pub struct VecDb { /* LanceDB connection and table handle */ }
```

### Connecting

```rust,ignore
use cosh_recall::embed::VecDb;

let db = VecDb::connect("data/lancedb", "my_table", 384).await?;
```

The `dim` parameter specifies the vector dimension. When creating a new table, this dimension is used to define the schema. When opening an existing table, the dimension is inferred from the table's schema (use `connect_readonly` to open in read-only mode and infer dimension automatically).

### Reading and writing

```rust,ignore
// Insert a pre-computed vector
db.post("doc-1", "content text", vec![0.1, 0.2, /* ... */]).await?;

// Search with a pre-computed query vector
let results = db.get(&query_vector, 5).await?;
// Note: if the query vector dimension doesn't match the database dimension,
// an empty vector is returned (no error is raised).

// Delete by id
db.delete("doc-1").await?;

// Count entries
let count = db.entry_count().await?;
```

### Schema

LanceDB tables created by `VecDb` have three columns:

| Column | Type | Description |
|---|---|---|
| `id` | `Utf8` | Unique identifier (max 100 chars, no `'`, `;`, `"`) |
| `content` | `Utf8` | Raw text content |
| `vector` | `FixedSizeList(Float32, dim)` | Embedding vector |

### Validation

`validate_table_name` enforces table name constraints:

- Must start with a letter
- Must contain only letters, numbers, hyphens, and underscores
- Maximum 100 characters

```rust,ignore
use cosh_recall::embed::validate_table_name;

validate_table_name("my_docs")?;       // OK
validate_table_name("123_invalid")?;   // Error: must start with a letter
validate_table_name("invalid!")?;      // Error: invalid character
```

---

## `Entry`

```rust
pub struct Entry {
    pub id: String,
    pub content: String,
}
```

The data structure returned by search operations. Contains the stored identifier and the original text content.

---

## `LocalEmbedModel`

```rust
pub enum LocalEmbedModel {
    AllMiniLML6V2, AllMiniLML6V2Q, AllMiniLML12V2, AllMiniLML12V2Q,
    AllMpnetBaseV2,
    BGEBaseENV15, BGEBaseENV15Q, BGELargeENV15, BGELargeENV15Q,
    BGESmallENV15, BGESmallENV15Q, BGESmallZHV15, BGELargeZHV15,
    BGEM3,
    NomicEmbedTextV1, NomicEmbedTextV15, NomicEmbedTextV15Q,
    ParaphraseMLMiniLML12V2, ParaphraseMLMiniLML12V2Q,
    ParaphraseMLMpnetBaseV2,
    MultilingualE5Small, MultilingualE5Base, MultilingualE5Large,
    MxbaiEmbedLargeV1, MxbaiEmbedLargeV1Q,
    GTEBaseENV15, GTEBaseENV15Q, GTELargeENV15, GTELargeENV15Q,
    ModernBertEmbedLarge,
    JinaEmbeddingsV2BaseCode, JinaEmbeddingsV2BaseEN,
    EmbeddingGemma300M, EmbeddingGemma300MQ4, EmbeddingGemma300MQ,
    ClipVitB32,
    SnowflakeArcticEmbedXS, SnowflakeArcticEmbedXSQ,
    SnowflakeArcticEmbedS, SnowflakeArcticEmbedSQ,
    SnowflakeArcticEmbedM, SnowflakeArcticEmbedMQ,
    SnowflakeArcticEmbedMLong, SnowflakeArcticEmbedMLongQ,
    SnowflakeArcticEmbedL, SnowflakeArcticEmbedLQ,
}
```

Enumerates the local embedding models supported by fastembed. The `Q` suffix indicates a quantized variant (smaller model file, slightly lower quality). Models are downloaded from Hugging Face on first use.

Use `fastembed_model_from_str` to parse from a string:

```rust,ignore
use cosh_recall::embed::fastembed_model_from_str;

let fastembed_model = fastembed_model_from_str("BGESmallENV15")?;
```

---

## Error handling

### `RagError`

```rust
pub enum RagError {
    Embedding(String),
    Database(VecDbError),
    LockPoisoned,
}
```

Covers embedding failures, LanceDB errors, and internal mutex poisoning.

### `VecDbError`

```rust
pub enum VecDbError {
    InvalidId(String),
    DimensionMismatch { expected: usize, actual: usize },
    NotFound(String),
    Database(String),
}
```

Covers invalid identifiers, dimension mismatches between input and table schema, missing entries, and LanceDB errors.

---

## Feature flags

| Feature | Required for |
|---|---|
| `lancedb` | Vector storage (always needed for this module) |
| `fastembed` | Local ONNX embedding (`Embedder::Local`) |
| `cloud` | Remote embedding via aiface (`Embedder::Cloud`) |

At least one of `fastembed` or `cloud` must be enabled; otherwise the `Embedder` enum is empty and `Rag` cannot be constructed.

---

## Summary

- `Embedder` provides two backends: local ONNX (fastembed) and cloud (aiface). Both produce vectors from text.
- `Rag` is the high-level RAG index combining embedding + storage + search in a simple API.
- `VecDb` is the low-level LanceDB wrapper for vector-only operations (use when you have pre-computed vectors).
- `Entry` holds id + content for search results.
- `LocalEmbedModel` enumerates supported local models (BGE, E5, GTE, Nomic, etc.).
- Errors are reported via `RagError` (embedding/database/lock) and `VecDbError` (validation/mismatch/not found).
- Feature-gated: requires `lancedb` plus at least one of `fastembed` or `cloud`.

Next: [search — the vector search pipeline](search.md).
