//! Summarizes a namespace's tool descriptions into a short LLM-readable label
//! via a configured LLM provider. The label is cached by `NamespaceCache` and
//! used as a human-readable key by other agents.

use cosh_sdk::connector::Connector;
use std::sync::LazyLock;
use tokio::runtime::Runtime;

// ── Provider configuration ─────────────────────────────────────────
const PROVIDER: &str = "openai";

// ── Prompt ─────────────────────────────────────────────────────────
const SYSTEM_PROMPT: &str = "\
You produce concise namespace summaries from tool descriptions.
Your entire output must be a single short sentence that a different
LLM can immediately understand — what the namespace provides, in
plain terms. Never list tools. Never add preamble or commentary.";

// ── Runtime ────────────────────────────────────────────────────────
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());

fn block_on<F: std::future::Future<Output = T>, T>(f: F) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(f),
        Err(_) => RUNTIME.block_on(f),
    }
}

/// Summarize a namespace's tools into a short LLM-readable label.
///
/// Calls the configured LLM provider with the concatenated tool
/// descriptions + schemas. On failure the original namespace name is
/// returned as a fallback so the cache pipeline can continue.
#[must_use]
pub fn summarize_namespace(server: &str, namespace: &str, content: &str) -> String {
    let Ok(connector) = Connector::new(PROVIDER) else {
        return format!("{namespace} tools");
    };

    let prompt = format!(
        "Server: {server}\nNamespace: {namespace}\n\nTools:\n{content}\n\n\
         Summarize this namespace in one short sentence."
    );

    match block_on(connector.chat_with_system(&prompt, SYSTEM_PROMPT)) {
        Ok(output) => output.message().to_string(),
        Err(_) => format!("{namespace} tools"),
    }
}
