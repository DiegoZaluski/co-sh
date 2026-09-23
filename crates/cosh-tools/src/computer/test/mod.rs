//! Tests for the computer module, grouped by scope: the pipeline engines
//! (act/control/wait/touch), snapshot + shell surfaces, raw input
//! (mouse/keyboard), the shared error renderer, the annotated screenshot
//! and the apps focus queries. Extracted verbatim from the bottom of each
//! implementation file — logic and assertions unchanged.
mod apps;
mod errors;
mod input;
mod pipelines;
mod screenshot;
mod snapshot_surface;
