use zeroize::Zeroize;

use crate::connector::provider::COSH_SERVICE;

/// A Claude extended-thinking block captured from a previous response.
///
/// Replayed VERBATIM (text + signature) at the start of the assistant
/// message when the turn is re-sent in a multi-turn or tool-use conversation
/// — the Anthropic API validates the signature cryptographically and rejects
/// modified or missing blocks with HTTP 400. Only the Claude caller sets
/// this; `skip_serializing_if` keeps it off every other provider's wire
/// format.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ClaudeThinkingBlock {
    pub thinking: String,
    pub signature: String,
}

/// A chat message for structured conversation history with native tool call support.
///
/// This mirrors the `OpenAI` Chat Completion message format so the model
/// can natively understand tool calls (`role: "assistant"` with
/// `tool_calls`) and tool results (`role: "tool"` with `tool_call_id`).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallMsg>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Claude extended-thinking blocks that preceded this assistant turn
    /// (see [`ClaudeThinkingBlock`]). Internal transport detail — only the
    /// Claude caller reads it, and `skip_serializing_if` keeps it off the
    /// OpenAI-compatible wire format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_blocks: Option<Vec<ClaudeThinkingBlock>>,
}

/// A tool call within an assistant message (OpenAI-compatible format).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolCallMsg {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolCallFunctionMsg,
    /// Gemini 3.x thought signature, carried ONLY as an internal transport
    /// detail: when a thinking model emits a native `functionCall`, the part
    /// travels with a sibling `thoughtSignature` that MUST be replayed
    /// verbatim when the call is re-sent in the conversation history (the
    /// API rejects the replay with HTTP 400 otherwise). Other providers
    /// never set it, and `skip_serializing_if` keeps it off their wire
    /// format; the Gemini caller re-emits it as the `thoughtSignature`
    /// sibling of the `functionCall` part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// The function details within a tool call message.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolCallFunctionMsg {
    pub name: String,
    pub arguments: String,
}

/// Builder for `ChatMessage` with `role: "tool"`.
#[must_use]
pub fn tool_result_message(tool_call_id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(content.to_string()),
        tool_calls: None,
        tool_call_id: Some(tool_call_id.to_string()),
        thinking_blocks: None,
    }
}

/// Builder for `ChatMessage` with `role: "assistant"` containing tool calls.
#[must_use]
pub fn assistant_tool_call_message(tool_calls: Vec<ToolCallMsg>) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(tool_calls),
        tool_call_id: None,
        thinking_blocks: None,
    }
}

/// Builder for `ChatMessage` with `role: "user"`.
#[must_use]
pub fn user_message(content: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: Some(content.to_string()),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: None,
    }
}

/// Builder for `ChatMessage` with `role: "system"`.
#[must_use]
pub fn system_message(content: &str) -> ChatMessage {
    ChatMessage {
        role: "system".to_string(),
        content: Some(content.to_string()),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: None,
    }
}

/// Constrain the model's output format (e.g., JSON).
///
/// Use [`ResponseFormat::json_object`] to request valid JSON output.
///
/// # Example
///
/// ```
/// # use cosh_sdk::connector::ResponseFormat;
/// let fmt = ResponseFormat::json_object();
/// ```
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

impl ResponseFormat {
    /// Request JSON object mode — the model will be constrained to emit valid JSON.
    #[must_use]
    pub fn json_object() -> Self {
        Self {
            kind: "json_object".into(),
        }
    }
}

/// Describes a tool (function) the model may call.
///
/// Tools let the model request that your code execute a function. Use
/// [`ToolFunction`] to define the function signature, then wrap it in a
/// [`ToolDefinition`] and pass it to [`Connector::with_tools`](crate::connector::Connector::with_tools).
///
/// # Example
///
/// ```
/// # use cosh_sdk::connector::{ToolDefinition, ToolFunction};
/// let tool = ToolDefinition::new(
///     ToolFunction::new("get_weather")
///         .with_description("Get the current weather for a city")
///         .with_parameters(serde_json::json!({
///             "type": "object",
///             "properties": {
///                 "city": { "type": "string" }
///             },
///             "required": ["city"]
///         }))
/// );
/// ```
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolFunction {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<serde_json::Value>,
}

impl ToolFunction {
    /// Create a new function tool with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            parameters: None,
        }
    }

    /// Set a description of what the function does (helps the model decide when to call it).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the JSON Schema describing the function's parameters.
    #[must_use]
    pub fn with_parameters(mut self, parameters: serde_json::Value) -> Self {
        self.parameters = Some(parameters);
        self
    }
}

/// How tool calls are delivered between the model and the harness.
///
/// The two modes are mutually exclusive at the REQUEST level, so the paths
/// can never cross: in [`ToolCallMode::Inline`] the request carries no
/// native `tools` array (the API cannot produce structured tool calls), and
/// in [`ToolCallMode::Native`] the harness never parses text for tool
/// calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ToolCallMode {
    /// Structured tool calls via the provider's native mechanism
    /// (`tool_calls` / `tool_use` / `functionCall` parts). The request
    /// carries the native `tools` array. This is the default — the same
    /// contract the crush agent uses.
    #[default]
    Native,
    /// Inline-JSON tool calls the model writes into its text response
    /// (`{"name": ..., "arguments": ...}`), parsed by the harness
    /// extractor. The request carries NO native `tools`, so structured
    /// tool calls are impossible and the two paths never cross. Meant for
    /// local servers / small models without reliable native function
    /// calling.
    Inline,
}

/// A tool definition sent to the model, wrapping a [`ToolFunction`].
///
/// See [`ToolFunction`] for a full usage example.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    kind: String,
    pub(crate) function: ToolFunction,
}

impl ToolDefinition {
    /// Wrap a [`ToolFunction`] into a tool definition (type defaults to `"function"`).
    #[must_use]
    pub fn new(function: ToolFunction) -> Self {
        Self {
            kind: "function".into(),
            function,
        }
    }
}

/// Internal request parameters accumulated via the builder API.
///
/// Users configure these through [`Connector`](crate::connector::Connector)
/// builder methods rather than constructing this struct directly.
#[derive(Zeroize, Clone, Debug)]
pub struct Parameters {
    #[zeroize(skip)]
    pub(crate) model: Option<String>,
    #[zeroize(skip)]
    pub(crate) max_tokens: Option<u32>,
    #[zeroize(skip)]
    pub(crate) temperature: Option<f32>,
    #[zeroize(skip)]
    pub(crate) top_p: Option<f32>,
    #[zeroize(skip)]
    pub(crate) stop: Option<serde_json::Value>,
    #[zeroize(skip)]
    pub(crate) frequency_penalty: Option<f32>,
    #[zeroize(skip)]
    pub(crate) presence_penalty: Option<f32>,
    #[zeroize(skip)]
    pub(crate) seed: Option<i64>,
    #[zeroize(skip)]
    pub(crate) response_format: Option<ResponseFormat>,
    #[zeroize(skip)]
    pub(crate) logprobs: Option<bool>,
    #[zeroize(skip)]
    pub(crate) top_logprobs: Option<u32>,
    #[zeroize(skip)]
    pub(crate) reasoning_effort: Option<String>,
    #[zeroize(skip)]
    pub(crate) tools: Option<Vec<ToolDefinition>>,
    #[zeroize(skip)]
    pub(crate) tool_choice: Option<serde_json::Value>,
    /// Tool-call delivery mode (see [`ToolCallMode`]). Defaults to `Native`.
    #[zeroize(skip)]
    pub(crate) tool_call_mode: ToolCallMode,
    #[zeroize(skip)]
    pub(crate) user: Option<String>,
    #[zeroize(skip)]
    pub(crate) base_url: Option<String>,

    pub(crate) service_keyring: Option<String>,
    pub(crate) api_key: Option<String>,
}

impl Default for Parameters {
    fn default() -> Self {
        Self {
            model: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            response_format: None,
            logprobs: None,
            top_logprobs: None,
            reasoning_effort: None,
            tools: None,
            tool_choice: None,
            tool_call_mode: ToolCallMode::Native,
            user: None,
            base_url: None,
            // Default to the canonical cosh keyring service so callers only
            // need `with_service_keyring` when they want to override it.
            service_keyring: Some(COSH_SERVICE.to_string()),
            api_key: None,
        }
    }
}
