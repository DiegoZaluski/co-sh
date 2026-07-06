use super::super::common::{SseBuffer, send_get_request, send_request, send_request_stream};
use super::super::error::ConnectorError;
use super::super::output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
use super::super::params::{Parameters, ToolDefinition};
use super::super::provider::{ProviderConfig, get_api_key};

use async_stream::stream;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

// Request types

#[derive(serde::Serialize)]
struct MessageContent {
    role: String,
    content: String,
}

#[derive(serde::Serialize)]
struct AnthropicTool {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_schema: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
}

#[derive(serde::Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
}

// Response types

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(serde::Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

// Helpers

fn convert_stop(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::String(s) => Some(vec![s.clone()]),
        serde_json::Value::Array(arr) => {
            let strs: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if strs.is_empty() { None } else { Some(strs) }
        }
        _ => None,
    }
}

fn convert_tool_choice(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" | "any" | "none" => Some(serde_json::json!({"type": s})),
            _ => None,
        },
        v => Some(v.clone()),
    }
}

fn build_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.function.name.clone(),
            description: t.function.description.clone(),
            input_schema: t.function.parameters.clone(),
        })
        .collect()
}

fn build_messages(prompt: &str) -> Vec<MessageContent> {
    vec![MessageContent {
        role: "user".to_string(),
        content: prompt.to_string(),
    }]
}

fn build_request(
    model: String,
    messages: Vec<MessageContent>,
    params: &Parameters,
    stream: bool,
    system_prompt: Option<&str>,
) -> MessageRequest {
    let stop_sequences = params.stop.as_ref().and_then(convert_stop);
    let tools = params.tools.as_ref().map(|t| build_tools(t));
    let tool_choice = params.tool_choice.as_ref().and_then(convert_tool_choice);
    let metadata = params.user.as_ref().map(|uid| Metadata {
        user_id: Some(uid.clone()),
    });

    MessageRequest {
        model,
        max_tokens: params.max_tokens.unwrap_or(4096),
        messages,
        system: system_prompt.map(String::from),
        stream: if stream { Some(true) } else { None },
        temperature: params.temperature,
        top_p: params.top_p,
        stop_sequences,
        tools,
        tool_choice,
        metadata,
    }
}

fn extract_response_text(response: &MessageResponse) -> Result<String, ConnectorError> {
    let texts: Vec<&str> = response
        .content
        .iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text.as_deref())
        .collect();
    if texts.is_empty() {
        return Err(ConnectorError::NoContent);
    }
    Ok(texts.concat())
}

struct RequestContext {
    api_key: String,
    request: MessageRequest,
}

fn prepare_request(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
    stream: bool,
) -> Result<RequestContext, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;

    let messages = build_messages(prompt);
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());
    let request = build_request(model, messages, params, stream, system_prompt);

    Ok(RequestContext { api_key, request })
}

// Public API

pub(crate) async fn chat(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<ChatOutput, ConnectorError> {
    let ctx = prepare_request(config, params, prompt, system_prompt, false)?;
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/messages");

    let headers = &[
        ("x-api-key", ctx.api_key.as_str()),
        ("anthropic-version", "2023-06-01"),
    ];
    let response_text = send_request(config, &url, &ctx.request, headers).await?;
    let chat_response: MessageResponse = serde_json::from_str(&response_text)?;
    let message = extract_response_text(&chat_response)?;
    Ok(ChatOutput {
        raw: response_text,
        message,
    })
}

pub(crate) async fn chat_stream(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<ChatStream, ConnectorError> {
    let ctx = prepare_request(config, params, prompt, system_prompt, true)?;
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/messages");

    let headers = &[
        ("x-api-key", ctx.api_key.as_str()),
        ("anthropic-version", "2023-06-01"),
    ];
    let response = send_request_stream(config, &url, &ctx.request, headers).await?;

    let inner: Pin<Box<dyn Stream<Item = Result<StreamChunk, ConnectorError>> + Send>> = Box::pin(
        stream! {
            let mut response = response;
            let mut buf = SseBuffer::new();
            loop {
                let chunk = match tokio::time::timeout(Duration::from_secs(30), response.chunk()).await {
                    Ok(Ok(Some(c))) => c,
                    Ok(Ok(None)) => break,
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
                    match serde_json::from_str::<serde_json::Value>(&data) {
                        Ok(v) => {
                            let kind = v["type"].as_str().unwrap_or("");
                            match kind {
                                "content_block_delta" => {
                                    if v["delta"]["type"] == "text_delta" {
                                        let token = v["delta"]["text"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_owned();
                                        yield Ok(StreamChunk {
                                            raw: data,
                                            token,
                                            finish_reason: None,
                                        });
                                    }
                                }
                                "message_delta" => {
                                    let finish_reason = v["delta"]["stop_reason"]
                                        .as_str()
                                        .map(String::from);
                                    yield Ok(StreamChunk {
                                        raw: data,
                                        token: String::new(),
                                        finish_reason,
                                    });
                                }
                                "message_stop" => return,
                                "error" => {
                                    let msg = v["error"]["message"]
                                        .as_str()
                                        .unwrap_or("unknown error");
                                    yield Err(ConnectorError::Network(msg.to_string()));
                                    return;
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            yield Err(ConnectorError::Deserialization(e.to_string()));
                            return;
                        }
                    }
                }
            }
        },
    );

    Ok(ChatStream::new(inner))
}

#[derive(serde::Deserialize)]
struct ClaudeModelEntry {
    id: String,
}

#[derive(serde::Deserialize)]
struct ClaudeListModelsResponse {
    data: Vec<ClaudeModelEntry>,
}

/// Fetch the list of available models from the Anthropic Claude API.
///
/// Sends a GET to `{base_url}/models` and returns a [`LsOutput`].
pub(crate) async fn list_models(
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

    let headers = &[
        ("x-api-key", api_key.as_str()),
        ("anthropic-version", "2023-06-01"),
    ];
    let response_text = send_get_request(config, &url, headers).await?;

    let list: ClaudeListModelsResponse = serde_json::from_str(&response_text)?;
    let models: Vec<ModelInfo> = list
        .data
        .into_iter()
        .map(|entry| ModelInfo { id: entry.id })
        .collect();

    Ok(LsOutput::new(response_text, models))
}
