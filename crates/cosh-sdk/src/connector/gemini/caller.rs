use super::super::error::ConnectorError;
use super::super::params::{Parameters, ToolDefinition};
use super::super::provider::{ProviderConfig, get_api_key};

use async_stream::stream;
use std::pin::Pin;
use tokio_stream::Stream;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    text: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FunctionDeclaration {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct Tool {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<FunctionDeclaration>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FunctionCallingConfig {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_function_names: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
struct ToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: FunctionCallingConfig,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_logprobs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<u32>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<ToolConfig>,
}

// Response types (shared by both streaming and non-streaming responses)

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponsePart {
    text: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseContent {
    parts: Vec<ResponsePart>,
    #[serde(default)]
    role: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: ResponseContent,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
}

#[allow(dead_code)]
#[allow(clippy::struct_field_names)]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    total_token_count: u32,
}

// SSE streaming types — same shape as non-streaming response

// Embedding types

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EmbedContentRequest {
    content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddingValues {
    values: Vec<f32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbedContentResponse {
    embedding: EmbeddingValues,
}

// SSE buffer

struct SseBuffer {
    buf: Vec<u8>,
}

impl SseBuffer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

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

// Helpers

fn build_contents(prompt: &str, system_prompt: Option<&str>) -> (Vec<Content>, Option<Content>) {
    let system_instruction = system_prompt.map(|sys| {
        Content {
            role: None,
            parts: vec![Part { text: sys.to_string() }],
        }
    });

    let contents = vec![Content {
        role: Some("user".to_string()),
        parts: vec![Part { text: prompt.to_string() }],
    }];

    (contents, system_instruction)
}

fn build_generation_config(params: &Parameters) -> Option<GenerationConfig> {
    let response_mime_type = params.response_format.as_ref().and_then(|rf| {
        if rf.kind == "json_object" {
            Some("application/json".to_string())
        } else {
            None
        }
    });

    let stop_sequences = params.stop.as_ref().and_then(|v| match v {
        serde_json::Value::String(s) => Some(vec![s.clone()]),
        serde_json::Value::Array(arr) => {
            let strs: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            if strs.is_empty() { None } else { Some(strs) }
        }
        _ => None,
    });

    let has_any = params.max_tokens.is_some()
        || params.temperature.is_some()
        || params.top_p.is_some()
        || stop_sequences.is_some()
        || params.frequency_penalty.is_some()
        || params.presence_penalty.is_some()
        || params.seed.is_some()
        || response_mime_type.is_some()
        || params.logprobs.is_some()
        || params.top_logprobs.is_some();

    if !has_any {
        return None;
    }

    Some(GenerationConfig {
        max_output_tokens: params.max_tokens,
        temperature: params.temperature,
        top_p: params.top_p,
        top_k: None,
        stop_sequences,
        frequency_penalty: params.frequency_penalty,
        presence_penalty: params.presence_penalty,
        seed: params.seed,
        response_mime_type,
        response_logprobs: params.logprobs,
        logprobs: params.top_logprobs,
    })
}

fn build_tools(tools: &[ToolDefinition]) -> Vec<Tool> {
    if tools.is_empty() {
        return vec![];
    }
    let declarations: Vec<FunctionDeclaration> = tools.iter().map(|tool| {
        FunctionDeclaration {
            name: tool.function.name.clone(),
            description: tool.function.description.clone(),
            parameters: tool.function.parameters.clone(),
        }
    }).collect();
    vec![Tool { function_declarations: declarations }]
}

fn build_tool_config(tool_choice: &serde_json::Value) -> Option<ToolConfig> {
    match tool_choice {
        serde_json::Value::String(s) => {
            let mode = s.to_uppercase();
            if mode == "AUTO" || mode == "ANY" || mode == "NONE" {
                Some(ToolConfig {
                    function_calling_config: FunctionCallingConfig {
                        mode,
                        allowed_function_names: None,
                    },
                })
            } else {
                None
            }
        }
        serde_json::Value::Object(obj) => {
            let mode = obj.get("type").and_then(|v| v.as_str()).map_or("ANY".to_string(), str::to_uppercase);
            let names = obj.get("function")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
                .map(|n| vec![n.to_string()]);
            Some(ToolConfig {
                function_calling_config: FunctionCallingConfig {
                    mode,
                    allowed_function_names: names,
                },
            })
        }
        _ => None,
    }
}

fn extract_response_text(response: &GenerateContentResponse) -> Result<String, ConnectorError> {
    let candidate = response.candidates.first().ok_or(ConnectorError::NoChoices)?;
    candidate.content.parts.first()
        .and_then(|p| p.text.as_deref())
        .filter(|t| !t.is_empty())
        .map(String::from)
        .ok_or(ConnectorError::NoContent)
}

// Helpers

struct RequestContext {
    api_key: String,
    request: GenerateContentRequest,
}

fn prepare_request(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<RequestContext, ConnectorError> {
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(config.name))
        .ok_or_else(|| ConnectorError::MissingApiKey(config.name.to_string()))?;

    let (contents, system_instruction) = build_contents(prompt, system_prompt);
    let generation_config = build_generation_config(params);
    let tools = params.tools.as_ref().map(|t| build_tools(t));
    let tool_config = params.tool_choice.as_ref().and_then(build_tool_config);

    let request = GenerateContentRequest {
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    };

    Ok(RequestContext { api_key, request })
}

// Public API

pub(crate) async fn chat(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<String, ConnectorError> {
    let ctx = prepare_request(config, params, prompt, system_prompt)?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/models/{model}:generateContent");

    let response_text = send_request(config, &ctx.api_key, &url, &ctx.request).await?;
    let chat_response: GenerateContentResponse = serde_json::from_str(&response_text)?;
    extract_response_text(&chat_response)
}

pub(crate) async fn chat_stream(
    config: &ProviderConfig,
    params: &Parameters,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Result<Pin<Box<dyn Stream<Item = Result<String, ConnectorError>> + Send>>, ConnectorError> {
    let ctx = prepare_request(config, params, prompt, system_prompt)?;
    let model = params
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.to_string());
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/models/{model}:streamGenerateContent");

    let response = send_request_stream(config, &ctx.api_key, &url, &ctx.request).await?;

    Ok(Box::pin(stream! {
        let mut response = response;
        let mut buf = SseBuffer::new();
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
                match serde_json::from_str::<GenerateContentResponse>(&data) {
                    Ok(ccr) => {
                        if let Some(text) = ccr.candidates.first()
                            .and_then(|c| c.content.parts.first())
                            .and_then(|p| p.text.as_deref())
                            .filter(|c| !c.is_empty())
                        {
                            yield Ok(text.to_owned());
                        }
                    }
                    Err(e) => {
                        yield Err(ConnectorError::Deserialization(e.to_string()));
                        return;
                    }
                }
            }
        }
    }))
}

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
        .unwrap_or_else(|| "gemini-embedding-001".to_string());

    let request = EmbedContentRequest {
        content: Content {
            role: None,
            parts: vec![Part { text: input.to_string() }],
        },
        output_dimensionality: None,
    };
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/models/{model}:embedContent");

    let response_text = send_request(config, &api_key, &url, &request).await?;
    let embed_response: EmbedContentResponse = serde_json::from_str(&response_text)?;
    Ok(embed_response.embedding.values)
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
        .header("x-goog-api-key", api_key)
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
