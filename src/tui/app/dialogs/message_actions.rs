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
                        self.run_pane_message_action(action, pane, user_ordinal, timeline_index);
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
    /// opened with `pane_action`). The pane's timeline entries carry no ids,
    /// so the dialog hands over the user-echo ordinal and the entry index;
    /// TWO routes answer for them, chosen by what backs the pane:
    ///
    /// - a MANAGER pane has a cosh session in the store — the clicked prompt
    ///   resolves to one of its messages and `run_pane_session_action` runs
    ///   the store's branch machinery (a real truncation);
    /// - an ACP pane has no cosh session (its `session_id` is the agent's),
    ///   so `run_pane_timeline_action` runs the actions over the timeline.
    pub(in crate::app) fn run_pane_message_action(
        &mut self,
        action: usize,
        pane: usize,
        user_ordinal: Option<usize>,
        timeline_index: usize,
    ) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        // Snapshot the pane's identity and LOCAL timeline ONCE: every arm
        // below needs them, and no arm may hold a split borrow across a
        // `&mut self` call. The clone is per-click, not per-frame.
        let Some((agent, session_id, timeline)) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .map(|p| {
                (
                    p.agent.clone(),
                    p.session_id.clone(),
                    p.activity.timeline.clone(),
                )
            })
        else {
            return;
        };
        // Copy does not need a session: the timeline entry's own text is the
        // source (exactly what the sender typed/sent).
        if action == 1 {
            let text = timeline
                .get(timeline_index)
                .and_then(|entry| match entry {
                    crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                        text,
                    }
                    | crate::routes::session::right_panel::types::SubagentTimelineEntry::AgentMessage {
                        text,
                    } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
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
        let Some(ordinal) = user_ordinal else {
            // An `agents_call` input is the OTHER agent's prompt: it has no
            // user message of its own to restore and no prompt to hand back.
            self.toast_state.show(ToastOptions {
                title: Some("Message Actions".into()),
                message: "Revert/Fork need one of your own prompts.".into(),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
            return;
        };
        // Pane kinds differ in what backs their conversation:
        //
        // - the MANAGER (cosh) pane owns a real cosh session in the store,
        //   whose context snapshot IS the agent's memory — Revert/Fork run
        //   the store's own branch machinery (and really truncate what the
        //   next turn sends);
        // - an ACP pane has no cosh session at all (its `session_id` is the
        //   AGENT's id, not a store id) and the protocol offers no way to
        //   truncate the agent's history. Those actions run on the pane's
        //   local timeline instead (`run_pane_timeline_action`): Fork
        //   branches a new cosh session from the transcript prefix, Revert
        //   truncates the transcript and restarts the agent session.
        let store_session = if agent == crate::routes::session::agents_split::MANAGER_LABEL {
            match session_id.as_deref() {
                // Never persisted: the pane's transcript is all there is, so
                // the timeline branch is the only thing that can act.
                None => None,
                // A BOUND manager pane whose session the store cannot read
                // is refused, never demoted: the timeline branch ABANDONS the
                // binding and rewrites the metadata as an unbound `Name`
                // pane, which would silently drop a conversation that is
                // still on disk (and is the honest refusal this path had
                // before the ACP branch existed).
                Some(id) => match self.session_store.load_session(id) {
                    Some(session) => Some(session),
                    None => {
                        self.toast_state.show(ToastOptions {
                            title: Some("Message Actions".into()),
                            message: "Could not load this pane's session.".into(),
                            variant: ToastVariant::Warning,
                            duration_ms: 4000,
                        });
                        return;
                    }
                },
            }
        } else {
            None
        };
        match store_session {
            Some(session) => {
                let Some((message_id, idx)) = resolve_pane_message(&session, &timeline, ordinal)
                else {
                    self.toast_state.show(ToastOptions {
                        title: Some("Message Actions".into()),
                        message: "Could not match that prompt to the pane's session.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 3000,
                    });
                    return;
                };
                let session_id = session_id.unwrap_or_default();
                self.run_pane_session_action(action, &session_id, &message_id, idx, pane);
            }
            None => self.run_pane_timeline_action(action, pane, timeline_index, &timeline),
        }
    }

    /// Revert/Fork on a pane with NO cosh store session (an ACP pane): the
    /// pane's own timeline is the transcript.
    ///
    /// Revert drops the clicked prompt and everything after it from the
    /// pane, hands the prompt back, and ABANDONS the agent session — ACP has
    /// no truncate, so the honest equivalent is a fresh session on the next
    /// turn, primed with the kept prefix (the rewind handoff).
    ///
    /// Fork branches a new cosh session from the transcript prefix (prose
    /// only: user prompts and agent replies, one context item each, so the
    /// store records the boundaries the branch resolves) and opens it in the
    /// MAIN view — the pane keeps its own conversation untouched.
    pub(in crate::app) fn run_pane_timeline_action(
        &mut self,
        action: usize,
        pane: usize,
        timeline_index: usize,
        timeline: &[crate::routes::session::right_panel::types::SubagentTimelineEntry],
    ) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        match action {
            0 => {
                // No success toast: the pane itself is the receipt (the
                // transcript shrinks and the prompt comes back for editing),
                // and a bubble claiming a revert would keep lying if the
                // rewind turned out not to stick. Refusals still toast.
                self.rewind_pane_timeline(pane, timeline_index, "Revert");
            }
            2 => {
                if self.pane_turn_blocked(pane, "Fork") {
                    return;
                }
                let (messages, context) = pane_timeline_prefix(timeline, timeline_index);
                if messages.is_empty() {
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "Nothing to branch from yet.".into(),
                        variant: ToastVariant::Info,
                        duration_ms: 3000,
                    });
                    return;
                }
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
                let agent = self
                    .agents_split
                    .as_ref()
                    .or(self.parked_agents_split.as_ref())
                    .and_then(|split| split.panes.get(pane))
                    .map(|p| p.agent.clone())
                    .unwrap_or_default();
                let forked = crate::types::Session {
                    id: claimed_id.clone(),
                    // An ACP pane has no session title of its own; the agent's
                    // own name is the closest label (the cosh pane's fork
                    // reuses its session title instead).
                    title: format!("{agent} (fork)"),
                    messages,
                    created_at,
                    title_generated: false,
                    provider: None,
                    model: None,
                    reasoning: None,
                    agents_panes: None,
                    agents_mode_open: false,
                    agents_hidden: Vec::new(),
                    ctx_ids: std::collections::HashMap::new(),
                    main_agent_worktree: None,
                };
                // Saved WITH its context so the fork is a real conversation
                // for the MAIN agent (a display-only branch would open with
                // an empty model context and answer as if it were new).
                self.session_store
                    .save_session_with_context(&forked, &context);
                if self.session_store.load_session(&claimed_id).is_none() {
                    self.session_store.release_session_lock(&claimed_id);
                    self.toast_state.show(ToastOptions {
                        title: Some("Fork".into()),
                        message: "Could not create the branch.".into(),
                        variant: ToastVariant::Warning,
                        duration_ms: 4000,
                    });
                    return;
                }
                self.open_forked_session(forked);
            }
            _ => {}
        }
    }

    /// Whether pane `pane` cannot take a Message Action right now: its own
    /// turn is live (streaming or being rehydrated) or the MAIN agent is
    /// working. Toasts the refusal under `title`; the caller only returns.
    fn pane_turn_blocked(&mut self, pane: usize, title: &str) -> bool {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let pane_busy = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .is_some_and(|p| p.running || p.rehydrating);
        if !pane_busy && self.state.status != SessionStatus::Working {
            return false;
        }
        self.toast_state.show(ToastOptions {
            title: Some(title.into()),
            message: "The agent is working — wait for it to finish.".into(),
            variant: ToastVariant::Warning,
            duration_ms: 4000,
        });
        true
    }

    /// Rewind a pane with NO truncatable agent session (an ACP pane) to
    /// transcript index `timeline_index`: drop that entry and everything
    /// after it from the pane, hand the prompt back to the focused pane's
    /// prompt box, and ABANDON the agent session — the protocol has no
    /// truncate, so the honest equivalent is a fresh one on the next turn,
    /// primed with the kept prefix (the rewind handoff).
    ///
    /// Shared by the Message Actions Revert and the `/undo` checkpoint box
    /// (`title` only names the refusal toast), so the two surfaces cannot
    /// drift. Returns `false` — with nothing changed — when the pane or that
    /// entry is gone, or the pane cannot be rewound right now.
    pub(in crate::app) fn rewind_pane_timeline(
        &mut self,
        pane: usize,
        timeline_index: usize,
        title: &str,
    ) -> bool {
        let Some(timeline) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .map(|p| p.activity.timeline.clone())
        else {
            return false;
        };
        if timeline_index >= timeline.len() || self.pane_turn_blocked(pane, title) {
            return false;
        }
        let prompt_text = match timeline.get(timeline_index) {
            Some(
                crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                    text,
                },
            ) => text.clone(),
            _ => String::new(),
        };
        let keep = timeline_index;
        let kept = &timeline[..keep];
        // The fresh session's opening context: the kept transcript itself
        // (the same bounded summary the worktree handoff uses). Nothing
        // kept = nothing to carry.
        let rewind =
            (!kept.is_empty()).then(|| crate::app::agents_dispatch::pane_handoff_summary(kept));
        let pane_focused = self
            .agents_split
            .as_ref()
            .and_then(|split| split.focused)
            .is_some_and(|focused| focused == pane);
        if let Some(split) = self
            .agents_split
            .as_mut()
            .or(self.parked_agents_split.as_mut())
        {
            // The truncation IS the revert: if it did not land, nothing
            // else below may claim one (the old code ignored the result and
            // carried on, restoring the prompt as if the pane had been
            // rewound — a silent no-op dressed as success).
            if !split.truncate_timeline(pane, keep) {
                return false;
            }
            split.clear_hover();
            split.set_rewind_context(pane, rewind);
            // The ACP session cannot be truncated: drop the live binding so
            // the next turn opens a FRESH agent session, and FENCE the pane
            // against cold-restore replays of the session it abandoned (a
            // replay would put every rewound turn back on screen — the undo
            // would look like it did nothing).
            if let Some(p) = split.panes.get_mut(pane) {
                p.session_id = None;
                p.rewound = true;
                // Re-engage the sticky bottom: the rewind's whole point is
                // the NEW tail, and a pane left scrolled up would keep the
                // removed turn off-screen (reading as "nothing happened").
                p.scroll_to_bottom();
            }
        }
        // The persisted pane reference drops the abandoned session too, so a
        // rebuild from metadata (picker confirm, hidden-pane restore, app
        // restart) cannot re-bind it and replay the rewound turns back in.
        self.persist_rewound_pane(pane);
        self.sync_agents_pane_mirrors();
        if pane_focused && !prompt_text.is_empty() {
            self.prompt_view.set_draft(prompt_text);
            self.prompt_view.focus();
        }
        true
    }

    /// Persist a rewound pane as UNBOUND (see [`Self::rewind_pane_timeline`]):
    /// the agent session it abandoned must never be bound again, or a replay
    /// would restore the transcript the user just rolled back. The entry is
    /// the pane's OWN [`crate::routes::session::agents_split::AgentPane::to_ref`],
    /// so the worktree pin survives the rewind and a restart comes back with
    /// the same isolated cwd and the honest, empty transcript.
    fn persist_rewound_pane(&mut self, pane: usize) {
        let Some((agent, fresh)) = self
            .agents_split
            .as_ref()
            .or(self.parked_agents_split.as_ref())
            .and_then(|split| split.panes.get(pane))
            .map(|p| (p.agent.clone(), p.to_ref()))
        else {
            return;
        };
        let Some(session) = self.state.current_session_mut() else {
            return;
        };
        let Some(panes) = session.agents_panes.as_mut() else {
            return;
        };
        let Some(entry) = panes.get_mut(pane) else {
            return;
        };
        if entry.agent() != agent || entry.session_id().is_none() {
            return;
        }
        *entry = fresh;
        self.save_current_agents_metadata();
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
        // Every USER echo is a prompt the pane can be rewound to, on BOTH
        // pane kinds: the manager pane runs the store's branch machinery over
        // its own cosh session, and an ACP pane runs them over its local
        // timeline (`run_pane_message_action` picks the path). An
        // `agents_call` input still offers Copy only — it is the other
        // agent's prompt, not one of the user's.
        let is_user = hit.user_ordinal.is_some();
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
        pane: usize,
    ) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
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
                // the pane's timeline from the reverted session — the SAME
                // replay the cold restore uses, so tool rows survive a
                // revert exactly like they survive a restart.
                let events = match self.session_store.load_context(session_id) {
                    Some(context) => {
                        crate::app::agents_dispatch::manager_context_replay_events(&context)
                    }
                    // A pre-feature session with no context records: fold the
                    // display projection (prose only — all it has).
                    None => {
                        let Some(truncated) = self.session_store.load_session(session_id) else {
                            return;
                        };
                        let mut events = Vec::new();
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
                                        events.push(
                                            cosh_tools::subagent::events::SubagentEvent::Thought {
                                                text: reasoning.text.clone(),
                                            },
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                        events
                    }
                };
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
                // No success toast: the repainted pane is the receipt, and a
                // bubble claiming a revert is exactly the false positive
                // that hid the no-op for so long.
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
                self.open_forked_session(forked);
            }
            _ => {}
        }
    }

    /// Show a freshly created fork on the VISIBLE screen, shared by BOTH
    /// fork routes (the cosh pane's store branch and the ACP pane's timeline
    /// prefix) so the hand-off cannot drift between them.
    ///
    /// The action fired FROM the /agents split, and switching the session
    /// behind the split used to leave the screen parked on the pane grid
    /// while the fork only existed behind it (a success toast and a screen
    /// that "did nothing"). The split parks first — turns, bindings and
    /// metadata survive, the mode closes honestly — and the fork takes the
    /// main view.
    fn open_forked_session(&mut self, forked: crate::types::Session) {
        use crate::ui::toast::{ToastOptions, ToastVariant};
        let new_id = forked.id.clone();
        self.state.add_session(forked);
        self.finalize_stale_compaction_lines();
        if self.agents_split.is_some() || self.parked_agents_split.is_some() {
            self.park_agents_split();
        }
        self.state.right_panel = crate::routes::session::right_panel::types::RightPanelState::new();
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
}

/// Resolve the clicked user echo to the pane session's message.
///
/// The pane's PROMPT sequence is what the store persists as user messages —
/// the user's own prompts AND the `agents_call` inputs other agents sent
/// (they are `ContextItem::User` in the pane's context). The echo ordinals
/// only count the user's prompts, so the two lists are aligned by TEXT in
/// order: an agent-call prompt sitting between two prompts must not shift
/// every later target (the old raw-ordinal lookup reverted the wrong turn).
///
/// Returns the message id and its index in `session.messages`, or `None`
/// when the clicked prompt cannot be identified — refusing is the honest
/// answer to an ambiguous click, because acting on the wrong message is
/// destructive and silent.
fn resolve_pane_message(
    session: &crate::types::Session,
    timeline: &[crate::routes::session::right_panel::types::SubagentTimelineEntry],
    ordinal: usize,
) -> Option<(String, usize)> {
    use crate::routes::session::right_panel::types::SubagentTimelineEntry;

    fn prompt_text(entry: &SubagentTimelineEntry) -> Option<&str> {
        match entry {
            SubagentTimelineEntry::UserMessage { text }
            | SubagentTimelineEntry::AgentMessage { text } => Some(text.as_str()),
            _ => None,
        }
    }

    let mut prompts: Vec<&str> = Vec::new();
    // The prompt-sequence position of the clicked (ordinal-th) user echo.
    let mut target_position = None;
    let mut echoes = 0usize;
    for entry in timeline {
        if matches!(entry, SubagentTimelineEntry::UserMessage { .. }) {
            if echoes == ordinal {
                target_position = Some(prompts.len());
            }
            echoes += 1;
        }
        if let Some(text) = prompt_text(entry) {
            prompts.push(text);
        }
    }

    let store_messages: Vec<&crate::types::Message> = session
        .messages
        .iter()
        .filter(|m| m.role == crate::types::MessageRole::User)
        .collect();

    // Greedy in-order alignment by text (a prompt may carry a handoff
    // prefix, so an `ends_with` match counts too).
    let mut aligned: Vec<Option<&crate::types::Message>> = vec![None; prompts.len()];
    let mut cursor = 0usize;
    for (i, prompt) in prompts.iter().enumerate() {
        while let Some(candidate) = store_messages.get(cursor).copied() {
            cursor += 1;
            let text = super::super::message_prompt_text(candidate);
            if text == *prompt || text.ends_with(prompt) {
                aligned[i] = Some(candidate);
                break;
            }
        }
    }

    let position = target_position?;
    if let Some(message) = aligned.get(position).copied().flatten() {
        let idx = session.messages.iter().position(|m| m.id == message.id)?;
        return Some((message.id.clone(), idx));
    }

    // Alignment failed. The positional lookup is only safe when it cannot be
    // off by anything: no `agents_call` prompt sits in the timeline and the
    // two lists are the same length (a legacy/hand-written session). When an
    // agent prompt IS present, position `k` and store message `k` disagree —
    // the exact mis-target this function exists to fix — so refuse instead of
    // reverting the wrong turn.
    let has_agent_prompts = timeline
        .iter()
        .any(|entry| matches!(entry, SubagentTimelineEntry::AgentMessage { .. }));
    if has_agent_prompts || prompts.len() != store_messages.len() {
        return None;
    }
    let message = store_messages.get(ordinal).copied()?;
    let idx = session.messages.iter().position(|m| m.id == message.id)?;
    Some((message.id.clone(), idx))
}

/// The pane's transcript prefix as a forked cosh conversation: one message
/// and one context item per PROSE entry (user prompts, `agents_call`
/// inputs, agent replies). Thoughts, tool rows and plans are skipped — they
/// are not model-facing conversation, and keeping the two lists 1:1 is what
/// lets the store record the boundaries the fork later resolves.
fn pane_timeline_prefix(
    timeline: &[crate::routes::session::right_panel::types::SubagentTimelineEntry],
    through: usize,
) -> (
    Vec<crate::types::Message>,
    cosh::harness::context::ContextManagerState,
) {
    use crate::routes::session::right_panel::types::SubagentTimelineEntry;
    use crate::types::{Message, MessageRole, Part, TextPart};
    use cosh::harness::context::{ContextItem, ContextManagerState};

    let mut messages: Vec<Message> = Vec::new();
    let mut items: std::collections::VecDeque<ContextItem> = std::collections::VecDeque::new();
    let mut next_id = 1u64;
    for entry in timeline.iter().take(through.saturating_add(1)) {
        let (role, text) = match entry {
            SubagentTimelineEntry::UserMessage { text }
            | SubagentTimelineEntry::AgentMessage { text } => (MessageRole::User, text),
            SubagentTimelineEntry::Message { text } => (MessageRole::Assistant, text),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let id = next_id;
        next_id += 1;
        items.push_back(match role {
            MessageRole::User => ContextItem::User {
                id,
                original: text.clone(),
                synthetic: false,
            },
            MessageRole::Assistant => ContextItem::Assistant {
                id,
                original: text.clone(),
                closable: false,
                reasoning: None,
            },
        });
        messages.push(Message {
            id: format!("msg-{}", messages.len()),
            role,
            parts: vec![Part::Text(TextPart {
                text: text.clone(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    }
    let context = ContextManagerState {
        items,
        next_id,
        ..Default::default()
    };
    (messages, context)
}
