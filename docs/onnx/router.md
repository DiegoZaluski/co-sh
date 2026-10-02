# The `Router`: choosing a resident checkpoint

Loading one checkpoint is enough when the input language is known. A router
is useful when the same process receives mixed traffic. It detects the input,
selects a checkpoint, keeps a bounded set resident and records the decision in
the result.

## Start with the default

`Router::new()` uses the bundled model table, English as the default for
undecided Latin text, and a maximum of two resident checkpoints. Models load
lazily on first use.

```rust,ignore
use cosh_onnx::prelude::*;
use serde_json::json;

let router = Router::new()?;
let state = json!({"message": "Quero cancelar meu pedido"});
let questions = email_questions(None);

let result = router.predict(&state, &questions, &PredictOptions::default())?;
let prediction = Prediction::from(result);
println!("model = {:?}", prediction.routing_model());
```

The prediction contains a `routing` object with the selected model, repository,
reason, detection data and optional workflow. This makes automatic behavior
inspectable instead of magical.

## Routing precedence

The router resolves an individual request in this order:

1. explicit `model`;
2. explicit `task` (`typed_decisions` is accepted as an alias);
3. detected workflow, when `auto_task_detection` is enabled;
4. explicit `lang`;
5. a supplied `lang_guess`;
6. script and best-effort Latin language detection; and
7. the configured `default`.

Use `PredictOptions` when the routing decision is part of the call:

```rust,ignore
let mut opts = PredictOptions::default();
opts.model = Some("multilingual");
opts.max_len = Some(256);
let result = router.predict(&state, &questions, &opts)?;
```

Use `route` when an application needs to inspect or override the choice before
it performs inference. `route` does not load a model or run a forward pass.

## Residency and LRU behavior

`max_loaded` is a memory policy, not a model-quality setting. A cap of two is
the useful default for the English/multilingual hot path. When the cap is
reached, the least-recently-used checkpoint is evicted. The lifecycle methods
are:

```rust,ignore
router.preload(Some(&["english", "multilingual"]))?;
println!("loaded = {:?}", router.loaded());
println!("revisions = {:?}", router.loaded_revisions());
router.unload(Some("english"))?;
```

`attach` is the extension seam for a caller that already owns an
`AgentLike` implementation. This is also how tests exercise routing without
downloading a checkpoint.

## Batches preserve order

`predict_batch` groups compatible requests so several states can share one
question schema and one resident agent. It returns results in input order,
even when routing creates multiple internal groups. `predict_many` is the
convenience form for requests with different schemas.

## Summary

- `Router::new` is lazy and keeps at most two checkpoints by default.
- Explicit model/task/language hints take precedence over detection.
- `route` observes the decision without inference; `predict` performs both.
- `routing` metadata makes every automatic choice explainable.
- LRU residency and batch grouping are performance policies around the same
  `AgentLike` contract.
