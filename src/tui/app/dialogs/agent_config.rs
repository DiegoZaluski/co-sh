//! The `/agents` standardized config box: a mapped command group (models /
//! provider / reasoning / mode) opens ONE cosh-style picker bound to the
//! focused pane's OWN ACP config option (matched by id or category — see
//! `agent_command_map::ConfigTarget`). Confirming a row records the pick
//! on the pane's config cache; every turn re-applies the pane's picks
//! inside its own ACP connection (`apply_session_picks`), the only path
//! that survives — ACP config options are per-connection state.

use super::super::App;
use crossterm::event::KeyCode;

use crate::routes::session::agent_command_map::ConfigTarget;
use crate::component::list_dialog::FilterState;
use crate::ui::dialogs::{DialogType, filter_matches};
use cosh_tools::subagent::events::AgentConfigOption;

impl App {
    /// Open the standardized config box for the FOCUSED pane, bound to the
    /// option `target` selects. Requires the pane's cached config options —
    /// populated by the focus-time snapshot probe, or a harness-initiated
    /// config-option update notification. Without a cache (probe still in
    /// flight, or the harness never advertised options) nothing opens — the
    /// pane's own state shows why.
    pub(in crate::app) fn open_agent_config_dialog(&mut self, target: ConfigTarget) {
        let Some((pane, agent, option)) = self.matched_agent_config_option(target) else {
            return;
        };
        // Nothing selectable (a boolean-only or unmirrored option): an
        // empty box would only confirm an empty value at the harness.
        if option.values.is_empty() {
            return;
        }
        // Open-time context: the split generation, pane index and agent
        // name the box was opened FOR. Confirm re-validates all three
        // against the live split, so a teardown-and-rebuild under an open
        // box can never dispatch a stale config id into a rebuilt pane
        // that merely shares the index.
        self.agent_config_edit = Some((
            self.agents_pane_epoch,
            pane,
            agent,
            option.id.clone(),
        ));
        self.dialog.replace(DialogType::AgentConfigList {
            title: option.name.clone(),
            options: option
                .values
                .iter()
                .map(|v| (v.value.clone(), v.name.clone()))
                .collect(),
            current: option.current.clone(),
            filter: FilterState::new(),
        });
        // Preselect the current value so Enter re-applies it harmlessly.
        // `filter.selected` is the box's SINGLE source of truth (render,
        // keyboard, mouse and confirm all read it — a `d.selected` mirror
        // here would let the highlight and the applied value drift apart).
        if let Some(d) = self.dialog.current_mut()
            && let DialogType::AgentConfigList { filter, .. } = &mut d.dialog_type
        {
            filter.selected = option
                .values
                .iter()
                .position(|v| v.value == option.current)
                .unwrap_or(0);
        }
    }

    /// The focused pane's config option that `target` selects: `(pane
    /// index, agent name, option)` from the pane's cached option list,
    /// matched by id first (exact) and then by the option's mirrored
    /// category string.
    pub(in crate::app) fn matched_agent_config_option(
        &self,
        target: ConfigTarget,
    ) -> Option<(usize, String, AgentConfigOption)> {
        let split = self.agents_split.as_ref()?;
        let pane = split.focused?;
        let p = split.panes.get(pane)?;
        let agent = p.agent.clone();
        let options = p.config_options.as_ref()?;
        let option = crate::routes::session::agent_command_map::match_option(options, target)?
            .clone();
        Some((pane, agent, option))
    }

    pub(in crate::app) fn is_agent_config_dialog_visible(&self) -> bool {
        self.dialog
            .current()
            .is_some_and(|d| matches!(d.dialog_type, DialogType::AgentConfigList { .. }))
    }

    /// Keyboard handling while the AgentConfigList is on top: Up/Down walk
    /// the (filtered) option rows with the component's wrap, Enter records
    /// the selected value as the pane's pick (every turn re-applies it),
    /// Esc closes, and typing/Backspace edits the shared Search filter
    /// (resetting the selection to the first row — the cosh list-box
    /// behavior). All three read/write `filter.selected` — the box's
    /// single source of truth, the same field the render arm and the
    /// confirm path use, so the highlight can never drift from what Enter
    /// applies. Returns true when the key was consumed.
    pub(in crate::app) fn handle_agent_config_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_agent_config_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Up => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::AgentConfigList { options, filter, .. } = &mut d.dialog_type
                {
                    filter.select_prev(filter_matches(options, &filter.filter).len());
                }
                true
            }
            KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::AgentConfigList { options, filter, .. } = &mut d.dialog_type
                {
                    filter.select_next(filter_matches(options, &filter.filter).len());
                }
                true
            }
            KeyCode::Enter => {
                self.confirm_agent_config_selection();
                true
            }
            KeyCode::Esc => {
                self.agent_config_edit = None;
                self.dialog.pop();
                true
            }
            KeyCode::Backspace => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::AgentConfigList { filter, .. } = &mut d.dialog_type
                {
                    filter.pop_char();
                }
                true
            }
            KeyCode::Char(ch) => {
                if let Some(d) = self.dialog.current_mut()
                    && let DialogType::AgentConfigList { filter, .. } = &mut d.dialog_type
                {
                    filter.push_char(ch);
                }
                true
            }
            _ => false,
        }
    }

    /// Resolve the box's selected row and record it on the PANE. ACP config
    /// options are per-connection state (a set issued on a disposable
    /// connection dies with it — verified against opencode and kilo), so
    /// the pick's home is the pane cache: every turn re-applies it inside
    /// its own connection (`run_session`'s `apply_session_picks`), the
    /// only path the harnesses respect. Recording is synchronous — no
    /// background set whose slow response could race a newer pick.
    pub(in crate::app) fn confirm_agent_config_selection(&mut self) {
        let Some(context) = self.agent_config_edit.take() else {
            self.dialog.pop();
            return;
        };
        let (open_epoch, pane, open_agent, config_id) = context;
        let selection = self
            .dialog
            .current()
            .and_then(|d| match &d.dialog_type {
                DialogType::AgentConfigList { options, filter, .. } => {
                    // The FILTERED rows, indexed by the box's single source
                    // of truth — Enter must apply the row the search
                    // painted and the highlight sits on, not an invisible
                    // unfiltered twin. An empty filtered list (a filter
                    // that matches nothing) must NOT fall back to an empty
                    // value at the harness — the same guard the open path
                    // applies for value-less options.
                    let rows = filter_matches(options, &filter.filter);
                    rows.get(filter.clamped(rows.len()))
                        .map(|(value, _)| value.clone())
                }
                _ => None,
            })
            .filter(|value| !value.is_empty());
        self.dialog.pop();
        let Some(selection) = selection else {
            // No row to apply (empty filtered list): close the box without
            // dispatching anything — the same no-op the mouse arm gives a
            // click on the header.
            return;
        };

        // Re-validate the OPEN-TIME context against the live split: the
        // same split generation, the same pane, and the pane still bound
        // to the same agent. A teardown-and-rebuild under an open box
        // (session switch, re-pick) would otherwise dispatch a stale
        // config id into a rebuilt pane that merely shares the index —
        // the confirm-time epoch read the comment below used to claim
        // protection was sampled too late to ever reject anything.
        if self.agents_pane_epoch != open_epoch
            || self
                .agents_split
                .as_ref()
                .and_then(|split| split.panes.get(pane))
                .is_none_or(|p| p.agent != open_agent)
        {
            return;
        }
        // The pick lives on the PANE, not on a remote session: ACP config
        // options are per-connection state (a set issued on a disposable
        // connection dies with it — verified against opencode and kilo),
        // so what makes the choice stick is the pane cache re-applied by
        // EVERY turn (`run_session`'s `apply_session_picks`). Record it
        // now — even before the pane has any session — and move the badge
        // with it, so the pane shows the pick from its next prompt on.
        if let Some(p) = self
            .agents_split
            .as_mut()
            .and_then(|split| split.panes.get_mut(pane))
            && let Some(options) = p.config_options.as_mut()
            && let Some(option) = options.iter_mut().find(|o| o.id == config_id)
        {
            option.current = selection.clone();
            // Mark the option as USER-picked: `dispatch_to_pane` sends only
            // picked ids' currents as turn picks, so an untouched option
            // keeps riding the harness's own default (and `acp::call`'s
            // registry default still covers the model case).
            if !p.picked_config_ids.iter().any(|id| id == &config_id) {
                p.picked_config_ids.push(config_id.clone());
            }
            if option.id == "model" || option.category.as_deref() == Some("model") {
                let info = p.session_info.get_or_insert_with(Default::default);
                info.model = Some(selection.clone());
            }
        }
    }

    /// Mouse routing hook for the AgentConfigList (called from the dialog
    /// Confirm arm): the box confirms on a row click like ToolCallList.
    pub(in crate::app) fn confirm_agent_config_from_mouse(&mut self) -> bool {
        if !self.is_agent_config_dialog_visible() {
            return false;
        }
        self.confirm_agent_config_selection();
        true
    }
}

