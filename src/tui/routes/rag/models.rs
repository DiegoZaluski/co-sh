pub use cosh_recall::embed::models::{
    CloudEmbedConfig, EmbedderConfig, LocalEmbedModel, cloud_known_dim,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagDb {
    pub name: String,
    pub uri: String,
    pub description: String,
    pub embedder: EmbedderConfig,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedModelEntry {
    Local(LocalEmbedModel),
    Cloud(String, String),
}

/// Initial capacity hint for the model list (avoids reallocation during build).
const MODEL_LIST_INITIAL_CAPACITY: usize = 50;

impl EmbedModelEntry {
    pub fn all_available() -> Vec<Self> {
        let mut entries: Vec<Self> = Vec::with_capacity(MODEL_LIST_INITIAL_CAPACITY);

        entries.push(Self::Local(LocalEmbedModel::AllMiniLML6V2));
        entries.push(Self::Local(LocalEmbedModel::AllMiniLML6V2Q));
        entries.push(Self::Local(LocalEmbedModel::AllMiniLML12V2));
        entries.push(Self::Local(LocalEmbedModel::AllMiniLML12V2Q));
        entries.push(Self::Local(LocalEmbedModel::AllMpnetBaseV2));
        entries.push(Self::Local(LocalEmbedModel::BGEBaseENV15));
        entries.push(Self::Local(LocalEmbedModel::BGEBaseENV15Q));
        entries.push(Self::Local(LocalEmbedModel::BGELargeENV15));
        entries.push(Self::Local(LocalEmbedModel::BGELargeENV15Q));
        entries.push(Self::Local(LocalEmbedModel::BGESmallENV15));
        entries.push(Self::Local(LocalEmbedModel::BGESmallENV15Q));
        entries.push(Self::Local(LocalEmbedModel::BGESmallZHV15));
        entries.push(Self::Local(LocalEmbedModel::BGELargeZHV15));
        entries.push(Self::Local(LocalEmbedModel::BGEM3));
        entries.push(Self::Local(LocalEmbedModel::NomicEmbedTextV1));
        entries.push(Self::Local(LocalEmbedModel::NomicEmbedTextV15));
        entries.push(Self::Local(LocalEmbedModel::NomicEmbedTextV15Q));
        entries.push(Self::Local(LocalEmbedModel::ParaphraseMLMiniLML12V2));
        entries.push(Self::Local(LocalEmbedModel::ParaphraseMLMiniLML12V2Q));
        entries.push(Self::Local(LocalEmbedModel::ParaphraseMLMpnetBaseV2));
        entries.push(Self::Local(LocalEmbedModel::MultilingualE5Small));
        entries.push(Self::Local(LocalEmbedModel::MultilingualE5Base));
        entries.push(Self::Local(LocalEmbedModel::MultilingualE5Large));
        entries.push(Self::Local(LocalEmbedModel::MxbaiEmbedLargeV1));
        entries.push(Self::Local(LocalEmbedModel::MxbaiEmbedLargeV1Q));
        entries.push(Self::Local(LocalEmbedModel::GTEBaseENV15));
        entries.push(Self::Local(LocalEmbedModel::GTEBaseENV15Q));
        entries.push(Self::Local(LocalEmbedModel::GTELargeENV15));
        entries.push(Self::Local(LocalEmbedModel::GTELargeENV15Q));
        entries.push(Self::Local(LocalEmbedModel::ModernBertEmbedLarge));
        entries.push(Self::Local(LocalEmbedModel::JinaEmbeddingsV2BaseCode));
        entries.push(Self::Local(LocalEmbedModel::JinaEmbeddingsV2BaseEN));
        entries.push(Self::Local(LocalEmbedModel::EmbeddingGemma300M));
        entries.push(Self::Local(LocalEmbedModel::EmbeddingGemma300MQ4));
        entries.push(Self::Local(LocalEmbedModel::EmbeddingGemma300MQ));
        entries.push(Self::Local(LocalEmbedModel::ClipVitB32));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedXS));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedXSQ));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedS));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedSQ));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedM));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMQ));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMLong));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedMLongQ));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedL));
        entries.push(Self::Local(LocalEmbedModel::SnowflakeArcticEmbedLQ));

        if std::env::var("OPENAI_API_KEY").is_ok() {
            entries.push(Self::Cloud(
                "openai".into(),
                "text-embedding-3-small".into(),
            ));
            entries.push(Self::Cloud(
                "openai".into(),
                "text-embedding-3-large".into(),
            ));
            entries.push(Self::Cloud(
                "openai".into(),
                "text-embedding-ada-002".into(),
            ));
        }
        if std::env::var("GEMINI_API_KEY").is_ok() {
            entries.push(Self::Cloud("gemini".into(), "text-embedding-004".into()));
            entries.push(Self::Cloud("gemini".into(), "gemini-embedding-001".into()));
        }
        if std::env::var("OLLAMA_API_KEY").is_ok() || std::env::var("OLLAMA_HOST").is_ok() {
            entries.push(Self::Cloud("ollama".into(), "nomic-embed-text".into()));
            entries.push(Self::Cloud("ollama".into(), "mxbai-embed-large".into()));
            entries.push(Self::Cloud("ollama".into(), "all-minilm".into()));
            entries.push(Self::Cloud(
                "ollama".into(),
                "snowflake-arctic-embed".into(),
            ));
            entries.push(Self::Cloud("ollama".into(), "bge-m3".into()));
        }

        entries
    }

    pub fn label(&self) -> String {
        match self {
            Self::Local(m) => m.label(),
            Self::Cloud(provider, model) => {
                let dim = cloud_known_dim(provider, model);
                format!("{provider} / {model} ({dim}d)")
            }
        }
    }

    pub fn vector_dim(&self) -> usize {
        match self {
            Self::Local(m) => m.vector_dim(),
            Self::Cloud(provider, model) => cloud_known_dim(provider, model),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateDbFocus {
    Name,
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RagMode {
    Idle,
    Fetching,
    Previewing,
    Embedding,
}

#[derive(Debug, Clone)]
pub enum RagAction {
    Consumed,
    Back,
    ClosePreview,
    FetchUrlOrPath(String),
    EmbedContent {
        content: String,
        db_name: String,
        db_description: String,
        model: Option<EmbedModelEntry>,
    },
    CreateDb {
        name: String,
        description: String,
        embedder: EmbedderConfig,
    },
    ShowWarning(String),
}
