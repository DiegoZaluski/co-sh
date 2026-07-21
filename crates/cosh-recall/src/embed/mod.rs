pub mod vec_db;

pub mod rag;

pub use rag::{Embedder, Rag, RagError};
pub use vec_db::{Entry, VecDb, VecDbError};

/// Convert a model name string to `fastembed::EmbeddingModel`.
///
/// This mirrors the enum-to-fastembed mapping in the TUI's `LocalEmbedModel`
/// but works from a string, avoiding the need for serde Deserialize.
///
/// Requires the `fastembed` feature.
#[cfg(feature = "fastembed")]
pub fn fastembed_model_from_str(name: &str) -> Result<fastembed::EmbeddingModel, String> {
    use fastembed::EmbeddingModel;
    match name {
        "AllMiniLML6V2" => Ok(EmbeddingModel::AllMiniLML6V2),
        "AllMiniLML6V2Q" => Ok(EmbeddingModel::AllMiniLML6V2Q),
        "AllMiniLML12V2" => Ok(EmbeddingModel::AllMiniLML12V2),
        "AllMiniLML12V2Q" => Ok(EmbeddingModel::AllMiniLML12V2Q),
        "AllMpnetBaseV2" => Ok(EmbeddingModel::AllMpnetBaseV2),
        "BGEBaseENV15" => Ok(EmbeddingModel::BGEBaseENV15),
        "BGEBaseENV15Q" => Ok(EmbeddingModel::BGEBaseENV15Q),
        "BGELargeENV15" => Ok(EmbeddingModel::BGELargeENV15),
        "BGELargeENV15Q" => Ok(EmbeddingModel::BGELargeENV15Q),
        "BGESmallENV15" => Ok(EmbeddingModel::BGESmallENV15),
        "BGESmallENV15Q" => Ok(EmbeddingModel::BGESmallENV15Q),
        "BGESmallZHV15" => Ok(EmbeddingModel::BGESmallZHV15),
        "BGELargeZHV15" => Ok(EmbeddingModel::BGELargeZHV15),
        "BGEM3" => Ok(EmbeddingModel::BGEM3),
        "NomicEmbedTextV1" => Ok(EmbeddingModel::NomicEmbedTextV1),
        "NomicEmbedTextV15" => Ok(EmbeddingModel::NomicEmbedTextV15),
        "NomicEmbedTextV15Q" => Ok(EmbeddingModel::NomicEmbedTextV15Q),
        "ParaphraseMLMiniLML12V2" => Ok(EmbeddingModel::ParaphraseMLMiniLML12V2),
        "ParaphraseMLMiniLML12V2Q" => Ok(EmbeddingModel::ParaphraseMLMiniLML12V2Q),
        "ParaphraseMLMpnetBaseV2" => Ok(EmbeddingModel::ParaphraseMLMpnetBaseV2),
        "MultilingualE5Small" => Ok(EmbeddingModel::MultilingualE5Small),
        "MultilingualE5Base" => Ok(EmbeddingModel::MultilingualE5Base),
        "MultilingualE5Large" => Ok(EmbeddingModel::MultilingualE5Large),
        "MxbaiEmbedLargeV1" => Ok(EmbeddingModel::MxbaiEmbedLargeV1),
        "MxbaiEmbedLargeV1Q" => Ok(EmbeddingModel::MxbaiEmbedLargeV1Q),
        "GTEBaseENV15" => Ok(EmbeddingModel::GTEBaseENV15),
        "GTEBaseENV15Q" => Ok(EmbeddingModel::GTEBaseENV15Q),
        "GTELargeENV15" => Ok(EmbeddingModel::GTELargeENV15),
        "GTELargeENV15Q" => Ok(EmbeddingModel::GTELargeENV15Q),
        "ModernBertEmbedLarge" => Ok(EmbeddingModel::ModernBertEmbedLarge),
        "JinaEmbeddingsV2BaseCode" => Ok(EmbeddingModel::JinaEmbeddingsV2BaseCode),
        "JinaEmbeddingsV2BaseEN" => Ok(EmbeddingModel::JinaEmbeddingsV2BaseEN),
        "EmbeddingGemma300M" => Ok(EmbeddingModel::EmbeddingGemma300M),
        "EmbeddingGemma300MQ4" => Ok(EmbeddingModel::EmbeddingGemma300MQ4),
        "EmbeddingGemma300MQ" => Ok(EmbeddingModel::EmbeddingGemma300MQ),
        "ClipVitB32" => Ok(EmbeddingModel::ClipVitB32),
        "SnowflakeArcticEmbedXS" => Ok(EmbeddingModel::SnowflakeArcticEmbedXS),
        "SnowflakeArcticEmbedXSQ" => Ok(EmbeddingModel::SnowflakeArcticEmbedXSQ),
        "SnowflakeArcticEmbedS" => Ok(EmbeddingModel::SnowflakeArcticEmbedS),
        "SnowflakeArcticEmbedSQ" => Ok(EmbeddingModel::SnowflakeArcticEmbedSQ),
        "SnowflakeArcticEmbedM" => Ok(EmbeddingModel::SnowflakeArcticEmbedM),
        "SnowflakeArcticEmbedMQ" => Ok(EmbeddingModel::SnowflakeArcticEmbedMQ),
        "SnowflakeArcticEmbedMLong" => Ok(EmbeddingModel::SnowflakeArcticEmbedMLong),
        "SnowflakeArcticEmbedMLongQ" => Ok(EmbeddingModel::SnowflakeArcticEmbedMLongQ),
        "SnowflakeArcticEmbedL" => Ok(EmbeddingModel::SnowflakeArcticEmbedL),
        "SnowflakeArcticEmbedLQ" => Ok(EmbeddingModel::SnowflakeArcticEmbedLQ),
        _ => Err(format!("unknown fastembed model '{name}'")),
    }
}

#[cfg(test)]
pub mod test;
