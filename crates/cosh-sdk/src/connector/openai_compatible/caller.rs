use super::super::common::{
    SSE_CHUNK_TIMEOUT, SseBuffer, apply_provider_headers, retry_mid_stream, send_get_request,
    send_request, send_request_stream, send_with_retry, shared_client,
};
use super::super::error::ConnectorError;
use super::super::output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
use super::super::params::{Parameters, ResponseFormat, ToolCallMode, ToolDefinition};
use super::super::provider::{ProviderConfig, get_api_key, is_local_provider};
use crate::extract_action::NativeToolCall;

use async_stream::stream;
use std::pin::Pin;
use tokio_stream::Stream;

// Public ChatMessage from params module with native tool-call support.
use super::super::params::{
    ChatMessage as ApiChatMessage, system_message, user_message as pub_user_message,
};

/// DeepSeek/GLM-style thinking-mode toggle — `thinking: {type: "enabled"}`.
/// Sent ONLY for providers whose APIs use this exact wire shape (deepseek,
/// zai) alongside a reasoning effort: without the toggle, such deployments
/// ignore `reasoning_effort` entirely and the model never enters thinking
/// mode. Other providers must never receive it (OpenAI rejects unknown
/// top-level fields with 400).
#[derive(serde::Serialize)]
struct ThinkingToggle {
    #[serde(rename = "type")]
    kind: String,
}

/// NVIDIA NIM's thinking-mode opt-in body (`chat_template_kwargs`).
#[derive(serde::Serialize)]
struct ChatTemplateKwargs {
    thinking: bool,
}

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
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingToggle>,
    /// NVIDIA NIM hybrid-thinking opt-in — `chat_template_kwargs:
    /// {"thinking": true}`. NIM ignores `reasoning_effort` for its
    /// hybrid-thinking models (DeepSeek, Qwen, …) unless this is present;
    /// sent ONLY for the nvidia provider for the same unknown-field reason
    /// as `thinking` above.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
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

impl ApiErrorDetail {
    fn error_type(&self) -> Option<&str> {
        self.r#type.as_deref()
    }
}

/// Whether the provider's error type code names a transient server-side
/// condition worth retrying. Mirrors fantasy's `TransientStreamErrorTypes`.
/// Mid-stream SSE error events ride inside an already-successful 200
/// response, so the HTTP status code cannot signal retryability — the
/// providers classify the payload against this set.
fn is_transient_error_type(err: &ApiErrorDetail) -> bool {
    matches!(
        err.error_type(),
        Some(
            "server_error"
                | "internal_error"
                | "overloaded_error"
                | "api_error"
                | "rate_limit_error"
        )
    )
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
    /// Reasoning/thinking text delta (DeepSeek/GLM-style reasoning models).
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Alternate reasoning field name used by some OpenAI-compatible models.
    #[serde(default)]
    reasoning: Option<String>,
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

/// Drain accumulated tool calls and return structured [`StreamChunk`] items
/// carrying the provider's native call verbatim (no inline-JSON round trip:
/// the harness extractor validates the structured fields directly).
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
            token: String::new(),
            reasoning: String::new(),
            finish_reason: Some("tool_calls".to_string()),
            thinking_blocks: None,
            tool_call: Some(NativeToolCall {
                id: tc.id,
                name: tc.name,
                arguments: tc.arguments,
                thought_signature: String::new(),
            }),
            reset: false,
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
            thinking_blocks: None,
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

/// Drop the internal transport-only fields before the messages reach the
/// OpenAI-compatible wire format (which has no such fields — the API rejects
/// unknown keys with 400): the Gemini `thought_signature` on tool calls and
/// the Claude `thinking_blocks` on assistant messages. Both are internal
/// details only their native callers read.
fn strip_internal_fields(messages: &[ApiChatMessage]) -> Vec<ApiChatMessage> {
    let has_internal = messages.iter().any(|m| {
        m.thinking_blocks.is_some()
            || m.tool_calls
                .as_ref()
                .is_some_and(|tcs| tcs.iter().any(|tc| tc.thought_signature.is_some()))
    });
    // The common path (no cross-provider history) must not pay a full clone
    // of the message array just to strip nothing.
    if !has_internal {
        return messages.to_vec();
    }
    let mut out = Vec::with_capacity(messages.len());
    for msg in messages {
        let mut msg = msg.clone();
        msg.thinking_blocks = None;
        if let Some(tcs) = &mut msg.tool_calls {
            for tc in tcs {
                tc.thought_signature = None;
            }
        }
        out.push(msg);
    }
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
            let (frames, ended) =
                match tokio::time::timeout(SSE_CHUNK_TIMEOUT, response.chunk()).await {
                    Ok(Ok(Some(c))) => (buf.push_and_drain(&c), false),
                    Ok(Ok(None)) => {
                        // End-of-stream: emit any final frame that arrived
                        // without a trailing blank line (the SSE spec
                        // dispatches pending data on EOF) instead of
                        // silently dropping it.
                        (buf.flush(), true)
                    }
                    Ok(Err(e)) => {
                        yield Err(ConnectorError::Network(e.to_string()));
                        return;
                    }
                    Err(_) => {
                        yield Err(ConnectorError::Network(format!(
                            "stream timed out after {}s",
                            SSE_CHUNK_TIMEOUT.as_secs()
                        )));
                        return;
                    }
                };
            for data in frames {
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
                                let idx = tc.index.unwrap_or(0).unsigned_abs() as usize;
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
                        let reasoning = ccr.choices.first()
                            .and_then(|c| c.delta.reasoning.as_deref().or(c.delta.reasoning_content.as_deref()))
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
                                reasoning: String::new(),
                                finish_reason: None,
                                thinking_blocks: None,
                                tool_call: None,
                                reset: false,
                            });
                        }

                        // Emit a reasoning delta whenever the model streams one
                        // (even without text), so the TUI can show it live.
                        if !reasoning.is_empty() {
                            yield Ok(StreamChunk {
                                raw: data.clone(),
                                token: String::new(),
                                reasoning,
                                finish_reason: None,
                                thinking_blocks: None,
                                tool_call: None,
                                reset: false,
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
                                reasoning: String::new(),
                                finish_reason,
                                thinking_blocks: None,
                                tool_call: None,
                                reset: false,
                            });
                            return;
                        }
                    }
                    Err(e) => {
                        if let Ok(api_err) = serde_json::from_str::<ApiErrorResponse>(&data) {
                            let transient = is_transient_error_type(&api_err.error);
                            let err =
                                ConnectorError::classify_http(200, api_err.error.message);
                            yield Err(if transient {
                                err.mark_transient()
                            } else {
                                err
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
            if ended {
                break;
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
    provider: &str,
) -> ChatRequest {
    // Providers whose hybrid-thinking deployments ignore `reasoning_effort`
    // without an explicit toggle — scoped so no other OpenAI-compatible
    // backend ever receives the non-standard field.
    let thinking = match provider {
        "deepseek" | "zai" if params.reasoning_effort.is_some() => Some(ThinkingToggle {
            kind: "enabled".to_string(),
        }),
        _ => None,
    };
    // NVIDIA NIM needs a different opt-in shape for its hybrid-thinking
    // models (DeepSeek/Qwen on integrate.api.nvidia.com).
    // NVIDIA NIM needs a different opt-in shape for its hybrid-thinking
    // models (DeepSeek/Qwen on integrate.api.nvidia.com). NOTE: like the
    // DeepSeek toggle above, this keys off the provider name — a generic
    // openai connector pointed at a NIM base URL will not receive it.
    let chat_template_kwargs = (provider == "nvidia" && params.reasoning_effort.is_some())
        .then_some(ChatTemplateKwargs { thinking: true });
    // In inline mode the request must NOT carry the native `tools` array:
    // the model is instructed to write tool calls as JSON into its text
    // response, and the harness parses them. Without `tools` the API can
    // never produce structured tool calls, so the two delivery paths are
    // mutually exclusive at the wire level.
    let native_tools = params.tool_call_mode == ToolCallMode::Native;
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
        tools: if native_tools {
            params.tools.clone()
        } else {
            None
        },
        tool_choice: if native_tools {
            params.tool_choice.clone()
        } else {
            None
        },
        reasoning_effort: params.reasoning_effort.clone(),
        thinking,
        chat_template_kwargs,
        user: params.user.clone(),
    }
}

/// Resolve the API key for a request, if any.
///
/// Local providers are configured by URL and never require a key (most
/// servers ignore the `Authorization` header entirely); cloud providers fall
/// back to the OS keyring / environment and must have a key. Returns an
/// error when a cloud provider has no key configured.
fn resolve_api_key(
    config: &ProviderConfig,
    params: &Parameters,
    service: Option<&str>,
) -> Result<Option<String>, ConnectorError> {
    let key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name, service));
    if !is_local_provider(config.name) && key.is_none() {
        return Err(ConnectorError::MissingApiKey(config.name.to_string()));
    }
    Ok(key)
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
    service: Option<&str>,
) -> Result<ChatOutput, ConnectorError> {
    let api_key = resolve_api_key(config, params, service)?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    let messages = build_messages(prompt, system_prompt);
    let request = build_chat_request(model, messages, params, false, config.name);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let auth = api_key.as_ref().map(|key| format!("Bearer {key}"));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let response_text = send_request(config, &url, &request, &headers).await?;
    let chat_response: ChatResponse = match serde_json::from_str(&response_text) {
        Ok(r) => r,
        Err(e) => {
            if let Ok(api_err) = serde_json::from_str::<ApiErrorResponse>(&response_text) {
                let transient = is_transient_error_type(&api_err.error);
                let err = ConnectorError::classify_http(200, api_err.error.message);
                return Err(if transient { err.mark_transient() } else { err });
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
    service: Option<&str>,
) -> Result<ChatStream, ConnectorError> {
    let api_key = resolve_api_key(config, params, service)?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    let messages = build_messages(prompt, system_prompt);
    let request = build_chat_request(model, messages, params, true, config.name);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let auth = api_key.as_ref().map(|key| format!("Bearer {key}"));
    log::debug!("chat_stream: sending request to {url}");
    let stream = if params.retry_enabled {
        let json_body = serde_json::to_string(&request)?;
        let mut request_builder = apply_provider_headers(
            shared_client()
                .post(&url)
                .header("Content-Type", "application/json"),
            config,
        );
        if let Some(a) = &auth {
            request_builder = request_builder.header("Authorization", a.as_str());
        }
        let response = send_with_retry(
            &request_builder,
            json_body.clone(),
            params.retry_delay_override,
        )
        .await?;
        retry_mid_stream(
            response,
            request_builder,
            json_body,
            params.retry_delay_override,
            process_sse_response,
        )
    } else {
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(a) = &auth {
            headers.push(("Authorization", a.as_str()));
        }
        let response = send_request_stream(config, &url, &request, &headers).await?;
        process_sse_response(response)
    };
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
    service: Option<&str>,
) -> Result<ChatStream, ConnectorError> {
    let api_key = resolve_api_key(config, params, service)?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());

    // OpenAI's wire format has no `thought_signature` — a Gemini-originated
    // call (carried internally on ToolCallMsg) must be stripped before
    // serialization, otherwise the request would be rejected with 400.
    let all_messages = build_full_messages(system, messages);
    let all_messages = strip_internal_fields(&all_messages);
    let request = build_chat_request(model, all_messages, params, true, config.name);
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let auth = api_key.as_ref().map(|key| format!("Bearer {key}"));
    log::debug!("chat_stream_with_messages: sending request to {url}");
    let stream = if params.retry_enabled {
        let json_body = serde_json::to_string(&request)?;
        let mut request_builder = apply_provider_headers(
            shared_client()
                .post(&url)
                .header("Content-Type", "application/json"),
            config,
        );
        if let Some(a) = &auth {
            request_builder = request_builder.header("Authorization", a.as_str());
        }
        let response = send_with_retry(
            &request_builder,
            json_body.clone(),
            params.retry_delay_override,
        )
        .await?;
        retry_mid_stream(
            response,
            request_builder,
            json_body,
            params.retry_delay_override,
            process_sse_response,
        )
    } else {
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(a) = &auth {
            headers.push(("Authorization", a.as_str()));
        }
        let response = send_request_stream(config, &url, &request, &headers).await?;
        process_sse_response(response)
    };
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
    service: Option<&str>,
) -> Result<Vec<f32>, ConnectorError> {
    let api_key = resolve_api_key(config, params, service)?;
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

    let auth = api_key.as_ref().map(|key| format!("Bearer {key}"));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let response_text = send_request(config, &url, &request, &headers).await?;
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
    service: Option<&str>,
) -> Result<LsOutput, ConnectorError> {
    let api_key = resolve_api_key(config, params, service)?;

    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/models");

    let auth = api_key.as_ref().map(|key| format!("Bearer {key}"));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let response_text = send_get_request(config, &url, &headers).await?;

    let list: ListModelsResponse = serde_json::from_str(&response_text)?;
    let models: Vec<ModelInfo> = list
        .data
        .into_iter()
        .map(|item| ModelInfo { id: item.id })
        .collect();

    Ok(LsOutput::new(response_text, models))
}
