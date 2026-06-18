//! Web search via Exa AI, with local fallback.
//!
//! Requires `EXA_API_KEY` to use the Exa REST API (`api.exa.ai/search`).
//! Falls back to the free MCP endpoint (`mcp.exa.ai/mcp`) when the key
//! is absent.
use serde::{Deserialize, Serialize};

use super::fetch::{mcp_call, strip_na};

const EXA_API: &str = "https://api.exa.ai/search";

/// Parameters for a web search.
///
/// `query` must be provided. `num_results` is silently capped at 10.
#[derive(Debug, Clone, Serialize)]
pub struct SearchArgs {
    /// Search query sent to Exa.
    pub query: String,
    /// Number of results to request (max 10).
    #[serde(rename = "numResults")]
    pub num_results: u32,
}

impl SearchArgs {
    fn validate(&self) -> Result<(), &'static str> {
        if self.query.is_empty() {
            return Err("query required");
        }
        if self.num_results == 0 {
            return Err("num_results must be >= 1");
        }
        Ok(())
    }

    fn num(&self) -> u32 {
        self.num_results.min(10)
    }
}

#[derive(Serialize)]
struct ApiReq {
    query: String,
    #[serde(rename = "numResults")]
    num: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    contents: Contents,
}

#[derive(Serialize)]
struct Contents {
    highlights: bool,
}

#[derive(Deserialize)]
struct ApiRes {
    results: Vec<ApiResult>,
}

#[derive(Deserialize)]
struct ApiResult {
    title: Option<String>,
    url: String,
    #[serde(default)]
    highlights: Vec<String>,
}

/// Search the web, returning clean markdown for LLM context.
///
/// Exa search is used (REST API if `EXA_API_KEY` is set, otherwise MCP
/// `web_search_exa`).
///
/// # Errors
///
/// Returns `Err` if validation fails, the search fails, or all fallback
/// methods are exhausted.
pub async fn search(args: SearchArgs) -> Result<String, String> {
    args.validate()?;

    let q = &args.query;
    let n = args.num();

    if let Ok(key) = std::env::var("EXA_API_KEY")
        && !key.trim().is_empty()
        && let Ok(res) = rest_search(q, n, &key).await
    {
        return Ok(res);
    }

    mcp_call(
        "web_search_exa",
        serde_json::json!({
            "query": q,
            "numResults": n,
        }),
    )
    .await
    .map(|c| strip_na(&c))
}

async fn rest_search(query: &str, num: u32, key: &str) -> Result<String, String> {
    let body = ApiReq {
        query: query.to_string(),
        num,
        kind: "auto",
        contents: Contents { highlights: true },
    };

    let resp = reqwest::Client::new()
        .post(EXA_API)
        .header("Content-Type", "application/json")
        .header("x-api-key", key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("api: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("api {status}: {text}"));
    }

    let data: ApiRes = resp.json().await.map_err(|e| format!("parse: {e}"))?;

    if data.results.is_empty() {
        return Err("no results".into());
    }

    Ok(data
        .results
        .iter()
        .map(|r| {
            let mut parts = vec![];
            if let Some(t) = &r.title {
                parts.push(format!("Title: {t}"));
            }
            parts.push(format!("URL: {}", r.url));
            if !r.highlights.is_empty() {
                parts.push("Highlights:".into());
                for h in &r.highlights {
                    parts.push(format!("  - {h}"));
                }
            }
            parts.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n"))
}
