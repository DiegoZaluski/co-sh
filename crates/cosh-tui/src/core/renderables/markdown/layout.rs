use super::md::estimate_height_impl;

/// Estimate the number of terminal rows required to render `text` as markdown
/// within a viewport `max_w` columns wide.
///
/// Callers (e.g. `SessionView`) use this to pre-allocate the correct area
/// height before rendering.
///
/// The estimate is produced by measuring each top-level markdown block with
/// the renderer's own block pipeline ([`super::md`]) — see
/// [`estimate_height_impl`] — so it is exact by construction: any change to
/// the renderer's wrapping or spacing automatically applies to this estimate.
/// Results are memoized in an LRU keyed by (content, width).
#[must_use]
pub fn estimate_height(text: &str, max_w: u16) -> u16 {
    estimate_height_impl(text, max_w)
}
