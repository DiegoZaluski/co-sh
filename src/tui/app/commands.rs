use cosh::harness::HarnessEvent;
use cosh_tui::core::lib::rgba::RGBA;

use super::App;
use super::BUG_REPORT_URL;
use crate::session_store::format_session_timestamp;
use crate::ui::dialogs::DialogType;

impl App {
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
        } else {
            let cmd_name = format!("/{} ", cmd.name);
            self.prompt_view.input = cmd_name;
            self.prompt_view.cursor_pos = self.prompt_view.input.len();
        }
        self.slash_menu.visible = false;
    }

    /// User-triggered `/compact`: run the compaction funnel NOW (the
    /// deterministic phases across every segment, then the LLM summary)
    /// instead of waiting for the 80% trigger. Refusals are surfaced as
    /// toasts; the work runs on a one-off tokio task that rebuilds a Harness
    /// from the persisted `.ctx` snapshot — between loops no harness exists,
    /// and while a loop runs its context manager is untouchable, so both
    /// cases refuse.
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
        let Some(ctx_bytes) = self.session_store.load_ctx(&id) else {
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
        let cwd = self.state.working_directory.clone();
        let stop_signal = self.stop_signal.clone();
        let event_tx = self.event_tx.clone();
        self.manual_compaction_active = true;
        self.tokio_handle.spawn(async move {
            use cosh::harness::{Harness, ManualCompactionOutcome};
            let outcome = match cosh_sdk::connector::Connector::new(&provider) {
                Ok(mut connector) => {
                    if let Some(ref m) = model {
                        connector = connector.with_model(m);
                    }
                    if let Some(ref url) = base_url {
                        connector = connector.with_base_url(url.clone());
                    }
                    let mut harness =
                        Harness::new(connector, &cwd, std::collections::HashSet::new());
                    if let Ok(state) =
                        bincode::deserialize::<cosh::harness::ContextManagerState>(&ctx_bytes)
                    {
                        harness.context_manager.restore_state(&state);
                    }
                    harness.compact_on_demand(&event_tx, stop_signal).await
                }
                Err(e) => {
                    let _ = event_tx.send(HarnessEvent::Error(format!("connector: {e}")));
                    ManualCompactionOutcome::Failed
                }
            };
            let _ = event_tx.send(HarnessEvent::CompactOnDemand { outcome });
        });
    }
}
