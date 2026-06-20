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
#[derive(Default, Debug)]
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


