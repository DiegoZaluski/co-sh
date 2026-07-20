pub mod vec_db;

pub mod rag;

pub use rag::{Embedder, Rag, RagError};
pub use vec_db::{Entry, VecDb, VecDbError};

#[cfg(test)]
pub mod test;
