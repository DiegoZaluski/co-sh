# `solid::utils` — utilities

Small helpers shared across the solid layer:

| Source file | Page | Contents |
|---|---|---|
| `utils/id_counter.rs` | [id_counter](id_counter.md) | `get_next_id` — global per-type id counter. |
| `utils/log.rs` | [log](log.md) | `set_debug` / `is_debug` + `log_reconciler!` macro. |

Both are fully functional. `get_next_id` is the canonical way to mint unique
element ids for components; the debug flag gates `[Reconciler]`-prefixed
diagnostics on stderr.
