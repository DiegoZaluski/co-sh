use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;
use cosh::harness::HarnessEvent;

impl App {
    pub(in crate::app) fn open_model_dialog(&mut self) {
        use cosh::ModelEntry;
        use cosh_sdk::connector::Connector;

        let current = self.llm_config.model.clone().unwrap_or_default();

        // Store the current model (and reasoning) so we can restore on cancel
        self.model_dialog_original = Some(current.clone());
        self.reasoning_dialog_original = self.llm_config.reasoning.clone();

        // Collect providers and check if their API key is still configured.
        // If a provider has no key (env var or keyring), invalidate its cache
        // entry so stale models don't appear as available options. Local
        // providers qualify when configured in setup.json or when they
        // respond on their default port (probed below); unqualified ones are
        // purged too.
        let providers_to_check: Vec<&'static str> = self.active_providers();
        {
            use cosh_sdk::connector::{is_local_provider, known_providers_with_env};
            for (provider, _) in known_providers_with_env() {
                if !is_local_provider(provider) && !providers_to_check.contains(&provider) {
                    // Provider no longer configured — purge cached models
                    self.model_cache.invalidate(&provider.to_string());
                }
            }
            for provider in cosh_sdk::connector::known_local_providers() {
                if !providers_to_check.contains(&provider) {
                    self.model_cache.invalidate(&provider.to_string());
                }
            }
        }

        // Try to populate the dialog from cache first (instant, no network)
        let mut cached_models: Vec<ModelEntry> = Vec::new();
        for &provider in &providers_to_check {
            if let Some(models) = self.model_cache.get(&provider.to_string()) {
                cached_models.extend(models.iter().cloned());
            }
        }
        // Deduplicate cached models as a safety net — the cache should
        // already be unique after update_model_cache runs, but this
        // protects against stale on-disk data from older versions.
        {
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            cached_models.retain(|m| seen.insert((m.provider.clone(), m.model.clone())));
        }

        let auto_entry = ModelEntry {
            provider: String::new(),
            model: "auto".to_string(),
        };
        let auto_current = if current == "auto" {
            current.clone()
        } else {
            String::new()
        };
        if !cached_models.is_empty() {
            let mut models_with_auto = vec![auto_entry];
            models_with_auto.extend(cached_models);
            self.dialog.replace(DialogType::ModelList {
                models: models_with_auto,
                current: auto_current,
                filter: String::new(),
            });
        } else {
            self.dialog.replace(DialogType::ModelList {
                models: vec![auto_entry],
                current: auto_current,
                filter: String::new(),
            });
        }

        // Check if any provider is already being revalidated — if so,
        // a background fetch is already in progress, skip spawning another.
        let any_revalidating = providers_to_check
            .iter()
            .any(|p| self.model_cache.is_revalidating(&p.to_string()));

        if !any_revalidating {
            // Mark all providers as revalidating to prevent redundant fetches
            for &provider in &providers_to_check {
                self.model_cache.start_revalidation(provider.to_string());
            }

            // Always revalidate in background (stale-while-revalidate)
            let dialog_tx = self.event_tx.clone();
            let dialog_tx_clone = dialog_tx;
            let base_urls = self.configured_local_base_urls();

            self.tokio_handle.spawn(async move {
                let mut all_models: Vec<ModelEntry> = Vec::new();

                for provider in providers_to_check {
                    if let Ok(mut connector) = Connector::new(provider) {
                        if let Some(url) = base_urls.get(provider) {
                            connector = connector.with_base_url(url.clone());
                        }
                        if let Ok(output) = connector.list_models().await {
                            for model_info in output.models() {
                                all_models.push(ModelEntry {
                                    provider: provider.to_string(),
                                    model: model_info.id().to_string(),
                                });
                            }
                        }
                    }
                }

                let _ = dialog_tx_clone.send(HarnessEvent::ModelsLoaded {
                    models: all_models,
                    current,
                });
            });
        }
    }

    /// Group models by provider and update the cache for each provider.
    pub(in crate::app) fn update_model_cache(&mut self, models: &[cosh::ModelEntry]) {
        use cosh::ModelEntry;
        use std::collections::HashMap;
        use std::collections::HashSet;

        // Group by provider
        let mut grouped: HashMap<&str, Vec<ModelEntry>> = HashMap::new();
        for entry in models {
            grouped
                .entry(entry.provider.as_str())
                .or_default()
                .push(entry.clone());
        }

        // Deduplicate models per provider: some APIs may return the same
        // model ID multiple times (Mistral, transient API glitches, etc.).
        // Using a per-provider HashSet avoids O(n²) on each Vec.
        for provider_models in grouped.values_mut() {
            let mut seen: HashSet<String> = HashSet::new();
            provider_models.retain(|m| seen.insert(m.model.clone()));
        }

        // Update cache for each provider with results
        for (provider, provider_models) in grouped {
            self.model_cache
                .finish_revalidation(provider.to_string(), provider_models);
        }

        // Providers still in the revalidation set failed or returned nothing
        // (expired API key, network error, etc.). Invalidate their cache so
        // stale models don't appear as available options.
        let failed: Vec<String> = self.model_cache.drain_revalidation();
        for provider in &failed {
            self.model_cache.invalidate(provider);
        }
    }

    pub(in crate::app) fn is_model_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ModelList { .. })
            )
    }

    pub(in crate::app) fn handle_model_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_model_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                // Build flat entries in the same grouped-by-provider order as the render
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    let new_selected = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if flat_entries.is_empty() {
                            None
                        } else {
                            let old = d.selected;
                            let next = if old == 0 || old >= flat_entries.len() {
                                flat_entries.len() - 1
                            } else {
                                old - 1
                            };
                            Some(next)
                        }
                    };
                    if let Some(ns) = new_selected
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        d_mut.selected = ns;
                    }
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                {
                    // Compute new selection in a separate scope so the borrow drops
                    // before calling current_mut().
                    let new_selected = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if flat_entries.is_empty() {
                            None
                        } else {
                            let old = d.selected;
                            let next = if old + 1 >= flat_entries.len() {
                                0
                            } else {
                                old + 1
                            };
                            Some(next)
                        }
                    };
                    if let Some(ns) = new_selected
                        && let Some(d_mut) = self.dialog.current_mut()
                    {
                        d_mut.selected = ns;
                    }
                }
                true
            }
            KeyCode::Enter => {
                if let Some(d) = self.dialog.current()
                    && let DialogType::ModelList { models, filter, .. } = &d.dialog_type
                    && !models.is_empty()
                {
                    let selection = {
                        let flat_entries = Self::model_dialog_flat_entries(models, filter);
                        if !flat_entries.is_empty() {
                            let selected_idx = d.selected.min(flat_entries.len().saturating_sub(1));
                            let entry = flat_entries[selected_idx];
                            Some((entry.model.clone(), entry.provider.clone()))
                        } else {
                            None
                        }
                    };
                    if let Some((model, provider)) = selection {
                        self.confirm_model_entry(&model, &provider);
                    }
                }
                true
            }
            KeyCode::Esc => {
                // Restore original model + reasoning
                self.restore_model_dialog();
                true
            }
            KeyCode::Backspace => {
                let Some(d) = self.dialog.current_mut() else {
                    return true;
                };
                let DialogType::ModelList { filter, .. } = &mut d.dialog_type else {
                    return true;
                };
                filter.pop();
                d.selected = 0;
                d.cursor.note_activity();
                true
            }
            KeyCode::Char(ch) => {
                self.model_dialog_push_filter(ch);
                true
            }
            _ => false,
        }
    }

    /// Apply a picked model — or, when the model supports configurable
    /// reasoning, push the reasoning sub-dialog on top of the model list.
    pub(in crate::app) fn confirm_model_entry(&mut self, model: &str, provider: &str) {
        if model == "auto" {
            // Auto mode may land on ANY fallback model, so it offers the
            // standard effort set; the choice is re-mapped onto the closest
            // level each fallback model actually accepts (both here and on
            // every harness fallback switch via `resolve_reasoning_effort`).
            let current = self.llm_config.reasoning.clone().unwrap_or_default();
            let levels = vec![
                "default".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ];
            let start = levels.iter().position(|l| l == &current).unwrap_or(0);
            self.dialog.show(DialogType::ReasoningList {
                model: model.to_string(),
                provider: provider.to_string(),
                levels,
                current,
            });
            if let Some(inst) = self.dialog.current_mut() {
                inst.selected = start;
            }
            return;
        }

        if crate::config::model_supports_reasoning(model) {
            let current = self.llm_config.reasoning.clone().unwrap_or_default();
            let levels = crate::config::model_reasoning_levels(model);
            let start = levels.iter().position(|l| l == &current).unwrap_or(0);
            // Push the sub-dialog FIRST, then set its initial selection —
            // mutating the ModelList's `selected` (the current top before
            // the push) would corrupt the model highlight after Esc.
            self.dialog.show(DialogType::ReasoningList {
                model: model.to_string(),
                provider: provider.to_string(),
                levels,
                current,
            });
            if let Some(inst) = self.dialog.current_mut() {
                inst.selected = start;
            }
        } else {
            // Keep the previous reasoning level — the new model either
            // ignores it or uses it; user can change it from the dialog.
            let reasoning = self.llm_config.reasoning.clone();
            self.commit_model_selection(model, provider, reasoning.as_deref());
            self.model_dialog_original = None;
            self.reasoning_dialog_original = None;
            self.dialog.pop();
        }
    }

    /// Restore the model + reasoning that were active when the dialog opened.
    pub(in crate::app) fn restore_model_dialog(&mut self) {
        if let Some(ref orig) = self.model_dialog_original {
            self.llm_config.model = if orig.is_empty() {
                None
            } else {
                Some(orig.clone())
            };
        }
        if let Some(ref orig) = self.reasoning_dialog_original {
            self.llm_config.reasoning = if orig.is_empty() {
                None
            } else {
                Some(orig.clone())
            };
        }
        self.model_dialog_original = None;
        self.reasoning_dialog_original = None;
        self.dialog.pop();
    }

    /// Apply a (model, provider, reasoning) selection: update the live LLM
    /// config, persist the selection globally in `setup.json`, and record it
    /// on the current session so its JSONL header carries it. This is the
    /// single write path shared by every model selection in the UI.
    pub(in crate::app) fn commit_model_selection(
        &mut self,
        model: &str,
        provider: &str,
        reasoning: Option<&str>,
    ) {
        self.set_llm_model(
            model.to_owned(),
            provider.to_owned(),
            reasoning.map(String::from),
        );
        self.setup.set_model_selection(provider, model, reasoning);
        self.record_model_on_current_session();
        // Flush the session header right away so the selection survives the
        // window before the next ContextSnapshot (~10 s) or Done/Stopped save.
        if let Some(session) = self.state.current_session() {
            self.session_store.save_session_async(session);
        }
    }

    /// Point the active LLM config at a model selection. Owned values: the
    /// callers have already copied them out of `self` to release the borrow,
    /// so the copies are moved in rather than cloned a second time.
    pub(in crate::app) fn set_llm_model(
        &mut self,
        model: String,
        provider: String,
        reasoning: Option<String>,
    ) {
        self.llm_config.model = if model.is_empty() { None } else { Some(model) };
        self.llm_config.provider = provider;
        self.llm_config.reasoning = reasoning;
    }

    /// Record the active model selection on the current session so it is
    /// persisted in the session's JSONL header on the next save.
    pub(in crate::app) fn record_model_on_current_session(&mut self) {
        if let Some(session) = self.state.current_session_mut() {
            session.provider = if self.llm_config.provider.is_empty() {
                None
            } else {
                Some(self.llm_config.provider.clone())
            };
            session.model = self.llm_config.model.clone();
            session.reasoning = self.llm_config.reasoning.clone();
        }
    }

    /// Restore the globally persisted model (the last one the user selected)
    /// into the active LLM config. Used when entering a brand-new session; a
    /// stored selection wins over the env-var defaults.
    pub(in crate::app) fn apply_global_model(&mut self) {
        let Some(model) = self.setup.persisted_model().map(String::from) else {
            return;
        };
        let provider = self.setup.model.provider.clone();
        let reasoning = self.setup.model.reasoning.clone();
        self.set_llm_model(model, provider, reasoning);
        self.record_model_on_current_session();
    }

    /// Restore the model recorded on the current session into the active LLM
    /// config. Used when switching back to a session so the last model used
    /// there comes back. A session without a recorded selection leaves the
    /// active config untouched — before this feature, switching sessions
    /// never mutated the config, so that legacy behavior is preserved.
    pub(in crate::app) fn restore_current_session_model(&mut self) {
        let Some(session) = self.state.current_session() else {
            return;
        };
        let (provider, model, reasoning) = (
            session.provider.clone(),
            session.model.clone(),
            session.reasoning.clone(),
        );
        match (provider, model) {
            // A recorded explicit selection fully replaces the active config —
            // model, provider AND reasoning (the reasoning could legitimately
            // be `None`, which must clear a leftover effort from elsewhere).
            (Some(provider), Some(model)) => {
                self.set_llm_model(model, provider, reasoning);
            }
            // A recorded `auto` selection has no provider to pin — the
            // fallback chain stays the source of truth, so clear any leftover
            // provider from the previously active config.
            (None, Some(model)) if model == "auto" => {
                self.set_llm_model(model, String::new(), reasoning);
            }
            // No usable recorded selection: either the session never had one,
            // or the header only derived a bare model id without a provider
            // (no `/`). Best-effort fallback for slash'd legacy headers in
            // the `(Some, Some)` arm above; here we cannot pin a provider
            // confidently, so the active config stays as-is.
            _ => {}
        }
    }

    pub(in crate::app) fn is_reasoning_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ReasoningList { .. })
            )
    }

    pub(in crate::app) fn handle_reasoning_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_reasoning_dialog_visible() {
            return false;
        }

        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ReasoningList { levels, .. } = &d.dialog_type
                    && !levels.is_empty()
                {
                    d.selected = if d.selected == 0 {
                        levels.len() - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::ReasoningList { levels, .. } = &d.dialog_type
                    && !levels.is_empty()
                {
                    d.selected = (d.selected + 1) % levels.len();
                }
                true
            }
            KeyCode::Enter => {
                let (model, provider, level) = {
                    let Some(d) = self.dialog.current() else {
                        return true;
                    };
                    let DialogType::ReasoningList {
                        model,
                        provider,
                        levels,
                        ..
                    } = &d.dialog_type
                    else {
                        return true;
                    };
                    let idx = d.selected.min(levels.len().saturating_sub(1));
                    (model.clone(), provider.clone(), levels[idx].clone())
                };
                let reasoning = if level == "default" {
                    None
                } else {
                    Some(level.as_str())
                };
                self.commit_model_selection(&model, &provider, reasoning);
                self.model_dialog_original = None;
                self.reasoning_dialog_original = None;
                // Pop both the reasoning sub-dialog and the model list.
                self.dialog.pop();
                self.dialog.pop();
                true
            }
            KeyCode::Esc => {
                // Back to the model list (nothing applied yet).
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Add a character to the model filter and reset selection
    pub(in crate::app) fn model_dialog_push_filter(&mut self, ch: char) {
        if let Some(d) = self.dialog.current_mut() {
            if let DialogType::ModelList { filter, .. } = &mut d.dialog_type {
                filter.push(ch);
            }
            d.selected = 0;
            d.cursor.note_activity();
        }
    }

    pub(in crate::app) fn collect_cached_models(&self) -> Vec<cosh::ModelEntry> {
        let mut models = Vec::new();
        for provider in self.active_providers() {
            if let Some(cached) = self.model_cache.get(&provider.to_string()) {
                models.extend(cached.iter().cloned());
            }
        }
        models
    }

    /// Compute the flat list of models in the same grouped-by-provider order
    /// used by the dialog render, so navigation and rendering stay in sync.
    pub(in crate::app) fn model_dialog_flat_entries<'a>(
        models: &'a [cosh::ModelEntry],
        filter: &str,
    ) -> Vec<&'a cosh::ModelEntry> {
        use std::collections::BTreeMap;
        let mut grouped: BTreeMap<String, Vec<&'a cosh::ModelEntry>> = BTreeMap::new();
        for entry in models {
            if crate::ui::dialogs::model_entry_matches_filter(entry, filter) {
                grouped
                    .entry(entry.provider.clone())
                    .or_default()
                    .push(entry);
            }
        }
        grouped.values().flatten().copied().collect()
    }
}
