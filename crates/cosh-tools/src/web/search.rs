//! Web search tool powered by Exa AI via MCP.
//!
//! Sends a JSON-RPC request to `mcp.exa.ai` and returns cleaned markdown
//! ready to be injected into an LLM context window.

use serde::{Deserialize, Serialize};

const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp";

/// Validated search parameters.
///
/// `num_results` is capped at 10; values above that are silently clamped.
#[derive(Debug, Clone, Serialize)]
pub struct SearchArgs {
    pub query: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "numResults")]
    pub num_results: u32,
}

impl SearchArgs {
    /// Returns an error prompt if the query is empty or num_results is zero,
    /// so the LLM can self-correct before the network call is made.
    fn validate(&self) -> Result<(), String> {
        if self.query == None && self.url == None {
            return Err("Invalid argument: `query` must not be empty. \
                 Provide a non-empty search string."
                .into());
        }
        if self.num_results == 0 {
            return Err("Invalid argument: `num_results` must be at least 1.".into());
        }
        Ok(())
    }

    fn clamped_results(&self) -> u32 {
        self.num_results.min(10)
    }
}

#[derive(Serialize)]
struct RpcRequest<T: Serialize> {
    jsonrpc: &'static str,
    id: u32,
    method: &'static str,
    params: RpcParams<T>,
}

#[derive(Serialize)]
struct RpcParams<T: Serialize> {
    name: &'static str,
    arguments: T,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<RpcResult>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcResult {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

/// Searches the web and returns cleaned markdown suitable for LLM context.
///
/// # Errors
///
/// - Returns a human-readable correction prompt on invalid arguments so the
///   caller (or LLM) can retry with fixed parameters.
/// - Returns an error string on network failure or an unexpected response shape.
pub async fn search(args: SearchArgs) -> Result<String, String> {
    args.validate()?;

    if let Some(url) = args.url {
        let req = reqwest::get(url)
            .await
            .map_err(|err| err.to_string())?
            .text()
            .await
            .map_err(|err| err.to_string())?;

        return Ok(req);
    }

    let body = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: RpcParams {
            name: "web_search_exa",
            arguments: SearchArgs {
                num_results: args.clamped_results(),
                ..args
            },
        },
    };

    let client = reqwest::Client::new();

    let raw = client
        .post(EXA_MCP_URL)
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    let json_str = raw
        .lines()
        .find(|l| l.starts_with("data: "))
        .map(|l| &l["data: ".len()..])
        .ok_or_else(|| "Unexpected response format: no SSE data line found.".to_string())?;

    let rpc: RpcResponse = serde_json::from_str(json_str)
        .map_err(|e| format!("Failed to parse response JSON: {e}"))?;

    if let Some(err) = rpc.error {
        return Err(format!("Exa MCP error: {}", err.message));
    }

    let content = rpc
        .result
        .ok_or_else(|| "Empty result from Exa MCP.".to_string())?
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .map(|b| clean(b.text))
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(content)
}

/// Strips tokens that waste context budget without adding information.
///
/// Removes `Published: N/A`, `Author: N/A`, and blank `Highlights:` headers
/// since they appear consistently in Exa responses but carry no signal.
fn clean(text: String) -> String {
    text.lines()
        .filter(|l| !matches!(l.trim(), "Published: N/A" | "Author: N/A" | "Highlights:"))
        .collect::<Vec<_>>()
        .join("\n")
}
