//! Data models for the RAG Knowledge Base feature.
//!
//! Pure type definitions extracted from the original monolith:
//! - Embedding model enums (local ONNX + cloud providers)
//! - Database entry schema
//! - UI action/mode enums

use serde::{Deserialize, Serialize};

// ── Local fastembed models ─────────────────────────────────────────────

/// All text-embedding models available in `fastembed` v5.17.2.
///
/// Quantized variants (Q / Q4 suffix) have the same vector dimension
/// as their non-quantized counterpart but are smaller and faster.
///
/// Serde renames match `fastembed::EmbeddingModel` variant names exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalEmbedModel {
    // ── sentence-transformers ─────────────────────────────────────────
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

    // ── BAAI/bge ──────────────────────────────────────────────────────
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

    // ── nomic-ai ──────────────────────────────────────────────────────
    #[serde(rename = "NomicEmbedTextV1")]
    NomicEmbedTextV1,
    #[serde(rename = "NomicEmbedTextV15")]
    NomicEmbedTextV15,
    #[serde(rename = "NomicEmbedTextV15Q")]
    NomicEmbedTextV15Q,

    // ── paraphrase ────────────────────────────────────────────────────
    #[serde(rename = "ParaphraseMLMiniLML12V2")]
    ParaphraseMLMiniLML12V2,
    #[serde(rename = "ParaphraseMLMiniLML12V2Q")]
    ParaphraseMLMiniLML12V2Q,
    #[serde(rename = "ParaphraseMLMpnetBaseV2")]
    ParaphraseMLMpnetBaseV2,

    // ── intfloat/multilingual-e5 ──────────────────────────────────────
    #[serde(rename = "MultilingualE5Small")]
    MultilingualE5Small,
    #[serde(rename = "MultilingualE5Base")]
    MultilingualE5Base,
    #[serde(rename = "MultilingualE5Large")]
    MultilingualE5Large,

    // ── mixedbread-ai ─────────────────────────────────────────────────
    #[serde(rename = "MxbaiEmbedLargeV1")]
    MxbaiEmbedLargeV1,
    #[serde(rename = "MxbaiEmbedLargeV1Q")]
    MxbaiEmbedLargeV1Q,

    // ── Alibaba-NLP/gte ───────────────────────────────────────────────
    #[serde(rename = "GTEBaseENV15")]
    GTEBaseENV15,
    #[serde(rename = "GTEBaseENV15Q")]
    GTEBaseENV15Q,
    #[serde(rename = "GTELargeENV15")]
    GTELargeENV15,
    #[serde(rename = "GTELargeENV15Q")]
    GTELargeENV15Q,

    // ── modernbert ────────────────────────────────────────────────────
    #[serde(rename = "ModernBertEmbedLarge")]
    ModernBertEmbedLarge,

    // ── jina-ai ───────────────────────────────────────────────────────
    #[serde(rename = "JinaEmbeddingsV2BaseCode")]
    JinaEmbeddingsV2BaseCode,
    #[serde(rename = "JinaEmbeddingsV2BaseEN")]
    JinaEmbeddingsV2BaseEN,

    // ── EmbeddingGemma ────────────────────────────────────────────────
    #[serde(rename = "EmbeddingGemma300M")]
    EmbeddingGemma300M,
    #[serde(rename = "EmbeddingGemma300MQ4")]
    EmbeddingGemma300MQ4,
    #[serde(rename = "EmbeddingGemma300MQ")]
    EmbeddingGemma300MQ,

    // ── Qdrant CLIP text component ────────────────────────────────────
    #[serde(rename = "ClipVitB32")]
    ClipVitB32,

    // ── snowflake/arctic-embed ───────────────────────────────────────
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
    /// Human-readable label with dimension.
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
            Self::NomicEmbedTextV15 | Self::NomicEmbedTextV15Q => "nomic-embed-text-v1.5 (768d)".into(),

            Self::ParaphraseMLMiniLML12V2 | Self::ParaphraseMLMiniLML12V2Q =>
                "paraphrase-MiniLM-L12-v2 (384d)".into(),
            Self::ParaphraseMLMpnetBaseV2 =>
                "paraphrase-multilingual-mpnet-base-v2 (768d)".into(),

            Self::MultilingualE5Small => "multilingual-e5-small (384d)".into(),
            Self::MultilingualE5Base => "multilingual-e5-base (768d)".into(),
            Self::MultilingualE5Large => "multilingual-e5-large (1024d)".into(),

            Self::MxbaiEmbedLargeV1 | Self::MxbaiEmbedLargeV1Q =>
                "mxbai-embed-large-v1 (1024d)".into(),

            Self::GTEBaseENV15 | Self::GTEBaseENV15Q => "GTE-Base-EN-v1.5 (768d)".into(),
            Self::GTELargeENV15 | Self::GTELargeENV15Q => "GTE-Large-EN-v1.5 (1024d)".into(),

            Self::ModernBertEmbedLarge => "modernbert-embed-large (1024d)".into(),

            Self::JinaEmbeddingsV2BaseCode => "jina-embeddings-v2-base-code (768d)".into(),
            Self::JinaEmbeddingsV2BaseEN => "jina-embeddings-v2-base-en (768d)".into(),

            Self::EmbeddingGemma300M | Self::EmbeddingGemma300MQ4 | Self::EmbeddingGemma300MQ =>
                "EmbeddingGemma-300M (3584d)".into(),

            Self::ClipVitB32 => "CLIP-ViT-B-32 (text, 512d)".into(),

            Self::SnowflakeArcticEmbedXS | Self::SnowflakeArcticEmbedXSQ =>
                "snowflake-arctic-embed-xs (384d)".into(),
            Self::SnowflakeArcticEmbedS | Self::SnowflakeArcticEmbedSQ =>
                "snowflake-arctic-embed-s (384d)".into(),
            Self::SnowflakeArcticEmbedM | Self::SnowflakeArcticEmbedMQ =>
                "snowflake-arctic-embed-m (768d)".into(),
            Self::SnowflakeArcticEmbedMLong | Self::SnowflakeArcticEmbedMLongQ =>
                "snowflake-arctic-embed-m-long (768d)".into(),
            Self::SnowflakeArcticEmbedL | Self::SnowflakeArcticEmbedLQ =>
                "snowflake-arctic-embed-l (1024d)".into(),
        }
    }

    /// Vector dimension for this model.
    pub const fn vector_dim(&self) -> usize {
        match self {
            Self::AllMiniLML6V2 | Self::AllMiniLML6V2Q
            | Self::AllMiniLML12V2 | Self::AllMiniLML12V2Q => 384,

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

            Self::EmbeddingGemma300M | Self::EmbeddingGemma300MQ4 | Self::EmbeddingGemma300MQ => 3584,

            Self::ClipVitB32 => 512,

            Self::SnowflakeArcticEmbedXS | Self::SnowflakeArcticEmbedXSQ
            | Self::SnowflakeArcticEmbedS | Self::SnowflakeArcticEmbedSQ => 384,
            Self::SnowflakeArcticEmbedM | Self::SnowflakeArcticEmbedMQ
            | Self::SnowflakeArcticEmbedMLong | Self::SnowflakeArcticEmbedMLongQ => 768,
            Self::SnowflakeArcticEmbedL | Self::SnowflakeArcticEmbedLQ => 1024,
        }
    }

    /// Convert to `fastembed::EmbeddingModel` (requires `fastembed` feature).
    #[cfg(feature = "fastembed")]
    pub fn to_fastembed_model(&self) -> fastembed::EmbeddingModel {
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
            Self::SnowflakeArcticEmbedMLong => fastembed::EmbeddingModel::SnowflakeArcticEmbedMLong,
            Self::SnowflakeArcticEmbedMLongQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedMLongQ,
            Self::SnowflakeArcticEmbedL => fastembed::EmbeddingModel::SnowflakeArcticEmbedL,
            Self::SnowflakeArcticEmbedLQ => fastembed::EmbeddingModel::SnowflakeArcticEmbedLQ,
        }
    }
}

// ── Cloud embed config ─────────────────────────────────────────────────

/// Cloud/provider-based embedding models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudEmbedConfig {
    /// Provider name (e.g. "openai", "gemini", "ollama").
    pub provider: String,
    /// Model name (e.g. "text-embedding-3-small").
    pub model: String,
}

// ── EmbedderConfig ─────────────────────────────────────────────────────

/// The embedding strategy used for a database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbedderConfig {
    /// Local ONNX embedding via fastembed.
    Local { model: LocalEmbedModel },
    /// Cloud/provider-based embedding.
    Cloud(CloudEmbedConfig),
}

impl EmbedderConfig {
    /// Human-readable label for display in the UI.
    pub fn label(&self) -> String {
        match self {
            EmbedderConfig::Local { model } => model.label(),
            EmbedderConfig::Cloud(c) => {
                let dim = self.vector_dim();
                let dim_str = if dim > 0 { format!(" ({dim}d)") } else { String::new() };
                format!("{} / {}{dim_str}", c.provider, c.model)
            }
        }
    }

    /// The vector dimension this model produces.
    pub fn vector_dim(&self) -> usize {
        match self {
            EmbedderConfig::Local { model } => model.vector_dim(),
            EmbedderConfig::Cloud(c) => cloud_known_dim(&c.provider, &c.model),
        }
    }
}

/// Lookup known vector dimensions for cloud embedding models.
fn cloud_known_dim(provider: &str, model: &str) -> usize {
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
        _ => 384, // fallback
    }
}

// ── RagDb ──────────────────────────────────────────────────────────────

/// A single database entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagDb {
    /// User-given name (also used as the directory name on disk).
    pub name: String,
    /// Absolute path to the LanceDB directory.
    pub uri: String,
    /// Description of what knowledge this DB contains (user-provided).
    pub description: String,
    /// The embedding model used to create this DB.
    pub embedder: EmbedderConfig,
    /// Unix timestamp (ms) of creation.
    pub created_at: u64,
}

// ── EmbedModelEntry for the UI selector ────────────────────────────────

/// Entry in the embed model selector dropdown.
/// Represents either a local fastembed model or a cloud provider model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedModelEntry {
    /// Local fastembed model.
    Local(LocalEmbedModel),
    /// Cloud provider + model name.
    Cloud(String, String), // (provider, model)
}

impl EmbedModelEntry {
    /// All available models (local + cloud, gated by env vars).
    pub fn all_available() -> Vec<Self> {
        // ── Local models (all real fastembed text models) ─────────────
        #[allow(unused_mut)]
        let mut entries: Vec<Self> = vec![
            // sentence-transformers
            Self::Local(LocalEmbedModel::AllMiniLML6V2),
            Self::Local(LocalEmbedModel::AllMiniLML6V2Q),
            Self::Local(LocalEmbedModel::AllMiniLML12V2),
            Self::Local(LocalEmbedModel::AllMiniLML12V2Q),
            Self::Local(LocalEmbedModel::AllMpnetBaseV2),
            // BAAI/bge
            Self::Local(LocalEmbedModel::BGEBaseENV15),
            Self::Local(LocalEmbedModel::BGEBaseENV15Q),
            Self::Local(LocalEmbedModel::BGELargeENV15),
            Self::Local(LocalEmbedModel::BGELargeENV15Q),
            Self::Local(LocalEmbedModel::BGESmallENV15),
            Self::Local(LocalEmbedModel::BGESmallENV15Q),
            Self::Local(LocalEmbedModel::BGESmallZHV15),
            Self::Local(LocalEmbedModel::BGELargeZHV15),
            Self::Local(LocalEmbedModel::BGEM3),
            // nomic-ai
            Self::Local(LocalEmbedModel::NomicEmbedTextV1),
            Self::Local(LocalEmbedModel::NomicEmbedTextV15),
            Self::Local(LocalEmbedModel::NomicEmbedTextV15Q),
            // paraphrase
            Self::Local(LocalEmbedModel::ParaphraseMLMiniLML12V2),
            Self::Local(LocalEmbedModel::ParaphraseMLMiniLML12V2Q),
            Self::Local(LocalEmbedModel::ParaphraseMLMpnetBaseV2),
            // intfloat/multilingual-e5
            Self::Local(LocalEmbedModel::MultilingualE5Small),
            Self::Local(LocalEmbedModel::MultilingualE5Base),
            Self::Local(LocalEmbedModel::MultilingualE5Large),
            // mixedbread-ai
            Self::Local(LocalEmbedModel::MxbaiEmbedLargeV1),
            Self::Local(LocalEmbedModel::MxbaiEmbedLargeV1Q),
            // Alibaba-NLP/gte
            Self::Local(LocalEmbedModel::GTEBaseENV15),
            Self::Local(LocalEmbedModel::GTEBaseENV15Q),
            Self::Local(LocalEmbedModel::GTELargeENV15),
            Self::Local(LocalEmbedModel::GTELargeENV15Q),
            // modernbert
            Self::Local(LocalEmbedModel::ModernBertEmbedLarge),
            // jina-ai
            Self::Local(LocalEmbedModel::JinaEmbeddingsV2BaseCode),
            Self::Local(LocalEmbedModel::JinaEmbeddingsV2BaseEN),
            // EmbeddingGemma
            Self::Local(LocalEmbedModel::EmbeddingGemma300M),
            Self::Local(LocalEmbedModel::EmbeddingGemma300MQ4),
            Self::Local(LocalEmbedModel::EmbeddingGemma300MQ),
            // CLIP text
            Self::Local(LocalEmbedModel::ClipVitB32),
            // snowflake/arctic-embed
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedXS),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedXSQ),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedS),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedSQ),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedM),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMQ),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMLong),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMLongQ),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedL),
            Self::Local(LocalEmbedModel::SnowflakeArcticEmbedLQ),
        ];

        // ── Cloud models (show if env var is set) ─────────────────────
        if std::env::var("OPENAI_API_KEY").is_ok() {
            entries.push(Self::Cloud("openai".into(), "text-embedding-3-small".into()));
            entries.push(Self::Cloud("openai".into(), "text-embedding-3-large".into()));
            entries.push(Self::Cloud("openai".into(), "text-embedding-ada-002".into()));
        }
        if std::env::var("GEMINI_API_KEY").is_ok() {
            entries.push(Self::Cloud("gemini".into(), "text-embedding-004".into()));
            entries.push(Self::Cloud("gemini".into(), "gemini-embedding-001".into()));
        }
        if std::env::var("OLLAMA_API_KEY").is_ok() || std::env::var("OLLAMA_HOST").is_ok() {
            entries.push(Self::Cloud("ollama".into(), "nomic-embed-text".into()));
            entries.push(Self::Cloud("ollama".into(), "mxbai-embed-large".into()));
            entries.push(Self::Cloud("ollama".into(), "all-minilm".into()));
            entries.push(Self::Cloud("ollama".into(), "snowflake-arctic-embed".into()));
            entries.push(Self::Cloud("ollama".into(), "bge-m3".into()));
        }

        entries
    }

    /// Human-readable label.
    pub fn label(&self) -> String {
        match self {
            Self::Local(m) => m.label(),
            Self::Cloud(provider, model) => {
                let dim = cloud_known_dim(provider, model);
                format!("{provider} / {model} ({dim}d)")
            }
        }
    }

    /// Vector dimension for this model.
    pub fn vector_dim(&self) -> usize {
        match self {
            Self::Local(m) => m.vector_dim(),
            Self::Cloud(provider, model) => cloud_known_dim(provider, model),
        }
    }
}

// ── RagMode ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RagMode {
    Idle,
    Fetching,
    Previewing,
    Embedding,
}

// ── RagAction ──────────────────────────────────────────────────────────

/// Actions returned from the RAG view's input handlers.
/// The caller (app.rs) interprets each action to mutate state.
#[derive(Debug, Clone)]
pub enum RagAction {
    /// Key was consumed (no side-effect needed).
    Consumed,
    /// Exit the RAG view entirely (back to home).
    Back,
    /// Close the content preview overlay (stay in RAG view).
    ClosePreview,
    /// Fetch a URL or read a file path.
    FetchUrlOrPath(String),
    /// Embed the previewed content into a database.
    EmbedContent {
        content: String,
        db_name: String,
        db_description: String,
        model: Option<EmbedModelEntry>,
    },
}
