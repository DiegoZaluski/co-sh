# `shortlist` — embedding-based choice reduction

The `shortlist` module reduces high-cardinality choice questions by embedding the state and each option, then keeping only the top-`k` most semantically similar labels. This conserves the token budget when a choice has hundreds of options.

---

## The problem it solves

Choice questions share one `head_max_len` budget. A large label set (e.g., 200 categories) leaves only a few tokens per label after serialization, degrading model quality. Shortlisting selects the most relevant subset before the model sees the question, then runs a single prediction on the reduced criteria set.

```
Original: 200 labels → ~5 tokens/label → poor discrimination
Shortlisted: top 20 labels → ~50 tokens/label → better discrimination
```

---

## The three public functions

| Function | Purpose |
|---|---|
| [`shortlist_choice`](#shortlist_choice) | Return top-`k` choice labels for a state |
| [`predict_shortlist`](#predict_shortlist) | Shortlist each choice, then call predict once |
| [`cached_embed_fn`](#cached_embed_fn) | LRU cache wrapper for embedding functions |

---

## `EmbedFn` trait

The embedding seam. Implementations must convert text strings to float vectors.

```rust
pub trait EmbedFn: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>>;
}
```

- `Send + Sync` allows sharing the embedder between threads.
- Returns one vector per input string.
- Vectors must have consistent dimension (the same length for all outputs).
- `NaN`, `+Inf`, and `-Inf` values are replaced with `0.0` automatically.

Function pointers implement this trait automatically:

```rust,ignore
let embed_fn = |texts: &[String]| -> Result<Vec<Vec<f64>>> {
    // Your embedding logic here
    Ok(vectors)
};
```

---

## `shortlist_choice`

Returns the top-`k` choice labels for a given state, ranked by cosine similarity to the state embedding.

```rust
pub fn shortlist_choice(
    state: &Value,
    criteria: &Value,
    embed_fn: &dyn EmbedFn,
    k: usize,
    instructions: Option<&str>,
) -> Result<Vec<Value>>
```

### How it works

1. **Serialize state** — `state` is compacted to a string (via `serialize_state`). If `instructions` is provided, it prefixes the state.
2. **Render options** — each choice criterion (label → description) is rendered to a string via `render_options`.
3. **Embed everything** — `embed_fn` is called once with `[query, option1, option2, ...]`.
4. **Compute cosine similarity** — similarity between the query vector and each option vector.
5. **Rank and truncate** — options are sorted by similarity (descending, stable order, ties keep the earlier label), then truncated to `k`.

### Example

```rust,ignore
use cosh_onnx::shortlist::{shortlist_choice, EmbedFn};
use serde_json::json;

let state = json!({"message": "fix authentication bug"});
let criteria = json!({
    "backend": "Server-side fix",
    "frontend": "UI change",
    "docs": "Documentation update",
    "test": "Test suite change",
    "ci": "CI pipeline change"
});

let embed_fn = |texts: &[String]| -> Result<Vec<Vec<f64>>> {
    // Your embedding implementation
    Ok(vectors)
};

let top3 = shortlist_choice(&state, &criteria, &embed_fn, 3, None)?;
// Returns the 3 most similar labels, e.g. ["backend", "test", "ci"]
```

### When `k >= n`

If `k` is at least the number of labels, every label is returned in its original order and `embed_fn` is **not** called (no embedding work needed).

---

## `predict_shortlist`

Shortlists each choice question in a `questions` map, then calls `predict` once on the reduced set. Non-choice questions are forwarded unchanged.

```rust
pub fn predict_shortlist(
    runner: &impl PredictRunner,
    state: &Value,
    questions: &Map<String, Value>,
    embed_fn: &dyn EmbedFn,
    k: usize,
) -> Result<Value>
```

### How it works

1. **Iterate questions** — for each `(qid, qdef)` in the map:
   - If `type != "choice"`, forward unchanged.
   - If `type == "choice"` and label count `<= k`, forward unchanged (no shortlisting needed).
   - Otherwise, call `rank` to get the top-`k` labels and build a reduced criteria set.
2. **Call predict** — `runner.predict(state, &reduced)` runs the model on the shortlisted questions.
3. **Attach metadata** — the result includes a `shortlist` entry with `labels`, `scores`, `k`, `n`, and `passthrough` for each shortlisted question.

### The `shortlist` metadata

The returned value includes:

```json
{
  "answers": { /* model predictions */ },
  "shortlist": {
    "qid": {
      "labels": ["label1", "label2", ...],
      "scores": [0.92, 0.87, ...],
      "k": 20,
      "n": 200,
      "passthrough": false
    }
  }
}
```

- `labels` — the kept labels in rank order.
- `scores` — cosine similarity scores (or `null` when nothing was dropped).
- `k` — the shortlist size.
- `n` — the original label count.
- `passthrough` — `true` if the question was forwarded unchanged (label count `<= k`).

### Probabilities

When a choice is shortlisted, the model's probabilities are over the **kept labels only**, not the original set. This is intentional: the model sees only the reduced criteria.

### Example

```rust,ignore
use cosh_onnx::shortlist::{predict_shortlist, EmbedFn};
use serde_json::{json, Map};

let state = json!({"user_input": "login failure"});
let mut questions = Map::new();
questions.insert(
    "category".into(),
    json!({
        "type": "choice",
        "instructions": "Classify the issue.",
        "criteria": {
            "auth": "Authentication problem",
            "network": "Network issue",
            "ui": "UI bug",
            // ... 50 more categories
        }
    })
);

let runner = /* your PredictRunner implementation */;
let embed_fn = /* your EmbedFn implementation */;

let result = predict_shortlist(&runner, &state, &questions, &embed_fn, 10)?;
// result contains model predictions + shortlist metadata
```

---

## `cached_embed_fn`

Wraps an `EmbedFn` with an LRU cache to avoid re-embedding repeated inputs. Returns a `CachedEmbedFn` struct with methods for cache inspection and management.

```rust
pub fn cached_embed_fn(embed_fn: impl EmbedFn + 'static, maxsize: usize) -> Result<CachedEmbedFn>
```

### How it works

- Lookups are **exact string matches**.
- Missing texts are deduplicated and embedded in a single `embed_fn` call (cold cache costs the same as uncached).
- When the cache exceeds `maxsize`, the oldest entry is evicted (LRU policy).
- The cache is **not** populated if `embed_fn` raises or returns a bad shape.
- The wrapper is thread-safe: the lock covers only cache reads/writes, never the embedding call.

### Cache management

```rust,ignore
let cached = cached_embed_fn(my_embed_fn, 1000)?;

// Inspect cache statistics
let info = cached.cache_info();
// info = { "size": 150, "maxsize": 1000, "hits": 800, "misses": 250 }

// Clear the cache (e.g., after changing the model)
// Note: cache_clear() also resets the hit and miss counters
cached.cache_clear();
```

### When to clear

Clear the cache if the model or weights behind `embed_fn` change. Otherwise, stale embeddings may persist.

---

## Validation and errors

### `k` validation

`k` must be a positive integer. `k < 1` returns an error:

```
Error: Value("k must be a positive integer, got 0")
```

### Criteria validation

Choice criteria must be a **dict or list** with at least one option:

```rust,ignore
// Valid dict
json!({"a": "Option A", "b": "Option B"})

// Valid list
json!(["Option A", "Option B"])

// Invalid
json!("not a dict or list")  // Error
json!({})  // Error: empty criteria
```

Label duplication is rejected:

```
Error: Value("choice criteria label 'foo' is duplicated")
```

### Embedding shape validation

`embed_fn` must return a matrix of shape `(len(texts), dim)` where `dim >= 1`:

```
Error: Value("embed_fn must return an array of shape (3, dim), got (2, 5)")
```

### Render validation

If option rendering fails (e.g., two labels stringify to the same string), an error is raised:

```
Error: Value("could not render every choice option")
```

---

## Cosine similarity

The ranking metric is cosine similarity between the query vector and each option vector:

```
similarity = (query · option) / (||query|| * ||option||)
```

- Results are clamped to `[-1, 1]`.
- Zero-norm vectors (all zeros) score `0.0` and do not outrank earlier labels.
- Sorting is **stable**: ties keep the earlier label (matching `np.argsort(..., kind="mergesort")`).

---

## DEFAULT_SHORTLIST_K

```rust
pub const DEFAULT_SHORTLIST_K: usize = 20;
```

The default shortlist size used when no explicit `k` is provided.

---

## Summary

- `shortlist` reduces high-cardinality choice questions to the top-`k` most semantically similar labels using embeddings.
- `EmbedFn` is the embedding seam (text → vector). Function pointers implement it automatically.
- `shortlist_choice` returns top-`k` labels ranked by cosine similarity to the state.
- `predict_shortlist` shortlists each choice question, then calls `predict` once on the reduced set.
- `cached_embed_fn` wraps an `EmbedFn` with an LRU cache (thread-safe, dedupes missing texts).
- Validation covers `k` positivity, criteria shape/emptiness/duplication, embedding shape, and rendering.
- Cosine similarity is the ranking metric; zero-norm vectors score 0; ties keep the earlier label.
- Shortlisted choice probabilities are over the kept labels only, not the original set.
- `DEFAULT_SHORTLIST_K = 20` is the default shortlist size.

Next: [runtime — tokenization, batching, and ONNX sessions](runtime.md).
