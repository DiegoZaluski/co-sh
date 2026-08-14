# `solid::utils::log` — debug logging

A tiny process-global debug flag and a logging macro for reconciler
diagnostics.

```rust
pub fn set_debug(enabled: bool)   // turn debug logging on/off
pub fn is_debug() -> bool         // read the flag
```

The `log_reconciler!` macro writes to stderr **only when debug is enabled**:

```rust
#[macro_export]
macro_rules! log_reconciler {
    ($($arg:tt)*) => {
        if $crate::solid::utils::log::is_debug() {
            eprintln!("[Reconciler] {}", format!($($arg)*));
        }
    };
}
```

## Example

```rust,ignore
use cosh_tui::solid::utils::log::set_debug;

set_debug(true);
log_reconciler!("inserting node {}", node_id);   // prints "[Reconciler] inserting node …"
set_debug(false);
```

The flag is a process-global `AtomicBool` (defaults to `false`), so debug
output is off until explicitly enabled.

Back to [solid::utils — utilities](utils.md).
