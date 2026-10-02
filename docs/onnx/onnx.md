# The `cosh-onnx` crate: making a decision from structured state

`cosh-onnx` runs a small ONNX decision model over JSON state. It is useful
when an application needs a bounded answer — a choice, a score or a yes/no
probability — rather than an open-ended generation.

The crate is split into two layers:

1. the stable decision contract, which describes questions, answers and
   confidence; and
2. the Laya implementation, which loads an ONNX checkpoint and optionally
   routes each request to the right checkpoint.

The important idea is that callers depend on a model contract, not on ONNX
Runtime details. The [`DecisionModel`](https://docs.rs/cosh-onnx/latest/cosh_onnx/trait.DecisionModel.html)
trait is the boundary; `load` returns an `Arc<dyn DecisionModel>`.

## Modules

| Module | Covers |
|---|---|
| [Decision contract](decision.md) | Questions, answers, and confidence values |
| [Router](router.md) | Selection between English and multilingual models |
| [Runtime](runtime.md) | Tokenization, batching, and ONNX sessions |
| [Hooks](hooks.md) | Observation and transformation of predictions |
| [Hub and integrity](hub.md) | Checkpoint download, caching, and verification |
| [Language detection](language.md) | Evidence used by automatic routing |

## First prediction

This example shows the complete path. It is `ignore` because the first load
may download a large checkpoint from Hugging Face.

```rust,ignore
use cosh_onnx::{load, LoadOptions, ModelKind, Prediction};
use serde_json::json;

fn main() -> cosh_onnx::Result<()> {
    let model = load(ModelKind::English, &LoadOptions::default())?;
    let state = json!({"message": "the invoice was charged twice"});
    let questions = json!({
        "action": {
            "type": "choice",
            "instructions": "Choose the next action.",
            "criteria": {
                "refund": "request a refund",
                "explain": "explain the charge"
            }
        }
    });

    let questions = questions.as_object().expect("question map");
    let raw = model.decide(&state, questions, None, None, None)?;
    let prediction = Prediction::from(raw);

    println!("action = {:?}", prediction.choice("action"));
    println!("confidence = {:?}", prediction.answer_confidence("action"));
    Ok(())
}
```

There are three details worth noticing:

- state is any JSON value; structured values are serialized compactly before
  they enter the model sequence;
- a question has a stable id (`action`) and a typed definition; and
- `answer_confidence` is the probability of the reported answer, which is the
  useful threshold for application policy.

## What this crate does not do

- It does not generate free-form text. Questions must map to fixed options.
- It does not require CUDA or PyTorch. The published configuration uses CPU
  ONNX Runtime binaries and Hugging Face tokenizers.
- It does not make a model failure fatal to the whole harness by itself. The
  surrounding application can treat the decision engine as an improver and
  keep its own fallback path.
- It does not hide model downloads behind a progress UI. The Hub installer
  accepts callbacks, while the loader uses silent callbacks.

## Summary

- `cosh-onnx` turns JSON state into bounded decisions.
- `DecisionModel` is the backend-neutral seam.
- `load` chooses one checkpoint; `Router` chooses among residents.
- The decision vocabulary is `choice`, `score` and `noul`.
- Runtime, hooks, Hub installation and language detection are separate
  modules so each can be tested without requiring a real model.
