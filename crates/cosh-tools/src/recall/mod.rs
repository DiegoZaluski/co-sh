//! Read-only vector search tool backed by `LanceDB`.
//!
//! [`Recall`] provides a semantic search operation that looks up entries
//! in a `LanceDB` vector store. The tool is a **low-level building block**
//! — it accepts a pre-computed embedding vector directly and returns matching
//! entries. The harness (`CoshTools` dispatch) wraps this with embedding logic
//! so the LLM only sees a simple `{ db_name, query, limit }` interface.
//!
//! # Design decisions
//!
//! - **Read-only**: This tool never writes to the database. It only performs
//!   similarity search queries.
//! - **Embedding-agnostic**: The caller (harness) is responsible for embedding
//!   the query text. The tool itself only handles the vector search.
//! - **Low-level by design**: The `search()` method expects a fully populated
//!   [`RecallSearchInput`] with database URI, table name, dimension, query,
//!   and pre-computed vector. The harness provides the high-level interface.
//!
//! # High-level usage (harness dispatch)
//!
//! The LLM calls `recall_search` with just:
//! - `db_name` — which database to query
//! - `query` — natural-language search text (embedded by the harness)
//! - `limit` — how many results to return (optional, default 5)
//!
//! The harness resolves the DB from its registry, creates the appropriate
//! embedder (local fastembed or cloud provider), embeds the query, and
//! calls [`Recall::search`] with the complete parameters.
//!
//! # Low-level example (direct usage, e.g. in tests)
//!
//! ```ignore
//! use cosh_tools::recall::{Recall, types::RecallSearchInput};
//!
//! let recall = Recall::new();
//! let output = recall.search(&RecallSearchInput {
//!     db_uri: "/tmp/my_db".into(),
//!     table_name: "vectors".into(),
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
    "Search an active knowledge base for entries semantically similar to your query. ",
    "Returns matching entries with their IDs and content. ",
    "Read-only — never writes to the database. ",
    "Use this to retrieve relevant context from stored knowledge.\n",
    "The query will be automatically embedded using the target database's configured ",
    "embedding model. Only the database name and query text are needed."
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

    /// Rebuild the tool description with a new suffix.
    ///
    /// The given `suffix` replaces whatever was previously set via
    /// [`with_description`](Self::with_description). If `suffix` is empty
    /// the description reverts to the default (no DB context).
    pub fn rebuild_description(&mut self, suffix: impl Into<String>) {
        let extra = suffix.into();
        let full = if extra.is_empty() {
            DEFAULT_DESCRIPTION.to_string()
        } else {
            format!("{DEFAULT_DESCRIPTION}\n\n{extra}")
        };
        self.description_search = Self::make_tool_description(&full);
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
                    "db_name": {
                        "type": "string",
                        "description": "Name of the database to search (must be one of the active databases listed above)"
                    },
                    "query": {
                        "type": "string",
                        "description": "The search query text — will be automatically embedded using the DB's configured model"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 5)",
                        "default": 5
                    }
                },
                "required": ["db_name", "query"]
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
