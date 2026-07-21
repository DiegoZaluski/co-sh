#[cfg(feature = "fastembed")]
use std::sync::Mutex;

use super::vec_db::{Entry, VecDb, VecDbError};
use thiserror::Error;

/// Unified error type for RAG operations.
///
/// Covers errors from embedding models, the `LanceDB` vector store,
/// and lock poisoning (internal mutex).
#[derive(Debug, Error)]
pub enum RagError {
    /// The embedding model failed to process the input text.
    #[error("embedding error: {0}")]
    Embedding(String),

    /// The `LanceDB` vector store operation failed.
    #[error("database error: {0}")]
    Database(#[from] VecDbError),

    /// An internal mutex was poisoned (another task panicked while holding the lock).
    #[error("internal lock poisoned")]
    LockPoisoned,
}

// -- From impls for external error types (feature-gated) --

#[cfg(feature = "fastembed")]
impl From<fastembed::Error> for RagError {
    fn from(e: fastembed::Error) -> Self {
        Self::Embedding(e.to_string())
    }
}

#[cfg(feature = "cloud")]
impl From<cosh_sdk::connector::ConnectorError> for RagError {
    fn from(e: cosh_sdk::connector::ConnectorError) -> Self {
        Self::Embedding(e.to_string())
    }
}

// -- Embedder --

/// Embedding provider used by [`Rag`].
///
/// Two variants are available, each gated behind a Cargo feature:
///
/// | Variant | Feature | Description |
/// |---|---|---|
/// | [`Local`][Embedder::Local] | `fastembed` | ONNX-based local embedding via fastembed-rs |
/// | [`Cloud`][Embedder::Cloud] | `cloud` | Remote embedding via cosh-sdk connector |
///
/// # Feature flags
/// - `fastembed` — enables the [`Local`][Embedder::Local] variant
/// - `cloud` — enables the [`Cloud`][Embedder::Cloud] variant
///
/// At least one feature must be active; otherwise the enum is empty and
/// `Rag` cannot be constructed.
///
/// # Examples
///
/// ```rust,ignore
/// // Local embedder (requires feature "fastembed")
/// let local = Embedder::try_new_local(EmbeddingModel::BGESmallENV15)?;
///
/// // Cloud embedder (requires feature "cloud")
/// let connector = Connector::new("openai")?.with_model("text-embedding-3-small");
/// let cloud  = Embedder::new_cloud(connector, 1536);
/// ```
pub enum Embedder {
    /// Local ONNX embedding via [fastembed-rs](https://github.com/Anush008/fastembed-rs).
    ///
    /// Runs on CPU using ONNX Runtime. Models are downloaded from `HuggingFace`
    /// on first use and cached locally.
    ///
    /// Requires the `fastembed` feature.
    #[cfg(feature = "fastembed")]
    Local {
        /// ONNX session wrapped in a mutex (the session is not `Sync`).
        model: Box<Mutex<fastembed::TextEmbedding>>,
        /// Output dimension of the loaded model.
        dim: usize,
    },

    /// Remote embedding via a [`cosh_sdk::connector::Connector`].
    ///
    /// Sends text to a provider API (e.g. `OpenAI`, Gemini) and returns the
    /// embedding vector.
    ///
    /// Requires the `cloud` feature.
    #[cfg(feature = "cloud")]
    Cloud {
        /// A configured SDK connector pointing at the embedding provider.
        connector: cosh_sdk::connector::Connector,
        /// Output dimension of the chosen model.
        dim: usize,
    },
}

impl Embedder {
    /// Create a local ONNX embedder from a built-in model.
    ///
    /// Downloads the model files from `HuggingFace` on first use and caches
    /// them under the platform cache directory.
    ///
    /// Requires the `fastembed` feature.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Embedding`] if the model cannot be loaded
    /// (e.g. network failure on first download, incompatible ONNX runtime).
    #[cfg(feature = "fastembed")]
    pub fn try_new_local(model: fastembed::EmbeddingModel) -> Result<Self, RagError> {
        let dim = fastembed::TextEmbedding::get_model_info(&model)
            .map_err(|e| RagError::Embedding(e.to_string()))?
            .dim;
        // Suppress fastembed's download progress bar to avoid breaking the TUI.
        let mut options = fastembed::InitOptions::new(model);
        options.show_download_progress = false;
        let text_embedding = fastembed::TextEmbedding::try_new(options)?;
        Ok(Self::Local {
            model: Box::new(Mutex::new(text_embedding)),
            dim,
        })
    }

    /// Create a cloud embedder from a pre-configured SDK connector.
    ///
    /// `dim` is the output dimension of the chosen embedding model; it must
    /// match what the provider returns (e.g. 1536 for `text-embedding-3-small`,
    /// 768 for `text-embedding-3-large`, 384 for `text-embedding-ada-002`).
    ///
    /// The connector must be pre-configured with the correct model name and
    /// API key (see the [`cosh-sdk`] documentation).
    ///
    /// Requires the `cloud` feature.
    ///
    /// [`cosh-sdk`]: https://docs.rs/cosh-sdk
    #[cfg(feature = "cloud")]
    #[must_use]
    pub const fn new_cloud(connector: cosh_sdk::connector::Connector, dim: usize) -> Self {
        Self::Cloud { connector, dim }
    }

    /// Embed a batch of texts, returning one vector per input.
    #[allow(clippy::unused_async)]
    pub async fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, RagError> {
        match self {
            #[cfg(feature = "fastembed")]
            Self::Local { model, dim: _ } => {
                let mut guard = model.lock().map_err(|_| RagError::LockPoisoned)?;
                let embeddings = guard.embed(_texts, None)?;
                drop(guard);
                Ok(embeddings)
            }
            #[cfg(feature = "cloud")]
            Self::Cloud { connector, dim: _ } => {
                let mut results = Vec::with_capacity(_texts.len());
                for text in _texts {
                    let emb = connector.embed(text).await?;
                    results.push(emb);
                }
                Ok(results)
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("no embedder feature enabled (fastembed or cloud)"),
        }
    }

    /// Return the output dimension of the embedding model.
    pub(crate) const fn dim(&self) -> usize {
        match self {
            #[cfg(feature = "fastembed")]
            Self::Local { dim, .. } => *dim,
            #[cfg(feature = "cloud")]
            Self::Cloud { dim, .. } => *dim,
            #[allow(unreachable_patterns)]
            _ => unreachable!(),
        }
    }
}

// -- Rag --

/// High-level RAG (Retrieval-Augmented Generation) index.
///
/// Combines a text embedding model with a [LanceDB] vector store to provide
/// a simple ingest-and-search workflow.
///
/// # Feature flags
///
/// The underlying `LanceDB` storage is activated with the `lancedb` feature.
/// At least one embedding provider feature (`fastembed` or `cloud`) is also
/// required.
///
/// | Feature | Required for |
/// |---|---|
/// | `lancedb` | Vector storage (always needed) |
/// | `fastembed` | Local ONNX embedding |
/// | `cloud` | Remote embedding via cosh-sdk |
///
/// # Examples
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use cosh_recall::embed::{Rag, Embedder};
///
/// // 1. Pick an embedder
/// let embedder = Embedder::try_new_local(EmbeddingModel::BGESmallENV15)?;
///
/// // 2. Open (or create) a LanceDB table
/// let rag = Rag::connect("data/lancedb", "my_docs", embedder).await?;
///
/// // 3. Ingest documents – embedding + storage is automatic
/// rag.ingest("doc-1", "Retrieval-Augmented Generation (RAG) combines ...").await?;
/// rag.ingest("doc-2", "LanceDB is a vector database built on Lance ...").await?;
///
/// // 4. Search semantically
/// let results = rag.search("What is RAG?", 5).await?;
/// for entry in &results {
///     println!("[{}] {}", entry.id, entry.content);
/// }
/// ```
///
/// [LanceDB]: https://lancedb.com
pub struct Rag {
    embedder: Embedder,
    db: VecDb,
}

impl Rag {
    /// Open (or create) a `LanceDB` table with the given embedder.
    ///
    /// If the table already exists its schema is validated; otherwise a new
    /// table is created with columns `id`, `content`, and `vector` (dimension
    /// is inferred from the embedder).
    ///
    /// # Arguments
    ///
    /// * `uri` — Local directory or cloud URI for the `LanceDB` database.
    /// * `table_name` — Name of the table inside the database.
    /// * `embedder` — An [`Embedder`] instance (local or cloud).
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Database`] if `LanceDB` cannot be reached or the
    /// table cannot be created.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let rag = Rag::connect("data/lancedb", "my_table", embedder).await?;
    /// ```
    pub async fn connect(
        uri: &str,
        table_name: &str,
        embedder: Embedder,
    ) -> Result<Self, RagError> {
        let dim = embedder.dim();
        let db = VecDb::connect(uri, table_name, dim).await?;
        Ok(Self { embedder, db })
    }

    /// Embed `content` and store it in the vector index.
    ///
    /// If the same `content` string already exists in the table, the
    /// existing id is returned and no duplicate is inserted (content-based
    /// deduplication).
    ///
    /// # Arguments
    ///
    /// * `id` — A unique identifier (max 100 chars, no `'`, `;`, or `"`).
    /// * `content` — The raw text to embed and index.
    ///
    /// # Returns
    ///
    /// The `id` of the stored entry (either the one you passed or the
    /// pre-existing one if the content was already indexed).
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Embedding`] if the model fails to process the text,
    /// or [`RagError::Database`] if `LanceDB` rejects the insert.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let id = rag.ingest("doc-42", "Some text to index.").await?;
    /// ```
    pub async fn ingest(&self, id: &str, content: &str) -> Result<String, RagError> {
        let embeddings = self.embedder.embed(&[content]).await?;
        let vector = embeddings.into_iter().next().ok_or_else(|| {
            RagError::Embedding("embedder returned zero vectors for a single input".into())
        })?;
        Ok(self.db.post(id, content, vector).await?)
    }

    /// Embed a batch of documents and store them in a single call.
    ///
    /// Each entry is a `(id, content)` pair. Content-based deduplication is
    /// applied per entry.
    ///
    /// # Arguments
    ///
    /// * `entries` — A slice of `(id, content)` tuples.
    ///
    /// # Returns
    ///
    /// The list of ids that were actually stored (potentially shorter than the
    /// input if some content already existed).
    ///
    /// # Errors
    ///
    /// The whole batch is processed sequentially. If any single entry fails,
    /// the error is returned immediately and earlier entries remain in the
    /// database.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let docs = vec![
    ///     ("a".into(), "First document.".into()),
    ///     ("b".into(), "Second document.".into()),
    /// ];
    /// let ids = rag.ingest_batch(&docs).await?;
    /// ```
    pub async fn ingest_batch(
        &self,
        entries: &[(String, String)],
    ) -> Result<Vec<String>, RagError> {
        let contents: Vec<&str> = entries.iter().map(|(_, c)| c.as_str()).collect();
        let embeddings = self.embedder.embed(&contents).await?;

        let mut ids = Vec::with_capacity(entries.len());
        for (i, (id, content)) in entries.iter().enumerate() {
            let vector = embeddings[i].clone();
            ids.push(self.db.post(id, content, vector).await?);
        }
        Ok(ids)
    }

    /// Embed `query` and return the `limit` most semantically similar entries.
    ///
    /// # Arguments
    ///
    /// * `query` — Natural-language search query.
    /// * `limit` — Maximum number of results to return.
    ///
    /// # Returns
    ///
    /// A vector of [`Entry`] items sorted by relevance (most similar first).
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Embedding`] if the query cannot be embedded, or
    /// [`RagError::Database`] if the ANN search fails.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let results = rag.search("machine learning", 5).await?;
    /// ```
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Entry>, RagError> {
        let embeddings = self.embedder.embed(&[query]).await?;
        let query_vector = &embeddings[0];
        Ok(self.db.get(query_vector, limit).await?)
    }

    /// Remove an entry by its id.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Database`] (via [`VecDbError::NotFound`]) if the
    /// id does not exist.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// rag.delete("doc-42").await?;
    /// ```
    pub async fn delete(&self, id: &str) -> Result<(), RagError> {
        Ok(self.db.delete(id).await?)
    }

    /// Return the total number of entries in the index.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::Database`] if the count query fails.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// println!("Total entries: {}", rag.entry_count().await?);
    /// ```
    pub async fn entry_count(&self) -> Result<usize, RagError> {
        Ok(self.db.entry_count().await?)
    }
}
