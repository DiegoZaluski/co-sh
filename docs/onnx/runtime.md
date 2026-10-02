# The runtime: keeping ONNX details behind small seams

The runtime is deliberately boring. It turns text into token ids, pads a
batch, supplies the five tensors the graph expects, and turns the two output
matrices back into rows. The decision layer above it does not need to know
which ONNX library is in use.

## The three seams

| Module | Responsibility | Test seam |
|---|---|---|
| `runtime::tokenizer` | Hugging Face tokenizer and special-token ids | `Tokenizer` |
| `runtime::batch` | padding and row-major input buffers | `collate_items` |
| `runtime::session` | ONNX inputs/outputs and forward-pass locking | `SessionRunner` |

`SessionRunner` is the key boundary:

```rust
use cosh_onnx::runtime::session::{SessionOutput, SessionRunner};
use cosh_onnx::runtime::batch::CollatedBatch;

struct FixedSession;

impl SessionRunner for FixedSession {
    fn run(&self, _batch: &CollatedBatch) -> cosh_onnx::Result<Vec<SessionOutput>> {
        Ok(vec![SessionOutput {
            logits: vec![1.0, 0.0],
            act_logits: vec![0.5, -0.5],
        }])
    }
}
```

Production code uses `OrtSession`. Tests use a fixed runner, so confidence,
question validation, hooks and routing can be checked without a graph file.

## What the graph receives

The real session builds these inputs from `CollatedBatch`:

- `input_ids`: padded token ids;
- `attention_mask`: which sequence positions are real;
- `marker_pos`: the positions of question markers;
- `marker_mask`: which marker positions are present; and
- `qtype`: the numeric code for `choice`, `score` or `noul`.

It reads `logits` and `act_logits` as row-major `f32` matrices. A missing
output or a disagreement in row counts is a runtime error with the output
name, rather than a later indexing panic.

## Concurrency has a narrow lock

Tokenization, hooks and answer decoding can run concurrently. The ONNX
forward pass is the exception: `ort::Session::run` requires mutable access, so
`OrtSession` protects only that call with an inner mutex. Cloning a model
handle shares the same resident session; it does not create another copy of
the weights.

## Tokenizer configuration

`HfTokenizer::from_dir` reads `tokenizer.json` and resolves the `[CLS]`,
`[SEP]`, `[MASK]` and `[PAD]` ids. When a checkpoint uses a newer
`tokenizer_config.json` shape, `fix_tokenizer_config` applies an atomic patch
to the snapshot. Atomic replacement matters because Hub snapshots can contain
links into a shared blob store.

## CPU-only by construction

The crate enables the standard, ndarray and Rustls features of `ort`, plus
`download-binaries`; it does not enable a CUDA execution provider. This keeps
the dependency graph predictable and makes the same CPU behavior available to
the standalone crate and to the `cosh` `onnx` feature.

## Summary

- Tokenization, collation and inference are separate modules.
- `SessionRunner` makes model-free tests possible.
- `OrtSession` locks only the mutable forward pass.
- The graph contract is five inputs and two output matrices.
- Tokenizer fixes are atomic because Hub cache entries can be shared blobs.
