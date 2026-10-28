use super::super::App;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;
use cosh::harness::HarnessEvent;

impl App {
    pub(in crate::app) fn open_summarization_model_dialog(&mut self, index: Option<usize>) {
        self.open_model_dialog();
        self.summarization_model_edit = Some(index);
        self.model_dialog_original = None;
        self.reasoning_dialog_original = None;
        if let Some(d) = self.dialog.current_mut()
            && let DialogType::ModelList {
                models, current, ..
            } = &mut d.dialog_type
        {
            models.retain(|entry| entry.model != "auto" && !entry.provider.is_empty());
            current.clear();
        }
    }

    pub(in crate::app) fn open_model_dialog(&mut self) {
        self.summarization_model_edit = None;
        self.pane_model_edit = None;
        let current = self.llm_config.model.clone().unwrap_or_default();

        // Store the current model (and reasoning) so we can restore on cancel
        self.model_dialog_original = Some(current.clone());
        self.reasoning_dialog_original = self.llm_config.reasoning.clone();
        self.open_model_dialog_with_current(current);
    }

    /// The pane-scoped variant: the picker serves ONE focused cosh pane.
    /// The machinery below (provider checks, cache, background
    /// revalidation) is shared with the global picker, but the commit lands
    /// on the pane's own selection and its session's model markers instead
    /// of the global config — and cancel restores nothing, because nothing
    /// global was ever mutated (the originals stay `None`).
    pub(in crate::app) fn open_pane_model_dialog(&mut self, pane: usize) {
        self.summarization_model_edit = None;
        self.pane_model_edit = Some(pane);
        self.model_dialog_original = None;
        self.reasoning_dialog_original = None;
        // The pane's own selection is what the picker edits; a pane that
        // never picked preselects the global model it inherits.
        let current = self
            .pane_model_selection(pane)
            .map(|selection| selection.model)
            .or_else(|| self.llm_config.model.clone())
            .unwrap_or_default();
        self.open_model_dialog_with_current(current);
    }

    /// The focused pane's OWN selection, from whichever split the pane
    /// lives in. `None` = the pane never picked.
    pub(in crate::app) fn pane_model_selection(
        &self,
        pane: usize,
    ) -> Option<crate::routes::session::agents_split::PaneModelSelection> {
        self.agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.model_selection.clone())
    }

    /// The pane's bound cosh session id, same split resolution.
    fn pane_session_id(&self, pane: usize) -> Option<String> {
        self.agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .and_then(|p| p.session_id.clone())
    }

    /// The reasoning level the picker preselects in the reasoning
    /// sub-dialog: a pane-scoped picker starts from the PANE's level (its
    /// own selection, or the global one it inherits when it never picked);
    /// the global picker starts from the global config.
    fn dialog_reasoning_current(&self) -> String {
        if let Some(pane) = self.pane_model_edit
            && let Some(selection) = self.pane_model_selection(pane)
        {
            return selection.reasoning.unwrap_or_default();
        }
        self.llm_config.reasoning.clone().unwrap_or_default()
    }

    /// Shared picker body: provider credential checks, cache fill and the
    /// background revalidation. `current` is whatever model this picker
    /// edits — the global config's, or the focused pane's own.
    fn open_model_dialog_with_current(&mut self, current: String) {
        use cosh::ModelEntry;
        use cosh_sdk::connector::Connector;

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
                loading: false,
            });
        } else {
            // Nothing cached (first run, or every provider failed before):
            // the background API fetch below may take a few seconds, so flag
            // the dialog as loading — the list area shows a spinner until
            // ModelsLoaded replaces the list (or clears the flag when the
            // fetch finds nothing).
            self.dialog.replace(DialogType::ModelList {
                models: vec![auto_entry],
                current: auto_current,
                filter: String::new(),
                loading: true,
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
                // The shared list-dialog filter edit: pop + selection reset
                // + cursor blink, one definition for every cosh list box.
                crate::component::list_dialog::pop_filter(
                    filter,
                    &mut d.selected,
                    &mut d.cursor,
                );
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
        if let Some(index) = self.summarization_model_edit {
            if model == "auto" || model.trim().is_empty() || provider.trim().is_empty() {
                return;
            }
            let entry = cosh::setup::FallbackEntry {
                provider: provider.to_string(),
                model: model.to_string(),
            };
            let list = &mut self.setup.routing.summarization_models;
            if let Some(index) = index {
                let Some(slot) = list.get_mut(index) else {
                    return;
                };
                *slot = entry;
            } else {
                list.push(entry);
            }
            self.setup.save();
            self.summarization_model_edit = None;
            self.dialog.pop();
            return;
        }
        if model == "auto" {
            // Auto mode may land on ANY fallback model, so it offers the
            // standard effort set; the choice is re-mapped onto the closest
            // level each fallback model actually accepts (both here and on
            // every harness fallback switch via `resolve_reasoning_effort`).
            let current = self.dialog_reasoning_current();
            let levels = crate::config::auto_reasoning_levels();
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
            let current = self.dialog_reasoning_current();
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
            // The pane-scoped picker keeps the PANE's previous level (its
            // own selection, or the global one it inherits).
            let previous = self.dialog_reasoning_current();
            let reasoning = (!previous.is_empty()).then_some(previous);
            self.commit_model_selection(model, provider, reasoning.as_deref());
            self.model_dialog_original = None;
            self.reasoning_dialog_original = None;
            self.dialog.pop();
        }
    }

    /// Restore the model + reasoning that were active when the dialog opened.
    pub(in crate::app) fn restore_model_dialog(&mut self) {
        self.summarization_model_edit = None;
        self.pane_model_edit = None;
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
    /// on the current session so its JSONL history carries it. This is the
    /// single write path shared by every model selection in the UI.
    pub(in crate::app) fn commit_model_selection(
        &mut self,
        model: &str,
        provider: &str,
        reasoning: Option<&str>,
    ) {
        // A pane-scoped picker commits to THAT pane only: the pane's own
        // selection plus its session's model markers — the global config,
        // the persisted global slot and the main session stay untouched.
        if let Some(pane) = self.pane_model_edit {
            self.commit_pane_model_selection(pane, model, provider, reasoning);
            return;
        }
        self.set_llm_model(
            model.to_owned(),
            provider.to_owned(),
            reasoning.map(String::from),
        );
        self.setup.set_model_selection(provider, model, reasoning);
        self.record_model_on_current_session();
        // Append the session metadata right away so the selection survives the
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
    /// persisted in the session's JSONL history on the next save.
    pub(in crate::app) fn record_model_on_current_session(&mut self) {
        if let Some(session) = self.state.current_session_mut() {
            record_session_model_fields(
                session,
                &self.llm_config.provider,
                self.llm_config.model.as_deref().unwrap_or(""),
                self.llm_config.reasoning.as_deref(),
            );
        }
    }

    /// Commit a pane-scoped model pick: the selection lands on THAT pane
    /// only — the pane's live state (which the dispatch route reads for
    /// every later turn) and the SAME provider/model/reasoning markers the
    /// main session records, written onto the pane's own cosh session so
    /// the pick survives restarts with the existing marker precedence. The
    /// global config, the persisted global slot and the main session are
    /// never touched. A pane that has not run its first turn yet has no
    /// session to stamp — the in-memory selection rides along until
    /// `ensure_manager_pane_session` mints the session shell.
    pub(in crate::app) fn commit_pane_model_selection(
        &mut self,
        pane: usize,
        model: &str,
        provider: &str,
        reasoning: Option<&str>,
    ) {
        if let Some(session_id) = self.pane_session_id(pane)
            && let Some(mut session) = self.session_store.load_session(&session_id)
        {
            record_session_model_fields(&mut session, provider, model, reasoning);
            // Synchronous like the manager shell save: one tiny metadata
            // write per dialog confirm, and the markers land before any
            // later save could race them.
            self.session_store.save_session(&session);
        }
        if let Some(pane_state) = self
            .agents_split
            .as_mut()
            .or(self.parked_agents_split.as_mut())
            .and_then(|split| split.panes.get_mut(pane))
        {
            pane_state.model_selection = Some(
                crate::routes::session::agents_split::PaneModelSelection::new(
                    provider.to_string(),
                    model.to_string(),
                    reasoning.map(String::from),
                ),
            );
            // The prompt badge reads the pane's chrome, not the selection:
            // overwrite its serving pair too, so the pick shows the moment
            // it is committed (the manager pane's bridge emits no
            // `SessionModel` event to repaint it later).
            pane_state.session_info = Some(
                crate::routes::session::agents_split::PaneModelSelection::new(
                    provider.to_string(),
                    model.to_string(),
                    reasoning.map(String::from),
                )
                .to_session_info(),
            );
        }
        self.pane_model_edit = None;
    }

    /// Restore the globally persisted model (the last one the user selected)
    /// into the active LLM config. Used when entering a brand-new session; a
    /// stored selection wins over the env-var defaults.
    pub(in crate::app) fn apply_global_model(&mut self) {
        let Some(model) = self.setup.persisted_model().map(String::from) else {
            return;
        };
        let provider = self.setup.model.provider.clone();
        // Same credential gate as the boot restore — with the same `auto`
        // exception: an empty provider is the persisted `auto` selection,
        // not an orphan. A concrete provider must still have a credential,
        // or the selection died with its key.
        if !provider.is_empty() && !crate::app::providers::provider_has_credential(&provider) {
            return;
        }
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
            // or the projection only derived a bare model id without a provider
            // (no `/`). Best-effort fallback for slash'd legacy histories in
            // the `(Some, Some)` arm above; here we cannot pin a provider
            // confidently, so the active config stays as-is.
            _ => {}
        }
    }

    /// Cycle the active model's reasoning effort to the next level
    /// (Ctrl+R). Walks the same level list the reasoning sub-dialog of the
    /// model picker offers — `default` first, then the model's advertised
    /// efforts in display order — and wraps back to the start when the end
    /// is reached. The new level goes through `commit_model_selection`, so
    /// it persists globally and on the current session exactly like a pick
    /// made through /models. A model that exposes no configurable reasoning
    /// (or `auto` with only the default effort) leaves the config untouched.
    pub(in crate::app) fn cycle_reasoning(&mut self) {
        let model = self.llm_config.model.clone().unwrap_or_default();

        // `auto` may land on ANY fallback model, so it cycles the standard
        // effort set (mirroring the auto branch of `confirm_model_entry`);
        // a concrete model only cycles when it supports reasoning at all.
        let levels = if model == "auto" {
            crate::config::auto_reasoning_levels()
        } else if crate::config::model_supports_reasoning(&model) {
            crate::config::model_reasoning_levels(&model)
        } else {
            return;
        };
        if levels.len() <= 1 {
            return;
        }

        // An effort left over from a previous model (not in this list)
        // maps to index 0 (`default`) — the next press then walks the list
        // from the top instead of erroring or skipping.
        let current = self.llm_config.reasoning.clone().unwrap_or_default();
        let start = levels.iter().position(|l| l == &current).unwrap_or(0);
        let next = &levels[(start + 1) % levels.len()];
        let reasoning: Option<&str> = if next == "default" { None } else { Some(next) };

        let provider = self.llm_config.provider.clone();
        self.commit_model_selection(&model, &provider, reasoning);
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
                // The shared list-dialog filter edit: push + selection reset
                // + cursor blink, one definition for every cosh list box.
                crate::component::list_dialog::push_filter(
                    filter,
                    &mut d.selected,
                    &mut d.cursor,
                    ch,
                );
            } else {
                d.selected = 0;
                d.cursor.note_activity();
            }
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

/// Write a model selection onto a session record — the field mapping BOTH
/// the main session and the per-pane commits share: an empty provider is
/// the persisted `auto` selection (no provider to pin), an empty model
/// clears the record entirely.
pub(in crate::app) fn record_session_model_fields(
    session: &mut crate::types::Session,
    provider: &str,
    model: &str,
    reasoning: Option<&str>,
) {
    session.provider = if provider.is_empty() {
        None
    } else {
        Some(provider.to_string())
    };
    session.model = if model.is_empty() {
        None
    } else {
        Some(model.to_string())
    };
    session.reasoning = reasoning.map(String::from);
}
