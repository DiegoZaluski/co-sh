# `elements::catalogue` — component widgets and registry

The catalogue holds the built-in Solid-style components and the global
registry that resolves a tag name to a widget constructor. This is the
most-used part of `solid`.

## The widgets

### `SpanRenderable` — styled text span

A single run of styled text with optional attributes (bold/italic/underline)
and an optional link color.

```rust
SpanRenderable::new() -> Self            // id "span-{n}"
span.set_text(String)                    // the text to draw
span.set_attributes(u32)                 // bit 1=bold, 2=italic, 4=underline
span.set_link(Option<String>)            // if Some(url), text draws in link blue
```

Rendering draws the text left-to-right on the area's first row, clipped to
the width. Attribute bits map to ratatui modifiers; a set link colors the
text `Rgb(66, 133, 244)`.

Type aliases reuse `SpanRenderable` with different intended defaults:

```rust
pub type BoldSpanRenderable = SpanRenderable;
pub type ItalicSpanRenderable = SpanRenderable;
pub type UnderlineSpanRenderable = SpanRenderable;
```

### `LineBreakRenderable` — line break

```rust
LineBreakRenderable::new() -> Self       // id "br-{n}"
```

Draws a `\n` at the area's top-left cell. A structural spacer component.

### `LinkRenderable` — clickable-styled text

Wraps a `SpanRenderable` with a URL already set (link blue).

```rust
LinkRenderable::new(url: String) -> Self
link.set_text(String)                    // the visible label
```

Rendering delegates to the inner span; `id()`/`num()`/children also
delegate, so it composes like a normal renderable.

## The component registry

A process-global map from tag name to constructor:

```rust
pub fn register_component(name: &'static str, ctor: ComponentConstructor)
pub fn create_component(name: &str) -> Option<Box<dyn Renderable>>

type ComponentConstructor = Box<dyn Fn() -> Box<dyn Renderable> + Send + Sync>;
```

- **Built-in tags** are pre-registered: `"span"` → `SpanRenderable`,
  `"br"` → `LineBreakRenderable`.
- `register_component` adds or replaces a tag's constructor.
- `create_component` invokes the constructor for a tag, returning `None` for
  unregistered names.

The [`reconciler`](../reconciler.md) uses `create_component` to turn a tag
name into a node, and [`DynamicRenderable`](extras.md) uses it to wrap a
component by name.

```rust,ignore
// Register a custom component tag.
register_component("status", Box::new(|| -> Box<dyn Renderable> {
    Box::new(SpanRenderable::new())
}));

// Resolve it (returns a fresh instance each call).
let node = create_component("status").expect("registered above");
```

Next: [extras — DynamicRenderable](extras.md).
