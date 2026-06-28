//! Shared-state wrapper for web tool operations.
//!
//! [`Web`] holds configuration — such as result count — so callers don't
//! have to construct [`WebFetch`] / [`WebSearch`] on every invocation.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::web::Web;
//!
//! let web = Web::new().num_results(5);
//! web.fetch("https://example.com").await;
//! web.search("rust programming").await;
//! ```

pub mod fetch;
pub mod search;
#[cfg(test)]
mod test;

pub use fetch::{WebFetch, fetch};
pub use search::{WebSearch, search};

use crate::ToolDescription;

/// Shared-state wrapper for web tool operations.
///
/// Use the builder method [`num_results`](Self::num_results) after
/// [`new`](Self::new) to configure search result count, then call the
/// operation methods directly.
pub struct Web {
    num_results: u32,

    /// MCP Tool description for `fetch`.
    pub description_fetch: ToolDescription,
    /// MCP Tool description for `search`.
    pub description_search: ToolDescription,
}

impl Default for Web {
    fn default() -> Self {
        Self::new()
    }
}

impl Web {
    /// Create a new `Web` with default search result count (10).
    #[must_use]
    pub fn new() -> Self {
        Self {
            num_results: 10,
            description_fetch: serde_json::json!({
                "name": "web_fetch",
                "description": concat!(
                    "Fetch a URL and return its content as clean, readable markdown. ",
                    "Strips navigation, scripts, and boilerplate HTML to produce ",
                    "LLM-friendly text. Falls back through multiple extraction ",
                    "methods if the primary one fails."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The full HTTP or HTTPS URL to fetch"
                        }
                    },
                    "required": ["url"]
                }
            }),
            description_search: serde_json::json!({
                "name": "web_search",
                "description": concat!(
                    "Search the web using a text query and return results as clean ",
                    "markdown. Each result includes a title, snippet, and URL. ",
                    "Use this to find current information, documentation, or ",
                    "answers that are not available in the local codebase."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query string"
                        }
                    },
                    "required": ["query"]
                }
            }),
        }
    }

    /// Set the number of search results (capped at 10 by the underlying API).
    #[must_use]
    pub fn num_results(mut self, n: u32) -> Self {
        self.num_results = n;
        self
    }

    /// Fetch a URL, returning clean markdown for LLM context.
    ///
    /// See [`fetch`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the fetch fails or all fallback methods are exhausted.
    pub async fn fetch(&self, url: &str) -> Result<String, String> {
        fetch(&WebFetch, url).await
    }

    /// Search the web, returning clean markdown for LLM context.
    ///
    /// See [`search`] for details.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the query is empty, validation fails, or the search
    /// itself fails.
    pub async fn search(&self, query: &str) -> Result<String, String> {
        search(
            &WebSearch {
                num_results: self.num_results,
            },
            query,
        )
        .await
    }
}
