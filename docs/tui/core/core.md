# `core` — rendering primitives and widget implementations

`core` is the rendering engine of `cosh-tui`. It provides the
[`Renderable`](renderable.md) trait — the contract every visible element of a
terminal UI implements — together with a widget library
([`renderables`](renderables/renderables.md)), a layout engine
([`LayoutTree`](layout.md)), a screen driver ([`Renderer`](renderer.md)), and
the low-level building blocks they share (colors, borders, styled text,
unicode width, syntax styles — see [`lib/`](lib/primitives.md)).

Everything in this module renders into a **`ratatui::buffer::Buffer`** — the
cell grid of a terminal frame — so cosh-tui widgets can be drawn either
through the full [`Renderer`](renderer.md) loop or dropped straight into any
existing ratatui application.

```text
cosh-tui/src/core/
├── renderable.rs      — the Renderable trait + node helpers      → renderable.md
├── types.rs           — attributes, mouse events, selection      → types.md
├── layout.rs          — LayoutTree (taffy wrapper)               → layout.md
├── renderer.rs        — Renderer + RendererConfig                → renderer.md
├── syntax_style.rs    — SyntaxStyle registry                     → syntax_style.md
├── utils.rs           — text-attribute packing helpers           → utils.md
├── lib/               — colors, borders, text, unicode, palette  → lib/primitives.md
└── renderables/       — the widget library                       → renderables/renderables.md
```

---

## How a UI is structured

A UI is a **tree of renderables**. Every widget — a box, a paragraph, a
textarea, a scroll bar — implements `Renderable`, which gives it:

- an identity (`id()`, `num()`),
- lifecycle flags (`is_visible()`, `is_focusable()`, `is_destroyed()`),
- a parent link (`parent_num()`),
- a tree of children (`children()`, `add_child()`, `remove_child()`),
- and the actual drawing method, `render_self(buf, area)`.

Composing a UI is then just nesting: a `BoxRenderable` (a bordered container)
holds a `TextRenderable` as a child; a `ScrollBoxRenderable` wraps a long
document; a `SelectRenderable` lists options. The [`Renderer`](renderer.md)
walks the tree once per frame, computes layout for every node, and asks each
one to draw itself into the frame's buffer.

```
RootRenderable
└── BoxRenderable            (bordered panel, title "Output")
    └── ScrollBoxRenderable  (scrollable region)
        └── MarkdownRenderable  (formatted markdown content)
```

## The two render paths

1. **Direct** — call `widget.render_self(&mut buffer, area)` yourself, e.g.
   from inside a ratatui `Frame`. This is what the cosh application itself
   does in several places (`render_markdown` in `src/tui/util/markdown.rs`
   builds a `MarkdownRenderable` and renders it straight into the frame's
   buffer).
2. **Managed** — hand a [`RootRenderable`](renderable.md#rootrenderable) to a
   [`Renderer`](renderer.md), call `render_frame()`, and let the renderer own
   layout, resizing, and drawing.

The direct path is simpler and works anywhere ratatui works; the managed path
adds automatic layout via `taffy` and a fixed render loop.

---

## What to read next

- [Renderable — the trait and node model](renderable.md) — start here; every
  widget is a `Renderable`.
- [LayoutTree — automatic layout](layout.md) — how sizes and positions are
  computed.
- [Renderer — the frame loop](renderer.md) — driving a full screen.
- [lib/ — colors, borders, text, unicode](lib/primitives.md) — the shared
  building blocks.
- [renderables/ — the widget library](renderables/renderables.md) — ready-made
  widgets you can drop into a tree.
