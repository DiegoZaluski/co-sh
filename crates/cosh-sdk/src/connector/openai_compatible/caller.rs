use super::super::common::{SseBuffer, send_get_request, send_request, send_request_stream};
use super::super::error::ConnectorError;
use super::super::output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
use super::super::params::{Parameters, ResponseFormat, ToolDefinition};
use super::super::provider::{ProviderConfig, get_api_key};

use async_stream::stream;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

// Public ChatMessage from params module with native tool-call support.
use super::super::params::{
    ChatMessage as ApiChatMessage, system_message, user_message as pub_user_message,
};

#[derive(serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ApiChatMessage>,
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

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ApiErrorResponse {
    error: ApiErrorDetail,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct ApiErrorDetail {
    message: String,
    #[serde(default)]
    r#type: Option<String>,
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
    /// Streaming tool call deltas (OpenAI-compatible API).
    /// Each chunk may carry a partial tool call for one or more indices.
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallChunk>>,
}

/// One element inside `delta.tool_calls` in a streaming SSE frame.
/// Fields are absent on all chunks except the first one for a given index.
#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ToolCallChunk {
    #[serde(default)]
    index: Option<i32>,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    function: Option<ToolCallFunctionChunk>,
}

#[derive(serde::Deserialize)]
struct ToolCallFunctionChunk {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Accumulated state for one tool call received over multiple SSE chunks.
#[derive(Default, Debug)]
struct PendingToolCall {
    /// The `tool_call_id` from the API's native mechanism.
    id: String,
    name: String,
    arguments: String,
}

/// Render an accumulated tool call as inline JSON text so the harness
/// extractor can detect and validate it.
fn pending_tool_call_to_json(tc: &PendingToolCall) -> String {
    let args: serde_json::Value = serde_json::from_str(&tc.arguments)
        .unwrap_or_else(|_| serde_json::Value::String(tc.arguments.clone()));
    serde_json::json!({"name": tc.name, "arguments": args, "id": tc.id}).to_string()
}

/// Drain accumulated tool calls and return synthetic [`StreamChunk`] items
/// with inline JSON text that the harness extractor can parse.
fn flush_tool_calls(
    pending: &mut Vec<PendingToolCall>,
    last_raw: &mut Option<String>,
) -> Vec<StreamChunk> {
    let raw = last_raw.take().unwrap_or_default();
    let mut out = Vec::new();
    for tc in pending.drain(..) {
        if tc.name.is_empty() {
            continue;
        }
        out.push(StreamChunk {
            raw: raw.clone(),
            token: pending_tool_call_to_json(&tc),
            finish_reason: Some("tool_calls".to_string()),
        });
    }
    out
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

fn build_messages(prompt: &str, system_prompt: Option<&str>) -> Vec<ApiChatMessage> {
    let mut messages = Vec::new();
    if let Some(system) = system_prompt {
        messages.push(ApiChatMessage {
            role: "system".to_string(),
            content: Some(system.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages.push(pub_user_message(prompt));
    messages
}

/// Build the full messages array including system message, conversation
/// history, and tool results with native tool-call formatting.
pub fn build_full_messages(system: &str, messages: &[ApiChatMessage]) -> Vec<ApiChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(system_message(system));
    out.extend_from_slice(messages);
    out
}

/// Shared SSE processing loop for streaming chat completions.
///
/// Handles parsing SSE frames, accumulating tool call deltas,
/// yielding text tokens, and flushing tool calls on completion.
/// Used by both [`chat_stream`] and [`chat_stream_with_messages`].
fn process_sse_response(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, ConnectorError>> + Send>> {
    let buf = SseBuffer::new();

    Box::pin(stream! {
        let mut response = response;
        let mut buf = buf;
        // Accumulate streaming tool calls across chunks.
        let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();
        // Keep the raw text of the last non-DONE data frame so we can
        // use it when yielding synthetic tool-call tokens.
        let mut last_raw: Option<String> = None;
        loop {
            let chunk = match tokio::time::timeout(Duration::from_secs(30), response.chunk()).await {
                Ok(Ok(Some(c))) => {
                    c
                }
                Ok(Ok(None)) => {
                    break;
                }
                Ok(Err(e)) => {
                    yield Err(ConnectorError::Network(e.to_string()));
                    return;
                }
                Err(_) => {
                    yield Err(ConnectorError::Network("stream timed out after 30s".to_string()));
                    return;
                }
            };
            for data in buf.push_and_drain(&chunk) {
                if data == "[DONE]" {
                    // Flush any accumulated tool calls before ending.
                    for chunk in flush_tool_calls(&mut pending_tool_calls, &mut last_raw) {
                        yield Ok(chunk);
                    }
                    return;
                }
                match serde_json::from_str::<ChatChunkResponse>(&data) {
                    Ok(ccr) => {
                        last_raw = Some(data.clone());

                        // Accumulate tool call deltas from this chunk.
                        if let Some(tcs) = ccr.choices.first()
                            .and_then(|c| c.delta.tool_calls.as_ref())
                        {
                            for tc in tcs {
                                let idx = tc.index.unwrap_or(0_i32) as usize;
                                if idx >= pending_tool_calls.len() {
                                    pending_tool_calls.resize_with(idx + 1, Default::default);
                                }
                                let ptc = &mut pending_tool_calls[idx];
                                // Capture the tool_call_id from the first
                                // chunk for this index (it only appears on
                                // the first chunk of each tool call).
                                if let Some(ref id) = tc.id {
                                    id.clone_into(&mut ptc.id);
                                }
                                if let Some(ref func) = tc.function {
                                    if let Some(ref name) = func.name {
                                        name.clone_into(&mut ptc.name);
                                    }
                                    if let Some(ref args) = func.arguments {
                                        ptc.arguments.push_str(args);
                                    }
                                }
                            }
                        }

                        let token = ccr.choices.first()
                            .and_then(|c| c.delta.content.as_deref())
                            .unwrap_or("")
                            .to_owned();
                        let finish_reason = ccr.choices.first()
                            .and_then(|c| c.finish_reason.as_deref())
                            .map(String::from);
                        let should_stop = finish_reason.is_some();

                        // Emit text token if there is any.
                        if !token.is_empty() {
                            yield Ok(StreamChunk {
                                raw: data.clone(),
                                token,
                                finish_reason: None,
                            });
                        }

                        // If the model signalled a tool call via the API-level
                        // mechanism, convert the accumulated tool calls to inline
                        // JSON text so the harness extractor can parse them.
                        if should_stop && !pending_tool_calls.is_empty() {
                            for chunk in flush_tool_calls(&mut pending_tool_calls, &mut last_raw) {
                                yield Ok(chunk);
                            }
                            return;
                        }

                        if should_stop {
                            yield Ok(StreamChunk {
                                raw: data,
                                token: String::new(),
                                finish_reason,
                            });
                            return;
                        }
                    }
                    Err(e) => {
                        if let Ok(api_err) = serde_json::from_str::<ApiErrorResponse>(&data) {
                            yield Err(ConnectorError::HttpError {
                                status: 200,
                                body: api_err.error.message,
                            });
                            return;
                        }
                        yield Err(ConnectorError::Deserialization(format!(
                            "{e}. Raw chunk: {data}"
                        )));
                        return;
                    }
                }
            }
        }
        // Stream ended without [DONE]
        for chunk in flush_tool_calls(&mut pending_tool_calls, &mut last_raw) {
            yield Ok(chunk);
        }
        yield Err(ConnectorError::StreamTerminated);
    })
}

fn build_chat_request(
    model: String,
    messages: Vec<ApiChatMessage>,
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
pub async fn chat(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<ChatOutput, ConnectorError> {
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

    let auth = format!("Bearer {api_key}");
    let response_text =
        send_request(config, &url, &request, &[("Authorization", auth.as_str())]).await?;
    let chat_response: ChatResponse = match serde_json::from_str(&response_text) {
        Ok(r) => r,
        Err(e) => {
            if let Ok(api_err) = serde_json::from_str::<ApiErrorResponse>(&response_text) {
                return Err(ConnectorError::HttpError {
                    status: 200,
                    body: api_err.error.message,
                });
            }
            return Err(ConnectorError::Deserialization(format!(
                "{e}. Raw response: {response_text}"
            )));
        }
    };
    let first_choice = chat_response
        .choices
        .first()
        .ok_or(ConnectorError::NoChoices)?;
    let message = first_choice
        .message
        .content
        .clone()
        .ok_or(ConnectorError::NoContent)?;
    Ok(ChatOutput {
        raw: response_text,
        message,
    })
}

/// Send a streaming chat completion request.
///
/// Returns an async [`Stream`] of content chunks parsed from SSE frames.
/// The stream terminates with `StreamTerminated` if the connection closes
/// without a `[DONE]` signal.
pub async fn chat_stream(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<ChatStream, ConnectorError> {
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

    let auth = format!("Bearer {api_key}");
    log::debug!("chat_stream: sending request to {url}");
    let response =
        send_request_stream(config, &url, &request, &[("Authorization", auth.as_str())]).await?;
    log::debug!("chat_stream: got response status={}", response.status());

    let stream = process_sse_response(response);
    Ok(ChatStream::new(stream))
}

/// Send a streaming chat completion with a full messages array including
/// system, user, assistant (with `tool_calls`), and tool (with `tool_call_id`)
/// messages.
///
/// This is the native tool-calling path: tool definitions are sent via the
/// API's `tools` parameter, tool calls arrive as structured `delta.tool_calls`,
/// and tool results are returned as `role: "tool"` messages.
pub async fn chat_stream_with_messages(
    config: &ProviderConfig,
    params: &Parameters,
    system: &str,
    messages: &[ApiChatMessage],
) -> Result<ChatStream, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    let all_messages = build_full_messages(system, messages);
    let request = build_chat_request(model, all_messages, params, true);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let auth = format!("Bearer {api_key}");
    log::debug!("chat_stream_with_messages: sending request to {url}");
    let response =
        send_request_stream(config, &url, &request, &[("Authorization", auth.as_str())]).await?;

    let stream = process_sse_response(response);
    Ok(ChatStream::new(stream))
}

/// Send an embedding request and return the embedding vector.
///
/// Resolves the API key, builds the request body, and extracts the first
/// embedding from the OpenAI-compatible response.
pub async fn embed(
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

    let auth = format!("Bearer {api_key}");
    let response_text =
        send_request(config, &url, &request, &[("Authorization", auth.as_str())]).await?;
    let embed_response: EmbeddingResponse = serde_json::from_str(&response_text)?;
    let first_data = embed_response
        .data
        .first()
        .ok_or(ConnectorError::NoEmbeddings)?;
    Ok(first_data.embedding.clone())
}

#[derive(serde::Deserialize)]
struct ListModelsData {
    id: String,
}

#[derive(serde::Deserialize)]
struct ListModelsResponse {
    data: Vec<ListModelsData>,
}

/// Fetch the list of available models from the provider.
///
/// Resolves the API key, sends a GET to `{base_url}/models`, and parses
/// the OpenAI-compatible JSON response into a [`LsOutput`].
pub async fn list_models(
    config: &ProviderConfig,
    params: &Parameters,
) -> Result<LsOutput, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;

    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/models");

    let auth = format!("Bearer {api_key}");
    let response_text = send_get_request(config, &url, &[("Authorization", auth.as_str())]).await?;

    let list: ListModelsResponse = serde_json::from_str(&response_text)?;
    let models: Vec<ModelInfo> = list
        .data
        .into_iter()
        .map(|item| ModelInfo { id: item.id })
        .collect();

    Ok(LsOutput::new(response_text, models))
}
