use super::error::ConnectorError;
use super::output::{ChatOutput, ChatStream, LsOutput};
use super::params::{ChatMessage, Parameters, ResponseFormat, ToolDefinition};
use super::provider::{Family, ProviderConfig, get_provider};

use super::claude;
use super::gemini;
use super::openai_compatible;

/// A unified client for any LLM provider.
///
/// All providers share the same API — just change the name passed to `new()`.
///
/// ```
/// # use cosh_sdk::connector::Connector;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let reply = Connector::new("openai")?
///     .with_model("gpt-4o")
///     .with_temperature(0.7)
///     .with_api_key("sk-...")
///     .chat("Explain Rust ownership")
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
#[must_use]
pub struct Connector {
    provider: Option<&'static ProviderConfig>, // None before new() succeeds
    params: Parameters,
}

impl Connector {
    fn provider(&self) -> Result<&'static ProviderConfig, ConnectorError> {
        self.provider.ok_or(ConnectorError::NotImplemented(
            "Connector used without calling new()",
        ))
    }

    /// Create a new Connector for the given provider name.
    ///
    /// Supported providers: `claude`, `openai`, `groq`, `mistral`, `together`, `openrouter`,
    /// `xai`, `deepseek`, `perplexity`, `fireworks`, `cohere`, `huggingface`,
    /// `sambanova`, `poe`, `cerebras`, `nvidia`, `anyscale`, `vercel`, `cloudflare`,
    /// `azure`, `ollama`, `lmstudio`, `vllm`, `llamacpp`, `gemini`, `zai`.
    ///
    /// # Errors
    ///
    /// Returns `UnknownProvider` if the name is not recognized.
    pub fn new(provider: &str) -> Result<Self, ConnectorError> {
        let cfg = get_provider(provider)
            .ok_or_else(|| ConnectorError::UnknownProvider(provider.to_string()))?;
        Ok(Self {
            provider: Some(cfg),
            params: Parameters::default(),
        })
    }

    /// Set the model to use (e.g., `"gpt-4o"`, `"claude-3-opus"`).
    ///
    /// Each provider has a sensible default — call this only when you need a
    /// different model.
    pub fn with_model(mut self, v: impl Into<String>) -> Self {
        self.params.model = Some(v.into());
        self
    }

    /// Maximum number of tokens to generate.
    pub const fn with_max_tokens(mut self, v: u32) -> Self {
        self.params.max_tokens = Some(v);
        self
    }

    /// Sampling temperature (0.0 – 2.0). Higher values make output more random.
    pub const fn with_temperature(mut self, v: f32) -> Self {
        self.params.temperature = Some(v);
        self
    }

    /// Nucleus sampling threshold (0.0 – 1.0).
    pub const fn with_top_p(mut self, v: f32) -> Self {
        self.params.top_p = Some(v);
        self
    }

    /// Sequences where the API will stop generating.
    pub fn with_stop(mut self, v: serde_json::Value) -> Self {
        self.params.stop = Some(v);
        self
    }

    /// Frequency penalty (-2.0 – 2.0). Positive values penalize frequent tokens.
    pub const fn with_frequency_penalty(mut self, v: f32) -> Self {
        self.params.frequency_penalty = Some(v);
        self
    }

    /// Presence penalty (-2.0 – 2.0). Positive values penalize tokens that have appeared.
    pub const fn with_presence_penalty(mut self, v: f32) -> Self {
        self.params.presence_penalty = Some(v);
        self
    }

    /// Seed for deterministic sampling.
    pub const fn with_seed(mut self, v: i64) -> Self {
        self.params.seed = Some(v);
        self
    }

    /// Constrain the model to produce a specific output format (e.g., JSON).
    pub fn with_response_format(mut self, v: ResponseFormat) -> Self {
        self.params.response_format = Some(v);
        self
    }

    /// Whether to return log probabilities of the output tokens.
    pub const fn with_logprobs(mut self, v: bool) -> Self {
        self.params.logprobs = Some(v);
        self
    }

    /// Number of most probable tokens to return log probabilities for.
    pub const fn with_top_logprobs(mut self, v: u32) -> Self {
        self.params.top_logprobs = Some(v);
        self
    }

    /// Tool definitions the model may call.
    pub fn with_tools(mut self, v: Vec<ToolDefinition>) -> Self {
        self.params.tools = Some(v);
        self
    }

    /// Set tool definitions in-place (mutates the existing connector).
    pub fn set_tools(&mut self, v: Vec<ToolDefinition>) {
        self.params.tools = Some(v);
    }

    /// Controls which tool is called (e.g., `"auto"`, `"none"`, or a specific tool).
    pub fn with_tool_choice(mut self, v: serde_json::Value) -> Self {
        self.params.tool_choice = Some(v);
        self
    }

    /// A unique identifier for the end-user (for monitoring/abuse detection).
    pub fn with_user(mut self, v: impl Into<String>) -> Self {
        self.params.user = Some(v.into());
        self
    }

    /// Override the base URL for the API endpoint.
    pub fn with_base_url(mut self, v: impl Into<String>) -> Self {
        self.params.base_url = Some(v.into());
        self
    }

    /// Set the API key explicitly (otherwise resolved from env vars).
    pub fn with_api_key(mut self, v: impl Into<String>) -> Self {
        self.params.api_key = Some(v.into());
        self
    }

    /// Send a chat completion request with a user prompt.
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// `Deserialization` if the response is malformed, or `Network` on transport failure.
    pub async fn chat(&self, prompt: &str) -> Result<ChatOutput, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat(provider, &self.params, prompt, None).await
            }
            Family::Gemini => gemini::chat(provider, &self.params, prompt, None).await,
            Family::Claude => claude::chat(provider, &self.params, prompt, None).await,
        }
    }

    /// Send a chat completion request with both system and user prompts.
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// `Deserialization` if the response is malformed, or `Network` on transport failure.
    pub async fn chat_with_system(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<ChatOutput, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat(provider, &self.params, prompt, Some(system)).await
            }
            Family::Gemini => gemini::chat(provider, &self.params, prompt, Some(system)).await,
            Family::Claude => claude::chat(provider, &self.params, prompt, Some(system)).await,
        }
    }

    /// Generate an embedding vector for the given input text.
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// `Deserialization` if the response is malformed, or `Network` on transport failure.
    pub async fn embed(&self, input: &str) -> Result<Vec<f32>, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::embed(provider, &self.params, input).await
            }
            Family::Gemini => gemini::embed(provider, &self.params, input).await,
            Family::Claude => Err(ConnectorError::NotImplemented("embedding")),
        }
    }

    /// Stream a chat completion with a user prompt.
    ///
    /// Returns a [`ChatStream`] that yields [`StreamChunk`] items. After the
    /// stream ends, call [`raw`](ChatStream::raw) to get the last SSE frame
    /// (typically containing `usage`, `finish_reason`, etc.).
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// or `Network` on transport failure before the stream starts.
    pub async fn stream_chat(&self, prompt: &str) -> Result<ChatStream, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat_stream(provider, &self.params, prompt, None).await
            }
            Family::Gemini => gemini::chat_stream(provider, &self.params, prompt, None).await,
            Family::Claude => claude::chat_stream(provider, &self.params, prompt, None).await,
        }
    }

    /// Stream a chat completion with both system and user prompts.
    ///
    /// Returns a [`ChatStream`] that yields [`StreamChunk`] items. After the
    /// stream ends, call [`raw`](ChatStream::raw) to get the last SSE frame
    /// (typically containing `usage`, `finish_reason`, etc.).
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// or `Network` on transport failure before the stream starts.
    pub async fn stream_chat_with_system(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<ChatStream, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat_stream(provider, &self.params, prompt, Some(system)).await
            }
            Family::Gemini => {
                gemini::chat_stream(provider, &self.params, prompt, Some(system)).await
            }
            Family::Claude => {
                claude::chat_stream(provider, &self.params, prompt, Some(system)).await
            }
        }
    }

    /// Stream a chat completion with a full messages array (including system,
    /// user, assistant with `tool_calls`, and tool roles).
    ///
    /// The `system` parameter provides the base system prompt (instructions +
    /// tool definitions). The `messages` array contains the conversation
    /// history with proper roles, tool calls, and tool results.
    ///
    /// Use this method instead of [`stream_chat_with_system`] when the model
    /// needs to see structured tool call history (`role: "tool"` messages).
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// or `Network` on transport failure before the stream starts.
    pub async fn stream_chat_with_messages(
        &self,
        system: &str,
        messages: &[ChatMessage],
    ) -> Result<ChatStream, ConnectorError> {
        let provider = self.provider()?;
        let params = &self.params;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat_stream_with_messages(provider, params, system, messages)
                    .await
            }
            Family::Gemini => {
                // TODO: implement for Gemini
                Err(ConnectorError::NotImplemented(
                    "stream_chat_with_messages for Gemini",
                ))
            }
            Family::Claude => {
                claude::chat_stream_with_messages(provider, params, system, messages).await
            }
        }
    }

    /// Extract completion tokens from a raw API response (chat or stream).
    ///
    /// Dispatches to the correct provider-family extractor based on the
    /// configured [`Family`](super::provider::Family):
    /// - `OpenAICompatible` → `usage.completion_tokens`
    /// - `Claude` → `usage.output_tokens`
    /// - `Gemini` → `usageMetadata.candidatesTokenCount`
    ///
    /// Returns `None` when the field is absent or the raw JSON is malformed.
    #[must_use]
    pub fn tokens(&self, raw: &str) -> Option<u32> {
        let provider = self.provider().ok()?;
        match provider.family {
            Family::OpenAICompatible => openai_compatible::extract_tokens(raw),
            Family::Claude => claude::extract_tokens(raw),
            Family::Gemini => gemini::extract_tokens(raw),
        }
    }

    /// The provider name (e.g. `"openai"`, `"claude"`, `"gemini"`).
    ///
    /// Returns `None` if the connector was not properly initialised via [`new`](Self::new).
    #[must_use]
    pub fn provider_name(&self) -> Option<&'static str> {
        self.provider.map(|p| p.name)
    }

    /// The model override, if one was set via [`with_model`](Self::with_model).
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.params.model.as_deref()
    }

    /// Whether native tool definitions are currently registered on this
    /// connector (`set_tools`/`with_tools` was called and the list is
    /// non-empty).
    #[must_use]
    pub fn has_tools(&self) -> bool {
        self.params.tools.as_ref().is_some_and(|t| !t.is_empty())
    }

    /// Fetch the list of available models from the provider.
    ///
    /// Returns a [`LsOutput`] with structured model names ([`models`](LsOutput::models))
    /// and the raw JSON response ([`raw`](LsOutput::raw)).
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// `Deserialization` if the response is malformed, or `Network` on transport failure.
    pub async fn list_models(&self) -> Result<LsOutput, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::list_models(provider, &self.params).await
            }
            Family::Gemini => gemini::list_models(provider, &self.params).await,
            Family::Claude => claude::list_models(provider, &self.params).await,
        }
    }
}
