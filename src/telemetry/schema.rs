//! Schema constants and closed enums for the telemetry module.
//!
//! Everything that leaves the client must come from this file (or be derived
//! through [`crate::telemetry::sanitize`]). No free-form strings cross the
//! boundary: providers, features and error categories are closed enums, and
//! unknown values are hashed instead of shipped.

/// Bump when event payloads change semantics. Additive-only within a version:
/// new fields may appear, removed fields become `null`, meaning changes = bump.
/// The ingest accepts `SCHEMA_VERSION` down to `SCHEMA_MIN_SUPPORTED_VERSION`.
pub const SCHEMA_VERSION: u16 = 1;

/// Oldest schema version the ingest is expected to keep accepting.
pub const SCHEMA_MIN_SUPPORTED_VERSION: u16 = 1;

/// Event types. Closed set: adding a variant is a schema decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Install,
    SessionSummary,
    Error,
    Crash,
    Update,
}

impl EventType {
    /// Wire name of the event type (also used for queue filenames).
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Install => "install",
            EventType::SessionSummary => "session_summary",
            EventType::Error => "error",
            EventType::Crash => "crash",
            EventType::Update => "update",
        }
    }
}

/// Closed set of LLM providers, mirroring the SDK's allowlist. Any provider
/// name not in this list is hashed by [`crate::telemetry::sanitize`] before it
/// can enter an event — a custom provider name can embed identity (company,
/// personal name, internal host), exactly like a filesystem path.
pub const PROVIDER_ALLOWLIST: &[&str] = &[
    "openai",
    "claude",
    "gemini",
    "groq",
    "mistral",
    "deepseek",
    "together",
    "ollama",
    "openrouter",
    "xai",
    "opencode",
];

/// Error categories. Closed set — an error is represented by its category plus
/// a fingerprint of its normalized message, never by the raw message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ProviderAuth,
    ProviderRateLimit,
    ProviderServer,
    ProviderNetwork,
    ContextOverflow,
    ToolFailed,
    FsIo,
    RenderPanic,
    Panic,
    Unknown,
}

impl ErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCategory::ProviderAuth => "provider_auth",
            ErrorCategory::ProviderRateLimit => "provider_rate_limit",
            ErrorCategory::ProviderServer => "provider_server",
            ErrorCategory::ProviderNetwork => "provider_network",
            ErrorCategory::ContextOverflow => "context_overflow",
            ErrorCategory::ToolFailed => "tool_failed",
            ErrorCategory::FsIo => "fs_io",
            ErrorCategory::RenderPanic => "render_panic",
            ErrorCategory::Panic => "panic",
            ErrorCategory::Unknown => "unknown",
        }
    }
}

/// A validated, app-internal error source identifier (closed
/// allowlist, not a charset validator — a syntactic check still lets a
/// private project name like `acme::confidential_merger` through).
/// Construct only through [`ErrorSource::validate`]; anything outside the
/// allowlist is rejected (fail closed).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct ErrorSource(String);

/// Closed set of telemetry-visible internal modules (wire names). Adding a
/// module is a deliberate, reviewed change — exactly like the provider and
/// tool allowlists.
pub const SOURCE_ALLOWLIST: &[&str] = &["harness::core", "harness::a", "harness::b"];

impl ErrorSource {
    /// Accept ONLY a name on [`SOURCE_ALLOWLIST`]. The value is
    /// app-internal by construction: no path, URL, email or arbitrary error
    /// text can ever enter `source`, even from a caller mistake.
    pub fn validate(raw: &str) -> Option<Self> {
        SOURCE_ALLOWLIST
            .contains(&raw.trim())
            .then(|| Self(raw.trim().to_string()))
    }

    /// Validated source string (never user content by construction).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Test-only: an over-long filler source used to exercise size caps.
    /// NOT on [`SOURCE_ALLOWLIST`], so it cannot be produced by `validate` —
    /// tests use it only to inflate serialized size before the cap check.
    #[doc(hidden)]
    pub fn test_filler(len: usize) -> Self {
        let mut s = String::new();
        while s.len() < len {
            s.push_str("a::");
        }
        Self(s)
    }
}

/// Feature wire names (mirror of `Feature::as_str`), for re-validation of
/// deserialized aggregate-map keys.
pub const FEATURE_ALLOWLIST: &[&str] = &[
    "session", "home", "settings", "tools", "rag", "add_provider", "prompt", "other",
];

/// Features (routes, dialogs, commands) instrumented in the TUI. Closed set;
/// anything new must be added here before it can be counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Session,
    Home,
    Settings,
    Tools,
    Rag,
    AddProvider,
    Prompt,
    Other,
}

impl Feature {
    pub fn as_str(&self) -> &'static str {
        match self {
            Feature::Session => "session",
            Feature::Home => "home",
            Feature::Settings => "settings",
            Feature::Tools => "tools",
            Feature::Rag => "rag",
            Feature::AddProvider => "add_provider",
            Feature::Prompt => "prompt",
            Feature::Other => "other",
        }
    }
}

/// Known tool names (allowlist). Unknown tool names are NOT shipped raw — they
/// fall into the `other` bucket, since a tool name can embed project context.
pub const TOOL_ALLOWLIST: &[&str] = &[
    "bash", "fs_read", "fs_write", "fs_edit", "find_glob", "find_grep", "plan", "skills",
    "web_search", "web_fetch", "lsp", "recall", "subagent", "question",
];
