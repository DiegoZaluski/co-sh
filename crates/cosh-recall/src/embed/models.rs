#![allow(clippy::match_same_arms)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalEmbedModel {
    #[serde(rename = "AllMiniLML6V2")]
    AllMiniLML6V2,
    #[serde(rename = "AllMiniLML6V2Q")]
    AllMiniLML6V2Q,
    #[serde(rename = "AllMiniLML12V2")]
    AllMiniLML12V2,
    #[serde(rename = "AllMiniLML12V2Q")]
    AllMiniLML12V2Q,
    #[serde(rename = "AllMpnetBaseV2")]
    AllMpnetBaseV2,
    #[serde(rename = "BGEBaseENV15")]
    BGEBaseENV15,
    #[serde(rename = "BGEBaseENV15Q")]
    BGEBaseENV15Q,
    #[serde(rename = "BGELargeENV15")]
    BGELargeENV15,
    #[serde(rename = "BGELargeENV15Q")]
    BGELargeENV15Q,
    #[serde(rename = "BGESmallENV15")]
    BGESmallENV15,
    #[serde(rename = "BGESmallENV15Q")]
    BGESmallENV15Q,
    #[serde(rename = "BGESmallZHV15")]
    BGESmallZHV15,
    #[serde(rename = "BGELargeZHV15")]
    BGELargeZHV15,
    #[serde(rename = "BGEM3")]
    BGEM3,
    #[serde(rename = "NomicEmbedTextV1")]
    NomicEmbedTextV1,
    #[serde(rename = "NomicEmbedTextV15")]
    NomicEmbedTextV15,
    #[serde(rename = "NomicEmbedTextV15Q")]
    NomicEmbedTextV15Q,
    #[serde(rename = "ParaphraseMLMiniLML12V2")]
    ParaphraseMLMiniLML12V2,
    #[serde(rename = "ParaphraseMLMiniLML12V2Q")]
    ParaphraseMLMiniLML12V2Q,
    #[serde(rename = "ParaphraseMLMpnetBaseV2")]
    ParaphraseMLMpnetBaseV2,
    #[serde(rename = "MultilingualE5Small")]
    MultilingualE5Small,
    #[serde(rename = "MultilingualE5Base")]
    MultilingualE5Base,
    #[serde(rename = "MultilingualE5Large")]
    MultilingualE5Large,
    #[serde(rename = "MxbaiEmbedLargeV1")]
    MxbaiEmbedLargeV1,
    #[serde(rename = "MxbaiEmbedLargeV1Q")]
    MxbaiEmbedLargeV1Q,
    #[serde(rename = "GTEBaseENV15")]
    GTEBaseENV15,
    #[serde(rename = "GTEBaseENV15Q")]
    GTEBaseENV15Q,
    #[serde(rename = "GTELargeENV15")]
    GTELargeENV15,
    #[serde(rename = "GTELargeENV15Q")]
    GTELargeENV15Q,
    #[serde(rename = "ModernBertEmbedLarge")]
    ModernBertEmbedLarge,
    #[serde(rename = "JinaEmbeddingsV2BaseCode")]
    JinaEmbeddingsV2BaseCode,
    #[serde(rename = "JinaEmbeddingsV2BaseEN")]
    JinaEmbeddingsV2BaseEN,
    #[serde(rename = "EmbeddingGemma300M")]
    EmbeddingGemma300M,
    #[serde(rename = "EmbeddingGemma300MQ4")]
    EmbeddingGemma300MQ4,
    #[serde(rename = "EmbeddingGemma300MQ")]
    EmbeddingGemma300MQ,
    #[serde(rename = "ClipVitB32")]
    ClipVitB32,
    #[serde(rename = "SnowflakeArcticEmbedXS")]
    SnowflakeArcticEmbedXS,
    #[serde(rename = "SnowflakeArcticEmbedXSQ")]
    SnowflakeArcticEmbedXSQ,
    #[serde(rename = "SnowflakeArcticEmbedS")]
    SnowflakeArcticEmbedS,
    #[serde(rename = "SnowflakeArcticEmbedSQ")]
    SnowflakeArcticEmbedSQ,
    #[serde(rename = "SnowflakeArcticEmbedM")]
    SnowflakeArcticEmbedM,
    #[serde(rename = "SnowflakeArcticEmbedMQ")]
    SnowflakeArcticEmbedMQ,
    #[serde(rename = "SnowflakeArcticEmbedMLong")]
    SnowflakeArcticEmbedMLong,
    #[serde(rename = "SnowflakeArcticEmbedMLongQ")]
    SnowflakeArcticEmbedMLongQ,
    #[serde(rename = "SnowflakeArcticEmbedL")]
    SnowflakeArcticEmbedL,
    #[serde(rename = "SnowflakeArcticEmbedLQ")]
    SnowflakeArcticEmbedLQ,
}

impl LocalEmbedModel {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::AllMiniLML6V2 | Self::AllMiniLML6V2Q => "All-MiniLM-L6-v2 (384d)".into(),
            Self::AllMiniLML12V2 | Self::AllMiniLML12V2Q => "All-MiniLM-L12-v2 (384d)".into(),
            Self::AllMpnetBaseV2 => "all-mpnet-base-v2 (768d)".into(),
            Self::BGEBaseENV15 | Self::BGEBaseENV15Q => "BGE-Base-EN-v1.5 (768d)".into(),
            Self::BGELargeENV15 | Self::BGELargeENV15Q => "BGE-Large-EN-v1.5 (1024d)".into(),
            Self::BGESmallENV15 | Self::BGESmallENV15Q => "BGE-Small-EN-v1.5 (384d)".into(),
            Self::BGESmallZHV15 => "BGE-Small-ZH-v1.5 (512d)".into(),
            Self::BGELargeZHV15 => "BGE-Large-ZH-v1.5 (1024d)".into(),
            Self::BGEM3 => "BGE-M3 (1024d)".into(),
            Self::NomicEmbedTextV1 => "nomic-embed-text-v1 (768d)".into(),
            Self::NomicEmbedTextV15 | Self::NomicEmbedTextV15Q => {
                "nomic-embed-text-v1.5 (768d)".into()
            }
            Self::ParaphraseMLMiniLML12V2 | Self::ParaphraseMLMiniLML12V2Q => {
                "paraphrase-MiniLM-L12-v2 (384d)".into()
            }
            Self::ParaphraseMLMpnetBaseV2 => {
                "paraphrase-multilingual-mpnet-base-v2 (768d)".into()
            }
            Self::MultilingualE5Small => "multilingual-e5-small (384d)".into(),
            Self::MultilingualE5Base => "multilingual-e5-base (768d)".into(),
            Self::MultilingualE5Large => "multilingual-e5-large (1024d)".into(),
            Self::MxbaiEmbedLargeV1 | Self::MxbaiEmbedLargeV1Q => {
                "mxbai-embed-large-v1 (1024d)".into()
            }
            Self::GTEBaseENV15 | Self::GTEBaseENV15Q => "GTE-Base-EN-v1.5 (768d)".into(),
            Self::GTELargeENV15 | Self::GTELargeENV15Q => "GTE-Large-EN-v1.5 (1024d)".into(),
            Self::ModernBertEmbedLarge => "modernbert-embed-large (1024d)".into(),
            Self::JinaEmbeddingsV2BaseCode => "jina-embeddings-v2-base-code (768d)".into(),
            Self::JinaEmbeddingsV2BaseEN => "jina-embeddings-v2-base-en (768d)".into(),
            Self::EmbeddingGemma300M | Self::EmbeddingGemma300MQ4 | Self::EmbeddingGemma300MQ => {
                "EmbeddingGemma-300M (3584d)".into()
            }
            Self::ClipVitB32 => "CLIP-ViT-B-32 (text, 512d)".into(),
            Self::SnowflakeArcticEmbedXS | Self::SnowflakeArcticEmbedXSQ => {
                "snowflake-arctic-embed-xs (384d)".into()
            }
            Self::SnowflakeArcticEmbedS | Self::SnowflakeArcticEmbedSQ => {
                "snowflake-arctic-embed-s (384d)".into()
            }
            Self::SnowflakeArcticEmbedM | Self::SnowflakeArcticEmbedMQ => {
                "snowflake-arctic-embed-m (768d)".into()
            }
            Self::SnowflakeArcticEmbedMLong | Self::SnowflakeArcticEmbedMLongQ => {
                "snowflake-arctic-embed-m-long (768d)".into()
            }
            Self::SnowflakeArcticEmbedL | Self::SnowflakeArcticEmbedLQ => {
                "snowflake-arctic-embed-l (1024d)".into()
            }
        }
    }

    #[must_use]
    pub const fn vector_dim(&self) -> usize {
        match self {
            Self::AllMiniLML6V2
            | Self::AllMiniLML6V2Q
            | Self::AllMiniLML12V2
            | Self::AllMiniLML12V2Q => 384,
            Self::AllMpnetBaseV2 => 768,
            Self::BGEBaseENV15 | Self::BGEBaseENV15Q => 768,
            Self::BGELargeENV15 | Self::BGELargeENV15Q => 1024,
            Self::BGESmallENV15 | Self::BGESmallENV15Q => 384,
            Self::BGESmallZHV15 => 512,
            Self::BGELargeZHV15 => 1024,
            Self::BGEM3 => 1024,
            Self::NomicEmbedTextV1 => 768,
            Self::NomicEmbedTextV15 | Self::NomicEmbedTextV15Q => 768,
            Self::ParaphraseMLMiniLML12V2 | Self::ParaphraseMLMiniLML12V2Q => 384,
            Self::ParaphraseMLMpnetBaseV2 => 768,
            Self::MultilingualE5Small => 384,
            Self::MultilingualE5Base => 768,
            Self::MultilingualE5Large => 1024,
            Self::MxbaiEmbedLargeV1 | Self::MxbaiEmbedLargeV1Q => 1024,
            Self::GTEBaseENV15 | Self::GTEBaseENV15Q => 768,
            Self::GTELargeENV15 | Self::GTELargeENV15Q => 1024,
            Self::ModernBertEmbedLarge => 1024,
            Self::JinaEmbeddingsV2BaseCode | Self::JinaEmbeddingsV2BaseEN => 768,
            Self::EmbeddingGemma300M | Self::EmbeddingGemma300MQ4 | Self::EmbeddingGemma300MQ => {
                3584
            }
            Self::ClipVitB32 => 512,
            Self::SnowflakeArcticEmbedXS
            | Self::SnowflakeArcticEmbedXSQ
            | Self::SnowflakeArcticEmbedS
            | Self::SnowflakeArcticEmbedSQ => 384,
            Self::SnowflakeArcticEmbedM
            | Self::SnowflakeArcticEmbedMQ
            | Self::SnowflakeArcticEmbedMLong
            | Self::SnowflakeArcticEmbedMLongQ => 768,
            Self::SnowflakeArcticEmbedL | Self::SnowflakeArcticEmbedLQ => 1024,
        }
    }

    #[cfg(feature = "fastembed")]
    #[must_use]
    pub const fn to_fastembed_model(&self) -> fastembed::EmbeddingModel {
        match self {
            Self::AllMiniLML6V2 => fastembed::EmbeddingModel::AllMiniLML6V2,
            Self::AllMiniLML6V2Q => fastembed::EmbeddingModel::AllMiniLML6V2Q,
            Self::AllMiniLML12V2 => fastembed::EmbeddingModel::AllMiniLML12V2,
            Self::AllMiniLML12V2Q => fastembed::EmbeddingModel::AllMiniLML12V2Q,
            Self::AllMpnetBaseV2 => fastembed::EmbeddingModel::AllMpnetBaseV2,
            Self::BGEBaseENV15 => fastembed::EmbeddingModel::BGEBaseENV15,
            Self::BGEBaseENV15Q => fastembed::EmbeddingModel::BGEBaseENV15Q,
            Self::BGELargeENV15 => fastembed::EmbeddingModel::BGELargeENV15,
            Self::BGELargeENV15Q => fastembed::EmbeddingModel::BGELargeENV15Q,
            Self::BGESmallENV15 => fastembed::EmbeddingModel::BGESmallENV15,
            Self::BGESmallENV15Q => fastembed::EmbeddingModel::BGESmallENV15Q,
            Self::BGESmallZHV15 => fastembed::EmbeddingModel::BGESmallZHV15,
            Self::BGELargeZHV15 => fastembed::EmbeddingModel::BGELargeZHV15,
            Self::BGEM3 => fastembed::EmbeddingModel::BGEM3,
            Self::NomicEmbedTextV1 => fastembed::EmbeddingModel::NomicEmbedTextV1,
            Self::NomicEmbedTextV15 => fastembed::EmbeddingModel::NomicEmbedTextV15,
            Self::NomicEmbedTextV15Q => fastembed::EmbeddingModel::NomicEmbedTextV15Q,
            Self::ParaphraseMLMiniLML12V2 => fastembed::EmbeddingModel::ParaphraseMLMiniLML12V2,
            Self::ParaphraseMLMiniLML12V2Q => fastembed::EmbeddingModel::ParaphraseMLMiniLML12V2Q,
            Self::ParaphraseMLMpnetBaseV2 => fastembed::EmbeddingModel::ParaphraseMLMpnetBaseV2,
            Self::MultilingualE5Small => fastembed::EmbeddingModel::MultilingualE5Small,
            Self::MultilingualE5Base => fastembed::EmbeddingModel::MultilingualE5Base,
            Self::MultilingualE5Large => fastembed::EmbeddingModel::MultilingualE5Large,
            Self::MxbaiEmbedLargeV1 => fastembed::EmbeddingModel::MxbaiEmbedLargeV1,
            Self::MxbaiEmbedLargeV1Q => fastembed::EmbeddingModel::MxbaiEmbedLargeV1Q,
            Self::GTEBaseENV15 => fastembed::EmbeddingModel::GTEBaseENV15,
            Self::GTEBaseENV15Q => fastembed::EmbeddingModel::GTEBaseENV15Q,
            Self::GTELargeENV15 => fastembed::EmbeddingModel::GTELargeENV15,
            Self::GTELargeENV15Q => fastembed::EmbeddingModel::GTELargeENV15Q,
            Self::ModernBertEmbedLarge => fastembed::EmbeddingModel::ModernBertEmbedLarge,
            Self::JinaEmbeddingsV2BaseCode => fastembed::EmbeddingModel::JinaEmbeddingsV2BaseCode,
            Self::JinaEmbeddingsV2BaseEN => fastembed::EmbeddingModel::JinaEmbeddingsV2BaseEN,
            Self::EmbeddingGemma300M => fastembed::EmbeddingModel::EmbeddingGemma300M,
            Self::EmbeddingGemma300MQ4 => fastembed::EmbeddingModel::EmbeddingGemma300MQ4,
            Self::EmbeddingGemma300MQ => fastembed::EmbeddingModel::EmbeddingGemma300MQ,
            Self::ClipVitB32 => fastembed::EmbeddingModel::ClipVitB32,
            Self::SnowflakeArcticEmbedXS => fastembed::EmbeddingModel::SnowflakeArcticEmbedXS,
            Self::SnowflakeArcticEmbedXSQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedXSQ,
            Self::SnowflakeArcticEmbedS => fastembed::EmbeddingModel::SnowflakeArcticEmbedS,
            Self::SnowflakeArcticEmbedSQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedSQ,
            Self::SnowflakeArcticEmbedM => fastembed::EmbeddingModel::SnowflakeArcticEmbedM,
            Self::SnowflakeArcticEmbedMQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedMQ,
            Self::SnowflakeArcticEmbedMLong => {
                fastembed::EmbeddingModel::SnowflakeArcticEmbedMLong
            }
            Self::SnowflakeArcticEmbedMLongQ => {
                fastembed::EmbeddingModel::SnowflakeArcticEmbedMLongQ
            }
            Self::SnowflakeArcticEmbedL => fastembed::EmbeddingModel::SnowflakeArcticEmbedL,
            Self::SnowflakeArcticEmbedLQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedLQ,
        }
    }
}

impl core::str::FromStr for LocalEmbedModel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "AllMiniLML6V2" => Ok(Self::AllMiniLML6V2),
            "AllMiniLML6V2Q" => Ok(Self::AllMiniLML6V2Q),
            "AllMiniLML12V2" => Ok(Self::AllMiniLML12V2),
            "AllMiniLML12V2Q" => Ok(Self::AllMiniLML12V2Q),
            "AllMpnetBaseV2" => Ok(Self::AllMpnetBaseV2),
            "BGEBaseENV15" => Ok(Self::BGEBaseENV15),
            "BGEBaseENV15Q" => Ok(Self::BGEBaseENV15Q),
            "BGELargeENV15" => Ok(Self::BGELargeENV15),
            "BGELargeENV15Q" => Ok(Self::BGELargeENV15Q),
            "BGESmallENV15" => Ok(Self::BGESmallENV15),
            "BGESmallENV15Q" => Ok(Self::BGESmallENV15Q),
            "BGESmallZHV15" => Ok(Self::BGESmallZHV15),
            "BGELargeZHV15" => Ok(Self::BGELargeZHV15),
            "BGEM3" => Ok(Self::BGEM3),
            "NomicEmbedTextV1" => Ok(Self::NomicEmbedTextV1),
            "NomicEmbedTextV15" => Ok(Self::NomicEmbedTextV15),
            "NomicEmbedTextV15Q" => Ok(Self::NomicEmbedTextV15Q),
            "ParaphraseMLMiniLML12V2" => Ok(Self::ParaphraseMLMiniLML12V2),
            "ParaphraseMLMiniLML12V2Q" => Ok(Self::ParaphraseMLMiniLML12V2Q),
            "ParaphraseMLMpnetBaseV2" => Ok(Self::ParaphraseMLMpnetBaseV2),
            "MultilingualE5Small" => Ok(Self::MultilingualE5Small),
            "MultilingualE5Base" => Ok(Self::MultilingualE5Base),
            "MultilingualE5Large" => Ok(Self::MultilingualE5Large),
            "MxbaiEmbedLargeV1" => Ok(Self::MxbaiEmbedLargeV1),
            "MxbaiEmbedLargeV1Q" => Ok(Self::MxbaiEmbedLargeV1Q),
            "GTEBaseENV15" => Ok(Self::GTEBaseENV15),
            "GTEBaseENV15Q" => Ok(Self::GTEBaseENV15Q),
            "GTELargeENV15" => Ok(Self::GTELargeENV15),
            "GTELargeENV15Q" => Ok(Self::GTELargeENV15Q),
            "ModernBertEmbedLarge" => Ok(Self::ModernBertEmbedLarge),
            "JinaEmbeddingsV2BaseCode" => Ok(Self::JinaEmbeddingsV2BaseCode),
            "JinaEmbeddingsV2BaseEN" => Ok(Self::JinaEmbeddingsV2BaseEN),
            "EmbeddingGemma300M" => Ok(Self::EmbeddingGemma300M),
            "EmbeddingGemma300MQ4" => Ok(Self::EmbeddingGemma300MQ4),
            "EmbeddingGemma300MQ" => Ok(Self::EmbeddingGemma300MQ),
            "ClipVitB32" => Ok(Self::ClipVitB32),
            "SnowflakeArcticEmbedXS" => Ok(Self::SnowflakeArcticEmbedXS),
            "SnowflakeArcticEmbedXSQ" => Ok(Self::SnowflakeArcticEmbedXSQ),
            "SnowflakeArcticEmbedS" => Ok(Self::SnowflakeArcticEmbedS),
            "SnowflakeArcticEmbedSQ" => Ok(Self::SnowflakeArcticEmbedSQ),
            "SnowflakeArcticEmbedM" => Ok(Self::SnowflakeArcticEmbedM),
            "SnowflakeArcticEmbedMQ" => Ok(Self::SnowflakeArcticEmbedMQ),
            "SnowflakeArcticEmbedMLong" => Ok(Self::SnowflakeArcticEmbedMLong),
            "SnowflakeArcticEmbedMLongQ" => Ok(Self::SnowflakeArcticEmbedMLongQ),
            "SnowflakeArcticEmbedL" => Ok(Self::SnowflakeArcticEmbedL),
            "SnowflakeArcticEmbedLQ" => Ok(Self::SnowflakeArcticEmbedLQ),
            _ => Err(format!("'{s}' is not a supported fastembed model")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudEmbedConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbedderConfig {
    Local { model: LocalEmbedModel },
    Cloud(CloudEmbedConfig),
}

impl EmbedderConfig {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Local { model } => model.label(),
            Self::Cloud(c) => {
                let dim = self.vector_dim();
                let dim_str = if dim > 0 {
                    format!(" ({dim}d)")
                } else {
                    String::new()
                };
                format!("{} / {}{dim_str}", c.provider, c.model)
            }
        }
    }

    #[must_use]
    pub fn vector_dim(&self) -> usize {
        match self {
            Self::Local { model } => model.vector_dim(),
            Self::Cloud(c) => cloud_known_dim(&c.provider, &c.model),
        }


    }
}

#[must_use]
pub fn cloud_known_dim(provider: &str, model: &str) -> usize {
    match (provider, model) {
        ("openai", "text-embedding-3-small") => 1536,
        ("openai", "text-embedding-3-large") => 3072,
        ("openai", "text-embedding-ada-002") => 1536,
        ("gemini", "text-embedding-004") => 768,
        ("gemini", "gemini-embedding-001") => 768,
        ("ollama", "nomic-embed-text") => 768,
        ("ollama", "mxbai-embed-large") => 1024,
        ("ollama", "all-minilm") => 384,
        ("ollama", "snowflake-arctic-embed") => 1024,
        ("ollama", "bge-m3") => 1024,
        _ => {
            log::warn!("unknown cloud embedding model '{provider}/{model}', falling back to 384d");
            384
        }
    }
}
