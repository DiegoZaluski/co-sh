pub(crate) mod vec_db;

pub mod rag;

pub use rag::{Embedder, Rag, RagError};
pub use vec_db::Entry;

#[cfg(test)]
pub mod test;
