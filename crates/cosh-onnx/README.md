# cosh-onnx

[![Crates.io](https://img.shields.io/crates/v/cosh-onnx.svg)](https://crates.io/crates/cosh-onnx)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DiegoZaluski/cosh/blob/main/LICENSE)

ONNX decision-engine crate for [cosh](https://github.com/DiegoZaluski/cosh) — a coding agent for the terminal. A Rust port of the laya "System 1" decision engine core.

`cosh-onnx` runs small ONNX decision models on CPU (`ort` + HuggingFace `tokenizers` — no torch, no CUDA) and answers structured questions (`choice` / `score` / `noul`) against a JSON state, with calibrated confidence numbers. In the cosh workspace it is gated behind the `onnx` feature so the main binary never links ONNX Runtime unless you opt in.

## Highlights

- **One facade, three checkpoints** — `load(ModelKind, &LoadOptions)` returns an `Arc<dyn DecisionModel>` (English, Multilingual — 100+ languages, TypedDecisions) or a custom/local checkpoint.
- **`Router`** — automatic multi-checkpoint routing with script detection and LRU residency; multiple consumers share one resident model.
- **Question presets** — router, triage, guard, moderation and email question builders, plus JSON-schema → questions mapping (`decide`).
- **Calibrated confidence** — `answer_confidence`, `confidence_from_probs`, `ece_score`, with Python-parity numerics (`pycompat`) for bit-stable results.
- **Prediction hooks** — opt-in lifecycle hooks with timeout and concurrency controls.
- **Supply-chain integrity** — cache-first HF hub downloads with opt-in revision pins and SHA-256 digest verification; a mismatch evicts the cache.
- **Fail-open design** — the decision model is an improver, never a single point of failure: on any failure the verdict falls back with confidence 0.0.
- **Extensible by contract** — `AgentLike` and `DecisionModel` are trait-seamed and contract-tested to be implementable outside the crate.

## Usage

```rust
use cosh_onnx::{load, LoadOptions, ModelKind};
use cosh_onnx::prelude::*;

// Load a checkpoint (downloaded from the hub on first use, then cached).
let model = load(ModelKind::Multilingual, &LoadOptions::default())?;

// Ask a structured question against a JSON state.
let prediction = model.decide(&state, &questions, None, None, None)?;
let answer = prediction.choice("qid")?;
let confidence = prediction.answer_confidence("qid")?;
```

Use `Router::new()` instead of `load` when you want automatic routing between checkpoints, or the `ModelKind::Custom { repo, subfolder }` variant for your own ONNX graphs (default graph name: `laya.onnx`).

## Requirements

- Rust 2024 edition (≥ 1.85)
- ONNX Runtime: shipped via `download-binaries` (prebuilt static link, no system lib needed) — CPU only
- Model files: laya checkpoints (~1.3 GB per graph) are downloaded from the HuggingFace hub on first use, or served from a local directory via `ModelKind::Custom`

## Integration notes

In the cosh app, this crate is enabled by the `onnx` feature (`cosh --features onnx`), which also builds the `cosh-decisiond` daemon — a decision daemon that owns the resident model once for all running cosh instances and serves decisions over IPC. Without the feature, dependents never link ONNX Runtime.

Standalone use is supported: the hub installer is repo-agnostic and `ModelKind::Custom` accepts any repo, subfolder or local directory.

## Related crates

- [`cosh-tools`](https://crates.io/crates/cosh-tools) — tool implementations
- [`cosh-sdk`](https://crates.io/crates/cosh-sdk) — agent harness SDK
- [`cosh-recall`](https://crates.io/crates/cosh-recall) — memory and context recall

## License

Apache-2.0. See [LICENSE](https://github.com/DiegoZaluski/cosh/blob/main/LICENSE).
