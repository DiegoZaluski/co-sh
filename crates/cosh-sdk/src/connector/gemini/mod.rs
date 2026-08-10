mod caller;
pub mod tokens;

pub use caller::{chat, chat_stream, chat_stream_with_messages, embed, list_models};
pub use tokens::extract_tokens;

/// Gemini API surface version used for every request.
///
/// `v1` is the stable, production-ready API. `v1beta` exposes experimental
/// features. This is an INTERNAL switch — library users cannot select the
/// version through the public `Connector` API. Flip it to `"v1beta"` only
/// to test unstable Gemini features locally; keep it `"v1"` in production.
///
/// The provider registry pins the stable `v1` base URL; the caller
/// re-writes the version segment with this constant so the switch is the
/// single source of truth.
pub(crate) const API_VERSION: &str = "v1";
