# `elements::hooks` — event-callback registration

Hooks register callbacks for UI events (resize, keyboard, paste, focus,
selection). **Port status: mostly scaffolded.** Only `on_resize` actually
invokes its callback; the rest are empty stubs awaiting renderer event
streams (each carries a `TODO: wire to renderer …` comment).

## Implemented

```rust
pub fn on_resize<B: Backend + 'static>(
    renderer: &Renderer<B>,
    mut callback: impl FnMut(u16, u16) + 'static,
)
```

Immediately invokes `callback(width, height)` once with the renderer's
configured size (from `renderer.config()`).

## Stubs (empty bodies)

| Hook | Signature | Intended behavior |
|---|---|---|
| `on_keyboard` | `(&impl HasKeyInput, impl Fn(&str) + 'static)` | Key events. |
| `on_paste` | `(&impl HasKeyInput, impl Fn(&str) + 'static)` | Paste events. |
| `on_focus` | `(&Renderer<B>, impl Fn() + 'static)` | Focus gained. |
| `on_blur` | `(&Renderer<B>, impl Fn() + 'static)` | Focus lost. |
| `on_selection` | `(&Renderer<B>, impl Fn(&Selection) + 'static)` | Selection changed. |

## Placeholder types

```rust
pub struct Timeline;                       // animation timeline placeholder
pub trait HasKeyInput { fn on_key(&mut self, _key: &str) {} }  // key-input marker
```

These exist so the API shape is stable while the underlying renderer input
support lands. For real input handling today, drive your widgets from your
own event loop (e.g. the `InputRenderable`/`TextareaRenderable` methods in
[`core::renderables`](../../core/renderables/renderables.md)) rather than
these hooks.

Next: [slot — slot placeholder system](slot.md).
