//! Re-export of the copy-by-selection text-region machinery.
//!
//! The implementation moved to `cosh-tui` (`cosh_tui::core::text_region`),
//! where it lives next to the rendering primitives it cooperates with
//! (`unicode_util::word_wrap`, the markdown renderer, cell buffers) and can
//! be reused by any host of the crate. The content-only region contract —
//! region `text` char 0 maps to screen column `x1`; full-width sampling
//! builders strip border/margin columns via `regions_from_full_width_cells`
//! — is documented there.
//!
//! Import from here (or straight from `cosh_tui`) in the app; this shim
//! keeps the existing `crate::util::text_region` paths stable.
//!
//! `cosh_tui::core::text_region` also exposes the phase-2 builder helpers
//! (`regions_from_full_width_cells`, `regions_from_wrapped_lines`,
//! `text_from_cell_row`); they are re-exported here once the region-builder
//! call sites migrate to them.

pub use cosh_tui::core::text_region::{TextRegion, cells_to_text_regions, extract_text_in_region};
