use super::super::error::ConnectorError;
use super::super::params::{Parameters, ResponseFormat, ToolDefinition};
use super::super::provider::{ProviderConfig, get_api_key};

use async_stream::stream;
use std::pin::Pin;
use tokio_stream::Stream;

#[derive(serde::Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_logprobs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[allow(clippy::struct_field_names)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ToolCallFunction,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ToolCallFunction {
    name: String,
    arguments: String,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

//  SSE streaming types

#[derive(serde::Deserialize)]
struct ChatChunkResponse {
    choices: Vec<ChunkChoice>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ChunkChoice {
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
}

struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Push new bytes and drain any complete SSE frames.
    /// Returns the `data:` payload of each frame.
    fn push_and_drain(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let raw: Vec<u8> = self.buf.drain(..=end + 1).collect();
            let text = String::from_utf8_lossy(&raw[..raw.len().saturating_sub(2)]);
            if let Some(data) = text.lines().find_map(|l| l.strip_prefix("data: ")) {
                frames.push(data.to_owned());
            }
        }
        frames
    }
}

// Embedding types

#[derive(serde::Serialize)]
struct EmbeddingRequest {
    model: String,
    input: String,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(serde::Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

fn build_messages(prompt: &str, system_prompt: Option<&str>) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: system.to_string(),
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: prompt.to_string(),
    });
    messages
}

fn build_chat_request(
    model: String,
    messages: Vec<ChatMessage>,
    params: &Parameters,
    stream: bool,
) -> ChatRequest {
    ChatRequest {
        model,
        messages,
        stream: if stream { Some(true) } else { None },
        max_tokens: params.max_tokens,
        temperature: params.temperature,
        top_p: params.top_p,
        stop: params.stop.clone(),
        frequency_penalty: params.frequency_penalty,
        presence_penalty: params.presence_penalty,
        seed: params.seed,
        response_format: params.response_format.clone(),
        logprobs: params.logprobs,
        top_logprobs: params.top_logprobs,
        tools: params.tools.clone(),
        tool_choice: params.tool_choice.clone(),
        user: params.user.clone(),
    }
}

/// Send a non-streaming chat completion request.
///
/// Resolves the API key and model, builds the request body, and parses the
/// OpenAI-compatible JSON response.
pub(crate) async fn chat(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<String, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    let messages = build_messages(prompt, system_prompt);
    let request = build_chat_request(model, messages, params, false);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let response_text = send_request(config, &api_key, &url, &request).await?;
    let chat_response: ChatResponse = serde_json::from_str(&response_text)?;
    let first_choice = chat_response
        .choices
        .first()
        .ok_or(ConnectorError::NoChoices)?;
    first_choice
        .message
        .content
        .clone()
        .ok_or(ConnectorError::NoContent)
}

/// Send a streaming chat completion request.
///
/// Returns an async [`Stream`] of content chunks parsed from SSE frames.
/// The stream terminates with `StreamTerminated` if the connection closes
/// without a `[DONE]` signal.
pub(crate) async fn chat_stream(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<Pin<Box<dyn Stream<Item = Result<String, ConnectorError>> + Send>>, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    let messages = build_messages(prompt, system_prompt);
    let request = build_chat_request(model, messages, params, true);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let response = send_request_stream(config, &api_key, &url, &request).await?;

    let buf = SseBuffer::new();

    Ok(Box::pin(stream! {
        let mut response = response;
        let mut buf = buf;
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    yield Err(ConnectorError::Network(e.to_string()));
                    return;
                }
            };
            for data in buf.push_and_drain(&chunk) {
                if data == "[DONE]" {
                    return;
                }
                match serde_json::from_str::<ChatChunkResponse>(&data) {
                    Ok(ccr) => {
                        if let Some(content) = ccr.choices.first()
                            .and_then(|c| c.delta.content.as_deref())
                            .filter(|c| !c.is_empty())
                        {
                            yield Ok(content.to_owned());
                        }
                    }
                    Err(e) => {
                        yield Err(ConnectorError::Deserialization(e.to_string()));
                        return;
                    }
                }
            }
        }
        // Stream ended without [DONE]
        yield Err(ConnectorError::StreamTerminated);
    }))
}

/// Send an embedding request and return the embedding vector.
///
/// Resolves the API key, builds the request body, and extracts the first
/// embedding from the OpenAI-compatible response.
pub(crate) async fn embed(
    config: &ProviderConfig,
    params: &Parameters,
    input: &str,
) -> Result<Vec<f32>, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| "text-embedding-3-small".to_string());

    let request = EmbeddingRequest {
        model,
        input: input.to_string(),
    };
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/embeddings");

    let response_text = send_request(config, &api_key, &url, &request).await?;
    let embed_response: EmbeddingResponse = serde_json::from_str(&response_text)?;
    let first_data = embed_response
        .data
        .first()
        .ok_or(ConnectorError::NoEmbeddings)?;
    Ok(first_data.embedding.clone())
}

async fn send_request(
    config: &ProviderConfig,
    api_key: &str,
    url: &str,
    body: &(impl serde::Serialize + Sync),
) -> Result<String, ConnectorError> {
    let response = send_request_stream(config, api_key, url, body).await?;
    Ok(response.text().await?)
}

async fn send_request_stream(
    config: &ProviderConfig,
    api_key: &str,
    url: &str,
    body: &(impl serde::Serialize + Sync),
) -> Result<reqwest::Response, ConnectorError> {
    let client = reqwest::Client::new();
    let mut request_builder = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json");

    if config.needs_extra_headers {
        request_builder = request_builder
            .header("HTTP-Referer", "https://localhost")
            .header("X-Title", "provider");
    }

    let json_body = serde_json::to_string(body)?;
    let response = request_builder.body(json_body).send().await?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unable to read error response".to_string());
        let error_msg = format!("HTTP {} - {}", status.as_u16(), error_text);
        log::error!("HTTP Error captured: {error_msg}");
        return Err(ConnectorError::HttpError {
            status: status.as_u16(),
            body: error_text,
        });
    }

    Ok(response)
}
