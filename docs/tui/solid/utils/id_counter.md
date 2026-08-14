# `solid::utils::id_counter` — unique id generation

A process-global per-type counter for minting unique element ids.

```rust
pub fn get_next_id(element_type: &str) -> String
```

`get_next_id` increments a global counter keyed by `element_type` and
returns `"{element_type}-{n}"` — e.g. `get_next_id("span")` yields
`"span-1"`, then `"span-2"`, and so on. Counters are per-type, so
`"box-1"` and `"span-1"` can coexist.

```rust,ignore
let a = get_next_id("span");   // "span-1"
let b = get_next_id("span");   // "span-2"
let c = get_next_id("box");    // "box-1"
```

**Panics** if the global mutex is poisoned (another thread panicked while
holding the lock).

Back to [solid::utils — utilities](utils.md).
