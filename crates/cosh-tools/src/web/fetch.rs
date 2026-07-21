//! URL content extraction via Exa AI and/or local extraction.
//!
//! The `EXA_FIRST` constant controls extraction priority:
//! - `true` (default): tries Exa Contents API / MCP `web_fetch_exa` first,
//!   falls back to `rs_trafilatura`.
//! - `false`: tries `rs_trafilatura` first, falls back to Exa.
use serde::{Deserialize, Serialize};

const EXA_CONTENTS: &str = "https://api.exa.ai/contents";
const EXA_MCP: &str = "https://mcp.exa.ai/mcp";

const EXA_FIRST: bool = false;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WebFetch {
    pub url: String,
}

/// Fetch a URL, returning clean markdown for LLM context.
///
/// Content is extracted via Exa Contents API / MCP `web_fetch_exa`,
/// falling back to `rs_trafilatura` (order controlled by `EXA_FIRST`).
///
/// # Errors
///
/// Returns `Err` if the fetch fails or all fallback methods are exhausted.
pub async fn fetch(fetch: &WebFetch) -> Result<String, String> {
    fetch_url(&fetch.url).await
}

async fn fetch_url(url: &str) -> Result<String, String> {
    let try_exa = || async {
        if let Ok(key) = std::env::var("EXA_API_KEY")
            && !key.trim().is_empty()
        {
            return rest_contents(url, &key).await;
        }
        // The Exa MCP `web_fetch_exa` tool expects `urls` as an array,
        // not a single `url` string. Match the format used by `rest_contents`.
        mcp_call("web_fetch_exa", serde_json::json!({ "urls": [url] })).await
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

        // rs_trafilatura spams stderr with debug messages that corrupt the TUI.
        // Redirect stderr to the OS null device during extraction.
        #[allow(unused_unsafe)]
        let extract_result = {
            // Open null device to suppress stderr (NUL on Windows, /dev/null on Unix)
            #[cfg(windows)]
            let null_cstr = c"NUL";
            #[cfg(unix)]
            let null_cstr = c"/dev/null";
            let null_fd = unsafe { libc::open(null_cstr.as_ptr(), libc::O_WRONLY) };
            if null_fd < 0 {
                // Fallback: no suppression
                rs_trafilatura::extract(&html)
            } else {
                let saved_stderr = unsafe { libc::dup(2) };
                unsafe { libc::dup2(null_fd, 2) };
                unsafe { libc::close(null_fd) };

                let result = rs_trafilatura::extract(&html);

                // Restore stderr
                unsafe { libc::dup2(saved_stderr, 2) };
                unsafe { libc::close(saved_stderr) };

                result
            }
        };

        let extracted = extract_result.map_err(|e| format!("extract: {e}"))?;

        // rs_trafilatura's extraction_quality heuristic (0.0–1.0) tells us
        // whether the content was successfully extracted or fell back to the
        // raw body (e.g. for JSON API responses). Low quality means we
        // should fall through to the Exa backend.
        if extracted.extraction_quality < 0.75_f64 {
            return Err("low extraction quality".to_string());
        }

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
