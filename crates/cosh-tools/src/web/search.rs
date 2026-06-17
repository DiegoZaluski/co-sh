//! Web search and content extraction via Exa AI, with local fallback.
//!
//! # Search
//! Requires `EXA_API_KEY` to use the Exa REST API (`api.exa.ai/search`).
//! Falls back to the free MCP endpoint (`mcp.exa.ai/mcp`) when the key
//! is absent.
//!
//! # Content extraction
//! Extracted via Exa Contents API or MCP `web_fetch_exa`. When both fail,
//! falls back to `rs_trafilatura` for local extraction.
//! Extraction priority is controlled by the `EXA_FIRST` constant.
use serde::{Deserialize, Serialize};

const EXA_API: &str = "https://api.exa.ai/search";
const EXA_CONTENTS: &str = "https://api.exa.ai/contents";
const EXA_MCP: &str = "https://mcp.exa.ai/mcp";

/// When true, Exa-based content extraction is tried before local
/// `rs_trafilatura` for URL fetching. Set to `false` to reverse the order.
const EXA_FIRST: bool = true;

/// Parameters for a web search or URL fetch.
///
/// At least one of `query` or `url` must be provided. If both are given,
/// `url` takes priority and the page content is returned instead.
/// `num_results` is silently capped at 10.
#[derive(Debug, Clone, Serialize)]
pub struct SearchArgs {
    /// Search query sent to Exa. Ignored when `url` is present.
    pub query: Option<String>,
    /// A specific URL to fetch and extract clean text from.
    pub url: Option<String>,
    /// Number of results to request (max 10). Ignored when `url` is present.
    #[serde(rename = "numResults")]
    pub num_results: u32,
}

impl SearchArgs {
    fn validate(&self) -> Result<(), &'static str> {
        if self.query.is_none() && self.url.is_none() {
            return Err("query or url required");
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

#[derive(Serialize)]
struct ContentsReq {
    urls: Vec<String>,
    text: bool,
}

#[derive(Deserialize)]
struct ContentsRes {
    results: Vec<ContentsResult>,
}

#[derive(Deserialize)]
struct ContentsResult {
    #[serde(default)]
    text: String,
}

#[derive(Serialize)]
struct McpReq {
    jsonrpc: &'static str,
    id: u32,
    method: &'static str,
    params: McpParams,
}

#[derive(Serialize)]
struct McpParams {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct McpRes {
    result: Option<McpResult>,
    error: Option<McpErr>,
}

#[derive(Deserialize)]
struct McpResult {
    content: Vec<McpBlock>,
}

#[derive(Deserialize)]
struct McpBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize)]
struct McpErr {
    message: String,
}

/// Search the web or fetch a URL, returning clean markdown for LLM context.
///
/// When `url` is provided, the page is fetched and extracted via Exa
/// Contents API / MCP `web_fetch_exa`, falling back to `rs_trafilatura`
/// (order controlled by `EXA_FIRST`).
///
/// When `query` is provided, Exa search is used (REST API if
/// `EXA_API_KEY` is set, otherwise MCP `web_search_exa`).
///
/// # Errors
///
/// Returns `Err` if validation fails, the search/fetch fails, or all
/// fallback methods are exhausted.
pub async fn search(args: SearchArgs) -> Result<String, String> {
    args.validate()?;

    if let Some(url) = args.url {
        return fetch_url(&url).await;
    }

    let q = args.query.as_deref().unwrap_or("");
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

async fn fetch_url(url: &str) -> Result<String, String> {
    let try_exa = || async {
        if let Ok(key) = std::env::var("EXA_API_KEY")
            && !key.trim().is_empty()
        {
            return rest_contents(url, &key).await;
        }
        mcp_call("web_fetch_exa", serde_json::json!({ "url": url })).await
    };

    let try_local = || async {
        let client = reqwest::Client::builder()
            .user_agent("Cosh/0.1")
            .build()
            .map_err(|e| e.to_string())?;
        let html = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("fetch: {e}"))?
            .text()
            .await
            .map_err(|e| format!("read: {e}"))?;
        let extracted = rs_trafilatura::extract(&html).map_err(|e| format!("extract: {e}"))?;
        Ok(extracted.content_text)
    };

    if EXA_FIRST {
        match try_exa().await {
            Ok(r) => Ok(r),
            Err(_) => try_local().await,
        }
    } else {
        match try_local().await {
            Ok(r) => Ok(r),
            Err(_) => try_exa().await,
        }
    }
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

async fn rest_contents(url: &str, key: &str) -> Result<String, String> {
    let body = ContentsReq {
        urls: vec![url.to_string()],
        text: true,
    };

    let resp = reqwest::Client::new()
        .post(EXA_CONTENTS)
        .header("Content-Type", "application/json")
        .header("x-api-key", key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("contents: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("contents {status}: {text}"));
    }

    let data: ContentsRes = resp.json().await.map_err(|e| format!("parse: {e}"))?;

    data.results
        .into_iter()
        .next()
        .map(|r| r.text)
        .ok_or_else(|| "no content".into())
}

async fn mcp_call(tool: &str, args: serde_json::Value) -> Result<String, String> {
    let body = McpReq {
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: McpParams {
            name: tool.to_string(),
            arguments: args,
        },
    };

    let raw = reqwest::Client::new()
        .post(EXA_MCP)
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("mcp: {e}"))?
        .text()
        .await
        .map_err(|e| format!("mcp body: {e}"))?;

    let line = raw
        .lines()
        .find(|l| l.starts_with("data: "))
        .map(|l| &l["data: ".len()..])
        .ok_or("no sse data")?;

    let res: McpRes = serde_json::from_str(line).map_err(|e| format!("bad json: {e}"))?;

    if let Some(e) = res.error {
        return Err(format!("mcp err: {}", e.message));
    }

    Ok(res
        .result
        .ok_or("empty mcp result")?
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}

fn strip_na(s: &str) -> String {
    s.lines()
        .filter(|l| !matches!(l.trim(), "Published: N/A" | "Author: N/A" | "Highlights:"))
        .collect::<Vec<_>>()
        .join("\n")
}
