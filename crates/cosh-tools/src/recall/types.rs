//! Input and output types for the recall search tool.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for `recall_search`.
///
/// Accepts a pre-computed embedding vector alongside the original query
/// text so the tool itself doesn't need an embedding model dependency.
/// The harness (or calling agent) is responsible for producing the vector
/// via whatever embedding strategy is configured.
#[derive(Debug, Deserialize)]
pub struct RecallSearchInput {
    /// `LanceDB` database URI (local directory or cloud URI).
    pub db_uri: String,
    /// Name of the table inside the database to search.
    pub table_name: String,
    /// Dimension of the embedding vectors stored in the table.
    pub vector_dim: usize,
    /// The original query text (echoed back for agent context).
    pub query: String,
    /// Pre-computed embedding vector for the query.
    pub query_vector: Vec<f32>,
    /// Maximum number of results to return (default: 5).
    pub limit: Option<usize>,
}

/// A single entry returned by the recall search.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecallEntry {
    /// Unique identifier for the entry.
    pub id: String,
    /// Text content of the entry.
    pub content: String,
}

/// Output from `recall_search`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallOutput {
    /// The original query text (echoed back).
    pub query: String,
    /// Matching entries sorted by relevance (most similar first).
    pub results: Vec<RecallEntry>,
}
