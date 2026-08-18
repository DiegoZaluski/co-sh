//! Unified multi-provider LLM client.
//!
//! Build a [`Connector`] with a provider name, configure parameters via the
//! builder API, then call [`chat`](Connector::chat),
//! [`stream_chat`](Connector::stream_chat), or [`embed`](Connector::embed).
//!
//! All providers share the same interface — swap the name passed to
//! [`Connector::new`] to switch backends.

pub(crate) mod claude;
pub(crate) mod common;
pub(crate) mod discovery;
pub(crate) mod gemini;
pub(crate) mod openai_compatible;

mod client;
mod error;
mod output;
mod params;
mod provider;

pub use client::Connector;
pub use discovery::{
    ModelReasoning, discover_context_window, effective_context_window, model_reasoning,
    model_reasoning_from_catalog, resolve_reasoning_effort,
};
pub use error::ConnectorError;
pub use output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
pub use params::{
    ChatMessage, ClaudeThinkingBlock, ResponseFormat, ToolCallFunctionMsg, ToolCallMsg,
    ToolDefinition, ToolFunction, assistant_tool_call_message, system_message, tool_result_message,
    user_message,
};
pub use provider::{
    COSH_SERVICE, clear_api_key_cache, detect_provider, get_api_key, get_provider_env_var,
    has_api_key, invalidate_api_key, known_providers, known_providers_with_env,
};

#[cfg(test)]
mod test;
