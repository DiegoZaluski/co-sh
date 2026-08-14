# `elements::extras` — `DynamicRenderable`

`DynamicRenderable` wraps another renderable that is resolved **at
construction time** from the [component catalogue](catalogue.md) by name —
the cosh-tui analogue of SolidJS's `<Dynamic>` component.

```rust
DynamicRenderable::try_new(component_name: &str) -> Option<Self>  // None if unregistered
DynamicRenderable::new(component_name: &str) -> Self              // panics if unregistered
```

| Constructor | Behavior |
|---|---|
| `try_new(name)` | Resolve `name` via `create_component`; returns `None` when the tag isn't registered. |
| `new(name)` | Same, but **panics** with `DynamicRenderable: unknown component \`{name}\`` on failure. |

## Behavior

- **No reactive prop spreading.** Unlike the TS original, props are set via
  builder methods on the wrapped component before the first render; there is
  no reactive prop-passing mechanism.
- `render_self` delegates to the wrapped inner renderable, so the dynamic
  component draws as itself.

```rust,ignore
// Resolve an existing catalogue tag.
let dynamic = DynamicRenderable::try_new("span").expect("span is built-in");
// With a custom-registered tag:
register_component("status", Box::new(|| -> Box<dyn Renderable> {
    Box::new(SpanRenderable::new())
}));
let status = DynamicRenderable::new("status");
```

Because the inner widget is resolved once at construction, this is a
convenience for name-based component selection, not a live indirection
mechanism.

Next: [hooks — event-callback registration](hooks.md).
