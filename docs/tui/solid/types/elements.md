# `solid::types` — props and constructor traits

Marker traits and placeholder types ported from the TypeScript original's
type-level machinery (conditional types over props). **Port status:
scaffolded** — these exist to carry the API shape; the concrete widget prop
types are commented out pending the widget-port work.

## `NonStyledProps`

```rust
pub trait NonStyledProps {}
```

Marker for property types that should be excluded from the style prop —
the Rust stand-in for the TS `NonStyledProps` union (which used
`on${string}` template-literal types).

## `ElementProps<TRenderable>`

```rust
pub struct ElementProps<TRenderable> {
    pub _phantom: std::marker::PhantomData<TRenderable>,
}
```

Solid-specific props shared by all components. In Rust it's a minimal marker
struct (no JSX event system), carrying only a `PhantomData` for the
renderable type.

## `RenderableConstructor<TOptions, TRenderable>`

```rust
pub trait RenderableConstructor<TOptions, TRenderable> {
    fn create<TCtx: RenderContext<TRenderable>>(ctx: &mut TCtx, options: TOptions) -> TRenderable;
}
```

The base trait for any renderable constructor — the analogue of the TS
`RenderableConstructor`. Implementors create an instance given a
[`RenderContext`](../../core/types.md#rendercontexttrenderable) and options.

## `GetNonStyledProperties`

```rust
pub trait GetNonStyledProperties {}
```

Marker for widget option types, standing in for the TS conditional types
that computed which properties to exclude from styling per widget. Rust
lacks conditional types, so this is a simplified marker.

## Placeholder prop types

The concrete per-widget prop aliases (`TextProps`, `SpanProps`, `LinkProps`,
`BoxProps`, `InputProps`, `TextareaProps`, `SelectProps`, `AsciiFontProps`,
`TabSelectProps`, `ScrollBoxProps`, `CodeProps`, `MarkdownProps`) are
commented out in the source, awaiting the widget-port work (items 18–32 in
the original porting list). Until they land, widget configuration is done
through the builder setters on each
[`core::renderables`](../../core/renderables/renderables.md) widget rather
than prop structs.

Back to [solid — module overview](../solid.md).
