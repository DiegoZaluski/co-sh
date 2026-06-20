//! Unified multi-provider LLM client.
//!
//! Build a [`Connector`] with a provider name, configure parameters via the
//! builder API, then call [`chat`](Connector::chat),
//! [`stream_chat`](Connector::stream_chat), or [`embed`](Connector::embed).
//!
//! All providers share the same interface — swap the name passed to
//! [`Connector::new`] to switch backends.

pub(crate) mod openai_compatible;

mod client;
mod error;
mod params;
mod provider;

pub use client::Connector;
pub use error::ConnectorError;
pub use params::{ResponseFormat, ToolDefinition, ToolFunction};

#[cfg(test)]
mod test;
