//! Read-only vector search tool backed by `LanceDB`.
//!
//! [`Recall`] provides a semantic search operation that looks up entries
//! in a `LanceDB` vector store by a pre-computed embedding vector. The tool
//! is **agnostic** — it has no hardcoded database path, table name, or
//! embedding model. All parameters are passed at call time.
//!
//! # Design decisions
//!
//! - **Read-only**: This tool never writes to the database. It only performs
//!   similarity search queries.
//! - **External embedding**: The query vector must be pre-computed by the
//!   caller (the harness). This keeps the tool free of embedding-model
//!   dependencies and lets the harness decide the embedding strategy.
//! - **Configurable per call**: The database URI, table name, vector
//!   dimension, query, and result limit are all passed as parameters.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::recall::{Recall, types::RecallSearchInput};
//!
//! let recall = Recall::new();
//! let output = recall.search(&RecallSearchInput {
//!     db_uri: "/tmp/my_db".into(),
//!     table_name: "docs".into(),
//!     vector_dim: 384,
//!     query: "What is RAG?".into(),
//!     query_vector: vec![0.1, 0.2, /* ... */],
//!     limit: Some(5),
//! }).await?;
//! ```

pub mod search;
#[cfg(test)]
mod test;
pub mod types;

pub use search::search;
pub use types::{RecallEntry, RecallOutput, RecallSearchInput};

use crate::ToolDescription;

/// Read-only vector search tool backed by `LanceDB`.
///
/// Stateless wrapper — all configuration is passed via
/// [`RecallSearchInput`] at call time.
///
/// Use [`with_description`](Self::with_description) to append custom context
/// (e.g. from build patterns) below the default description.
pub struct Recall {
    /// MCP Tool description for `search`.
    pub description_search: ToolDescription,
}

impl Default for Recall {
    fn default() -> Self {
        Self::new()
    }
}

/// Default description for `recall_search` — short, self-contained, standalone.
const DEFAULT_DESCRIPTION: &str = concat!(
    "Search a vector database for entries semantically similar to a query. ",
    "Returns matching entries with their IDs and content. ",
    "Read-only — never writes to the database. ",
    "Use this to retrieve relevant context from stored knowledge.\n",
    "The query vector must be pre-computed by an embedding model ",
    "and passed alongside the original query text."
);

impl Recall {
    /// Create a new `Recall` with the default tool description.
    ///
    /// Use [`with_description`](Self::with_description) to append custom
    /// context below the default.
    #[must_use]
    pub fn new() -> Self {
        Self::build(DEFAULT_DESCRIPTION)
    }

    /// Append custom context to the tool description.
    ///
    /// The given `suffix` is appended after the default description,
    /// separated by a blank line. Use this to inject build-pattern
    /// context (e.g. database path, knowledge domain) that the agent
    /// needs to decide when to call the tool.
    ///
    /// If not called, only the default description is used.
    #[must_use]
    pub fn with_description(self, suffix: impl Into<String>) -> Self {
        let extra = suffix.into();
        if extra.is_empty() {
            return Self::build(DEFAULT_DESCRIPTION);
        }
        let full = format!("{DEFAULT_DESCRIPTION}\n\n{extra}");
        Self::build(&full)
    }

    /// Build a `Recall` with a specific description string.
    fn build(description: &str) -> Self {
        Self {
            description_search: Self::make_tool_description(description),
        }
    }

    /// Construct the full `ToolDescription` JSON value.
    fn make_tool_description(description: &str) -> ToolDescription {
        serde_json::json!({
            "name": "recall_search",
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "db_uri": {
                        "type": "string",
                        "description": "Database URI (local directory or cloud URI)"
                    },
                    "table_name": {
                        "type": "string",
                        "description": "Name of the table inside the database to search"
                    },
                    "vector_dim": {
                        "type": "integer",
                        "description": "Dimension of the embedding vectors stored in the table"
                    },
                    "query": {
                        "type": "string",
                        "description": "The original query text to search for"
                    },
                    "query_vector": {
                        "type": "array",
                        "items": { "type": "number", "format": "float" },
                        "description": "Pre-computed embedding vector for the query"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 5)",
                        "default": 5
                    }
                },
                "required": ["db_uri", "table_name", "vector_dim", "query", "query_vector"]
            }
        })
    }

    /// Search a `LanceDB` vector store for entries similar to the query.
    ///
    /// See [`search`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the database connection or search fails.
    pub async fn search(&self, input: &RecallSearchInput) -> Result<RecallOutput, String> {
        search::search(input).await
    }
}
