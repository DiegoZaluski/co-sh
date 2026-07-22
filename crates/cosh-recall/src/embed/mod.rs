pub mod models;
pub mod vec_db;

pub mod rag;

pub use models::{CloudEmbedConfig, EmbedderConfig, LocalEmbedModel};
pub use rag::{Embedder, Rag, RagError};
pub use vec_db::{Entry, VecDb, VecDbError, validate_table_name};

/// Convert a model name string to `fastembed::EmbeddingModel`.
#[cfg(feature = "fastembed")]
pub fn fastembed_model_from_str(name: &str) -> Result<fastembed::EmbeddingModel, String> {
    name.parse::<LocalEmbedModel>()
        .map(|m| m.to_fastembed_model())
}

#[cfg(test)]
pub mod test;
