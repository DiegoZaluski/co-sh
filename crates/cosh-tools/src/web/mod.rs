pub mod fetch;
pub mod search;
#[cfg(test)]
mod test;

pub use fetch::{WebFetch, fetch};
pub use search::{WebSearch, search};
