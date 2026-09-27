//! The published laya checkpoints: the opt-in reviewed commit SHAs.
//!
//! Domain data of the laya model — nothing here is generic. They are not
//! applied implicitly, so existing Hub/offline caches keep working; pass one
//! explicitly to pin a load.

/// Opt-in reviewed commit SHAs of the published checkpoints. They are not
/// applied implicitly, so existing Hub/offline caches keep working; pass one
/// explicitly to pin a load.
pub const PINNED_REVISIONS: [(&str, &str); 3] = [
    (
        "convaiinnovations/laya",
        "55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851",
    ),
    (
        "convaiinnovations/laya-multilingual",
        "e4e9ddf21a7b1903b7acffd8814ad4307bf63a67",
    ),
    (
        "convaiinnovations/laya-typed-decisions",
        "1a793eb568e6718f15941d08f85432581df534e3",
    ),
];

#[cfg(test)]
mod tests;
