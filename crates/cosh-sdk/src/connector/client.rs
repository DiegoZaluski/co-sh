use super::error::ConnectorError;
use super::params::{Parameters, ResponseFormat, ToolDefinition};
use super::provider::{Family, ProviderConfig, get_provider};

use super::openai_compatible;
use tokio_stream::Stream;

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
#[derive(Debug)]
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
    /// Supported providers: `openai`, `groq`, `mistral`, `together`, `openrouter`,
    /// `xai`, `deepseek`, `perplexity`, `fireworks`, `cohere`, `huggingface`,
    /// `sambanova`, `poe`, `cerebras`, `nvidia`, `anyscale`, `vercel`, `cloudflare`,
    /// `azure`, `ollama`, `lmstudio`, `vllm`, `llamacpp`.
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
    pub fn with_max_tokens(mut self, v: u32) -> Self {
        self.params.max_tokens = Some(v);
        self
    }

    /// Sampling temperature (0.0 – 2.0). Higher values make output more random.
    pub fn with_temperature(mut self, v: f32) -> Self {
        self.params.temperature = Some(v);
        self
    }

    /// Nucleus sampling threshold (0.0 – 1.0).
    pub fn with_top_p(mut self, v: f32) -> Self {
        self.params.top_p = Some(v);
        self
    }

    /// Sequences where the API will stop generating.
    pub fn with_stop(mut self, v: serde_json::Value) -> Self {
        self.params.stop = Some(v);
        self
    }

    /// Frequency penalty (-2.0 – 2.0). Positive values penalize frequent tokens.
    pub fn with_frequency_penalty(mut self, v: f32) -> Self {
        self.params.frequency_penalty = Some(v);
        self
    }

    /// Presence penalty (-2.0 – 2.0). Positive values penalize tokens that have appeared.
    pub fn with_presence_penalty(mut self, v: f32) -> Self {
        self.params.presence_penalty = Some(v);
        self
    }

    /// Seed for deterministic sampling.
    pub fn with_seed(mut self, v: i64) -> Self {
        self.params.seed = Some(v);
        self
    }

    /// Constrain the model to produce a specific output format (e.g., JSON).
    pub fn with_response_format(mut self, v: ResponseFormat) -> Self {
        self.params.response_format = Some(v);
        self
    }

    /// Whether to return log probabilities of the output tokens.
    pub fn with_logprobs(mut self, v: bool) -> Self {
        self.params.logprobs = Some(v);
        self
    }

    /// Number of most probable tokens to return log probabilities for.
    pub fn with_top_logprobs(mut self, v: u32) -> Self {
        self.params.top_logprobs = Some(v);
        self
    }

    /// Tool definitions the model may call.
    pub fn with_tools(mut self, v: Vec<ToolDefinition>) -> Self {
        self.params.tools = Some(v);
        self
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
    pub async fn chat(&self, prompt: &str) -> Result<String, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat(provider, &self.params, prompt, None).await
            }
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
    ) -> Result<String, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat(provider, &self.params, prompt, Some(system)).await
            }
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
        }
    }

    /// Stream a chat completion with a user prompt.
    ///
    /// Returns an async stream of content chunks as they arrive.
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// or `Network` on transport failure before the stream starts.
    pub async fn stream_chat(
        &self,
        prompt: &str,
    ) -> Result<impl Stream<Item = Result<String, ConnectorError>>, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat_stream(provider, &self.params, prompt, None).await
            }
        }
    }

    /// Stream a chat completion with both system and user prompts.
    ///
    /// # Errors
    ///
    /// Returns `MissingApiKey` if no API key is found, `HttpError` on non-2xx status,
    /// or `Network` on transport failure before the stream starts.
    pub async fn stream_chat_with_system(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<impl Stream<Item = Result<String, ConnectorError>>, ConnectorError> {
        let provider = self.provider()?;
        match provider.family {
            Family::OpenAICompatible => {
                openai_compatible::chat_stream(provider, &self.params, prompt, Some(system)).await
            }
        }
    }
}
