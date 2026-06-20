//! URL content extraction via Exa AI and/or local extraction.
//!
//! The `EXA_FIRST` constant controls extraction priority:
//! - `true` (default): tries Exa Contents API / MCP `web_fetch_exa` first,
//!   falls back to `rs_trafilatura`.
//! - `false`: tries `rs_trafilatura` first, falls back to Exa.
use serde::{Deserialize, Serialize};

const EXA_CONTENTS: &str = "https://api.exa.ai/contents";
const EXA_MCP: &str = "https://mcp.exa.ai/mcp";

const EXA_FIRST: bool = true;

#[derive(Debug, Clone, Default)]
pub struct WebFetch;

/// Fetch a URL, returning clean markdown for LLM context.
///
/// Content is extracted via Exa Contents API / MCP `web_fetch_exa`,
/// falling back to `rs_trafilatura` (order controlled by `EXA_FIRST`).
///
/// # Errors
///
/// Returns `Err` if the fetch fails or all fallback methods are exhausted.
pub async fn fetch(fetch: &WebFetch, url: &str) -> Result<String, String> {
    let _ = fetch;
    fetch_url(url).await
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

pub(crate) async fn mcp_call(tool: &str, args: serde_json::Value) -> Result<String, String> {
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

pub(crate) fn strip_na(s: &str) -> String {
    s.lines()
        .filter(|l| !matches!(l.trim(), "Published: N/A" | "Author: N/A" | "Highlights:"))
        .collect::<Vec<_>>()
        .join("\n")
}
