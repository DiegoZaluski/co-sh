# Diagnostics Store

The `DiagnosticsEngine` aggregates diagnostics from all language servers in
a workspace and exposes them to tools via snapshots, settle-wait, and
model-ready rendering.

## Storage model

```text
HashMap<PathBuf, HashMap<server_name, Vec<Diagnostic>>>
```

Each publisher replaces its own entry wholesale (the protocol's own
replace-whole-document semantics). An empty publish clears that server's
entry; when no server has anything left for a file, the path disappears.

## Change tracking

A global `AtomicU64` version bumps on every **real** mutation — identical
re-publishes (tsserver re-emitting on file events) are detected by equality
and do NOT bump. A `watch` channel mirrors the version for cheap change
detection without polling the map itself.

## Settle-wait

`wait_for_settle(cap)` sleeps one full debounce window (300 ms), compares
the global version before vs after, and returns:

- `true` — no mutation during the window (quiet)
- `false` — cap elapsed while still churning

The cap may overshoot by up to one debounce window: the sleep always runs to
completion before the deadline is consulted.

## Hybrid push + pull

Push (`textDocument/publishDiagnostics`) is the primary source, handled by
the client's dispatcher. Pull (`textDocument/diagnostic`) is available on
`LanguageServer::pull_diagnostics()` for servers that dynamically register
it; results feed the same `ingest()` door as pushes. `Ok(None)` from a pull
(unchanged or partial answer) never clears pushed state.

## LLM formatting

`format_for_model(diags, filter, max_items)` renders diagnostics for model
consumption:

- Ordered by severity → line → column → message (deterministic across runs).
- Cross-server duplicates collapsed on (position, message).
- Severity filter: `ErrorsOnly`, `WarningAndUp`, or `All`. Omitted severity
  treated as ERROR per spec.
- Hard item cap with an explicit `… and N more` marker.

## Usage pattern

```rust,ignore
let engine = DiagnosticsEngine::new();
// Session layer pumps ManagedEvents into it:
//   while let Some(ev) = rx.recv().await { engine.ingest_event(&ev); }

// After editing a file:
engine.wait_for_settle(Duration::from_secs(5)).await;
let snapshot = engine.snapshot_for(&path);
let text = format_for_model(&snapshot, SeverityFilter::ErrorsOnly, 20);
if !text.is_empty() {
    // attach as system reminder to tool output
}
```
