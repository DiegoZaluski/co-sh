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
pub(crate) mod openai;
pub(crate) mod openai_compatible;

mod client;
mod error;
mod output;
mod params;
mod provider;

pub mod retry;

pub use client::Connector;
pub use discovery::{
    ModelReasoning, discover_context_window, effective_context_window, lookup_pricing,
    model_pricing, model_reasoning, model_reasoning_from_catalog, refresh_pricing_catalog,
    resolve_reasoning_effort,
};
pub use error::ConnectorError;
pub use output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
pub use provider::Family;
// Re-exported so `Connector::token_usage` consumers name the type without
// reaching into the private claude module path.
mod usage;
pub use params::{
    ChatMessage, ClaudeThinkingBlock, ResponseFormat, ToolCallFunctionMsg, ToolCallMode,
    ToolCallMsg, ToolDefinition, ToolFunction, assistant_tool_call_message, system_message,
    tool_result_message, user_message,
};
pub use provider::{
    COSH_SERVICE, OPENCODE_GO_PROVIDER, OPENCODE_ZEN_PROVIDER, ZEN_FREE_MODELS, ZEN_PROVIDER,
    ZEN_PUBLIC_KEY, clear_api_key_cache, detect_provider, get_api_key, get_provider,
    get_provider_env_var, has_api_key, invalidate_api_key, is_local_provider, is_opencode_gateway,
    is_zen_free_model, known_local_providers, known_providers, known_providers_with_env,
    normalize_local_base_url, set_zen_public_tier_enabled, zen_public_tier_enabled,
};
pub use usage::{Pricing, TokenUsage};

#[cfg(test)]
mod test;
