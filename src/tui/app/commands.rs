use cosh::harness::HarnessEvent;
use cosh_tui::core::lib::rgba::RGBA;

use super::App;
use super::BUG_REPORT_URL;
use crate::session_store::format_session_timestamp;
use crate::ui::dialogs::DialogType;

impl App {
    /// Manual compaction has no running agent connector to inherit in auto
    /// mode, so resolve only the user's auto chain (never the literal "auto").
    pub(super) fn manual_summarization_models(&self) -> Vec<(String, String)> {
        let entries = if !self.setup.routing.summarization_models.is_empty() {
            &self.setup.routing.summarization_models
        } else if self.llm_config.model.as_deref() == Some("auto") {
            &self.router_view.fallbacks
        } else {
            return Vec::new();
        };
        entries
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect()
    }
    /// Opens the bug-report page in the default browser and shows a toast.
    pub(super) fn open_bug_report_link(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.toast_state.show(ToastOptions {
            title: Some("Bug report".to_string()),
            message: format!("Opening {BUG_REPORT_URL}…"),
            variant: ToastVariant::Info,
            duration_ms: 2500,
        });
        std::thread::spawn(move || {
            if let Err(err) = open::that(BUG_REPORT_URL) {
                log::error!("Failed to open bug report URL: {err}");
            }
        });
    }

    /// Handle a click (or Enter/`u` key press) on the home banner's update
    /// announcement: run the update pipeline or open the changelog page.
    pub(super) fn handle_banner_action(&mut self, action: crate::routes::home::BannerAction) {
        use crate::routes::home::BannerAction;

        let Some(crate::routes::home::BannerContent::ReleaseUpdate { tag, .. }) =
            self.home_view.banner.content.clone()
        else {
            return;
        };

        match action {
            BannerAction::OpenChangelog => {
                let url = self.update_changelog_url.clone().unwrap_or_else(|| {
                    format!(
                        "https://github.com/{}/releases/tag/{tag}",
                        crate::update::REPO
                    )
                });
                std::thread::spawn(move || {
                    if let Err(err) = open::that(url) {
                        log::error!("Failed to open changelog URL: {err}");
                    }
                });
            }
            BannerAction::StartUpdate => {
                // Fire-and-forget: the pipeline downloads/installs and
                // RELAUNCHES cosh when it finishes, so the very next step is
                // to close this TUI. No toast, no spinner, no in-app state —
                // the terminal closing and reopening IS the feedback.
                crate::update::spawn_update_pipeline(&tag);
                self.should_quit = true;
            }
        }
    }

    /// Drain update-related events from the background release check (drives
    /// the banner content). Called once per frame.
    pub(super) fn pump_update_events(&mut self) {
        use crate::routes::home::{BannerContent, UpdateStatus};

        while let Ok(event) = self.update_event_rx.try_recv() {
            match event {
                crate::update::UpdateEvent::CheckFinished(Some(release)) => {
                    self.update_changelog_url = Some(release.url.clone());
                    self.home_view.banner.content = Some(BannerContent::ReleaseUpdate {
                        version: release.version.clone(),
                        tag: release.tag.clone(),
                    });
                    self.home_view.banner.status = UpdateStatus::Idle;
                }
                crate::update::UpdateEvent::CheckFinished(None) => {
                    // Up to date or check failed: banner stays hidden.
                }
            }
        }
    }

    /// Rebuild the right panel from the CURRENT session's persisted tool
    /// parts (todos, bash runs, subagent windows). Called after every
    /// session-switch path resets the panel: the panel content belongs to
    /// the session, so it must survive app restarts and session switches
    /// instead of silently disappearing.
    pub(super) fn rehydrate_right_panel(&mut self) {
        if let Some(session) = self.state.current_session().cloned() {
            self.state.right_panel.rehydrate_from_session(&session);
        }
    }

    /// Open the rename dialog for the current session, prefilled with its
    /// title (opencode-style prompt: edit in place, Enter applies, Esc
    /// cancels). No-op without a session.
    pub(super) fn open_rename_dialog(&mut self) {
        let Some(session) = self.state.current_session() else {
            return;
        };
        let input = session.title.clone();
        let cursor_pos = input.len();
        self.dialog
            .show(DialogType::RenameSession { input, cursor_pos });
    }

    /// Create a fresh empty session and select it — the shared path behind
    /// Home's "New session" (keyboard + mouse) and the `/new` slash command.
    /// Selecting the session flips the app into Session mode.
    pub(super) fn start_new_session(&mut self) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let id = format!("{now_ms}");
        let title = format_session_timestamp(now_ms);
        self.state.add_empty_session(id.clone(), title, now_ms);
        self.state.current_session_id = Some(id);
        // Fresh session: the header's cost widget starts clean — no
        // balance carried over from the previous session.
        self.hypercredit_balance = None;
        // A fresh session starts with a CLEAN right panel: the panel lives
        // on AppState (not inside the Session model), so without this reset
        // the previous session's todos, PTY/subagent sessions, panel focus,
        // scroll offsets and history-navigation state would leak into the
        // new session — the same reset every other session-switch path
        // (sidebar keyboard/mouse switch, Esc back to Home) performs.
        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
        // A new session is restored with the globally persisted model (the
        // last one the user selected), recorded on the session so its history
        // carries it as metadata deltas.
        self.apply_global_model();
        self.title_generated = false;
        self.prompt_view.focus();
    }

    /// `/background` toggle: repaint the TUI's base background with the
    /// terminal default (`Color::Reset`) instead of the theme color. Panels,
    /// elements and menus keep their theme colors.
    ///
    /// Flips the alpha of the current background IN PLACE: every theme
    /// mutation path (`App::set_theme`, the startup loader) preserves the RGB
    /// channels and only zeroes alpha while the toggle is on, so the pristine
    /// color is always recoverable locally — no registry/name lookup that
    /// could drift from what is actually on screen.
    pub(super) fn toggle_transparent_background(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        self.transparent_background = !self.transparent_background;
        let mut theme = self.theme.clone();
        let (r, g, b, _) = theme.background.to_ints();
        theme.background = if self.transparent_background {
            RGBA::from_ints(r, g, b, 0)
        } else {
            RGBA::from_ints(r, g, b, 255)
        };
        self.theme = theme;
        self.config.theme_gen += 1;
        self.setup.appearance.transparent_background = self.transparent_background;
        self.setup.save();
        let message = if self.transparent_background {
            "Now using the terminal's default background."
        } else {
            "Theme background restored."
        };
        self.toast_state.show(ToastOptions {
            title: Some("Background".into()),
            message: message.into(),
            variant: ToastVariant::Info,
            duration_ms: 3000,
        });
    }

    /// Execute a slash-menu command (Enter or click). Shared by the keyboard
    /// and mouse handlers so both dispatch identically — a command missed here
    /// silently degrades to filling the prompt with "/name ".
    pub(super) fn run_slash_command(&mut self, cmd: &crate::ui::slash_menu::SlashCommand) {
        if cmd.name == "themes" {
            self.open_theme_dialog();
        } else if cmd.name == "models" {
            self.open_model_dialog();
        } else if cmd.name == "toolcall" {
            self.open_tool_call_dialog();
        } else if cmd.name == "compact" {
            self.start_manual_compaction();
        } else if cmd.name == "export" {
            self.export_transcript();
        } else if cmd.name == "new" {
            if self.state.status == crate::types::SessionStatus::Idle {
                self.start_new_session();
            } else {
                use crate::ui::toast::{ToastOptions, ToastVariant};
                self.toast_state.show(ToastOptions {
                    title: Some("New session".into()),
                    message: "The agent is working — wait for it to finish.".into(),
                    variant: ToastVariant::Warning,
                    duration_ms: 4000,
                });
                self.slash_menu.visible = false;
                return;
            }
        } else if cmd.name == "rename" {
            self.open_rename_dialog();
        } else if cmd.name == "undo" {
            self.open_undo_dialog();
        } else if cmd.name == "background" {
            self.toggle_transparent_background();
        } else if cmd.name == "bell" {
            self.bell_enabled = !self.bell_enabled;
            use crate::ui::toast::{ToastOptions, ToastVariant};
            let state = if self.bell_enabled {
                "enabled"
            } else {
                "disabled"
            };
            self.toast_state.show(ToastOptions {
                title: Some("Bell".into()),
                message: format!("Completion bell {state}."),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            self.setup.appearance.bell_enabled = self.bell_enabled;
            self.setup.save();
        } else if cmd.name == "anim" {
            self.anim_enabled = !self.anim_enabled;
            use crate::ui::toast::{ToastOptions, ToastVariant};
            let message = if self.anim_enabled {
                "Animated logo enabled."
            } else {
                "Animated logo disabled — using static LOGO_CHAT."
            };
            self.toast_state.show(ToastOptions {
                title: Some("Anim".into()),
                message: message.into(),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            self.setup.appearance.anim_enabled = self.anim_enabled;
            self.setup.save();
        } else {
            let cmd_name = format!("/{} ", cmd.name);
            self.prompt_view.input = cmd_name;
            self.prompt_view.cursor_pos = self.prompt_view.input.len();
        }
        self.slash_menu.visible = false;
    }

    /// User-triggered `/export`: write the CURRENT agent-visible transcript
    /// of the active session to `{cwd}/{session-name}-{uuid}.md`. The export
    /// replays the persisted context records through the context manager, so
    /// only what the agent can see right now (visibility boundary, hidden
    /// items, checkpoint coverage, masked results) reaches the Markdown file.
    ///
    /// The whole load-replay-render-write pipeline runs on a worker thread:
    /// replaying a long JSONL history on the UI thread would freeze the TUI
    /// for the duration. The outcome arrives as a toast through the harness
    /// event channel, the same path the session store's lock notifications
    /// already use.
    pub(super) fn export_transcript(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let Some(session) = self.state.current_session().cloned() else {
            self.toast_state.show(ToastOptions {
                title: Some("Export".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let store = self.session_store.clone();
        let cwd = std::path::PathBuf::from(&self.state.working_directory);
        // The export is a snapshot of the PERSISTED records: a running loop's
        // in-flight turn has not landed on disk yet. Say so on the toast
        // instead of letting the user assume the live tail is in the file.
        let still_working = self.state.status != crate::types::SessionStatus::Idle;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result =
                crate::transcript_export::export_session_transcript(&store, &session, &cwd);
            let (message, variant) = match result {
                Ok(path) => {
                    let mut message = format!("Export: transcript written to {}.", path.display());
                    if still_working {
                        message.push_str(
                            " (snapshot as of the last persisted turn; the agent is still working)",
                        );
                    }
                    (message, cosh::harness::events::ToastVariant::Info)
                }
                Err(error) => {
                    log::warn!("transcript export failed: {error}");
                    (
                        format!("Export: could not write the transcript: {error}"),
                        cosh::harness::events::ToastVariant::Warning,
                    )
                }
            };
            let _ = event_tx.send(HarnessEvent::Toast { message, variant });
        });
    }

    /// User-triggered `/compact`: run the LLM summary NOW instead of waiting
    /// for the 80% trigger. Refusals are surfaced as toasts; the work runs on
    /// a one-off tokio task that rebuilds a Harness from the context records
    /// persisted in the session JSONL — between loops no harness exists, and
    /// while a loop runs its context manager is untouchable, so both cases
    /// refuse.
    pub(super) fn start_manual_compaction(&mut self) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        fn refuse(app: &mut App, message: String) {
            app.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message,
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
        }
        if self.manual_compaction_active {
            refuse(self, "A compaction is already running.".into());
            return;
        }
        if self.state.status != crate::types::SessionStatus::Idle {
            refuse(
                self,
                "The agent is working — /compact runs between messages.".into(),
            );
            return;
        }
        let Some(id) = self.state.current_session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "No active session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };
        let Some(ctx_state) = self.session_store.load_context(&id) else {
            self.toast_state.show(ToastOptions {
                title: Some("Compact".into()),
                message: "Nothing to compact yet.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 4000,
            });
            return;
        };

        let provider = self.llm_config.provider.clone();
        let model = self.llm_config.model.clone();
        let base_url = self.base_url_for(&provider);
        let summarization_models = self.manual_summarization_models();
        let local_base_urls = self.configured_local_base_urls();
        let anthropic_ttl = self.setup.anthropic_cache_ttl_1h();
        let retention = self.setup.openai_cache_retention().map(String::from);
        let reasoning = self.llm_config.reasoning.clone();
        let cwd = self.state.working_directory.clone();
        let stop_signal = self.stop_signal.clone();
        let event_tx = self.event_tx.clone();
        self.manual_compaction_active = true;
        self.tokio_handle.spawn(async move {
            use cosh::harness::{Harness, ManualCompactionOutcome};
            let base = if !summarization_models.is_empty() {
                summarization_models
                    .iter()
                    .find_map(|(provider, model)| {
                        if model.is_empty() || model == "auto" || provider.is_empty() {
                            return None;
                        }
                        cosh_sdk::connector::Connector::new(provider)
                            .ok()
                            .map(|mut connector| {
                                connector = connector.with_model(model);
                                if let Some(url) = local_base_urls.get(provider) {
                                    connector = connector.with_base_url(url.clone());
                                }
                                connector
                            })
                    })
                    .ok_or_else(|| {
                        "No configured summarization connector is available.".to_string()
                    })
            } else if model.as_deref() == Some("auto") {
                Err("Auto has no configured models.".to_string())
            } else {
                cosh_sdk::connector::Connector::new(&provider).map_err(|error| error.to_string())
            };
            let outcome = match base {
                Ok(mut connector) => {
                    if summarization_models.is_empty() {
                        if let Some(ref m) = model {
                            connector = connector.with_model(m);
                        }
                        if let Some(ref url) = base_url {
                            connector = connector.with_base_url(url.clone());
                        }
                        if let Some(effort) = reasoning {
                            connector = connector.with_reasoning_effort(effort);
                        }
                    }
                    connector = connector
                        .with_prompt_cache_ttl_1h(anthropic_ttl)
                        .with_prompt_cache_key(&id)
                        // Same session-affinity routing as the main loop:
                        // compaction requests carry the session's opaque
                        // hash so they reach the same cache-warm backend.
                        .with_session_id(super::session_affinity_id(&id));
                    if let Some(retention) = retention {
                        connector = connector.with_prompt_cache_retention(retention);
                    }
                    let mut harness =
                        Harness::new(connector, &cwd, std::collections::HashSet::new())
                            .with_summarization_models(summarization_models)
                            .with_local_base_urls(local_base_urls);
                    harness.context_manager.restore_state(&ctx_state);
                    harness.compact_on_demand(&event_tx, stop_signal).await
                }
                Err(e) => {
                    let _ = event_tx.send(HarnessEvent::Error {
                        message: format!("connector: {e}"),
                        context: None,
                    });
                    ManualCompactionOutcome::Failed
                }
            };
            let _ = event_tx.send(HarnessEvent::CompactOnDemand { outcome });
        });
    }
}
