# Checkpoints: cache first, verify before loading

ONNX weights are large and model artifacts are executable inputs. The Hub
module therefore treats installation as two separate jobs:

1. obtain a complete checkpoint in the local `hf-hub` cache; and
2. prove the artifacts match the caller's integrity policy before the runtime
   sees them.

## Loading named or custom checkpoints

`ModelKind` names the published English, multilingual and typed-decision
checkpoints. `ModelKind::Custom` accepts a Hub repository, an optional
subfolder, or a local checkpoint directory. `LoadOptions` can select the graph,
pin a revision and provide an artifact-to-SHA-256 map.

```rust,ignore
use cosh_onnx::{load, LoadOptions, ModelKind};

let options = LoadOptions {
    revision: Some("reviewed-commit-sha".into()),
    expected_sha256: Some([
        ("laya.onnx".into(), "<64-hex-digest>".into()),
    ].into_iter().collect()),
    ..Default::default()
};
let model = load(ModelKind::English, &options)?;
# let _ = model;
```

Revisions are opt-in. Without one, the Hub default is preserved so existing
offline caches remain usable. When an explicit digest map is present, the
graph must be covered by it; a missing or mismatched digest refuses the load.

## Installing explicitly

Embedders that need progress or want to install before loading can use
`hub::install`:

```rust,ignore
use cosh_onnx::hub::{InstallRequest, ProgressCallbacks, install};

let installed = install(InstallRequest {
    repo: "owner/model",
    subfolder: None,
    revision: Some("commit-sha"),
    graphs: &["laya.int8.onnx", "laya.onnx"],
    expected_sha256: None,
    progress: ProgressCallbacks::silent(),
})?;
println!("{} at {}", installed.graph, installed.revision);
```

The installer is cache-first. A complete cache hit does not touch the network
or fire progress callbacks. On a miss it downloads the config, tokenizer and
encoder artifacts, chooses the first graph candidate the repository contains,
and verifies the complete set before reporting success.

If no caller digest map is supplied, a published `sha256sums.txt` is used.
Digest failure evicts the revision's cache reference, so the same bad snapshot
cannot pass through the next cache probe.

## Safe digest paths

Digest maps are relative to the checkpoint and reject absolute paths and paths
that escape with `..`. The special `onnx` and `onnx_path` keys refer to the
graph selected for the install. These rules keep verification scoped to the
snapshot that the loader will actually consume.

## Local directories

Local checkpoints load directly. Passing a local directory to `install` is an
error because installation is the Hub-cache operation; this separation keeps
local development from unexpectedly copying files or contacting the network.

## Summary

- Installation is cache-first and integrity-checked.
- Revision pins and SHA-256 maps are opt-in through `LoadOptions`.
- The installer exposes progress callbacks but never owns a terminal UI.
- Failed verification evicts the cache reference.
- Local checkpoints bypass installation and go straight to the loader.
