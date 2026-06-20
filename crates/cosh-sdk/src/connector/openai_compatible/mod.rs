//! OpenAI-compatible API caller — shared implementation for every provider
//! in the `OpenAICompatible` family.
//!
//! All three operations (`chat`, `chat_stream`, `embed`) follow the same
//! pattern: build a `reqwest` request from the shared `Parameters`, send
//! it to the provider's base URL, and parse the OpenAI-shaped response.

pub(crate) mod caller;

pub(crate) use caller::{chat, chat_stream, embed};
