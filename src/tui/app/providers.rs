use crate::routes::add_provider::ProviderEntry;
use crate::ui::dialogs::DialogType;

use super::App;

impl App {
    /// The configured base URL for a provider, if the user saved one for a
    /// local provider in setup.json. Normalized so OpenAI-compatible servers
    /// get the `/v1` suffix their API exposes.
    #[must_use]
    pub(super) fn base_url_for(&self, provider: &str) -> Option<String> {
        self.setup
            .local_base_url(provider)
            .map(|url| cosh_sdk::connector::normalize_local_base_url(provider, url))
    }

    /// Map of provider → base URL for every local provider the user configured
    /// in setup.json. Used to build fallback connectors inside the harness.
    #[must_use]
    pub(super) fn configured_local_base_urls(&self) -> std::collections::HashMap<String, String> {
        self.setup
            .providers
            .local
            .iter()
            .map(|(provider, endpoint)| {
                (
                    provider.clone(),
                    cosh_sdk::connector::normalize_local_base_url(provider, &endpoint.base_url),
                )
            })
            .collect()
    }

    /// Providers whose models are eligible for the model picker.
    ///
    /// Cloud providers qualify when an API key is available. Local providers
    /// qualify when configured in setup.json OR when they respond on their
    /// default port — the model list probes every local provider and surfaces
    /// whatever answers, so a llama.cpp / ollama / vLLM server just needs to be
    /// running. Same-port servers (llamacpp/llamafile/localai all default to
    /// 8080, text-generation-webui/tabbyapi to 5000) are probed once: the
    /// first provider (registry order) represents the shared endpoint.
    #[must_use]
    pub(super) fn active_providers(&self) -> Vec<&'static str> {
        use cosh_sdk::connector::{
            get_provider, is_local_provider, known_local_providers, known_providers_with_env,
        };

        let mut active: Vec<&'static str> = Vec::new();
        for (provider, _) in known_providers_with_env() {
            if !is_local_provider(provider) && cosh_sdk::connector::has_api_key(provider) {
                active.push(provider);
            }
        }

        // Local providers: configured URL wins; otherwise the registry's
        // default endpoint. Probe each distinct endpoint only once.
        let mut endpoints: std::collections::HashSet<String> = std::collections::HashSet::new();
        for provider in known_local_providers() {
            let endpoint = if let Some(cfg) = self.setup.local_base_url(provider) {
                cosh_sdk::connector::normalize_local_base_url(provider, cfg)
            } else {
                get_provider(provider).map_or_else(String::new, |cfg| cfg.base_url.to_string())
            };
            if endpoints.insert(endpoint) {
                active.push(provider);
            }
        }
        active
    }

    /// Open the right credential dialog for a provider: local providers ask
    /// for a server URL (saved to setup.json); cloud providers ask for an API
    /// key (stored in the OS keyring).
    pub(super) fn open_provider_dialog(&mut self, entry: &ProviderEntry) {
        if entry.local {
            self.dialog.show(DialogType::LocalUrlInput {
                provider: entry.name.to_string(),
                input: self
                    .setup
                    .local_base_url(entry.name)
                    .map_or_else(String::new, str::to_string),
                cursor_pos: 0,
            });
        } else {
            self.dialog.show(DialogType::ApiKeyInput {
                provider: entry.name.to_string(),
                env_var: entry.hint.clone(),
                input: String::new(),
                cursor_pos: 0,
            });
        }
    }
}

/// Persist a provider API key in the OS credential store (keyring) under the
/// `cosh` service keyed by the provider's API key environment variable name.
///
/// The key is stored via the native store (Secret Service on Linux, Keychain
/// on macOS, Credential Manager on Windows). Errors are returned to the caller
/// so it can surface them instead of failing silently.
pub(super) fn save_provider_api_key(env_var: &str, api_key: &str) -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(cosh_sdk::connector::COSH_SERVICE, env_var)?;
    entry.set_password(api_key)?;
    Ok(())
}

/// Whether a string looks like a usable local server URL: starts with
/// `http://` or `https://` and has a non-empty host.
#[must_use]
pub(super) fn is_valid_local_url(url: &str) -> bool {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"));
    rest.is_some_and(|host| !host.is_empty())
}
