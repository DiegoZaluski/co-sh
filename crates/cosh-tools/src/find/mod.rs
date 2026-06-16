//! File-search tools built on the `cosh-sdk` find engine.
//!
//! - [`glob::glob`]: find files and directories by glob pattern.
//! - [`grep::grep`]: search file content with a regex.

pub mod glob;
pub mod grep;
pub mod types;

#[cfg(test)]
mod test;

pub use glob::glob;
pub use grep::grep;
pub use types::{
    ContextEntry, GlobEntry, GlobInput, GlobOutput, GrepInput, GrepMatchEntry, GrepOutput,
};
