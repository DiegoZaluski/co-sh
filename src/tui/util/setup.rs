//! Structured user preferences stored in `~/.config/cosh/setup.json`.
//!
//! Each category groups related settings so new fields have a natural home
//! and the on-disk format stays readable. The [`Setup`] struct is the single
//! owner of the file: callers read fields through accessor methods and write
//! changes through [`Setup::save`]. A missing or corrupt file is treated as
//! defaults.

use std::path::PathBuf;

use cosh::mcp::McpConfig;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

// Top-level struct

/// Root object persisted as `~/.config/cosh/setup.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    pub appearance: Appearance,
    pub tools: Tools,
    pub routing: Routing,
    pub hooks: Hooks,
    pub providers: Providers,
    pub model: Model,
    pub cache: Cache,
    /// Master switch for the LSP engine (kept flat: it has no sub-options).
    pub lsp: bool,
    /// Registered MCP servers (empty when the user never added one, so
    /// legacy files without the section keep loading unchanged).
    pub mcp: McpConfig,
    /// Terminal editor used by the file explorer (Ctrl+F). Empty: fall back
    /// to the first available of `nvim`, `vim`, `nano`.
    pub editor: String,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            tools: Tools::default(),
            routing: Routing::default(),
            hooks: Hooks::default(),
            providers: Providers::default(),
            model: Model::default(),
            cache: Cache::default(),
            lsp: true,
            mcp: McpConfig::default(),
            editor: String::new(),
        }
    }
}

// Categories

/// Theme, bell, and visual preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Selected theme name (empty → registry default).
    pub theme: String,
    /// Ring the terminal bell when the agent loop finishes.
    pub bell_enabled: bool,
    /// `/background` toggle: paint the TUI's base background with the
    /// terminal's own default color (`Color::Reset`) instead of the theme
    /// color. Panels/elements keep their theme colors.
    pub transparent_background: bool,
    /// `/anim` toggle: show the animated chat-logo on the empty-session
    /// landing screen. When off, the static `LOGO_CHAT` is drawn instead.
    pub anim_enabled: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: String::new(),
            bell_enabled: true,
            transparent_background: false,
            anim_enabled: true,
        }
    }
}

/// Tool-call mode and disabled tool set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    /// `"native"` or `"inline"`.
    pub tool_call_mode: String,
    /// Tool names the user has disabled (hidden from the model).
    pub disabled: Vec<String>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool_call_mode: "native".to_string(),
            disabled: Vec::new(),
        }
    }
}

/// Model routing / fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Routing {
    /// Ordered fallback entries (provider, model).
    pub fallbacks: Vec<FallbackEntry>,
    /// Explicit summarization chain. Empty uses the active agent model only.
    pub summarization_models: Vec<FallbackEntry>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            summarization_models: Vec::new(),
            fallbacks: crate::fallback::DEFAULT_FALLBACKS
                .iter()
                .map(|&(p, m)| FallbackEntry {
                    provider: p.to_string(),
                    model: m.to_string(),
                })
                .collect(),
        }
    }
}

/// A single provider+model pair in the fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackEntry {
    pub provider: String,
    pub model: String,
}

/// The last model the user selected, restored for NEW sessions
/// (global model persistence).
///
/// The whole struct represents a single selection slot: every time the user
/// picks a model, `model` is overwritten with the new choice, `provider` with
/// the provider that serves it, and `reasoning` with its reasoning effort.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Model {
    /// Provider name of the last selected model (empty when the selection is
    /// `auto` — the fallback chain routes there).
    pub provider: String,
    /// Model id of the last selected model. Empty → no global selection yet.
    pub model: String,
    /// Reasoning effort for the last selected model (`None` = model default).
    pub reasoning: Option<String>,
}

// Cache

/// Prompt-cache preferences.
///
/// The duration is the user's WISH in minutes; the connector layer maps it
/// onto the closest value the target API actually supports (Anthropic only
/// accepts 5m/1h TTLs; OpenAI only the 24h retention), so an arbitrary
/// choice can never produce a rejected request. `0` = provider default.
/// These are per-profile trade-offs (a longer-lived cache costs more per
/// write and only pays off for certain usage patterns), so they are user
/// settings chosen in the Settings screen rather than fixed behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Cache {
    /// Anthropic prompt-cache TTL wish, in minutes (`0` = the 5-minute
    /// default; any value above 5 maps onto the 1h TTL).
    pub anthropic_ttl_min: u32,
    /// OpenAI prompt-cache retention wish, in minutes (`0` = the
    /// model-dependent default; any value above 0 maps onto the 24h
    /// retention).
    pub openai_retention_min: u32,
}

/// Parse a user-entered cache duration into minutes: `45`, `45m`, `1h`,
/// `1h30m`, `90 m` (case-insensitive; a bare number means minutes).
/// `""`, `"0"` and `"default"` mean the provider default (`None`).
pub fn parse_cache_duration(input: &str) -> Result<Option<u32>, String> {
    const INVALID: &str =
        "Invalid duration. Use minutes or hours, e.g. 30m, 1h, 1h30m — or \"default\".";
    let s = input.trim().to_lowercase();
    if s.is_empty() || s == "default" {
        return Ok(None);
    }
    let mut total: u32 = 0;
    let mut num = String::new();
    for ch in s.chars() {
        match ch {
            '0'..='9' => num.push(ch),
            'm' => {
                total = total.saturating_add(parse_duration_segment(&mut num, 1, INVALID)?);
            }
            'h' => {
                total = total.saturating_add(parse_duration_segment(&mut num, 60, INVALID)?);
            }
            ' ' => {}
            _ => return Err(INVALID.to_string()),
        }
    }
    // A trailing bare number means minutes ("45" == "45m").
    if !num.is_empty() {
        total = total.saturating_add(parse_duration_segment(&mut num, 1, INVALID)?);
    }
    if total > 24 * 60 {
        return Err("Maximum cache duration is 24h.".to_string());
    }
    Ok(Some(total))
}

/// Consume the accumulated digits as `value × multiplier` minutes.
fn parse_duration_segment(num: &mut String, multiplier: u32, invalid: &str) -> Result<u32, String> {
    let value: u32 = num.parse().map_err(|_| invalid.to_string())?;
    num.clear();
    Ok(value.saturating_mul(multiplier))
}

/// Human rendering of a duration in minutes: `0` → "default", whole hours →
/// "1h", sub-hour → "45m", mixed → "1h30m".
pub fn format_cache_duration(min: u32) -> String {
    if min == 0 {
        "default".to_string()
    } else if min.is_multiple_of(60) {
        format!("{}h", min / 60)
    } else if min < 60 {
        format!("{min}m")
    } else {
        format!("{}h{:02}m", min / 60, min % 60)
    }
}

// Providers

/// Local model server endpoints configured through the ADD Provider screen.
/// Stored per provider name; used to override the registry's default
/// `localhost` port when building connectors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Providers {
    /// Map of local provider name → endpoint configuration.
    pub local: std::collections::HashMap<String, LocalEndpoint>,
}

/// A single local provider endpoint (`http://host:port`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalEndpoint {
    pub base_url: String,
}

// Hooks

/// A single hook configuration entry (stored in setup.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Friendly display name (falls back to command when empty).
    #[serde(default)]
    pub name: String,
    /// Regex pattern tested against the tool name. Empty = match all.
    #[serde(default)]
    pub matcher: String,
    /// Shell command to execute.
    pub command: String,
    /// Timeout in seconds (default 30).
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Hook lifecycle configuration: one toggle + entry list per event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hooks {
    /// Switch for PreToolUse hooks (on unless the user turns it off).
    ///
    /// `alias = "enabled"` keeps pre-split setups loading: their single
    /// legacy master switch now drives PreToolUse only. Without the alias,
    /// the old `"enabled": <bool>` key would fall into the flattened events
    /// map and reject the whole document.
    #[serde(default = "default_true", alias = "enabled")]
    pub pre_tool_use_enabled: bool,
    /// Switch for PostToolUse hooks (on unless the user turns it off).
    #[serde(default = "default_true")]
    pub post_tool_use_enabled: bool,
    /// Hook configs keyed by event name (e.g. "PreToolUse").
    #[serde(flatten)]
    pub events: std::collections::HashMap<String, Vec<HookEntry>>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            pre_tool_use_enabled: true,
            post_tool_use_enabled: true,
            events: std::collections::HashMap::new(),
        }
    }
}

impl Hooks {
    /// Whether the given event's hooks are switched on. Unknown events
    /// default to on so new sections work without schema changes.
    pub fn is_event_enabled(&self, event: &str) -> bool {
        match event {
            "PreToolUse" => self.pre_tool_use_enabled,
            "PostToolUse" => self.post_tool_use_enabled,
            _ => true,
        }
    }
}

/// Serde default for boolean switches that start enabled.
fn default_true() -> bool {
    true
}

// Persistence

/// Resolve the config directory path (`~/.config/cosh/`).
///
/// `COSH_CONFIG_DIR` overrides the location when set — the test harness
/// uses this because `directories::ProjectDirs` resolves through the
/// platform's known-folders API (on Windows: `APPDATA`/`LOCALAPPDATA`),
/// which IGNORES the `$HOME` redirect that `isolate_home()` performs. A
/// plain `$HOME` override cannot isolate the config on Windows; an explicit
/// path override can.
fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("COSH_CONFIG_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let proj = ProjectDirs::from("", "", "cosh")
        .expect("could not determine project directories (is $HOME set?)");
    proj.config_dir().to_path_buf()
}

/// Resolve the data directory path (`~/.local/share/cosh/` on Unix).
///
/// `COSH_DATA_DIR` overrides the location when set — same rationale as
/// [`config_dir`]'s `COSH_CONFIG_DIR` override.
pub(crate) fn data_dir_override() -> PathBuf {
    if let Ok(dir) = std::env::var("COSH_DATA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let proj = ProjectDirs::from("", "", "cosh")
        .expect("could not determine project directories (is $HOME set?)");
    proj.data_dir().to_path_buf()
}

/// Full path to `setup.json`.
fn setup_path() -> PathBuf {
    config_dir().join("setup.json")
}

impl Setup {
    /// Load from disk. Returns defaults if the file is missing or corrupt.
    pub fn load() -> Self {
        let path = setup_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Persist to disk, creating the config directory if needed.
    pub fn save(&self) {
        let path = setup_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            std::fs::write(&path, json).ok();
        }
    }

    /// The configured base URL for a local provider, if the user saved one
    /// through the ADD Provider screen.
    #[must_use]
    pub fn local_base_url(&self, provider: &str) -> Option<&str> {
        self.providers
            .local
            .get(provider)
            .map(|endpoint| endpoint.base_url.as_str())
    }

    /// Save (or replace) the base URL for a local provider and persist.
    pub fn set_local_base_url(&mut self, provider: &str, base_url: &str) {
        self.providers.local.insert(
            provider.to_string(),
            LocalEndpoint {
                base_url: base_url.to_string(),
            },
        );
        self.save();
    }

    /// Forget a local provider's configured base URL and persist.
    pub fn remove_local_base_url(&mut self, provider: &str) {
        if self.providers.local.remove(provider).is_some() {
            self.save();
        }
    }

    /// The globally persisted model id, if the user ever selected one.
    #[must_use]
    pub fn persisted_model(&self) -> Option<&str> {
        if self.model.model.is_empty() {
            None
        } else {
            Some(&self.model.model)
        }
    }

    /// Record the last model selected by the user and persist it globally.
    ///
    /// The selection is a single overwritten slot (provider + model +
    /// reasoning) — the same shape the per-session header stores.
    pub fn set_model_selection(&mut self, provider: &str, model: &str, reasoning: Option<&str>) {
        self.model.provider = provider.to_string();
        self.model.model = model.to_string();
        self.model.reasoning = reasoning.map(String::from);
        self.save();
    }

    /// Whether the user chose an extended Anthropic prompt-cache TTL (any
    /// wish above the 5-minute default maps onto the 1h TTL — the only
    /// extended value the API supports).
    #[must_use]
    pub fn anthropic_cache_ttl_1h(&self) -> bool {
        self.cache.anthropic_ttl_min > 5
    }

    /// The extended OpenAI prompt-cache retention the user chose
    /// (`None` = the model-dependent default; any wish maps onto the 24h
    /// retention — the only extended value the API supports).
    #[must_use]
    pub fn openai_cache_retention(&self) -> Option<&'static str> {
        (self.cache.openai_retention_min > 0).then_some("24h")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn summarization_routing_defaults_to_agent_and_round_trips_in_order() {
        let legacy: super::Setup = serde_json::from_str(r#"{"routing":{"fallbacks":[]}}"#).unwrap();
        assert!(legacy.routing.summarization_models.is_empty());
        let mut setup = legacy;
        setup.routing.summarization_models = vec![
            super::FallbackEntry {
                provider: "ollama".into(),
                model: "first".into(),
            },
            super::FallbackEntry {
                provider: "openrouter".into(),
                model: "second".into(),
            },
        ];
        let restored: super::Setup =
            serde_json::from_str(&serde_json::to_string(&setup).unwrap()).unwrap();
        assert_eq!(restored.routing.summarization_models[0].model, "first");
        assert_eq!(restored.routing.summarization_models[1].model, "second");
        assert!(restored.routing.fallbacks.is_empty());
    }
    use super::*;

    #[test]
    fn default_setup_is_valid_json() {
        let setup = Setup::default();
        let json = serde_json::to_string_pretty(&setup).unwrap();
        let parsed: Setup = serde_json::from_str(&json).unwrap();
        assert!(parsed.appearance.bell_enabled);
        assert!(!parsed.appearance.transparent_background);
        assert_eq!(parsed.tools.tool_call_mode, "native");
        assert!(parsed.providers.local.is_empty());
        assert_eq!(parsed.cache.anthropic_ttl_min, 0);
        assert_eq!(parsed.cache.openai_retention_min, 0);
    }

    /// Free-form duration parsing: bare minutes, unit suffixes, mixed
    /// forms, defaults — and the invalid inputs that must be rejected.
    #[test]
    fn cache_duration_parse_and_format_roundtrip() {
        assert_eq!(parse_cache_duration(""), Ok(None));
        assert_eq!(parse_cache_duration("default"), Ok(None));
        assert_eq!(parse_cache_duration("0m"), Ok(Some(0)));
        assert_eq!(parse_cache_duration("45"), Ok(Some(45)));
        assert_eq!(parse_cache_duration("45m"), Ok(Some(45)));
        assert_eq!(parse_cache_duration("90 M"), Ok(Some(90)));
        assert_eq!(parse_cache_duration("1h"), Ok(Some(60)));
        assert_eq!(parse_cache_duration("1H30m"), Ok(Some(90)));
        assert_eq!(parse_cache_duration("2 h"), Ok(Some(120)));
        assert!(parse_cache_duration("abc").is_err());
        assert!(parse_cache_duration("1.5h").is_err());
        assert!(parse_cache_duration("25h").is_err());
        assert!(parse_cache_duration("100000000000m").is_err());

        assert_eq!(format_cache_duration(0), "default");
        assert_eq!(format_cache_duration(45), "45m");
        assert_eq!(format_cache_duration(60), "1h");
        assert_eq!(format_cache_duration(90), "1h30m");
        assert_eq!(format_cache_duration(1440), "24h");

        // The mapping accessors: Anthropic maps anything above 5 minutes
        // onto the 1h TTL; OpenAI maps anything above 0 onto 24h.
        let mut setup = Setup::default();
        assert!(!setup.anthropic_cache_ttl_1h());
        assert_eq!(setup.openai_cache_retention(), None);
        setup.cache.anthropic_ttl_min = 30;
        setup.cache.openai_retention_min = 1440;
        assert!(setup.anthropic_cache_ttl_1h());
        assert_eq!(setup.openai_cache_retention(), Some("24h"));
    }

    #[test]
    fn roundtrip_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        setup.appearance.theme = "dracula".to_string();
        setup.appearance.transparent_background = true;
        setup.tools.disabled = vec!["bash".to_string()];
        setup.providers.local.insert(
            "llamacpp".to_string(),
            LocalEndpoint {
                base_url: "http://127.0.0.1:9999".to_string(),
            },
        );

        let json = serde_json::to_string_pretty(&setup).unwrap();
        std::fs::write(&path, &json).unwrap();

        let loaded: Setup = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.appearance.theme, "dracula");
        assert!(loaded.appearance.transparent_background);
        assert_eq!(loaded.tools.disabled, vec!["bash".to_string()]);
        assert_eq!(
            loaded.local_base_url("llamacpp"),
            Some("http://127.0.0.1:9999")
        );
    }

    #[test]
    fn mcp_section_defaults_empty_and_roundtrips() {
        // Legacy files without the section load as "no servers".
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert!(legacy.mcp.servers.is_empty());

        let mut setup = Setup::default();
        setup.mcp.servers.push(cosh::mcp::McpServerEntry {
            name: "docs".to_string(),
            transport: cosh::mcp::McpTransport::Http(cosh::mcp::HttpTransport {
                url: "https://example.com/mcp".to_string(),
                headers: Default::default(),
                api_key_env: None,
                timeout_ms: 1000,
            }),
            enabled: false,
        });
        let json = serde_json::to_string_pretty(&setup).unwrap();
        let loaded: Setup = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mcp.servers.len(), 1);
        assert_eq!(loaded.mcp.servers[0].name, "docs");
        assert!(!loaded.mcp.servers[0].enabled);
    }

    #[test]
    fn hooks_switches_default_on_and_roundtrip_independently() {
        // Missing fields → both events enabled (preserves old configs that
        // only carried the single legacy `enabled` switch).
        let parsed: Setup = serde_json::from_str(r#"{"hooks": {}}"#).unwrap();
        assert!(parsed.hooks.pre_tool_use_enabled);
        assert!(parsed.hooks.post_tool_use_enabled);

        let mut setup = Setup::default();
        setup.hooks.post_tool_use_enabled = false;
        setup.hooks.events.insert(
            "PreToolUse".to_string(),
            vec![HookEntry {
                name: "block rm".to_string(),
                matcher: "bash_run".to_string(),
                command: "exit 2".to_string(),
                timeout: Some(5),
            }],
        );
        let json = serde_json::to_string(&setup);
        let loaded: Setup = serde_json::from_str(&json.unwrap()).unwrap();
        assert!(loaded.hooks.pre_tool_use_enabled);
        assert!(!loaded.hooks.post_tool_use_enabled);
        assert_eq!(loaded.hooks.events["PreToolUse"].len(), 1);
        assert_eq!(loaded.hooks.events["PreToolUse"][0].command, "exit 2");
    }

    /// Unknown keys from older configs (e.g. the removed free-gateway
    /// opt-in) are ignored on load.
    #[test]
    fn unknown_legacy_keys_are_ignored() {
        let legacy: Setup =
            serde_json::from_str(r#"{"providers":{"zen_public_opt_in":true}}"#).unwrap();
        assert!(legacy.providers.local.is_empty());
    }

    /// The global model selection is a single overwritten slot that
    /// round-trips through setup.json, and an absent `model` category in an
    /// old config loads as "no selection yet".
    #[test]
    fn model_selection_is_overwritten_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        assert_eq!(setup.persisted_model(), None);

        // Selecting a model persists provider + model + reasoning.
        setup.set_model_selection("nvidia", "deepseek-ai/deepseek-v4-pro", Some("high"));
        assert_eq!(setup.persisted_model(), Some("deepseek-ai/deepseek-v4-pro"));
        assert_eq!(setup.model.provider, "nvidia");
        assert_eq!(setup.model.reasoning.as_deref(), Some("high"));

        // A later selection OVERWRITES the slot — no history is kept.
        setup.set_model_selection("groq", "openai/gpt-oss-120b", None);
        assert_eq!(setup.persisted_model(), Some("openai/gpt-oss-120b"));
        assert_eq!(setup.model.provider, "groq");
        assert_eq!(setup.model.reasoning, None);

        let json = serde_json::to_string_pretty(&setup).unwrap();
        std::fs::write(&path, &json).unwrap();
        let loaded: Setup = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.persisted_model(), Some("openai/gpt-oss-120b"));
        assert_eq!(loaded.model.provider, "groq");
        assert_eq!(loaded.model.reasoning, None);

        // An old config without the `model` category loads with no selection.
        let legacy: Setup = serde_json::from_str(r#"{"appearance": {}}"#).unwrap();
        assert_eq!(legacy.persisted_model(), None);
    }
}
