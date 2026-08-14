# `time_to_first_draw` — TTFD marker

A placeholder renderable intended to mark (and later measure) the
**time-to-first-draw** of the UI.

```rust
TimeToFirstDrawRenderable::new() -> Self
```

The struct carries no state and renders nothing — it is pure API scaffolding
for a future TTFD instrumentation pass.

```rust,ignore
// Place it in a tree where you want to measure first draw.
let marker = TimeToFirstDrawRenderable::new();
```

Next: [types — marker traits](types/elements.md).
