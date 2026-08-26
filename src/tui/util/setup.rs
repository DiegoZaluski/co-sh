//! Structured user preferences stored in `~/.config/cosh/setup.json`.
//!
//! Each category groups related settings so new fields have a natural home
//! and the on-disk format stays readable. The [`Setup`] struct is the single
//! owner of the file: callers read fields through accessor methods and write
//! changes through [`Setup::save`]. A missing or corrupt file is treated as
//! defaults.

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

// Top-level struct

/// Root object persisted as `~/.config/cosh/setup.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Setup {
    pub appearance: Appearance,
    pub tools: Tools,
    pub routing: Routing,
    pub hooks: Hooks,
    pub providers: Providers,
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
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: String::new(),
            bell_enabled: true,
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
}

impl Default for Routing {
    fn default() -> Self {
        Self {
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

// Providers

/// Local model server endpoints configured through the ADD Provider screen.
/// Stored per provider name; used to override the registry's default
/// `localhost` port when building connectors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Providers {
    /// Map of local provider name → endpoint configuration.
    pub local: std::collections::HashMap<String, LocalEndpoint>,
    /// One-time answer to the OpenCode Zen free-gateway prompt: `true` = the
    /// user opted in (anonymous free tier allowed when no API key is set),
    /// `false` = declined, `None` = never asked. Write-once from the UI: the
    /// prompt is shown exactly once and NEVER re-asked afterwards — the only
    /// way back is deleting this field from setup.json by hand.
    pub zen_public_opt_in: Option<bool>,
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
fn config_dir() -> PathBuf {
    let proj = ProjectDirs::from("", "", "cosh")
        .expect("could not determine project directories (is $HOME set?)");
    proj.config_dir().to_path_buf()
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

    /// The persisted answer to the one-time OpenCode Zen free-gateway
    /// prompt, if the user already answered it (`None` = never asked).
    #[must_use]
    pub const fn zen_public_opt_in(&self) -> Option<bool> {
        self.providers.zen_public_opt_in
    }

    /// Record the user's FINAL answer to the free-gateway prompt and
    /// persist. Deliberately write-once: a stored answer is never flipped by
    /// later calls, so the prompt can only come back by removing the field
    /// from setup.json manually.
    pub fn record_zen_public_opt_in(&mut self, opted_in: bool) {
        if self.providers.zen_public_opt_in.is_none() {
            self.providers.zen_public_opt_in = Some(opted_in);
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_setup_is_valid_json() {
        let setup = Setup::default();
        let json = serde_json::to_string_pretty(&setup).unwrap();
        let parsed: Setup = serde_json::from_str(&json).unwrap();
        assert!(parsed.appearance.bell_enabled);
        assert_eq!(parsed.tools.tool_call_mode, "native");
        assert!(parsed.providers.local.is_empty());
    }

    #[test]
    fn roundtrip_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        setup.appearance.theme = "dracula".to_string();
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
        assert_eq!(loaded.tools.disabled, vec!["bash".to_string()]);
        assert_eq!(
            loaded.local_base_url("llamacpp"),
            Some("http://127.0.0.1:9999")
        );
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

    /// The Zen free-gateway answer is write-once from the UI: the first
    /// `record_zen_public_opt_in` call persists, later calls are no-ops, and
    /// the field round-trips through setup.json.
    #[test]
    fn zen_opt_in_is_write_once_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.json");

        let mut setup = Setup::default();
        assert_eq!(setup.zen_public_opt_in(), None);

        setup.record_zen_public_opt_in(true);
        assert_eq!(setup.zen_public_opt_in(), Some(true));
        // A second call must NOT flip the recorded answer.
        setup.record_zen_public_opt_in(false);
        assert_eq!(setup.zen_public_opt_in(), Some(true));

        let json = serde_json::to_string_pretty(&setup).unwrap();
        std::fs::write(&path, &json).unwrap();
        let loaded: Setup = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.zen_public_opt_in(), Some(true));

        // An old config without the field still loads (defaults to None).
        let legacy: Setup = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.zen_public_opt_in(), None);
    }
}
