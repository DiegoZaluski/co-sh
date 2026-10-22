//! Tests for the computer module, grouped by scope: the pipeline engines
//! (act/control/wait/touch), snapshot + shell surfaces, raw input
//! (mouse/keyboard), the shared error renderer, the annotated screenshot,
//! the apps focus queries and window listing/management.
mod apps;
mod errors;
mod input;
mod pipelines;
mod screenshot;
mod snapshot_surface;
mod windows;
