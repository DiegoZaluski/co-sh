use std::io;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{App, AppMode, PendingSessionDelete};
use crate::component::prompt::PromptView;
use crate::component::prompt_history::{RedoOutcome, UndoOutcome};
use crate::fallback;
use crate::left_panel::sessions::SessionsAction;
use crate::left_panel::{MIN_WIDTH_FOR_LEFT_PANEL, Mode};
use crate::routes::home::HomeAction;
use crate::routes::router::FocusTarget;
use crate::ui::dialogs::DialogType;
use crate::util::selection;

impl App {
    /// Process a single key press (the press-only branch of
    /// `handle_events`). Extracted so tests can dispatch synthetic keys.
    pub(super) fn process_key_event(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.kind == KeyEventKind::Press {
            // Windows paste-burst coalescing (see `paste_burst` module docs):
            // a console paste arrives as one plain Char key per character plus
            // one plain Enter per line break (crossterm has no bracketed paste
            // on Windows), and each of those Enters used to submit the prompt
            // — splitting a large paste into one message per line. Characters
            // are NEVER withheld: they stream into the prompt in real time
            // while being mirrored into the coalescer's shadow; only Enters
            // within the burst window are absorbed, and at burst end the run
            // is replayed through `PromptView::handle_paste` with its real
            // line breaks (`[Pasted ~N lines]` compression, exactly like the
            // Unix Event::Paste path). The gate is a no-op unless the bare
            // prompt owns the keyboard.
            //
            // Land any pending run BEFORE this key is classified: the gate
            // below mirrors the key's own character into the shadow, so a
            // flush AFTER it would count this key's char in the retraction
            // length before it is typed (an off-by-one that duplicated the
            // first char after a mid-paste console stall). Flushing first
            // guarantees the shadow mirrors exactly the chars already in the
            // prompt; the gate's own fall-through paths (non-text keys,
            // outside-burst Enters) flush again after classification, which
            // is idempotent and never double-counts.
            self.paste_burst_flush_if_due();
            let burst_now = Instant::now();
            if self.paste_burst_handle_key(&key, burst_now) {
                return Ok(false);
            }

            // Modal sovereignty: while a Confirm dialog is on screen it owns
            // the ENTIRE keyboard — the slash menu, the prompt, the left panel
            // and every panel must not react to any key until the user
            // decides. Left/Right/Enter/Esc drive the dialog; any other key
            // is swallowed. (Without this gate a "/"-open slash menu used to
            // steal Enter from the "Quit cosh?" confirm and run a command.)
            if self.is_confirm_dialog_visible() {
                self.handle_confirm_dialog_key(key.code);
                return Ok(false);
            }

            // Same sovereignty for the provider-key forget/overwrite picker:
            // while it is open nothing else may react to the keyboard.
            if self.is_provider_key_choice_visible() {
                self.handle_provider_key_choice_key(key.code);
                return Ok(false);
            }

            // ESC sovereign while an agent loop is running: whatever
            // incidental UI state is active (a prompt/field text selection,
            // the left panel focus, the slash menu, the permission dialog, an
            // overlay), the FIRST job of ESC is to interrupt the loop.
            // Flag the shared stop signal here — before any gate below can
            // swallow the key — then let ESC fall through so it still
            // performs its normal local action (clear selection, close
            // menu/dialog). Regression: those gates used to consume ESC
            // without setting `stop_signal`, so the loop kept running.
            // `Retry` is covered too: an Error event can flip the status
            // while a loop is still winding down, and ESC must still stop
            // it. (Idle no-op: the flag is reset by `start_agent_loop`.)
            if key.code == KeyCode::Esc
                && matches!(
                    self.state.status,
                    crate::types::SessionStatus::Working
                        | crate::types::SessionStatus::Retry { .. }
                )
            {
                self.stop_signal.store(true, Ordering::Relaxed);
            }

            // ESC cancels an in-flight prompt correction the same way it
            // interrupts the agent loop: abort the request, drop the spinner
            // and keep the draft exactly as it is (the correction never
            // mutates the prompt before its answer is applied, so "restoring
            // the original text" is the natural result of the abort). Only
            // effective while the request is still running — after the
            // model's answer arrived the flag is already down and Esc keeps
            // its normal meanings below.
            //
            // Priority note: this gate sits BEFORE the selection-clearing
            // gate on purpose. A left-drag selection made while the
            // correction is in flight survives the first Esc (cancel is the
            // more urgent action); a second Esc then clears the selection,
            // mirroring how the agent-loop stop block above composes with
            // the same gates.
            if key.code == KeyCode::Esc && self.prompt_correction_active {
                self.cancel_prompt_correction();
                return Ok(false);
            }

            // Escape clears selection if there is one.
            if key.code == KeyCode::Esc && self.prompt_view.has_selection() {
                self.prompt_view.clear_selection();
                return Ok(false);
            }

            // Escape also clears a lingering create-db field selection.
            #[cfg(feature = "embed")]
            if key.code == KeyCode::Esc && self.rag_view.field_selection.is_some() {
                self.rag_view.clear_field_selection();
                return Ok(false);
            }

            // Escape also unfocuses the left panel.
            if key.code == KeyCode::Esc && self.sidebar_focused {
                self.sidebar_focused = false;
                return Ok(false);
            }

            // Escape also unfocuses the right panel slot — WITHOUT
            // consuming the key: Esc keeps its normal meanings
            // (interrupt generation, cancel, close dialogs) even
            // while a panel slot is focused.
            if key.code == KeyCode::Esc && self.state.right_panel.panel_focus.is_some() {
                self.state.right_panel.panel_focus = None;
            }

            // AltGr exclusion: Char + CTRL+ALT is the Windows-console
            // signature of an AltGr character (`@ # $` on ABNT layouts —
            // crossterm's parser reports it exactly that way), NEVER a
            // Ctrl+C hotkey. Without this guard, pasting text containing
            // the letter `c` through an ABNT layout quits the app
            // mid-paste (the quit-confirm dialog opening where a letter
            // was expected).
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                // If there is a drag selection in a create-db field, copy it.
                #[cfg(feature = "embed")]
                if matches!(self.mode(), AppMode::Rag) && self.rag_view.has_field_selection() {
                    let text = self.rag_view.selected_field_text();
                    selection::copy_selection(&text, &mut self.toast_state);
                    self.rag_view.clear_field_selection();
                    return Ok(false);
                }
                // If there is text selected in the prompt, copy it instead of quitting.
                if matches!(self.mode(), AppMode::Session) && self.prompt_view.has_selection() {
                    let text = self.prompt_view.selected_text();
                    selection::copy_selection(&text, &mut self.toast_state);
                    self.prompt_view.clear_selection();
                    return Ok(false);
                }
                // While a registration panel (hook, MCP server) owns the
                // keyboard, Ctrl+C copies the active field instead of
                // quitting — Esc already discards the panel.
                if self.is_registration_form_open() {
                    self.copy_registration_form_field();
                    return Ok(false);
                }
                self.pending_delete = None;
                self.dialog.show(DialogType::Confirm {
                    message: "Quit cosh?".into(),
                });
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = 1;
                }
                return Ok(false);
            }

            // Ctrl+V = clipboard paste (opencode mini-port, prompt-only): Windows
            // terminals do not reliably emit a bracketed paste for Ctrl+V
            // (many never emit one at all — crossterm's Windows backend has
            // no paste support whatsoever), so the key event IS the paste
            // signal. Read the system clipboard and route the text through
            // the normal paste path. A terminal that DOES intercept Ctrl+V
            // never delivers this key event, so there is no double-paste
            // path. The key is consumed even when the clipboard is empty —
            // a fall-through would type a stray `v` into the prompt (the
            // same reason opentui ignores ctrl-modified keys). Guarded
            // against AltGr (CTRL+ALT is an AltGr character on Windows
            // consoles, never a hotkey).
            //
            // SCOPE: only the chat prompt — the motivating bug is pasting a
            // multi-line message into it. The Unix `Event::Paste` router
            // (events.rs) additionally dispatches into registration fields,
            // the question dialog and RAG inputs; extending Ctrl+V to those
            // surfaces is a follow-up that should mirror that ownership
            // chain. Until then this gate's `prompt_owns_keyboard()` check
            // deliberately leaves those surfaces alone.
            if key.code == KeyCode::Char('v')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && self.prompt_owns_keyboard()
            {
                self.paste_clipboard_text_into_prompt();
                return Ok(false);
            }

            // Check theme dialog FIRST, before action lookup
            if self.is_theme_dialog_visible() && self.handle_theme_dialog_key(key.code) {
                return Ok(false);
            }

            // Check the tool-call mode dialog (standalone, like theme)
            if self.is_tool_call_dialog_visible() && self.handle_tool_call_dialog_key(key.code) {
                return Ok(false);
            }

            // Check the per-message actions dialog
            if self.is_message_actions_dialog_visible()
                && self.handle_message_actions_dialog_key(key.code)
            {
                return Ok(false);
            }

            // Check the /undo versions dialog
            if self.is_undo_dialog_visible() && self.handle_undo_dialog_key(key.code) {
                return Ok(false);
            }

            // Check the per-queued-message actions dialog
            if self.is_queue_actions_dialog_visible()
                && self.handle_queue_actions_dialog_key(key.code)
            {
                return Ok(false);
            }

            // Check reasoning sub-dialog SECOND (pushed on top of the
            // model list), before the model dialog.
            if self.is_reasoning_dialog_visible() && self.handle_reasoning_dialog_key(key.code) {
                return Ok(false);
            }

            // Check model dialog THIRD, before action lookup
            if self.is_model_dialog_visible() && self.handle_model_dialog_key(key.code) {
                return Ok(false);
            }

            // Check question dialog THIRD (inline)
            if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                let consumed = self.question_dialog.handle_key_event(key);
                if consumed {
                    // Check if user submitted answers (Enter on confirm tab)
                    if self.question_dialog.submitted {
                        let answers = self.question_dialog.build_answers();
                        let _ = self.answer_tx.send(Ok(answers));
                        self.question_dialog.visible = false;
                        self.question_dialog.submitted = false;
                        // Re-focus prompt when question is answered (like OpenCode)
                        self.prompt_view.focus();
                    } else if !self.question_dialog.visible {
                        // Dialog was dismissed via Esc (like OpenCode) — stop agent loop + send rejection
                        self.stop_signal.store(true, Ordering::Relaxed);
                        let _ = self
                            .answer_tx
                            .send(Err("User dismissed the question dialog".into()));
                        self.prompt_view.focus();
                    }
                    return Ok(false);
                }
            }

            // Check the queue-choice dialog (inline, shown when the user
            // sends a message while the agent loop is running).
            if self.queue_choice_dialog.visible && matches!(self.mode(), AppMode::Session) {
                let consumed = self.queue_choice_dialog.handle_key_event(key);
                if consumed {
                    if self.queue_choice_dialog.submitted {
                        let text = self.prompt_view.send_message();
                        let target = self.queue_choice_dialog.choice();
                        self.queue_choice_dialog.hide();
                        // Edited message returning to its queue → original
                        // position; otherwise appended at the end (helper).
                        self.enqueue_pending_message(target, text);
                        self.prompt_view.focus();
                    } else if !self.queue_choice_dialog.visible {
                        // Dismissed via Esc: re-focus the prompt (the
                        // message stays in the input, nothing was queued).
                        self.prompt_view.focus();
                    }
                    return Ok(false);
                }
            }

            // (Confirm dialog keys are handled by the sovereignty gate at the
            // top of this function — nothing here can run while it decides.)

            // Free-gateway recommendation dialog (inline, shown after HTTP
            // errors or when no API key is configured).
            if self.free_gateway_dialog.visible && matches!(self.mode(), AppMode::Session) {
                let consumed = self.free_gateway_dialog.handle_key_event(key);
                if consumed {
                    if self.free_gateway_dialog.submitted {
                        self.commit_gateway_choice();
                    } else if !self.free_gateway_dialog.visible {
                        // Dismissed via Esc: put the parked message back
                        self.restore_pending_gateway_message();
                    }
                    return Ok(false);
                }
            }

            // Registration panels (hook, MCP server): handled with the full
            // key event so ctrl-combos (word jumps) reach them intact.
            if self.is_registration_form_open() && self.handle_registration_form_key(key) {
                return Ok(false);
            }

            // Check ApiKey/LocalUrl input dialog
            if self.is_text_input_visible() {
                let handled = self.handle_text_input_dialog_key(key.code);
                if handled {
                    return Ok(false);
                }
            }

            // Check Shortcuts dialog for scrolling
            if self.is_shortcuts_dialog_visible() {
                match key.code {
                    KeyCode::Up => {
                        if let Some(d) = self.dialog.current_mut()
                            && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                        {
                            *scroll = scroll.saturating_sub(1);
                        }
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        if let Some(d) = self.dialog.current_mut()
                            && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                        {
                            *scroll = scroll.saturating_add(1);
                        }
                        return Ok(false);
                    }
                    KeyCode::Char('k') => {
                        // Only handle 'k' for scrolling if not Ctrl+K (which toggles the dialog)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && let Some(d) = self.dialog.current_mut()
                            && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                        {
                            *scroll = scroll.saturating_sub(1);
                        }
                        // Let Ctrl+K pass through to the action handler
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            return Ok(false);
                        }
                    }
                    KeyCode::Char('j') => {
                        if let Some(d) = self.dialog.current_mut()
                            && let DialogType::Shortcuts { scroll } = &mut d.dialog_type
                        {
                            *scroll = scroll.saturating_add(1);
                        }
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        // Don't close shortcuts dialog with Esc - use Ctrl+K to toggle
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // Check permission dialog for keyboard navigation
            if self.permission_dialog.visible {
                match key.code {
                    KeyCode::Up => {
                        self.permission_dialog.selected = (self.permission_dialog.selected + 2) % 3;
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        self.permission_dialog.selected = (self.permission_dialog.selected + 1) % 3;
                        return Ok(false);
                    }
                    KeyCode::Enter => {
                        let action = match self.permission_dialog.selected {
                            0 => cosh::harness::PermissionAction::Allow,
                            1 => cosh::harness::PermissionAction::AllowOnce,
                            _ => cosh::harness::PermissionAction::Deny,
                        };
                        self.permission_dialog.visible = false;
                        let _ = self.perm_tx.send(action);
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        self.permission_dialog.visible = false;
                        let _ = self.perm_tx.send(cosh::harness::PermissionAction::Deny);
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // Shift/Ctrl/Alt+Enter inserts a newline instead of sending.
            if key.code == KeyCode::Enter && key.modifiers != KeyModifiers::NONE {
                // In RAG mode with the create-db form open, the newline
                // goes into the focused field instead of the prompt. The
                // key is consumed even when the form is at its growth
                // limit (handle_insert_newline then does nothing), so it
                // never leaks into the hidden prompt.
                #[cfg(feature = "embed")]
                if self.is_rag_mode() && !self.dialog.visible() && self.rag_view.show_create_db {
                    self.rag_view.handle_insert_newline();
                    return Ok(false);
                }
                self.insert_newline_in_prompt();
                return Ok(false);
            }

            // Left-panel-focused arrow key scrolling (runs for ALL modes)
            // Must come before mode-specific handlers (Home, InternalTools,
            // Session) which also consume Up/Down before the action dispatch.
            if self.sidebar_focused
                && self.sidebar.open
                && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
            {
                // File explorer (Ctrl+F): the panel owns the keys while it
                // is the active left-panel view — but never while a modal
                // dialog is open (Enter on a file must not launch the
                // editor under a dialog). Navigation works while the agent
                // runs; it never touches session state.
                if matches!(self.left_panel, Mode::Explorer)
                    && !self.dialog.visible()
                    && !self.question_dialog.visible
                    && !self.permission_dialog.visible
                    && !self.queue_choice_dialog.visible
                {
                    match key.code {
                        KeyCode::Up => {
                            if let Some(explorer) = &mut self.file_explorer {
                                explorer.select_prev();
                            }
                            return Ok(false);
                        }
                        KeyCode::Down => {
                            if let Some(explorer) = &mut self.file_explorer {
                                explorer.select_next();
                            }
                            return Ok(false);
                        }
                        KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                            if let Some(explorer) = &mut self.file_explorer {
                                match explorer.handle_key(key.code) {
                                    crate::left_panel::file_explorer::ExplorerAction::OpenFile(
                                        path,
                                    ) => {
                                        self.open_file_in_editor(&path);
                                    }
                                    crate::left_panel::file_explorer::ExplorerAction::None => {}
                                }
                            }
                            return Ok(false);
                        }
                        _ => {}
                    }
                    // Other keys (typing, shortcuts) fall through to the
                    // normal dispatch below.
                }
                match key.code {
                    KeyCode::Up => {
                        if self.state.status == crate::types::SessionStatus::Idle {
                            self.sidebar.select_prev(self.state.session_summaries.len());
                        }
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        if self.state.status == crate::types::SessionStatus::Idle {
                            self.sidebar.select_next(self.state.session_summaries.len());
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter => match self
                        .sidebar
                        .handle_key(key.code, &self.state.session_summaries)
                    {
                        SessionsAction::SwitchTo(session_id) => {
                            if self.state.status != crate::types::SessionStatus::Idle {
                                use crate::ui::toast::{ToastOptions, ToastVariant};
                                self.toast_state.show(ToastOptions {
                                    title: Some("Switch session".into()),
                                    message: "The agent is working — wait for it to finish.".into(),
                                    variant: ToastVariant::Warning,
                                    duration_ms: 4000,
                                });
                                return Ok(false);
                            }
                            self.state.right_panel =
                                crate::routes::session::right_panel::types::RightPanelState::new();
                            self.finalize_stale_compaction_lines();
                            self.state
                                .switch_to_session(session_id, &self.session_store);
                            // Returning to a session restores the last model
                            // used there.
                            self.restore_current_session_model();
                            // The panel content belongs to the session: rebuild
                            // it from the newly selected session's history.
                            self.rehydrate_right_panel();
                            self.session_view.hovered_msg_idx = None;
                            self.title_generated = true;
                            self.finalize_stale_compaction_lines();
                            return Ok(false);
                        }
                        SessionsAction::RequestDelete(_)
                        | SessionsAction::RequestDeleteAll
                        | SessionsAction::None => {}
                    },
                    _ => {}
                }
            }

            let action = self.keymap.lookup(key.code, key.modifiers).cloned();
            // Some terminals deliver Alt+arrows with extra modifier bits,
            // which the exact-match lookup above misses. Normalize those to
            // the queue-cycling actions so switching never silently dies.
            let action = action.or_else(|| match key.code {
                KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                    Some(crate::keymap::Action::PrevAgent)
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                    Some(crate::keymap::Action::NextAgent)
                }
                _ => None,
            });

            // Home mode: navigation keys (skip when dialog is visible)
            if matches!(self.mode(), AppMode::Home) && !self.dialog.visible() {
                match key.code {
                    KeyCode::Up => {
                        self.home_view.select_prev();
                    }
                    KeyCode::Down => {
                        self.home_view.select_next();
                    }
                    KeyCode::Enter => {
                        match self.home_view.selected_action() {
                            HomeAction::NewSession => {
                                self.start_new_session();
                            }
                            HomeAction::ToggleSidebar => {
                                self.sidebar.open = !self.sidebar.open;
                            }
                            HomeAction::OpenInternalTools => {
                                self.show_internal_tools = true;
                            }
                            HomeAction::OpenShortcuts => {
                                self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                            }
                            HomeAction::OpenAddProvider => {
                                self.show_add_provider = true;
                            }
                            HomeAction::OpenSettings => {
                                self.telemetry_feature(cosh::telemetry::schema::Feature::Settings);
                                self.show_settings = true;
                            }
                            HomeAction::OpenModelRouter => {
                                // Refresh fallbacks from prefs cache and models from model cache
                                let saved = fallback::load_fallbacks(&self.setup);
                                self.router_view.set_fallbacks(saved);
                                let saved = fallback::load_prompt_corrector_fallbacks(&self.setup);
                                self.router_view.set_prompt_corrector_fallbacks(saved);
                                self.show_router = true;
                            }
                            #[cfg(feature = "embed")]
                            HomeAction::OpenRag => {
                                self.show_rag = true;
                            }
                            HomeAction::Banner(banner_action) => {
                                self.handle_banner_action(banner_action);
                            }
                        }
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // InternalTools mode: navigation and toggle keys
            // Each matched arm returns early so unmatched keys fall through
            // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
            if matches!(self.mode(), AppMode::InternalTools) && !self.dialog.visible() {
                match key.code {
                    KeyCode::Up => {
                        let list_area = 20; // max visible items estimate based on terminal
                        self.internal_tools_view.select_prev(list_area);
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        let list_area = 20;
                        self.internal_tools_view.select_next(list_area);
                        return Ok(false);
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        self.internal_tools_view.toggle_current();
                        crate::routes::tools::save_disabled_tools(
                            &mut self.setup,
                            &self.internal_tools_view.disabled,
                        );
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        self.show_internal_tools = false;
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // Settings mode: navigation and activation keys
            // Each matched arm returns early so unmatched keys fall through
            // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
            if matches!(self.mode(), AppMode::Settings) && !self.dialog.visible() {
                let operation = match key.code {
                    KeyCode::Delete => Some("remove"),
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => Some("up"),
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => Some("down"),
                    _ => None,
                };
                if let Some(operation) = operation {
                    if self
                        .settings_view
                        .change_summarizer(&mut self.setup, operation)
                    {
                        self.setup.save();
                    } else if operation == "remove"
                        && let Some(name) = self
                            .settings_view
                            .remove_selected_mcp_server(&mut self.setup)
                    {
                        // Purge the keyring credential too: a forgotten
                        // server must not leave a live secret behind.
                        // Failures are non-fatal — the entry is gone either
                        // way, and the store may be locked.
                        if let Err(err) = cosh::mcp::auth::forget_key(&name) {
                            log::warn!("keyring cleanup for removed MCP server failed: {}", err);
                        }
                        self.setup.save();
                    }
                    return Ok(false);
                }
                match key.code {
                    KeyCode::Up => {
                        self.settings_view.select_prev(20, &self.setup);
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        self.settings_view.select_next(20, &self.setup);
                        return Ok(false);
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        match self.settings_view.activate_selected(&mut self.setup) {
                            Some(
                                crate::routes::settings::SettingsAction::OpenSummarizationModel {
                                    index,
                                },
                            ) => {
                                self.open_summarization_model_dialog(index);
                            }
                            Some(crate::routes::settings::SettingsAction::ToggleSaved) => {
                                self.setup.save();
                            }
                            Some(crate::routes::settings::SettingsAction::LspToggled) => {
                                // Keep the harness's process-wide LSP flag and
                                // the prompt footer in sync with the new switch.
                                cosh::harness::lsp::set_lsp_enabled(self.setup.lsp);
                                self.state.lsp_available = self.setup.lsp;
                                // Disabling is the one user-driven
                                // invalidation: drop the visible servers now
                                // instead of waiting for the next turn.
                                if !self.setup.lsp {
                                    self.state.lsp_servers.clear();
                                }
                                self.setup.save();
                            }
                            Some(crate::routes::settings::SettingsAction::OpenHookForm {
                                event,
                                index,
                            }) => {
                                self.open_hook_form(event, index);
                            }
                            Some(crate::routes::settings::SettingsAction::OpenCacheInput {
                                setting,
                            }) => {
                                self.open_cache_ttl_input(setting);
                            }
                            Some(crate::routes::settings::SettingsAction::OpenEditorInput) => {
                                self.open_editor_input();
                            }
                            Some(crate::routes::settings::SettingsAction::OpenSkillsInput) => {
                                self.open_skills_input();
                            }
                            Some(crate::routes::settings::SettingsAction::McpToggled) => {
                                self.setup.save();
                            }
                            Some(crate::routes::settings::SettingsAction::TelemetryToggled) => {
                                // Consent persisted; the facade re-resolves it
                                // (env override + CI hard-off) on next launch.
                                self.setup.save();
                            }
                            Some(crate::routes::settings::SettingsAction::OpenMcpForm) => {
                                self.open_mcp_form();
                            }
                            None => {}
                        }
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        self.show_settings = false;
                        return Ok(false);
                    }
                    _ => {}
                }
            }
            // RAG mode: handle Ctrl+Backspace, Ctrl+Left, Ctrl+Right
            // before passing key.code (which loses modifier info).
            #[cfg(feature = "embed")]
            if self.is_rag_mode() && !self.dialog.visible() {
                match key.code {
                    KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.rag_view.handle_ctrl_backspace();
                        return Ok(false);
                    }
                    KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.rag_view.handle_ctrl_left();
                        return Ok(false);
                    }
                    KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.rag_view.handle_ctrl_right();
                        return Ok(false);
                    }
                    // Ctrl+W = delete word before cursor (universal terminal shortcut)
                    KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.rag_view.handle_ctrl_backspace();
                        return Ok(false);
                    }
                    // Ctrl+J = newline (universal ^J) in the create-db
                    // fields. Consumed whenever the form is open, even at
                    // its growth limit, so it never falls through and
                    // types a literal 'j'.
                    KeyCode::Char('j')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && self.rag_view.show_create_db =>
                    {
                        self.rag_view.handle_insert_newline();
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // Router mode: navigation and add/remove agent-loop fallbacks,
            // plus the independent ordered prompt-correction route.
            if matches!(self.mode(), AppMode::Router) && !self.dialog.visible() {
                let all_models = self.collect_cached_models();
                match key.code {
                    KeyCode::Up => {
                        match self.router_view.focus {
                            FocusTarget::Fallbacks => self.router_view.select_prev_fallback(),
                            FocusTarget::Models => self.router_view.select_prev(&all_models),
                            FocusTarget::PromptCorrectorModels => {
                                self.router_view
                                    .select_prev_prompt_corrector_model(&all_models);
                            }
                            FocusTarget::PromptCorrectorAcp => {
                                self.router_view.select_prev_prompt_corrector_acp();
                            }
                            FocusTarget::PromptCorrectorFallbacks => {
                                self.router_view.select_prev_prompt_corrector_fallback();
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        match self.router_view.focus {
                            FocusTarget::Fallbacks => self.router_view.select_next_fallback(),
                            FocusTarget::Models => self.router_view.select_next(&all_models),
                            FocusTarget::PromptCorrectorModels => {
                                self.router_view
                                    .select_next_prompt_corrector_model(&all_models);
                            }
                            FocusTarget::PromptCorrectorAcp => {
                                self.router_view.select_next_prompt_corrector_acp();
                            }
                            FocusTarget::PromptCorrectorFallbacks => {
                                self.router_view.select_next_prompt_corrector_fallback();
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter => {
                        match self.router_view.focus {
                            FocusTarget::Models => {
                                self.router_view.add_selected_to_fallback(&all_models);
                                fallback::save_fallbacks(
                                    &mut self.setup,
                                    &self.router_view.fallbacks,
                                );
                            }
                            FocusTarget::PromptCorrectorModels => {
                                if self
                                    .router_view
                                    .toggle_selected_prompt_corrector_model(&all_models)
                                {
                                    fallback::save_prompt_corrector_fallbacks(
                                        &mut self.setup,
                                        &self.router_view.prompt_corrector_fallbacks,
                                    );
                                }
                            }
                            FocusTarget::PromptCorrectorAcp => {
                                if self.router_view.toggle_selected_acp_agent() {
                                    fallback::save_prompt_corrector_fallbacks(
                                        &mut self.setup,
                                        &self.router_view.prompt_corrector_fallbacks,
                                    );
                                }
                            }
                            FocusTarget::Fallbacks => {}
                            FocusTarget::PromptCorrectorFallbacks => {}
                        }
                        return Ok(false);
                    }
                    KeyCode::Backspace | KeyCode::Delete => {
                        if self.router_view.focus == FocusTarget::Fallbacks {
                            if self.router_view.remove_selected_fallback().is_some() {
                                fallback::save_fallbacks(
                                    &mut self.setup,
                                    &self.router_view.fallbacks,
                                );
                            }
                        } else if matches!(
                            self.router_view.focus,
                            FocusTarget::PromptCorrectorFallbacks
                        ) {
                            if self.router_view.remove_selected_prompt_corrector() {
                                fallback::save_prompt_corrector_fallbacks(
                                    &mut self.setup,
                                    &self.router_view.prompt_corrector_fallbacks,
                                );
                            }
                        } else if self.router_view.focus == FocusTarget::Models
                            && !self.router_view.search_bar.is_empty()
                        {
                            self.router_view.pop_filter_char();
                        } else if self.router_view.focus == FocusTarget::PromptCorrectorModels
                            && !self.router_view.prompt_model_search_bar.is_empty()
                        {
                            self.router_view.pop_prompt_model_filter_char();
                        } else if let Some(selected) = self.router_view.selected_model(&all_models)
                        {
                            let idx = self.router_view.fallbacks.iter().position(|f| {
                                f.provider == selected.provider && f.model == selected.model
                            });
                            if let Some(i) = idx {
                                self.router_view.remove_fallback(i);
                                fallback::save_fallbacks(
                                    &mut self.setup,
                                    &self.router_view.fallbacks,
                                );
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        if self.router_view.acp_picker_open() {
                            self.router_view.close_acp_picker();
                            return Ok(false);
                        }
                        self.router_view.clear_num_buffer();
                        // Leave no stale hover wash behind: the route is gone
                        // from the screen, so the block highlight must be too.
                        self.router_view.clear_hover();
                        self.show_router = false;
                        return Ok(false);
                    }
                    KeyCode::Tab => {
                        self.router_view.cycle_focus();
                        return Ok(false);
                    }
                    // Left/Right switch the visible section; the active
                    // tab's lists own Up/Down.
                    KeyCode::Left => {
                        self.router_view.select_prev_tab();
                        return Ok(false);
                    }
                    KeyCode::Right => {
                        self.router_view.select_next_tab();
                        return Ok(false);
                    }
                    KeyCode::Char(ch) => match self.router_view.focus {
                        FocusTarget::Models => {
                            if ch.is_ascii_digit() {
                                self.router_view.handle_number_input(ch);
                            } else {
                                self.router_view.push_filter_char(ch);
                            }
                        }
                        FocusTarget::PromptCorrectorModels => {
                            self.router_view.push_prompt_model_filter_char(ch);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            // RAG mode: handle via RagView
            if self.handle_rag_key_event(key.code) {
                return Ok(false);
            }

            // AddProvider mode: navigation and select
            // Each matched arm returns early so unmatched keys fall through
            // to the keymap action dispatch (e.g. Ctrl+B, Ctrl+K).
            if matches!(self.mode(), AppMode::AddProvider) && !self.dialog.visible() {
                match key.code {
                    KeyCode::Up => {
                        self.add_provider_view.select_prev();
                        return Ok(false);
                    }
                    KeyCode::Down => {
                        self.add_provider_view.select_next();
                        return Ok(false);
                    }
                    KeyCode::Enter => {
                        if let Some(entry) = self.add_provider_view.selected_provider() {
                            self.open_provider_dialog(&entry);
                        }
                        return Ok(false);
                    }
                    KeyCode::Esc => {
                        self.show_add_provider = false;
                        return Ok(false);
                    }
                    KeyCode::Char(ch) => {
                        self.add_provider_view.push_filter_char(ch);
                        return Ok(false);
                    }
                    KeyCode::Backspace => {
                        self.add_provider_view.pop_filter_char();
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            // If slash menu is visible, arrow keys should move selection there
            if self.slash_menu.visible {
                match key.code {
                    KeyCode::Up => self.slash_menu.select_prev(),
                    KeyCode::Down => self.slash_menu.select_next(),
                    KeyCode::Enter => {
                        self.prompt_view.note_activity();
                        if let Some(cmd) = self.slash_menu.get_selected_command().cloned() {
                            self.run_slash_command(&cmd);
                        }
                    }
                    KeyCode::Esc => {
                        self.prompt_view.note_activity();
                        // Draft context ends: the edit history is dropped
                        // with it (nothing to undo back to).
                        self.prompt_view.clear_draft();
                        self.slash_menu.visible = false;
                    }
                    KeyCode::Backspace => {
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            self.prompt_view.delete_word_before_cursor();
                            self.slash_menu.update(&self.prompt_view.input);
                        } else {
                            self.prompt_view.note_activity();
                            if !self.prompt_view.input.is_empty() {
                                // Route through `backspace()` so a pasted
                                // virtual-text placeholder is still removed
                                // atomically instead of chipped char by char.
                                self.prompt_view.backspace();
                                self.slash_menu.update(&self.prompt_view.input);
                            }
                        }
                    }
                    KeyCode::Char(ch) => {
                        // A Ctrl-combo (Ctrl+Z undo, Ctrl+Y redo, Ctrl+J
                        // newline, Ctrl+W delete-word) must NEVER type its
                        // letter into the filtered command — the menu sits
                        // BEFORE the Session-mode Char handler, so handle
                        // the known combos here and swallow the rest.
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            match ch {
                                'z' => self.prompt_undo(),
                                'y' => self.prompt_redo(),
                                'j' => self.prompt_view.insert_newline(),
                                'w' => self.prompt_view.delete_word_before_cursor(),
                                _ => {}
                            }
                            return Ok(false);
                        }
                        // Typing hands keyboard control back
                        // from the right panel to the prompt.
                        self.state.right_panel.panel_focus = None;
                        self.prompt_view.note_activity();
                        self.prompt_view.type_char(ch);
                        let was_visible = self.slash_menu.visible;
                        self.slash_menu.update(&self.prompt_view.input);
                        if was_visible
                            && !self.slash_menu.visible
                            && self.prompt_view.input.starts_with('/')
                        {
                            self.prompt_view.strip_leading_slash();
                        }
                    }
                    _ => {}
                }
                return Ok(false);
            }

            // When the usage dashboard is open, Tab / Shift+Tab cycle the
            // period selector instead of toggling mode focus. (Shift+Tab
            // arrives as BackTab, which the keymap doesn't bind.)
            if self.sidebar.open
                && matches!(self.left_panel, Mode::Dashboard)
                && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
                && self.state.right_panel.panel_focus.is_none()
            {
                match key.code {
                    KeyCode::Tab => {
                        self.cycle_usage_period(true);
                        return Ok(false);
                    }
                    KeyCode::BackTab => {
                        self.cycle_usage_period(false);
                        return Ok(false);
                    }
                    _ => {}
                }
            }

            match action {
                Some(crate::keymap::Action::ScrollUp) => {
                    if self.sidebar_focused
                        && self.sidebar.open
                        && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
                    {
                        self.sidebar.select_prev(self.state.session_summaries.len());
                    } else if self.is_in_right_panel(self.last_mouse_x) {
                        self.state.right_panel.scroll_up_at(self.last_mouse_y, 3);
                    } else {
                        let vh = self.session_view.visible_height.max(1);
                        let delta = -(vh as f64 / 5.0);
                        self.session_view.scroll_by_raw(delta);
                        self.session_view.reset_scroll_accumulator();
                    }
                }
                Some(crate::keymap::Action::ScrollDown) => {
                    if self.sidebar_focused
                        && self.sidebar.open
                        && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
                    {
                        self.sidebar.select_next(self.state.session_summaries.len());
                    } else if self.is_in_right_panel(self.last_mouse_x) {
                        self.state.right_panel.scroll_down_at(self.last_mouse_y, 3);
                    } else {
                        let vh = self.session_view.visible_height.max(1);
                        let delta = vh as f64 / 5.0;
                        self.session_view.scroll_by_raw(delta);
                        self.session_view.reset_scroll_accumulator();
                    }
                }
                Some(crate::keymap::Action::ScrollUpPage) => {
                    if self.sidebar_focused
                        && self.sidebar.open
                        && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
                    {
                        self.sidebar
                            .select_first(self.state.session_summaries.len());
                    } else if self.is_in_right_panel(self.last_mouse_x) {
                        let vh = self.state.right_panel.visible_height.max(1);
                        self.state
                            .right_panel
                            .scroll_up_at(self.last_mouse_y, vh / 2);
                    } else {
                        let vh = self.session_view.visible_height.max(1);
                        let delta = -(vh as f64 / 2.0);
                        self.session_view.scroll_by_raw(delta);
                        self.session_view.reset_scroll_accumulator();
                    }
                }
                Some(crate::keymap::Action::ScrollDownPage) => {
                    if self.sidebar_focused
                        && self.sidebar.open
                        && self.terminal_size().width >= MIN_WIDTH_FOR_LEFT_PANEL
                    {
                        self.sidebar.select_last(self.state.session_summaries.len());
                    } else if self.is_in_right_panel(self.last_mouse_x) {
                        let vh = self.state.right_panel.visible_height.max(1);
                        self.state
                            .right_panel
                            .scroll_down_at(self.last_mouse_y, vh / 2);
                    } else {
                        let vh = self.session_view.visible_height.max(1);
                        let delta = vh as f64 / 2.0;
                        self.session_view.scroll_by_raw(delta);
                        self.session_view.reset_scroll_accumulator();
                    }
                }
                Some(crate::keymap::Action::ToggleSidebar) => {
                    // Ctrl+B: toggle the left panel open/closed.
                    self.sidebar.open = !self.sidebar.open;
                }
                Some(crate::keymap::Action::ToggleRightPanel) => {
                    // Ctrl+P: toggle the right panel open/closed.
                    self.state.right_panel.toggle_hidden();
                    if self.state.right_panel.user_hidden {
                        // A hidden panel cannot hold keyboard focus.
                        self.state.right_panel.panel_focus = None;
                    }
                }
                Some(crate::keymap::Action::ToggleUsage) => {
                    // Ctrl+U: always open the left panel showing the dashboard.
                    self.show_dashboard();
                }
                Some(crate::keymap::Action::ShowSessionHistory) => {
                    // Ctrl+S: always open the left panel showing session history.
                    self.show_session_history();
                }
                Some(crate::keymap::Action::ShowFileExplorer) => {
                    // Ctrl+F: always open the left panel showing the file
                    // explorer rooted at the working directory.
                    self.show_file_explorer();
                }
                Some(crate::keymap::Action::ToggleHelp) => {
                    if self.dialog.visible()
                        && matches!(self.dialog.current(), Some(d) if matches!(d.dialog_type, DialogType::Shortcuts { .. }))
                    {
                        self.dialog.clear();
                    } else {
                        self.dialog.show(DialogType::Shortcuts { scroll: 0 });
                    }
                }
                Some(
                    crate::keymap::Action::NextSession
                    | crate::keymap::Action::PrevSession
                    | crate::keymap::Action::FocusInput
                    | crate::keymap::Action::Quit,
                ) => {
                    // TBD
                }
                Some(crate::keymap::Action::NextAgent | crate::keymap::Action::PrevAgent) => {
                    // Cycle the right-panel keyboard focus across agent
                    // queues — the only way to reach a queue whose
                    // windows are hidden for lack of space.
                    let dir = if matches!(action, Some(crate::keymap::Action::PrevAgent)) {
                        -1
                    } else {
                        1
                    };
                    self.state.right_panel.cycle_agent_queue(dir);
                }
                Some(crate::keymap::Action::SendMessage | crate::keymap::Action::Confirm) => {
                    if let Some(dialog) = self.dialog.current()
                        && matches!(dialog.dialog_type, DialogType::Confirm { .. })
                    {
                        if dialog.selected == 0 {
                            match self.pending_delete.take() {
                                Some(PendingSessionDelete::One(session_id)) => {
                                    self.delete_session(&session_id);
                                    self.dialog.pop();
                                }
                                Some(PendingSessionDelete::AllOfCwd) => {
                                    self.delete_all_sessions();
                                    self.dialog.pop();
                                }
                                None if self.handle_rag_confirm_delete() => {
                                    // handled
                                }
                                None => {
                                    self.should_quit = true;
                                }
                            }
                        } else {
                            self.pending_delete = None;
                            self.clear_rag_pending_state();
                            self.dialog.pop();
                        }
                        return Ok(false);
                    }
                    self.prompt_view.note_activity();
                    if self.state.status == crate::types::SessionStatus::Working {
                        // The agent loop is running: ask which queue the
                        // message should join instead of sending it now.
                        let preview = self.prompt_view.input.clone();
                        if preview.trim().is_empty() {
                            return Ok(false);
                        }
                        self.queue_choice_dialog.open(preview);
                        return Ok(false);
                    }

                    let msg = self.prompt_view.send_message();
                    if msg.trim().is_empty() {
                        return Ok(false);
                    }
                    // An edited queued message outstanding? It goes back to
                    // its queue slot (never a direct provider input) and the
                    // queue chain resumes FIFO. Otherwise: normal send.
                    self.submit_prompt_message(msg);
                }
                Some(crate::keymap::Action::Interrupt) => {
                    if self.state.status == crate::types::SessionStatus::Working {
                        self.stop_signal.store(true, Ordering::Relaxed);
                    } else if self.queue_choice_dialog.visible {
                        self.queue_choice_dialog.hide();
                        self.prompt_view.focus();
                    } else if self.question_dialog.visible {
                        self.question_dialog.visible = false;
                        let _ = self
                            .answer_tx
                            .send(Err("User dismissed the question dialog".into()));
                        self.prompt_view.focus();
                    } else if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog.hide();
                        self.restore_pending_gateway_message();
                    } else if self.dialog.visible() {
                        self.pending_delete = None;
                        self.clear_rag_pending_state();
                        self.dialog.pop();
                    }
                }
                Some(crate::keymap::Action::Cancel) => {
                    if self.state.status == crate::types::SessionStatus::Working {
                        self.stop_signal.store(true, Ordering::Relaxed);
                        // Clear pending queues: the user explicitly
                        // cancelled, so queued follow-ups should not
                        // auto-start a new loop.
                        if let Some(id) = self.state.current_session_id.clone()
                            && let Some(queues) = self.state.pending_queues.get_mut(&id)
                        {
                            queues.clear();
                        }
                        self.next_request_in_flight = false;
                        self.queue_actions_deferred_start = false;
                        self.edit_requeue_hint = None;
                        self.hovered_queue_row = None;
                        return Ok(false);
                    }
                    if self.queue_choice_dialog.visible {
                        self.queue_choice_dialog.hide();
                        self.prompt_view.focus();
                    } else if self.question_dialog.visible {
                        self.question_dialog.visible = false;
                        let _ = self
                            .answer_tx
                            .send(Err("User dismissed the question dialog".into()));
                        self.prompt_view.focus();
                    } else if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog.hide();
                        self.restore_pending_gateway_message();
                    } else if self.dialog.visible() {
                        self.pending_delete = None;
                        self.clear_rag_pending_state();
                        self.dialog.pop();
                    } else if matches!(self.mode(), AppMode::Session) {
                        self.state.current_session_id = None;
                        // Back to Home: the header widget is gone, so its
                        // state must not survive into the next session.
                        self.hypercredit_balance = None;
                        self.state.right_panel =
                            crate::routes::session::right_panel::types::RightPanelState::new();
                    } else if matches!(self.mode(), AppMode::AddProvider) {
                        self.show_add_provider = false;
                    } else if matches!(self.mode(), AppMode::Settings) {
                        self.show_settings = false;
                    } else if self.is_rag_mode() {
                        self.handle_rag_cancel_action();
                    } else if matches!(self.mode(), AppMode::Home) {
                        self.pending_delete = None;
                        self.dialog.show(DialogType::Confirm {
                            message: "Quit cosh?".into(),
                        });
                        if let Some(d) = self.dialog.current_mut() {
                            d.selected = 1;
                        }
                    }
                }
                Some(crate::keymap::Action::ScrollToTop) => {
                    if self.is_in_right_panel(self.last_mouse_x) {
                        self.state.right_panel.reset_scroll();
                    } else {
                        self.session_view.scroll_to(0);
                    }
                }
                Some(crate::keymap::Action::ScrollToBottom) => {
                    if self.is_in_right_panel(self.last_mouse_x) {
                        self.state.right_panel.scroll_to_bottom();
                    } else {
                        self.session_view.scroll_to_bottom();
                    }
                }
                Some(crate::keymap::Action::ToggleConceal) => {
                    self.config.conceal = !self.config.conceal;
                }
                Some(crate::keymap::Action::ToggleThinking) => {
                    self.config.thinking_mode = !self.config.thinking_mode;
                }
                Some(crate::keymap::Action::ToggleDiagnostics) => {
                    self.config.diagnostics_mode = !self.config.diagnostics_mode;
                }
                Some(crate::keymap::Action::ToggleToolDetails) => {
                    self.config.show_tool_details = !self.config.show_tool_details;
                }
                Some(crate::keymap::Action::ToggleGenericToolOutput) => {
                    self.config.show_generic_tool_output = !self.config.show_generic_tool_output;
                }
                Some(crate::keymap::Action::ToggleMode) => {
                    if matches!(self.mode(), AppMode::Session) {
                        use cosh::harness::Mode;
                        self.state.mode = match self.state.mode {
                            Mode::Build => Mode::Ask,
                            Mode::Ask => Mode::Yolo,
                            Mode::Yolo => Mode::Command,
                            Mode::Command => Mode::Build,
                        };
                    }
                }
                Some(crate::keymap::Action::HistoryUp) => {
                    self.prompt_view.note_activity();
                    let user_msgs = self
                        .state
                        .current_session()
                        .map(PromptView::user_message_texts)
                        .unwrap_or_default();
                    self.prompt_view.history_up(&user_msgs);
                }
                Some(crate::keymap::Action::HistoryDown) => {
                    self.prompt_view.note_activity();
                    let user_msgs = self
                        .state
                        .current_session()
                        .map(PromptView::user_message_texts)
                        .unwrap_or_default();
                    self.prompt_view.history_down(&user_msgs);
                }
                Some(crate::keymap::Action::ClearQueue) => {
                    if let Some(id) = self.state.current_session_id.clone()
                        && let Some(queues) = self.state.pending_queues.get_mut(&id)
                    {
                        queues.clear();
                    }
                    // Nothing left to acknowledge, resume or highlight.
                    self.next_request_in_flight = false;
                    self.queue_actions_deferred_start = false;
                    self.edit_requeue_hint = None;
                    self.hovered_queue_row = None;
                }
                None => {
                    if self.slash_menu.visible {
                        match key.code {
                            KeyCode::Up => self.slash_menu.select_prev(),
                            KeyCode::Down => self.slash_menu.select_next(),
                            KeyCode::Enter => {
                                self.prompt_view.note_activity();
                                if let Some(cmd) = self.slash_menu.get_selected_command().cloned() {
                                    self.run_slash_command(&cmd);
                                }
                            }
                            KeyCode::Esc => {
                                self.prompt_view.note_activity();
                                // Draft context ends: the edit history is
                                // dropped with it (nothing to undo back to).
                                self.prompt_view.clear_draft();
                                self.slash_menu.visible = false;
                            }
                            KeyCode::Backspace => {
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.delete_word_before_cursor();
                                    self.slash_menu.update(&self.prompt_view.input);
                                } else {
                                    self.prompt_view.note_activity();
                                    if !self.prompt_view.input.is_empty() {
                                        // Route through `backspace()` so the
                                        // deletion lands in the edit history.
                                        self.prompt_view.backspace();
                                        self.slash_menu.update(&self.prompt_view.input);
                                    }
                                }
                            }
                            KeyCode::Char(ch) => {
                                // Same Ctrl-combo rule as the first menu
                                // arm above: modifiers never type.
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    match ch {
                                        'z' => self.prompt_undo(),
                                        'y' => self.prompt_redo(),
                                        'j' => self.prompt_view.insert_newline(),
                                        'w' => self.prompt_view.delete_word_before_cursor(),
                                        _ => {}
                                    }
                                    return Ok(false);
                                }
                                // Typing hands keyboard control back
                                // from the right panel to the prompt.
                                self.state.right_panel.panel_focus = None;
                                self.prompt_view.note_activity();
                                self.prompt_view.type_char(ch);
                                let was_visible = self.slash_menu.visible;
                                self.slash_menu.update(&self.prompt_view.input);
                                // If menu closed (e.g., user typed space), remove the leading "/"
                                if was_visible
                                    && !self.slash_menu.visible
                                    && self.prompt_view.input.starts_with('/')
                                {
                                    self.prompt_view.strip_leading_slash();
                                }
                            }
                            _ => {}
                        }
                        return Ok(false);
                    }

                    if matches!(self.mode(), AppMode::Session) {
                        match key.code {
                            KeyCode::Up => {
                                if self.prompt_view.is_focused {
                                    if self.prompt_view.input.is_empty()
                                        || self.prompt_view.history_index != -1
                                    {
                                        let user_msgs = self
                                            .state
                                            .current_session()
                                            .map(PromptView::user_message_texts)
                                            .unwrap_or_default();
                                        self.prompt_view.history_up(&user_msgs);
                                    } else {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.cursor_up(
                                            self.prompt_view.input_text_width.get().max(1),
                                        );
                                    }
                                } else if self.is_in_right_panel(self.last_mouse_x) {
                                    self.state.right_panel.scroll_up_at(self.last_mouse_y, 3);
                                } else {
                                    let vh = self.session_view.visible_height.max(1);
                                    let delta = -(vh as f64 / 5.0);
                                    self.session_view.scroll_by_raw(delta);
                                    self.session_view.reset_scroll_accumulator();
                                }
                            }
                            KeyCode::Down => {
                                if self.prompt_view.is_focused {
                                    if self.prompt_view.input.is_empty()
                                        || self.prompt_view.history_index != -1
                                    {
                                        let user_msgs = self
                                            .state
                                            .current_session()
                                            .map(PromptView::user_message_texts)
                                            .unwrap_or_default();
                                        self.prompt_view.history_down(&user_msgs);
                                    } else {
                                        self.prompt_view.note_activity();
                                        self.prompt_view.cursor_down(
                                            self.prompt_view.input_text_width.get().max(1),
                                        );
                                    }
                                } else if self.is_in_right_panel(self.last_mouse_x) {
                                    self.state.right_panel.scroll_down_at(self.last_mouse_y, 3);
                                } else {
                                    let vh = self.session_view.visible_height.max(1);
                                    let delta = vh as f64 / 5.0;
                                    self.session_view.scroll_by_raw(delta);
                                    self.session_view.reset_scroll_accumulator();
                                }
                            }
                            KeyCode::Left => {
                                if self.state.right_panel.panel_focus.is_some() {
                                    // Focused panel slot: bash toggles history
                                    // mode, an agent queue steps back one entry.
                                    self.state.right_panel.panel_left();
                                } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.cursor_word_left();
                                } else {
                                    self.prompt_view.note_activity();
                                    if self.prompt_view.cursor_pos > 0 {
                                        self.prompt_view.cursor_pos = self
                                            .prompt_view
                                            .input
                                            .floor_char_boundary(self.prompt_view.cursor_pos - 1);
                                    }
                                }
                            }
                            KeyCode::Right => {
                                if self.state.right_panel.panel_focus.is_some() {
                                    // Focused panel slot: bash returns to live,
                                    // an agent queue steps forward (live at newest).
                                    self.state.right_panel.panel_right();
                                } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.cursor_word_right();
                                } else {
                                    self.prompt_view.note_activity();
                                    let len = self.prompt_view.input.len();
                                    if self.prompt_view.cursor_pos < len {
                                        let c = self.prompt_view.input
                                            [self.prompt_view.cursor_pos..]
                                            .chars()
                                            .next()
                                            .unwrap();
                                        self.prompt_view.cursor_pos += c.len_utf8();
                                    }
                                }
                            }
                            KeyCode::Home => {
                                self.prompt_view.note_activity();
                                self.prompt_view.cursor_pos = 0;
                            }
                            KeyCode::End => {
                                self.prompt_view.note_activity();
                                self.prompt_view.cursor_pos = self.prompt_view.input.len();
                            }
                            KeyCode::Delete => {
                                self.prompt_view.delete();
                            }
                            KeyCode::PageUp => {
                                if self.is_in_right_panel(self.last_mouse_x) {
                                    let vh = self.state.right_panel.visible_height.max(1);
                                    self.state
                                        .right_panel
                                        .scroll_up_at(self.last_mouse_y, vh / 2);
                                } else {
                                    let vh = self.session_view.visible_height.max(1);
                                    let delta = -(vh as f64 / 2.0);
                                    self.session_view.scroll_by_raw(delta);
                                    self.session_view.reset_scroll_accumulator();
                                }
                            }
                            KeyCode::PageDown => {
                                if self.is_in_right_panel(self.last_mouse_x) {
                                    let vh = self.state.right_panel.visible_height.max(1);
                                    self.state
                                        .right_panel
                                        .scroll_down_at(self.last_mouse_y, vh / 2);
                                } else {
                                    let vh = self.session_view.visible_height.max(1);
                                    let delta = vh as f64 / 2.0;
                                    self.session_view.scroll_by_raw(delta);
                                    self.session_view.reset_scroll_accumulator();
                                }
                            }
                            KeyCode::Backspace => {
                                // Ctrl+Backspace = delete word before cursor
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.delete_word_before_cursor();
                                } else {
                                    self.prompt_view.backspace();
                                }
                            }
                            // Shift+B / Shift+N cycle agent queues while the
                            // right panel owns the keyboard — an alternative
                            // to Alt+arrows whose SHIFT modifier survives
                            // every terminal encoding (crossterm derives it
                            // from the uppercase letter). Without panel
                            // focus the guards fail and the letters fall
                            // through to normal typing below.
                            KeyCode::Char('B')
                                if key.modifiers.contains(KeyModifiers::SHIFT)
                                    && self.state.right_panel.panel_focus.is_some() =>
                            {
                                self.state.right_panel.cycle_agent_queue(-1);
                            }
                            KeyCode::Char('N')
                                if key.modifiers.contains(KeyModifiers::SHIFT)
                                    && self.state.right_panel.panel_focus.is_some() =>
                            {
                                self.state.right_panel.cycle_agent_queue(1);
                            }
                            KeyCode::Char(ch) => {
                                self.prompt_view.note_activity();

                                // Ctrl+J is the universal newline (^J = \n) — works in every terminal
                                if ch == 'j' && key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.insert_newline();
                                    return Ok(false);
                                }

                                // Ctrl+W = delete word before cursor (universal terminal shortcut)
                                if ch == 'w' && key.modifiers.contains(KeyModifiers::CONTROL) {
                                    self.prompt_view.delete_word_before_cursor();
                                    return Ok(false);
                                }

                                // Ctrl+Z / Ctrl+Y: prompt edit-history undo /
                                // redo. Raw mode means the terminal never maps
                                // ^Z to SIGTSTP — the key always reaches this
                                // handler, so the universal convention works
                                // with NO Shift fallback needed. A Ctrl+Z right
                                // after a prompt correction restores the
                                // ORIGINAL pre-correction draft (the correction
                                // is one atomic history group); the toast
                                // reports that transition, Ctrl+Y reapplies.
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    if ch == 'z' {
                                        self.prompt_undo();
                                        return Ok(false);
                                    }
                                    if ch == 'y' {
                                        self.prompt_redo();
                                        return Ok(false);
                                    }
                                }

                                // Vim-style scroll: j/k scroll the chat view only when
                                // the prompt is NOT focused. When focused, all characters
                                // type normally so the user can start messages with j/k.
                                if (ch == 'j' || ch == 'k') && !self.prompt_view.is_focused {
                                    let vh = self.session_view.visible_height.max(1);
                                    let delta = if ch == 'j' {
                                        vh as f64 / 5.0
                                    } else {
                                        -(vh as f64 / 5.0)
                                    };
                                    self.session_view.scroll_by_raw(delta);
                                    self.session_view.reset_scroll_accumulator();
                                    return Ok(false);
                                }

                                // Insert character normally — recorded into
                                // the edit history (one Ctrl+Z undoes the
                                // coalesced burst; len_utf8() keeps the cursor
                                // on a valid UTF-8 boundary for multi-byte
                                // chars, e.g. á, é, emoji).
                                self.prompt_view.type_char(ch);

                                // Typing modifies input, exit history browsing
                                self.prompt_view.reset_history_index();

                                // Check if "/" menu should open
                                self.slash_menu.update(&self.prompt_view.input);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    /// Insert a newline at the prompt cursor (Shift/Ctrl/Alt+Enter, and the
    /// paste-burst Enter suppression route through here so both paths keep
    /// identical cursor/activity semantics).
    fn insert_newline_in_prompt(&mut self) {
        self.prompt_view.note_activity();
        self.prompt_view.insert_newline();
    }

    /// Ctrl+Z on the chat prompt: step back one edit-history group (last
    /// typing/deletion burst, paste, programmatic load, or a WHOLE prompt
    /// correction back to the original draft). The slash menu re-syncs
    /// because the draft text just changed under it.
    fn prompt_undo(&mut self) {
        let outcome = self.prompt_view.undo();
        if outcome != UndoOutcome::Noop {
            self.prompt_view.note_activity();
        }
        if outcome == UndoOutcome::OriginalBeforeCorrection {
            self.toast_state.show(crate::ui::toast::ToastOptions {
                title: Some("Prompt correction".into()),
                message: "Original prompt restored — Ctrl+Y reapplies the correction.".into(),
                variant: crate::ui::toast::ToastVariant::Info,
                duration_ms: 3000,
            });
        }
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// Ctrl+Y on the chat prompt: reapply the most recently undone edit —
    /// including the corrector's rewrite, reported with a toast.
    fn prompt_redo(&mut self) {
        let outcome = self.prompt_view.redo();
        if outcome != RedoOutcome::Noop {
            self.prompt_view.note_activity();
        }
        if outcome == RedoOutcome::Correction {
            self.toast_state.show(crate::ui::toast::ToastOptions {
                title: Some("Prompt correction".into()),
                message: "Correction reapplied.".into(),
                variant: crate::ui::toast::ToastVariant::Info,
                duration_ms: 3000,
            });
        }
        self.slash_menu.update(&self.prompt_view.input);
    }

    /// Paste the system clipboard's TEXT into the chat prompt through the
    /// normal paste path (`PromptView::handle_paste`) — a mini-port of
    /// opencode's Windows fix (anomalyco/opencode#13871): Windows terminals
    /// do not reliably emit a bracketed paste for Ctrl+V, so the handler
    /// reads the clipboard itself and routes the text through the same
    /// pipeline as the terminal paste — line endings normalized, long pastes
    /// compressed into `[Pasted ~N lines]`.
    ///
    /// No-op (not an error) when the clipboard has no text: the caller
    /// consumes the key either way, so an empty clipboard never leaks a
    /// stray `v` into the prompt. Whitespace-only clipboards are dropped by
    /// the same guard (a lone `\n` pasted verbatim is visually identical to
    /// nothing, but would leave the "is the prompt dirty" heuristics set).
    ///
    /// NOTE: `selection::clipboard_text` is a synchronous IPC round-trip on
    /// Linux (X11/Wayland); a hung clipboard owner can stall the UI thread.
    /// Accepted for now — Windows, the motivating platform, uses a fast
    /// local API — but a worker-thread read with a timeout is the hardening
    /// path if that ever bites.
    fn paste_clipboard_text_into_prompt(&mut self) {
        let Some(text) = selection::clipboard_text() else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        self.prompt_view.note_activity();
        self.prompt_view.handle_paste(&text);
        self.slash_menu.update(&self.prompt_view.input);
    }
}
