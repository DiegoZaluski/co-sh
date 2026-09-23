//! Tests for the computer module, grouped by scope: the pipeline engines
//! (act/control/wait/touch), snapshot + shell surfaces, raw input
//! (mouse/keyboard), the shared error renderer, the annotated screenshot
//! and the apps focus queries. Extracted verbatim from the bottom of each
//! implementation file — logic and assertions unchanged.
mod pipelines;
mod snapshot_surface;
mod input;
mod errors;
mod screenshot;
mod apps;
