#[derive(serde::Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(serde::Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

impl ResponseFormat {
    #[must_use]
    pub fn json_object() -> Self {
        Self {
            kind: "json_object".into(),
        }
    }
}

#[derive(serde::Serialize)]
pub struct ToolFunction {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    kind: String,
    function: ToolFunction,
}

/// Optional parameters for advanced control of chat completions.
///
/// Build via builder methods:
/// ```
/// # use cosh_sdk::connector::openai_compatible::Parameters;
/// let _p = Parameters::default()
///     .with_model("gpt-4")
///     .with_temperature(0.7)
///     .with_max_tokens(500);
/// ```
#[derive(Default)]
#[must_use]
pub struct Parameters {
    pub(crate) model: Option<String>,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) temperature: Option<f32>,
    pub(crate) top_p: Option<f32>,
    pub(crate) stop: Option<serde_json::Value>,
    pub(crate) frequency_penalty: Option<f32>,
    pub(crate) presence_penalty: Option<f32>,
    pub(crate) seed: Option<i64>,
    pub(crate) response_format: Option<ResponseFormat>,
    pub(crate) logprobs: Option<bool>,
    pub(crate) top_logprobs: Option<u32>,
    pub(crate) tools: Option<Vec<ToolDefinition>>,
    pub(crate) tool_choice: Option<serde_json::Value>,
    pub(crate) user: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key: Option<String>,
}

impl Parameters {
    fn with<T>(opt: &mut Option<T>, val: T) {
        *opt = Some(val);
    }

    pub fn with_model(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.model, v.into());
        self
    }
    pub fn with_max_tokens(mut self, v: u32) -> Self {
        Self::with(&mut self.max_tokens, v);
        self
    }
    pub fn with_temperature(mut self, v: f32) -> Self {
        Self::with(&mut self.temperature, v);
        self
    }
    pub fn with_top_p(mut self, v: f32) -> Self {
        Self::with(&mut self.top_p, v);
        self
    }
    pub fn with_stop(mut self, v: serde_json::Value) -> Self {
        Self::with(&mut self.stop, v);
        self
    }
    pub fn with_frequency_penalty(mut self, v: f32) -> Self {
        Self::with(&mut self.frequency_penalty, v);
        self
    }
    pub fn with_presence_penalty(mut self, v: f32) -> Self {
        Self::with(&mut self.presence_penalty, v);
        self
    }
    pub fn with_seed(mut self, v: i64) -> Self {
        Self::with(&mut self.seed, v);
        self
    }
    pub fn with_response_format(mut self, v: ResponseFormat) -> Self {
        Self::with(&mut self.response_format, v);
        self
    }
    pub fn with_logprobs(mut self, v: bool) -> Self {
        Self::with(&mut self.logprobs, v);
        self
    }
    pub fn with_top_logprobs(mut self, v: u32) -> Self {
        Self::with(&mut self.top_logprobs, v);
        self
    }
    pub fn with_tools(mut self, v: Vec<ToolDefinition>) -> Self {
        Self::with(&mut self.tools, v);
        self
    }
    pub fn with_tool_choice(mut self, v: serde_json::Value) -> Self {
        Self::with(&mut self.tool_choice, v);
        self
    }
    pub fn with_user(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.user, v.into());
        self
    }
    pub fn with_base_url(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.base_url, v.into());
        self
    }
    pub fn with_api_key(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.api_key, v.into());
        self
    }
}

#[derive(serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
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

struct ProviderConfig {
    base_url: &'static str,
    default_model: &'static str,
    needs_extra_headers: bool,
}

const PROVIDERS: &[(&str, ProviderConfig)] = &[
    (
        "openai",
        ProviderConfig {
            base_url: "https://api.openai.com/v1",
            default_model: "gpt-4o-mini",
            needs_extra_headers: false,
        },
    ),
    (
        "groq",
        ProviderConfig {
            base_url: "https://api.groq.com/openai/v1",
            default_model: "llama-3.3-70b-versatile",
            needs_extra_headers: false,
        },
    ),
    (
        "mistral",
        ProviderConfig {
            base_url: "https://api.mistral.ai/v1",
            default_model: "mistral-small-latest",
            needs_extra_headers: false,
        },
    ),
    (
        "together",
        ProviderConfig {
            base_url: "https://api.together.xyz/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct-Turbo",
            needs_extra_headers: false,
        },
    ),
    (
        "openrouter",
        ProviderConfig {
            base_url: "https://openrouter.ai/api/v1",
            default_model: "qwen/qwen-2.5-72b-instruct",
            needs_extra_headers: true,
        },
    ),
    (
        "xai",
        ProviderConfig {
            base_url: "https://api.x.ai/v1",
            default_model: "grok-2-1212",
            needs_extra_headers: false,
        },
    ),
    (
        "deepseek",
        ProviderConfig {
            base_url: "https://api.deepseek.com/v1",
            default_model: "deepseek-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "perplexity",
        ProviderConfig {
            base_url: "https://api.perplexity.ai",
            default_model: "llama-3.1-sonar-small-128k-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "fireworks",
        ProviderConfig {
            base_url: "https://api.fireworks.ai/inference/v1",
            default_model: "qwen2.5-72b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cohere",
        ProviderConfig {
            base_url: "https://api.cohere.com/compatibility/v1",
            default_model: "command-r-plus",
            needs_extra_headers: false,
        },
    ),
    (
        "huggingface",
        ProviderConfig {
            base_url: "https://router.huggingface.co/v1",
            default_model: "meta-llama/Llama-3.3-70B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "sambanova",
        ProviderConfig {
            base_url: "https://api.sambanova.ai/v1",
            default_model: "Meta-Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "poe",
        ProviderConfig {
            base_url: "https://api.poe.com/v1",
            default_model: "GPT-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "cerebras",
        ProviderConfig {
            base_url: "https://api.cerebras.ai/v1",
            default_model: "llama-3.1-8b-chat-completion",
            needs_extra_headers: false,
        },
    ),
    (
        "nvidia",
        ProviderConfig {
            base_url: "https://integrate.api.nvidia.com/v1",
            default_model: "meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "anyscale",
        ProviderConfig {
            base_url: "https://api.endpoints.anyscale.com/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "vercel",
        ProviderConfig {
            base_url: "https://ai-gateway.vercel.sh/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cloudflare",
        ProviderConfig {
            base_url: "https://gateway.ai.cloudflare.com/v1/_/cloudflare/workers-ai/openai",
            default_model: "@cf/meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "azure",
        ProviderConfig {
            base_url: "",
            default_model: "gpt-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "ollama",
        ProviderConfig {
            base_url: "http://localhost:11434/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "lmstudio",
        ProviderConfig {
            base_url: "http://localhost:1234/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "vllm",
        ProviderConfig {
            base_url: "http://localhost:8000/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "llamacpp",
        ProviderConfig {
            base_url: "http://localhost:8080/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
];

const API_KEY_ENVS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("huggingface", "HUGGINGFACE_API_KEY"),
    ("nvidia", "NVIDIA_API_KEY"),
    ("github", "GITHUB_TOKEN"),
    ("cloudflare", "CLOUDFLARE_API_KEY"),
    ("fireworks", "FIREWORKS_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("perplexity", "PERPLEXITY_API_KEY"),
    ("cohere", "COHERE_API_KEY"),
    ("sambanova", "SAMBANOVA_API_KEY"),
    ("poe", "POE_API_KEY"),
    ("anyscale", "ANYSCALE_API_KEY"),
    ("vercel", "VERCEL_API_KEY"),
    ("azure", "AZURE_OPENAI_KEY"),
];

fn get_provider(name: &str) -> Option<&'static ProviderConfig> {
    PROVIDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, config)| config)
}

fn get_api_key(provider: &str) -> Option<String> {
    let env_var = API_KEY_ENVS
        .iter()
        .find(|(key, _)| *key == provider)
        .map_or("OPENAI_API_KEY", |(_, var)| *var);
    std::env::var(env_var).ok()
}

/// Send a chat completion request to an OpenAI-compatible provider.
///
/// # Errors
///
/// Returns `Err` if the provider is unknown, the API key is missing, the
/// HTTP request fails, or the response cannot be parsed.
pub async fn openai(
    provider: &str,
    prompt: &str,
    system_prompt: Option<&str>,
    params: Option<Parameters>,
) -> Result<String, Box<dyn std::error::Error>> {
    let config = get_provider(provider).ok_or("Unknown provider")?;
    let params = params.unwrap_or_default();
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(provider))
        .ok_or_else(|| format!("API key not set for provider: {provider}"))?;
    let model = params
        .model
        .unwrap_or_else(|| config.default_model.to_string());

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

    let request = ChatRequest {
        model,
        messages,
        max_tokens: params.max_tokens,
        temperature: params.temperature,
        top_p: params.top_p,
        stop: params.stop,
        frequency_penalty: params.frequency_penalty,
        presence_penalty: params.presence_penalty,
        seed: params.seed,
        response_format: params.response_format,
        logprobs: params.logprobs,
        top_logprobs: params.top_logprobs,
        tools: params.tools,
        tool_choice: params.tool_choice,
        user: params.user,
    };
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/chat/completions");

    let response_text = send_request(config, &api_key, &url, &request).await?;
    let chat_response: ChatResponse = serde_json::from_str(&response_text)?;
    let first_choice = chat_response
        .choices
        .first()
        .ok_or_else(|| Box::<dyn std::error::Error>::from("No choices in response"))?;
    first_choice
        .message
        .content
        .clone()
        .ok_or_else(|| Box::<dyn std::error::Error>::from("No content in response"))
}

/// Optional parameters for embedding requests.
#[derive(Default)]
#[must_use]
pub struct EmbedParams {
    pub(crate) model: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key: Option<String>,
}

impl EmbedParams {
    fn with<T>(opt: &mut Option<T>, val: T) {
        *opt = Some(val);
    }

    pub fn with_model(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.model, v.into());
        self
    }
    pub fn with_base_url(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.base_url, v.into());
        self
    }
    pub fn with_api_key(mut self, v: impl Into<String>) -> Self {
        Self::with(&mut self.api_key, v.into());
        self
    }
}

/// Generate an embedding vector for the given text.
///
/// # Errors
///
/// Returns `Err` if the provider is unknown, the API key is missing, the
/// HTTP request fails, or the response cannot be parsed.
pub async fn embed(
    provider: &str,
    text: &str,
    model: Option<&str>,
    params: Option<EmbedParams>,
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let config = get_provider(provider).ok_or("Unknown provider")?;
    let params = params.unwrap_or_default();
    let api_key = params
        .api_key
        .clone()
        .or_else(|| get_api_key(provider))
        .ok_or_else(|| format!("API key not set for provider: {provider}"))?;
    let model = params
        .model
        .unwrap_or_else(|| model.unwrap_or("text-embedding-3-small").to_string());

    let request = EmbeddingRequest {
        model,
        input: text.to_string(),
    };
    let base_url = params.base_url.as_deref().unwrap_or(config.base_url);
    let url = format!("{base_url}/embeddings");

    let response_text = send_request(config, &api_key, &url, &request).await?;
    let embed_response: EmbeddingResponse = serde_json::from_str(&response_text)?;
    let first_data = embed_response
        .data
        .first()
        .ok_or_else(|| Box::<dyn std::error::Error>::from("No embeddings in response"))?;
    Ok(first_data.embedding.clone())
}

async fn send_request(
    config: &ProviderConfig,
    api_key: &str,
    url: &str,
    body: &(impl serde::Serialize + Sync),
) -> Result<String, Box<dyn std::error::Error>> {
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
        return Err(error_msg.into());
    }

    Ok(response.text().await?)
}
