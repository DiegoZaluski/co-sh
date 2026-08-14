# `plugins::slot` — the slot registry

`SlotRegistry` maps slot names to lists of registered renderer closures —
the cosh-tui analogue of `createSlotRegistry` from `@opentui/core`. Plugins
register UI fragments under named slots; the host resolves them, possibly
filtering by [`SlotMode`](#slotmode). This is the **most functional part of
the plugin system** — fully implemented and synchronous.

```rust
SlotRegistry::new() -> Self
```

## Types

```rust
pub enum SlotMode {
    SingleWinner,   // render only the first registered entry
    Replace,        // render all entries, replacing previous output
    Append,         // render all entries, appending (default)
}

pub struct ResolvedEntry {
    pub id: String,
    pub renderable: Box<dyn Renderable>,
}

pub struct PluginErrorEvent {
    pub plugin_id: String,
    pub slot_name: String,
    pub phase: String,
    pub source: String,
    pub error: String,
}

pub type SlotRenderer = Box<dyn Fn() -> Box<dyn Renderable>>;
```

## Registering and resolving

| Method | Behavior |
|---|---|
| `register(slot_name, id, renderer)` | Add a renderer under a slot name with a stable id. |
| `resolve(slot_name, mode) -> Vec<ResolvedEntry>` | Build the entries: `SingleWinner` takes only the first; `Replace`/`Append` run all. Unknown slot → empty vec. |
| `unregister(slot_name)` | Remove all entries for a slot. |
| `has_entries(slot_name) -> bool` | Whether the slot has any registered renderers. |
| `slot_count() -> usize` | Number of slots with entries. |

## Error handling

```rust
on_error(handler)              // register a callback for plugin failures
report_error(&PluginErrorEvent) // dispatch an event to all handlers
```

## Example

```rust,ignore
use cosh_tui::solid::plugins::slot::{SlotRegistry, SlotMode};
use cosh_tui::core::renderables::text::TextRenderable;
use cosh_tui::core::lib::styled_text::string_to_styled_text;

let mut registry = SlotRegistry::new();

registry.register("statusbar", "clock", Box::new(|| -> Box<dyn Renderable> {
    Box::new(TextRenderable::new(Some(string_to_styled_text("12:00"))))
}));
registry.register("statusbar", "git", Box::new(|| -> Box<dyn Renderable> {
    Box::new(TextRenderable::new(Some(string_to_styled_text("main"))))
}));

let entries = registry.resolve("statusbar", SlotMode::Append);
assert_eq!(entries.len(), 2);
assert_eq!(entries[0].id, "clock");

// SingleWinner renders only the first plugin's fragment.
let winner = registry.resolve("statusbar", SlotMode::SingleWinner);
assert_eq!(winner.len(), 1);
```

Back to [solid — module overview](../solid.md).
