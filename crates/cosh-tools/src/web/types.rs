use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for `web_fetch`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WebFetchInput {
    /// The full HTTP or HTTPS URL to fetch.
    pub url: String,
}

/// Parameters for `web_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WebSearchInput {
    /// The search query string.
    pub query: String,
}
