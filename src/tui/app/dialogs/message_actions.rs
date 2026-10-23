use super::super::App;
use crossterm::event::KeyCode;

use super::super::message_prompt_text;
use crate::types::SessionStatus;
use crate::ui::dialogs::DialogType;

use crate::util::selection;

impl App {
    pub(in crate::app) fn is_message_actions_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::MessageActions { .. }))
    }

    pub(in crate::app) fn handle_message_actions_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_message_actions_dialog_visible() {
            return false;
        }

        let (message_id, is_user_message, pane_action) = match self.dialog.current() {
            Some(d) => match &d.dialog_type {
                DialogType::MessageActions {
                    message_id,
                    is_user_message,
                    pane_action,
                    ..
                } => (message_id.clone(), *is_user_message, *pane_action),
                _ => return false,
            },
            None => return false,
        };

        let max_options = if is_user_message { 3 } else { 1 };

        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = if d.selected == 0 {
                        max_options - 1
                    } else {
                        d.selected - 1
                    };
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected = (d.selected + 1) % max_options;
                }
                true
            }
            KeyCode::Enter => {
                let selected = self
                    .dialog
                    .current()
                    .map_or(0, |d| d.selected.min(max_options - 1));
                self.dialog.pop();
                let action = Self::message_action_index(selected, is_user_message);
                match pane_action {
                    Some((pane, timeline_index, user_ordinal)) => {
                        self.run_pane_message_action(
                            action,
                            pane,
                            user_ordinal,
                            timeline_index,
                            is_user_message,
                        );
                    }
                    None => self.run_message_action(action, &message_id),
                }
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => false,
        }
    }

    /// Map the visual selection index to the action enum index.
    ///
    /// - If `is_user_message`: selection 0=Revert, 1=Copy, 2=Fork
    /// - Otherwise: only Copy is shown (visual index 0) → action index 1
    pub(in crate::app) fn message_action_index(selected: usize, is_user_message: bool) -> usize {
        if is_user_message {
            debug_assert!(selected < 3, "selected must be 0..2 for a user message");
            selected
        } else {
            1 // Copy
        }
    }

    /// Execute the picked Message Actions entry: 0 = Revert, 1 = Copy,
    /// 2 = Fork (same order as the opencode dialog).
    pub(in crate::app) fn run_message_action(&mut self, action: usize, message_id: &str) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let working = self.state.status == SessionStatus::Working;
        let session = self.state.current_session();
        let msg_idx = session.and_then(|s| s.messages.iter().position(|m| m.id == message_id));
        let Some(session) = session else {
            return;
        };
        let Some(idx) = msg_idx else {
            return;
        };
        let msg = &session.messages[idx];

        match action {
            0 => {
                // Revert: drop this message and everything after it, and put
                // its text back into the prompt for editing/resending.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let prompt_text = message_prompt_text(msg);
                // Flush the current display projection first so the reference
                // is valid even for a session that has not completed its
                // first persisted agent loop yet.
                self.session_store.save_session(session);
                if !self.session_store.revert_session(&session.id, message_id) {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "Could not record the revert.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let session = self.state.current_session_mut().expect("session");
                session.messages.truncate(idx);
                // Keep the in-memory display projection aligned with the
                // reference selected by the appended Revert delta.
                let remaining: std::collections::HashSet<&str> =
                    session.messages.iter().map(|m| m.id.as_str()).collect();
                session
                    .ctx_ids
                    .retain(|k, _| remaining.contains(k.as_str()));
                self.finalize_stale_compaction_lines();
                self.session_view.hovered_msg_idx = None;
                if !prompt_text.is_empty() {
                    // One Replace group: a single Ctrl+Z undoes the load.
                    self.prompt_view.set_draft(prompt_text);
                }
                self.prompt_view.focus();
            }
            1 => {
                // Copy: join non-synthetic text parts into the clipboard.
                let text = message_prompt_text(msg);
                if !text.is_empty() {
                    selection::copy_selection(&text, &mut self.toast_state);
                } else {
                    self.toast_state.show(ToastOptions {
                        title: Some("Copy".into()),
                        message: "Nothing to copy in this message.".into(),
                        variant: ToastVariant::Info,
                        duration_ms: 2500,
                    });
                }
            }
            2 => {
                // Fork: branch a new session containing everything up to and
                // including the clicked message, then switch to it.
                if working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let parent_id = session.id.clone();
                let mut forked = session.clone();
                forked.messages.truncate(idx + 1);
                // Entry funnel: claim the fork's id (unique + exclusively
                // locked for this process) instead of a bare
                // generate_session_id — a raw timestamp has no
                // file-existence check and no issuance record, so a
                // same-millisecond collision could append fork events into
                // a foreign history. The claim also takes the open-lock,
                // keeping the fork all-or-nothing.
                let Some((claimed_id, created_at)) = self.session_store.claim_fresh_session_id()
                else {
                    self.toast_state.show(crate::ui::toast::ToastOptions {
                        title: Some("Fork".into()),
                        message: "Could not create the branch: the id claim failed.".into(),
                        variant: crate::ui::toast::ToastVariant::Error,
                        duration_ms: 4000,
                    });
                    return;
                };
                forked.id = claimed_id;
                forked.title = format!("{} (fork)", session.title);
                forked.created_at = created_at;
                forked.title_generated = session.title_generated;
                let kept_msg_ids: std::collections::HashSet<&str> =
                    forked.messages.iter().map(|m| m.id.as_str()).collect();
                forked
                    .ctx_ids
                    .retain(|k, _| kept_msg_ids.contains(k.as_str()));
                // Ensure the parent head includes any display changes made
                // since the last harness context snapshot before selecting a
                // prefix from it.
                self.session_store.save_session(session);
                if !self
                    .session_store
                    .fork_session(&parent_id, message_id, &forked)
                {
                    // The claim above is THIS process's lock: on a failed
                    // fork it must be released, or the id stays claimed by
                    // us until death (a leak — nothing was created).
                    self.session_store.release_session_lock(&forked.id);
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "Could not create the branch.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let new_id = forked.id.clone();
                // The open-lock is already this process's (taken by the
                // claim above, idempotent per process), so the fork becomes
                // selectable without another acquisition step.
                self.state.add_session(forked);
                self.finalize_stale_compaction_lines();
                // The fork branches the conversation: the panel starts clean
                // like every other session-selection path, then is rebuilt
                // from the fork's OWN message history (the part before the
                // fork point, which is what the fork persisted).
                self.state.right_panel =
                    crate::routes::session::right_panel::types::RightPanelState::new();
                // The claim above makes this switch's own lock check succeed
                // (claims are idempotent per process).
                self.state.switch_to_session(new_id, &self.session_store);
                self.rehydrate_right_panel();
                self.session_view.hovered_msg_idx = None;
                self.toast_state.show(ToastOptions {
                    title: Some("Fork".into()),
                    message: "New session created from this point.".into(),
                    variant: ToastVariant::Success,
                    duration_ms: 3000,
                });
            }
            _ => {}
        }
    }
}

impl App {
    /// A Message Actions pick on a `/agents` PANE input (the dialog was
    /// opened with `pane_action`): resolve the pane session's k-th user
    /// message (the dialog carries the ordinal, not a message id — the
    /// pane's timeline entries have no ids) and run the SAME core the
    /// chat's `run_message_action` runs.
    pub(in crate::app) fn run_pane_message_action(
        &mut self,
        action: usize,
        pane: usize,
        user_ordinal: Option<usize>,
        timeline_index: usize,
        is_user: bool,
    ) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        // Resolve the target session + message BEFORE touching anything:
        // the split may be live OR parked, and a non-cosh pane has no
        // store session (its actions degrade to Copy-only until phase 2).
        let split = match self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
        {
            Some(split) => split,
            None => return,
        };
        let Some(p) = split.panes.get(pane) else {
            return;
        };
        let Some(session_id) = p.session_id.clone() else {
            self.toast_state.show(ToastOptions {
                title: Some("Message Actions".into()),
                message: "This pane has no stored session yet.".into(),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            return;
        };
        let Some(session) = self.session_store.load_session(&session_id) else {
            self.toast_state.show(ToastOptions {
                title: Some("Message Actions".into()),
                message: "The pane's session could not be loaded.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 3000,
            });
            return;
        };
        // Copy does not need the message: the timeline entry's own text is
        // the source (exactly what the sender typed/sent).
        if action == 1 {
            let hit = crate::routes::session::agents_split::InputBoxHit {
                span: 0..0,
                timeline_index,
                user_ordinal: None,
            };
            let text = split.input_text(pane, &hit).unwrap_or_default();
            if !text.is_empty() {
                selection::copy_selection(&text, &mut self.toast_state);
            } else {
                self.toast_state.show(ToastOptions {
                    title: Some("Copy".into()),
                    message: "Nothing to copy in this message.".into(),
                    variant: ToastVariant::Info,
                    duration_ms: 2500,
                });
            }
            return;
        }
        // Revert/Fork: the user ordinal maps onto the pane session's user
        // messages positionally (the echo and replay builders follow the
        // same order, so the k-th user echo IS the k-th user message).
        let Some(ordinal) = user_ordinal else {
            // An agents_call input has no store message of its own (phase
            // 2 territory): refuse with the honest explanation.
            self.toast_state.show(ToastOptions {
                title: Some("Message Actions".into()),
                message: "Revert/Fork need the pane's own session (cosh panes only).".into(),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            return;
        };
        let user_messages: Vec<&crate::types::Message> = session
            .messages
            .iter()
            .filter(|m| m.role == crate::types::MessageRole::User)
            .collect();
        let Some(msg) = user_messages.get(ordinal) else {
            self.toast_state.show(ToastOptions {
                title: Some("Message Actions".into()),
                message: "That prompt is no longer in the pane's session.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 3000,
            });
            return;
        };
        let message_id = msg.id.clone();
        let idx = session
            .messages
            .iter()
            .position(|m| m.id == message_id)
            .unwrap_or(0);
        self.run_pane_session_action(action, &session_id, &message_id, idx, is_user, pane);
    }

    /// Open the Message Actions dialog for a `/agents` pane input click
    /// (the pane-side counterpart of the chat's pending_message_action
    /// handler). The hit carries the identity: a USER echo maps to the
    /// pane session's k-th user message (Revert/Fork offered); an
    /// `agents_call` input is Copy-only until phase 2.
    pub(in crate::app) fn open_pane_message_actions(
        &mut self,
        pane: usize,
        hit: crate::routes::session::agents_split::InputBoxHit,
    ) {
        let Some(split) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
        else {
            return;
        };
        // Revert/Fork act on the pane's OWN STORED session — only a bound
        // cosh pane has one. An ACP pane's user echo resolves no store
        // message (its session id is the agent's, not a store id — phase-2
        // territory), and an unbound pane never ran: offering the actions
        // there only produced a refusal toast on execute ("could not be
        // loaded" / "no stored session"), which read as a bug instead of a
        // scope limit. Those clicks offer Copy only — the honest menu.
        let offers_session_actions = split.panes.get(pane).is_some_and(|p| {
            p.agent == crate::routes::session::agents_split::MANAGER_LABEL && p.session_id.is_some()
        });
        let is_user = hit.user_ordinal.is_some() && offers_session_actions;
        // The dialog's preview: the raw timeline text (what was sent).
        let preview_text = split.input_text(pane, &hit).unwrap_or_default();
        let preview = super::super::dialog_preview_text(&preview_text);
        self.dialog
            .replace(crate::ui::dialogs::DialogType::MessageActions {
                // No store message id for a pane timeline entry: the ordinal
                // rides in `pane_ordinal`, the timeline entry in `pane_index`.
                message_id: String::new(),
                preview,
                is_user_message: is_user,
                pane_action: Some((pane, hit.timeline_index, hit.user_ordinal)),
            });
    }

    /// The pane-session Revert/Fork core — the chat's `run_message_action`
    /// arms against a pane's OWN session instead of the screen's current
    /// one. Revert truncates the store session AND rebuilds the pane's
    /// timeline from the truncated session (the replay builder's mapping);
    /// Fork branches the pane session and opens the fork in the MAIN view
    /// (the pane keeps its own session).
    pub(in crate::app) fn run_pane_session_action(
        &mut self,
        action: usize,
        session_id: &str,
        message_id: &str,
        idx: usize,
        is_user: bool,
        pane: usize,
    ) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let _ = is_user;
        match action {
            0 => {
                // Revert. The pane turn must not be live: the persist side
                // replaces the session at turn end and would overwrite the
                // revert.
                let pane_running = self
                    .agents_split
                    .as_ref()
                    .or(self.parked_agents_split.as_ref())
                    .and_then(|split| split.panes.get(pane))
                    .is_some_and(|p| p.running || p.rehydrating);
                if pane_running || self.state.status == SessionStatus::Working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let Some(session) = self.session_store.load_session(session_id) else {
                    return;
                };
                let Some(msg) = session.messages.get(idx) else {
                    return;
                };
                let prompt_text = super::super::message_prompt_text(msg);
                // Flush the current display projection first so the
                // reference selected by the revert is the head.
                self.session_store.save_session(&session);
                if !self.session_store.revert_session(session_id, message_id) {
                    self.toast_state.show(ToastOptions {
                        title: Some("Revert".into()),
                        message: "Could not record the revert.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                // The store's branch projection is now truncated: rebuild
                // the pane's timeline from the reverted session (the SAME
                // mapping the cold-restore replay uses).
                let Some(truncated) = self.session_store.load_session(session_id) else {
                    return;
                };
                let mut events: Vec<cosh_tools::subagent::events::SubagentEvent> = Vec::new();
                for message in &truncated.messages {
                    for part in &message.parts {
                        match part {
                            crate::types::Part::Text(text) => {
                                if matches!(message.role, crate::types::MessageRole::User) {
                                    events.push(
                                        cosh_tools::subagent::events::SubagentEvent::UserMessage {
                                            text: text.text.clone(),
                                        },
                                    );
                                } else {
                                    events.push(
                                        cosh_tools::subagent::events::SubagentEvent::Message {
                                            text: text.text.clone(),
                                        },
                                    );
                                }
                            }
                            crate::types::Part::Reasoning(reasoning) => {
                                events.push(cosh_tools::subagent::events::SubagentEvent::Thought {
                                    text: reasoning.text.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(split) = self
                    .agents_split
                    .as_mut()
                    .or(self.parked_agents_split.as_mut())
                {
                    split.reset_timeline(pane, &events);
                    split.clear_hover();
                }
                self.sync_agents_pane_mirrors();
                // The reverted prompt goes back into the shared prompt box
                // for editing/resending — but ONLY when the pane holds the
                // focus (the prompt dispatches to the focused pane).
                let pane_focused = self
                    .agents_split
                    .as_ref()
                    .and_then(|split| split.focused)
                    .is_some_and(|focused| focused == pane);
                if pane_focused && !prompt_text.is_empty() {
                    self.prompt_view.set_draft(prompt_text);
                    self.prompt_view.focus();
                }
                self.toast_state.show(ToastOptions {
                    title: Some("Revert".into()),
                    message: "Pane conversation reverted to this prompt.".into(),
                    variant: ToastVariant::Success,
                    duration_ms: 3000,
                });
            }
            2 => {
                // Fork. Same running guard as Revert.
                let pane_running = self
                    .agents_split
                    .as_ref()
                    .or(self.parked_agents_split.as_ref())
                    .and_then(|split| split.panes.get(pane))
                    .is_some_and(|p| p.running || p.rehydrating);
                if pane_running || self.state.status == SessionStatus::Working {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "The agent is working — wait for it to finish.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let Some(session) = self.session_store.load_session(session_id) else {
                    return;
                };
                let parent_id = session.id.clone();
                let mut forked = session.clone();
                forked.messages.truncate(idx + 1);
                let Some((claimed_id, created_at)) = self.session_store.claim_fresh_session_id()
                else {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "Could not create the branch: the id claim failed.".into(),
                        variant: ToastVariant::Error,
                        duration_ms: 4000,
                    });
                    return;
                };
                forked.id = claimed_id;
                forked.title = format!("{} (fork)", session.title);
                forked.created_at = created_at;
                forked.title_generated = session.title_generated;
                let kept_msg_ids: std::collections::HashSet<&str> =
                    forked.messages.iter().map(|m| m.id.as_str()).collect();
                forked
                    .ctx_ids
                    .retain(|k, _| kept_msg_ids.contains(k.as_str()));
                self.session_store.save_session(&session);
                if !self
                    .session_store
                    .fork_session(&parent_id, message_id, &forked)
                {
                    self.session_store.release_session_lock(&forked.id);
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "Could not create the branch.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                let new_id = forked.id.clone();
                self.state.add_session(forked);
                self.finalize_stale_compaction_lines();
                // The fork opens in the MAIN view — but the action fired
                // FROM the /agents split: switching the session behind the
                // split left the screen parked on the pane grid while the
                // fork only existed behind it (the user saw a success toast
                // and a screen that "did nothing"). Park the split first
                // (turns, bindings and metadata survive; the mode closes
                // honestly) and then switch: the fork takes the visible
                // screen. The pane keeps its own session untouched either
                // way.
                if self.agents_split.is_some() || self.parked_agents_split.is_some() {
                    self.park_agents_split();
                }
                self.state.right_panel =
                    crate::routes::session::right_panel::types::RightPanelState::new();
                self.state.switch_to_session(new_id, &self.session_store);
                self.rehydrate_right_panel();
                self.session_view.hovered_msg_idx = None;
                self.toast_state.show(ToastOptions {
                    title: Some("Fork".into()),
                    message: "New session created from this point.".into(),
                    variant: ToastVariant::Success,
                    duration_ms: 3000,
                });
            }
            _ => {}
        }
    }
}
