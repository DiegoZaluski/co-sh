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
pub(crate) mod gemini;
pub(crate) mod openai_compatible;

mod client;
mod error;
mod output;
mod params;
mod provider;

pub use client::Connector;
pub use error::ConnectorError;
pub use output::{ChatOutput, ChatStream, LsOutput, ModelInfo, StreamChunk};
pub use params::{ResponseFormat, ToolDefinition, ToolFunction};
pub use provider::{detect_provider, get_provider_env_var, known_providers, known_providers_with_env};

#[cfg(test)]
mod test;
