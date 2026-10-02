# The decision contract: questions in, answers out

The decision layer keeps model calls predictable. A caller describes a small
question schema, the model evaluates every question against one state, and
the result is a JSON object with answers, probabilities and usage metadata.

## Three question kinds

Every definition has `type`, `instructions` and `criteria`.

| Type | Criteria | Answer accessor |
|---|---|---|
| `choice` | JSON object mapping labels to descriptions | [`Prediction::choice`](https://docs.rs/cosh-onnx/latest/cosh_onnx/struct.Prediction.html#method.choice) |
| `score` | JSON array of ordered level descriptions | [`Prediction::score`](https://docs.rs/cosh-onnx/latest/cosh_onnx/struct.Prediction.html#method.score) |
| `noul` | Optional object with `false` and `true` descriptions | [`Prediction::noul`](https://docs.rs/cosh-onnx/latest/cosh_onnx/struct.Prediction.html#method.noul) |

`noul` means “not one of the other two”: it represents a statement whose
truth probability is the result. Its semantic order is always false, then
true, even when custom labels are supplied.

For example, these definitions ask three different questions:

```rust
use serde_json::json;

let questions = json!({
    "kind": {
        "type": "choice",
        "instructions": "Classify the request.",
        "criteria": {"bug": "a defect", "question": "a request for information"}
    },
    "urgency": {
        "type": "score",
        "instructions": "Score urgency from low to high.",
        "criteria": ["low", "medium", "high"]
    },
    "safe": {
        "type": "noul",
        "instructions": "Does the request stay within policy?",
        "criteria": {"false": "it violates policy", "true": "it stays within policy"}
    }
});
```

Validation happens before tokenization. A malformed question names its id in
the returned [`Error`](https://docs.rs/cosh-onnx/latest/cosh_onnx/enum.Error.html),
so a caller can fix the definition instead of diagnosing a low-level model
failure.

## Reading a prediction

`DecisionModel::decide` returns `serde_json::Value`. Convert it to
`Prediction` when you want typed read-only access:

```rust
use cosh_onnx::Prediction;
use serde_json::json;

let raw = json!({
    "answers": {
        "kind": {
            "choice": "bug",
            "confidence": 0.75,
            "answer_confidence": 0.90
        }
    },
    "usage": {"input_tokens": 42}
});
let prediction = Prediction::from(raw);

assert_eq!(prediction.choice("kind"), Some("bug"));
assert_eq!(prediction.answer_confidence("kind"), Some(0.90));
assert_eq!(prediction.input_tokens(), Some(42));
```

`confidence` describes how concentrated the whole answer distribution is.
`answer_confidence` describes the probability of the answer that was actually
reported. For gating, prefer the latter: it has the same meaning across
choice, score and noul questions.

## From JSON Schema to questions

When the desired output is already described by JSON Schema, the schema
helpers remove the manual mapping step:

```rust
use cosh_onnx::decision::schema::{answers_to_json, questions_from_json_schema};
use serde_json::json;

let schema = json!({
    "type": "object",
    "properties": {
        "priority": {"type": "integer", "minimum": 1, "maximum": 3},
        "escalate": {"type": "boolean"}
    }
});

let questions = questions_from_json_schema(&schema)?;
// After a model call, use `prediction.answers()` here.
let answers = serde_json::Map::new();
let values = answers_to_json(&answers, &schema)?;
# Ok::<(), cosh_onnx::Error>(())
```

The supported subset is intentionally small: enum/const values become
`choice`, bounded integers become `score`, and booleans become `noul`.
Free-form strings, arrays, nested objects and ambiguous unions are rejected.
That limit is what makes the result finite and explainable.

## Shortlisting large choices

A choice with hundreds of labels can spend most of its token budget describing
options. The [`shortlist`](https://docs.rs/cosh-onnx/latest/cosh_onnx/shortlist/index.html)
module lets a caller provide an `EmbedFn`, keep the top `k` labels by cosine
similarity, and then run one decision over the reduced criteria set.

Use `cached_embed_fn` when repeated states or labels are expected. The cache
is LRU, thread-safe, and reports `hits` and `misses`; it never caches a failed
embedding call or an invalid shape.

## Summary

- Questions are validated before model execution.
- `choice`, `score` and `noul` cover finite categorical, ordinal and boolean
  decisions.
- `Prediction` exposes answer values, confidence and usage without requiring
  callers to navigate raw JSON.
- JSON Schema support is deliberately bounded to finite option sets.
- Shortlisting reduces high-cardinality choices without changing the model
  contract.
