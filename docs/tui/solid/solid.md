# `solid` — the reactive rendering layer

`solid` is a port of the *OpenTUI* (formerly "solid-tui") SolidJS-style
reactive rendering layer: components, a component catalogue, slots,
reconciliation against a renderable tree, and renderer abstractions. It sits
on top of [`core`](../core/core.md) — every "DOM node" here is a
[`Renderable`](../core/renderable.md).

```text
cosh-tui/src/solid/
├── elements/            — component hooks, widgets, and the catalogue
│   ├── catalogue.rs     — Span/BR/Link widgets + component registry   → elements/catalogue.md
│   ├── extras.rs        — DynamicRenderable                          → elements/extras.md
│   ├── hooks.rs         — event-callback registrations               → elements/hooks.md
│   └── slot.rs          — slot placeholder system                    → elements/slot.md
├── plugins/slot.rs      — SlotRegistry for plugin-provided UI        → plugins/slot.md
├── renderer/            — Renderer / RendererOptions traits          → renderer/renderer.md
├── reconciler.rs        — DOM-node insert/remove/property ops        → reconciler.md
├── scrollback.rs        — scrollback snapshot writer                 → scrollback.md
├── time_to_first_draw.rs— TTFD marker renderable                     → time_to_first_draw.md
├── types/               — marker traits (props, constructors)        → types/elements.md
└── utils/               — id_counter, debug logging                  → utils/utils.md
```

## Port status — read this first

`solid` is an **in-progress port** of the TypeScript original, and the
modules are at very different levels of completeness:

- **Working and useful today:** the [catalogue](elements/catalogue.md)
  (`create_component`, `register_component`, `SpanRenderable`,
  `LineBreakRenderable`, `LinkRenderable`), [`DynamicRenderable`](elements/extras.md),
  [`SlotRenderable`](elements/slot.md) / `TextSlotRenderable`, the
  [`SlotRegistry`](plugins/slot.md), the [reconciler](reconciler.md) DOM ops,
  and the [`utils`](utils/utils.md) helpers (`get_next_id`, debug logging).
- **Scaffolded / placeholder:** the trait-based
  [`Renderer`](renderer/renderer.md), the [hooks](elements/hooks.md)
  (`on_keyboard`, `on_paste`, `on_focus`, … are empty stubs),
  [scrollback](scrollback.md), and `TimeToFirstDrawRenderable`. These exist
  as API surface with TODO comments and no reactive engine behind them yet.

The crate's public entry points (re-exported from `lib.rs`) are the catalogue
widgets, `DynamicRenderable`, and the slot types — those are what applications
actually use today.

## How the pieces fit

The SolidJS mental model maps onto cosh-tui like this:

| SolidJS concept | cosh-tui equivalent |
|---|---|
| Component tree | `Renderable` tree (see [`core::renderable`](../core/renderable.md)) |
| `createComponent` | [`create_component`](elements/catalogue.md) |
| `<Dynamic>` | [`DynamicRenderable`](elements/extras.md) |
| Slots / portals | [`SlotRenderable`](elements/slot.md) |
| Plugin slot registry | [`SlotRegistry`](plugins/slot.md) |
| DOM ops (`insert`, `remove`, `setProperty`) | [`reconciler`](reconciler.md) functions |
| Renderer interface | [`Renderer` / `RendererOptions`](renderer/renderer.md) traits |
| Reactive signals/effects | not yet ported (hooks are stubs) |

Next: [elements — the component widgets and catalogue](elements/catalogue.md).
