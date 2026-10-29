use std::io;
use std::time::Duration;

use cosh_tui::core::lib::rgba::RGBA;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::style::Color;

use super::{App, AppMode};
use super::agents_dispatch::{either_split, either_split_mut};
use crate::component::spinner_highlight::HighlightSpinner;
use crate::notification;
use crate::routes::session::delete::terminated_loop_save_target;
use crate::routes::session::queue_choice::QueueTarget;
use crate::session::store::is_valid_session;
use crate::types::{Part, Session, SessionStatus};
use crate::ui::dialogs::DialogType;
use cosh::harness::HarnessEvent;
use cosh::harness::context::ContextManagerState;
use cosh_tools::subagent::events::SubagentEvent;

/// Process-wide sequence for live-created tool parts' `tool_call_id`s. The
/// id is the identity per-box UI state (expand/collapse, LSP-notes toggle)
/// AND the spinner map key — unique per part across the WHOLE app run,
/// stable once assigned.
///
/// The counter is SEEDED above every id a restored session already carries:
/// persisted parts keep their `call-N` ids from the previous process run
/// (`ensure_tool_call_ids` only fills blanks, it does not renumber), so an
/// unseeded counter would restart at `call-0` on every app start and
/// collide with the transcript's historical ids — the new call would share
/// a spinner/expansion key with an old row, resurrecting the very
/// re-animation bug the call-id keying was introduced to fix. Seeding is
/// idempotent per run: each seed advances the counter past the observed
/// maximum, and the atomic takes the largest value seen.
pub(crate) static TOOL_CALL_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Advance [`TOOL_CALL_SEQ`] past every `call-N` id carried by the given
/// session's tool parts. Called when a persisted session becomes current
/// (and again whenever any session is loaded), so the next live `call-N`
/// allocation can never reuse an id already present in a restored
/// transcript.
pub fn seed_tool_call_seq_beyond_session(session: &Session) {
    let mut max_seen: u64 = TOOL_CALL_SEQ.load(std::sync::atomic::Ordering::Relaxed);
    for msg in &session.messages {
        for part in &msg.parts {
            if let Part::Tool(t) = part
                && let Some(id) = &t.tool_call_id
                && let Some(digits) = id.strip_prefix("call-")
                && let Ok(n) = digits.parse::<u64>()
            {
                max_seen = max_seen.max(n);
            }
        }
    }
    // `fetch_max` keeps the highest value across all seeds; the next
    // `fetch_add` then returns an id STRICTLY above every seen one.
    TOOL_CALL_SEQ.fetch_max(max_seen + 1, std::sync::atomic::Ordering::Relaxed);
}

impl App {
    pub(super) fn handle_events(&mut self) -> io::Result<bool> {
        self.handle_events_with_wait(Duration::from_millis(50))
    }

    /// How long the idle loop may block waiting for input before redrawing.
    ///
    /// Every animation in the TUI is time-based (cursor blink, toast
    /// auto-dismiss, sparkle, paste-burst window, dialog spinners), so an
    /// idle frame only needs to happen when the next one of those deadlines
    /// arrives. Blocking until then instead of the fixed 50 ms drops the
    /// idle redraw rate from ~20 fps to the animation cadence — on mid-range
    /// Windows laptops that removes a permanent CPU/thermal tax that made
    /// every other frame feel slow. Live frames (streaming, spinners,
    /// auto-scroll) never reach this path: `run()` bypasses it entirely via
    /// the frame-pacer wait.
    pub(super) fn idle_wait(&self) -> Duration {
        const DEFAULT: Duration = Duration::from_millis(200);
        const MIN: Duration = Duration::from_millis(16);

        // Candidate deadlines, composed with `min()` — NEVER early-returned.
        // An early return here lets the FIRST matching animation dictate the
        // idle cadence for everything else: in the session router the prompt
        // cursor is always present, so its blink deadline (up to 500 ms) used
        // to shadow the header sparkle's 33 ms animation wait entirely — the
        // starfield stepped at the cursor blink rate (~2 fps) and looked
        // frozen/laggy even though each frame renders in well under a
        // millisecond. Every animation that needs wakeups contributes its own
        // deadline; the loop redraws at the fastest one actually due.
        let mut wait = DEFAULT;

        // Toast countdown: repaint as soon as the remaining lifetime can
        // still change the visible state (its progress is not animated
        // frame-by-frame, so the expiry tick is the only deadline).
        if let Some(toast) = &self.toast_state.current {
            wait = wait.min(Duration::from_millis(
                toast.duration_ms.saturating_sub(self.toast_state.elapsed),
            ));
        }

        // Cursor blink: the prompt cursor is On for the first 500 ms after
        // activity, then toggles every 500 ms — wake at the next toggle.
        let now = std::time::SystemTime::now();
        let blink_deadline = |cursor: &crate::component::cursor::Cursor| -> Option<Duration> {
            let idle_ms = now.duration_since(cursor.last_input_at).ok()?.as_millis() as u64;
            if idle_ms < 500 {
                // Steady-on until the blink cycle starts.
                return Some(Duration::from_millis(500 - idle_ms));
            }
            let into_cycle = now.duration_since(cursor.blink_start).ok()?.as_millis() as u64 % 1000;
            Some(Duration::from_millis(1000 - into_cycle))
        };
        if matches!(self.mode(), AppMode::Session)
            && !self.question_dialog.visible
            && !self.permission_dialog.visible
            && !self.queue_choice_dialog.visible
            && !self.free_gateway_dialog.visible
            && let Some(blink) = blink_deadline(&self.prompt_view.cursor)
        {
            wait = wait.min(blink);
        }
        // Modal dialog stack cursors blink too.
        if let Some(d) = self.dialog.current()
            && let Some(blink) = blink_deadline(&d.cursor)
        {
            wait = wait.min(blink);
        }

        // RAG input and AddProvider search bar have blinking cursors as well
        // (neither route activates the live-render gate on its own).
        if let Some(blink) = blink_deadline(&self.add_provider_view.search_bar.cursor) {
            wait = wait.min(blink);
        }
        #[cfg(feature = "embed")]
        if let Some(blink) = blink_deadline(&self.rag_view.url_input.cursor) {
            wait = wait.min(blink);
        }

        // Paste burst in flight: the flush is due BURST_WINDOW after the
        // last absorbed key — wake exactly then so a long paste lands
        // atomically at the right moment instead of up to 200 ms late.
        if self.paste_burst_pending_flush_deadline() {
            wait = wait.min(MIN);
        }

        // Header sparkle flourish: time-driven starfield — keep redraws at
        // animation cadence while it plays.
        if self.sparkle.is_animating() {
            wait = wait.min(Duration::from_millis(33));
        }

        // ModelList loading spinner on top of the dialog stack: frame-counted
        // (advanced per rendered frame), so it needs regular idle frames.
        if let Some(d) = self.dialog.current()
            && matches!(d.dialog_type, DialogType::ModelList { loading: true, .. })
        {
            wait = wait.min(Duration::from_millis(33));
        }

        wait.max(MIN)
    }

    pub(super) fn handle_events_with_wait(&mut self, wait: Duration) -> io::Result<bool> {
        // NOTE: no toast tick here. `handle_events*()` is only reached when
        // input events are pending (during the agent loop it can be skipped
        // for whole seconds while the user watches the stream), so a tick
        // with a fixed step would let toasts outlive their programmed
        // duration. The tick runs once per frame in `run()` using the real
        // measured frame delta instead.

        // Block briefly for the FIRST event, then drain everything already
        // buffered. Input events (mouse drag during a copy selection, mouse
        // wheel, key auto-repeat) arrive in bursts far faster than one per
        // frame — consuming exactly one event per loop iteration let the
        // internal event queue back up, so the selection highlight / cursor
        // visibly lagged behind the mouse and then "normalized" once the
        // burst ended. Draining the whole buffer per call applies every
        // pending input BEFORE the next render, keeping the cursor speed
        // constant at any event rate.
        if !event::poll(wait)? {
            return Ok(false);
        }

        let mut quit = false;
        loop {
            let evt = event::read()?;
            if self.process_event(evt)? {
                quit = true;
                break;
            }
            if !event::poll(Duration::from_millis(0))? {
                break;
            }
        }
        Ok(quit)
    }

    /// Dispatch a single input event. Returns `Ok(true)` when the app should
    /// quit. Extracted from `handle_events` so a whole buffered batch can be
    /// drained with the same per-event semantics.
    fn process_event(&mut self, event: Event) -> io::Result<bool> {
        match event {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    return self.process_key_event(key);
                }
            }
            Event::FocusGained => {
                self.terminal_focused = true;
            }
            Event::FocusLost => {
                self.terminal_focused = false;
            }
            Event::Resize(_w, _h) => {}
            Event::Paste(text) => {
                // A registration panel (hook, MCP server) owns the
                // keyboard while open: paste into its active field.
                if self.is_registration_form_open() {
                    self.paste_registration_form(&text);
                } else if self.is_text_input_visible() {
                    if let Some(d) = self.dialog.current_mut()
                        && let DialogType::ApiKeyInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::LocalUrlInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::CacheTtlInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::CheckupMinConfidenceInput {
                            input, cursor_pos, ..
                        }
                        | DialogType::RenameSession {
                            input, cursor_pos, ..
                        } = &mut d.dialog_type
                    {
                        let cleaned: String =
                            text.chars().filter(|&c| c != '\n' && c != '\r').collect();
                        input.insert_str(*cursor_pos, &cleaned);
                        *cursor_pos += cleaned.len();
                        d.cursor.note_activity();
                    }
                } else if self.question_dialog.visible && matches!(self.mode(), AppMode::Session) {
                    // Paste into the inline question dialog's text answer.
                    self.question_dialog.handle_paste(&text);
                } else if self.is_rag_mode() {
                    self.handle_rag_paste(&text);
                } else {
                    self.prompt_view.note_activity();
                    self.prompt_view.handle_paste(&text);
                    self.slash_menu_refresh();
                }
            }
            Event::Mouse(crossterm_mouse) => {
                self.handle_mouse_event(crossterm_mouse)?;
            }
        }

        Ok(false)
    }

    /// Take the staged mid-stream-reset message as the CURRENT attempt's
    /// re-stream target (see `ClearAssistant`). Returns the message id only
    /// when the staged entry belongs to the given session; anything staged
    /// for another session stays staged (its own turn may still resume).
    fn take_cleared_stream_target(&mut self, sid: Option<&str>) -> Option<String> {
        let (staged_sid, mid) = self.cleared_stream_msg.take()?;
        if sid != Some(staged_sid.as_str()) {
            self.cleared_stream_msg = Some((staged_sid, mid));
            return None;
        }
        Some(mid)
    }

    /// Prune a staged mid-stream-reset message that was never re-streamed:
    /// the turn ended (Done/Stopped/Error) without the retry delivering a
    /// delta, so the failed attempt's partial content is removed — the same
    /// discard semantics the old remove-on-reset behavior applied, just
    /// deferred to the turn's end.
    fn prune_unreused_cleared_stream(&mut self) {
        let Some((sid, mid)) = self.cleared_stream_msg.take() else {
            return;
        };
        if let Some(session) = self.state.session_cache.get_mut(&sid) {
            session.messages.retain(|m| m.id != mid);
        }
    }

    pub(super) fn poll_events(&mut self) {
        use crate::types::{
            Message, MessageRole, Part, ReasoningPart, SessionStatus, TextPart, ToolPart,
            ToolStatus,
        };
        use crate::ui::toast::{ToastOptions, ToastVariant};

        while let Ok(event) = self.event_rx.try_recv() {
            // MAIN-AGENT MIRROR: while the `/agents` split is open AND the
            // main-agent mirror was explicitly opened (Ctrl+P), project
            // main-loop events into the Right Panel's subagent-window
            // viewer. A no-op otherwise (see `mirror_main_event`). The
            // projection runs BEFORE the match because the chat handlers
            // below consume the event by value. `running` follows the
            // loop status AT ARRIVAL — the terminal arms below seal the
            // mirror explicitly, before they flip the status.
            self.mirror_main_event(
                &event,
                self.state.status == crate::types::SessionStatus::Working,
            );
            match event {
                // /agents split: a pane streamed a live event. The pane's
                // own SubagentActivity does all the coalescing — the same
                // state machine the right-panel sub-agent box uses. A split
                // PARKED in the background (user left the mode with the
                // turn running) is an equal sink: the stream follows the
                // pane objects, not the screen.
                HarnessEvent::AgentsPaneEvent {
                    mut pane,
                    pane_id,
                    epoch,
                    event,
                } => {
                    // Route by the pane's stable identity FIRST: a pane set
                    // rebuilt around a running turn (a fresh picker confirm
                    // that keeps the pane) moved the pane to a new index —
                    // the id still finds it, so the stream follows the pane
                    // instead of dying or corrupting a replacement. The
                    // epoch gate below only judges events that name no live
                    // pane.
                    match either_split(&self.agents_split, &self.parked_agents_split)
                        .and_then(|split| split.pane_index_by_id(pane_id))
                    {
                        Some(resolved) => pane = resolved,
                        // The pane is gone (dropped by a rebuild, or the
                        // whole set torn down): its turn was cancelled with
                        // it — the event must never land in a replacement
                        // pane that merely shares the index.
                        None if pane_id != 0 => continue,
                        // Unstamped senders (`pane_id == 0` — the zero is
                        // reserved, no live pane carries it): the split
                        // generation still guards them — a stale epoch means
                        // the set was replaced or torn down while the turn
                        // was being cancelled.
                        None if epoch != self.agents_pane_epoch => continue,
                        None => {}
                    }
                    let target =
                        either_split_mut(&mut self.agents_split, &mut self.parked_agents_split);
                    // Pane chrome, not timeline content: the session's
                    // serving pair lands on the pane (the prompt badge
                    // reads it), everything else streams into the
                    // activity as before.
                    if let SubagentEvent::SessionModel {
                        model,
                        model_label,
                        provider,
                        provider_label,
                        reasoning,
                    } = &event
                    {
                        if let Some(split) = target {
                            split.set_session_info(
                                pane,
                                crate::routes::session::agents_split::AgentSessionInfo {
                                    model: model.clone(),
                                    model_label: model_label.clone(),
                                    provider: provider.clone(),
                                    provider_label: provider_label.clone(),
                                    reasoning: reasoning.clone(),
                                },
                            );
                        }
                    } else if let SubagentEvent::Commands { commands } = &event {
                        // Pane chrome like `SessionModel`: the agent's own
                        // slash commands cache on the pane (the focused
                        // pane's slash menu reads it through `slash_source`).
                        if let Some(split) = target {
                            split.set_agent_commands(pane, commands.clone());
                        }
                    } else if let SubagentEvent::ConfigOptions { options } = &event {
                        // Pane chrome like `SessionModel`: the config options
                        // cache on the pane (the standardized config boxes
                        // the mapped command groups open read it). The model
                        // option's `current` is the HARNESS default — every
                        // fresh connection reports it regardless of the
                        // persisted pick — so the persisted per-agent pick is
                        // seeded OVER it: the box's selected dot and the
                        // badge must show the model that will actually serve
                        // the turn (dispatch seeds the same pick; this keeps
                        // the visible truth aligned with it). A pick the
                        // option's own value list no longer offers is left
                        // out: the harness dropped that model, and a stale
                        // badge would lie about the serving default. Seeding
                        // is unconditional — a real `session/new` reports the
                        // harness default as a NON-empty `current`, so guarding
                        // on emptiness would skip the seed and the dot would
                        // show the default instead of the persisted pick.
                        // Every user confirm rewrites the store, so the
                        // persisted record always IS the user's freshest
                        // choice — except on a SIBLING pane of the same
                        // agent: there the pane's own confirm recorded a
                        // pane-local pick (`picked_config_ids`) that
                        // dispatch serves, so the seed must not stomp it
                        // (the same precedence `pane_turn_picks` applies).
                        if let Some(split) = target {
                            let mut options = options.clone();
                            // The model option's id, resolved the SAME
                            // two-pass way dispatch resolves it
                            // (`model_option_id`): cline carries BOTH a
                            // provider and a model option under category
                            // `model`, and a flat id-or-category scan would
                            // seed the provider option's badge.
                            let model_id =
                                super::agents_dispatch::model_option_id(&options)
                                    .map(str::to_string);
                            if let Some(p) = split.panes.get(pane) {
                                // The seed value in precedence order — the
                                // same order `pane_turn_picks` applies: the
                                // pane's OWN pick (its cached current, when
                                // it confirmed this very option) outranks
                                // the global record. A sibling pane's
                                // confirm rewrote the store; seeding that
                                // record over THIS pane's fresher choice
                                // would show a dot the turn will not serve.
                                // When the pane's pick is missing from the
                                // cache or the value list, the seed falls
                                // through (empty current filters out of the
                                // output picks in dispatch, so the persisted
                                // record; a dropped value skips the seed and
                                // the dot shows what the turn will really
                                // serve) — display and dispatch stay aligned.
                                let pane_picked_current = model_id
                                    .as_ref()
                                    .filter(|id| {
                                        p.picked_config_ids.iter().any(|picked| picked == *id)
                                    })
                                    .and_then(|id| {
                                        p.config_options
                                            .as_ref()
                                            .and_then(|opts| opts.iter().find(|o| &o.id == id))
                                            .map(|o| o.current.clone())
                                    })
                                    .filter(|current| !current.is_empty());
                                let persisted = self
                                    .agents_model_picks
                                    .get(&p.agent)
                                    .map(str::to_string);
                                if let Some(seed) = pane_picked_current.or(persisted)
                                    && let Some(model_id) = model_id.as_deref()
                                    && let Some(option) =
                                        options.iter_mut().find(|o| o.id == model_id)
                                    && option
                                        .values
                                        .iter()
                                        .any(|v| v.value == seed)
                                {
                                    option.current = seed;
                                }
                            }
                            split.set_agent_config_options(pane, options);
                        }
                    } else if let SubagentEvent::Modes { modes, current } = &event {
                        // Pane chrome like `SessionModel`: the advertised
                        // session modes cache on the pane (the client's own
                        // mode maps onto one of them for the NEXT turn's
                        // `session/set_mode` binding).
                        if let Some(split) = target {
                            split.set_agent_modes(pane, modes.clone(), current.clone());
                        }
                    } else if let SubagentEvent::PermissionRequest {
                        call_id,
                        title,
                        kind,
                        raw_input,
                        options,
                    } = &event
                    {
                        // The pane's OWN pending permission (the non-Yolo
                        // modes route `session/request_permission` here):
                        // the box opens over the pane and the user's answer
                        // flows back through the pane's channel. Default
                        // selection: the first allow-kind option (mirrors
                        // the main dialog's "Allow Once" default). Two
                        // guards keep the box honest:
                        // - only a RUNNING pane opens a box (a request the
                        //   turn thread emitted microseconds before the stop
                        //   landed must not paint a dead box over an idle
                        //   pane — the ACP handler already answered
                        //   `Cancelled` on the closed channel);
                        // - an existing pending request is never
                        //   overwritten (a pipelined second request whose
                        //   handler already got `Cancelled` would otherwise
                        //   show a dead box and cross-wire the first
                        //   request's answer).
                        if let Some(split) = target
                            && split.panes.get(pane).is_some_and(|p| p.running)
                            && split.pending_permission(pane).is_none()
                        {
                            let selected = options
                                .iter()
                                .position(|o| o.kind == "allow_always")
                                .or_else(|| {
                                    options.iter().position(|o| o.kind == "allow_once")
                                })
                                .unwrap_or(0);
                            split.set_pending_permission(
                                pane,
                                crate::routes::session::agents_split::PanePermissionRequest {
                                    call_id: call_id.clone(),
                                    title: title.clone(),
                                    kind: *kind,
                                    raw_input: raw_input.clone(),
                                    options: options.clone(),
                                    selected,
                                },
                            );
                        }
                    } else if let Some(split) = target {
                        split.apply_event(pane, &event);
                    }
                    // A Commands/ConfigOptions landing on the FOCUSED pane
                    // swaps the slash menu's spinner row for the real list
                    // (idempotent no-op for every other pane).
                    if matches!(
                        &event,
                        SubagentEvent::Commands { .. } | SubagentEvent::ConfigOptions { .. }
                    ) {
                        super::agents_dispatch::sync_slash_source(self);
                    }
                    // The pane's mirror in the shared subagent list follows
                    // the same stream (pane-index routed; see
                    // `sync_agents_pane_mirrors`).
                    self.sync_agents_pane_mirrors();
                }
                // /agents split: a pane's turn ENDED. The final report is
                // appended to the pane's activity so it is the last thing
                // visible; the session id seeds the pane's next resume.
                HarnessEvent::AgentsPaneDone {
                    mut pane,
                    pane_id,
                    epoch,
                    output,
                    stop_reason,
                    session,
                } => {
                    // The same identity routing the streamed events apply:
                    // the Done belongs to the pane its id names. A rebuilt
                    // set that kept the pane retargets the index (the turn
                    // SURVIVED the re-pick — its report lands in the pane's
                    // new slot); a pane the rebuild dropped delivers
                    // nothing — its terminal report must never seed a
                    // replacement pane's session id, seal the retargeted
                    // mirror, or bind into the CURRENT session's metadata
                    // under a same-named pane. The epoch gate only judges
                    // unstamped senders.
                    match either_split(&self.agents_split, &self.parked_agents_split)
                        .and_then(|split| split.pane_index_by_id(pane_id))
                    {
                        Some(resolved) => pane = resolved,
                        None if pane_id != 0 => continue,
                        None if epoch != self.agents_pane_epoch => continue,
                        None => {}
                    }
                    // A pane whose turn was already force-closed (the
                    // Ctrl+X interrupt's synthetic Done) or reset
                    // by a slot swap is NOT running: a late real Done from
                    // the winding-down turn thread must not append a
                    // second closure into the timeline, rebind a session,
                    // or re-resolve registry work already settled.
                    let pane_idle = either_split(&self.agents_split, &self.parked_agents_split)
                        .and_then(|split| split.panes.get(pane))
                        .is_none_or(|p| !p.running);
                    if pane_idle {
                        continue;
                    }
                    // Take the session id up front: the pane bookkeeping and
                    // the persistence below both want it, and the match-arm
                    // binding owns the value (no Copy).
                    let binding = session;
                    // How the turn ended, parsed ONCE from the wire spelling
                    // and shared by every consumer below (the pane timeline,
                    // the registry delivery, the mirror). Parsing here — not
                    // at each use — is what keeps `cancelled` meaning "the
                    // user stopped it" everywhere: the pane produces the
                    // reason from its own terminal event now, so a guard stop
                    // or a cut stream arrives as `incomplete`/`partial` and
                    // can never be mistaken for a deliberate interrupt.
                    let end = cosh::harness::agent_coworkers::AgentCoworkerTurnEnd::parse(&stop_reason);
                    let target =
                        either_split_mut(&mut self.agents_split, &mut self.parked_agents_split);
                    if let Some(split) = target {
                        split.set_running(pane, false);
                        split.set_stop_signal(pane, None);
                        // The turn is over: any still-pending permission
                        // request belongs to a turn the ACP handler just
                        // saw end (the closed receiver answers `Cancelled`
                        // per the spec) — the box must not outlive it.
                        split.clear_permission(pane);
                        if let (Some(id), Some(p)) = (binding.as_ref(), split.panes.get_mut(pane)) {
                            // The session id seeds the pane's next resume.
                            p.session_id = Some(id.clone());
                        }
                        // The report closes the pane's turn THROUGH the
                        // pane's own deduplication (`finish_turn`): the
                        // stream already played the final message into the
                        // timeline, and appending the report raw would let
                        // the coalescer merge it into that same entry —
                        // the "answer twice" bug.
                        split.finish_turn(pane, &output, end);
                    }
                    // The bridge's completion half: a pane turn the
                    // `agents_call` registry was tracking resolves HERE,
                    // keyed by the pane's registry id (the App never knows
                    // request ids). Wait-mode callers unblock with the
                    // report; background callers park it for their drain.
                    // The pane id is computed from the live-or-parked split
                    // BEFORE the mutable borrow below — a pane that left
                    // the set resolves nothing (its requests were failed by
                    // `set_coworkers` already).
                    let registry_id = either_split(&self.agents_split, &self.parked_agents_split)
                        .and_then(|split| split.pane_registry_id(pane));
                    if let Some(id) = registry_id {
                        // A failure's text IS the reason; a successful turn's
                        // text is the report. `end` rides along so the
                        // requester can qualify it (see `report_block`).
                        let report = if end.is_failure() {
                            Err(output.clone())
                        } else {
                            Ok(output.clone())
                        };
                        let requester = cosh::harness::agent_coworkers::complete_request(
                            &id,
                            cosh::harness::agent_coworkers::AgentCoworkerReport::new(report, end),
                        );
                        // The pane just freed: the queue service may
                        // dispatch its NEXT queued agent message in this
                        // same frame.
                        self.serve_agents_pane_queues();
                        // The wake promise (issue: the main agent must be
                        // woken by the report of a pane turn IT initiated,
                        // and never disturbed by a user-initiated pane
                        // turn): a MAIN-requested pane turn that completed
                        // while the main loop is idle starts a fresh
                        // main-agent turn carrying the report.
                        self.wake_main_agent_if_promised(requester);
                    }
                    // The mirror finishes: it inherits the pane's ALREADY-CLOSED
                    // timeline (`close_turn` ran in `finish_turn` above — the
                    // report is its LAST chronological entry) and the list
                    // entry stops spinning. Runs even when the split is gone
                    // (no pane to read from — the sync would no-op; the
                    // binding below still persists).
                    self.finish_agents_pane_mirror(pane, end.is_failure());
                    // Persist the binding for the NEXT open: a `Name` pane
                    // promotes to `Bound { agent, session_id }` in the
                    // session metadata, so a mode switch or app restart
                    // resumes THIS agent context. (Only for the CURRENT
                    // epoch: a set replaced or torn down mid-turn delivers
                    // its terminal Done stale and was already dropped
                    // above — its session id belongs to a pane set that no
                    // longer exists.)
                    if let Some(id) = binding {
                        self.bind_agents_pane_session(pane, id);
                    }
                }
                // /agents split: a pane's cold-restore REPLAY ended. The
                // replay stream (if any) already landed through the event
                // arm above; this terminal event clears the pane's
                // rehydrating flag and re-syncs its mirror out of the
                // spinner. A failed pass is surfaced on the pane itself —
                // the transcript is empty and the pane input works as
                // usual, so a toast would only shout at a degraded,
                // self-healing state.
                HarnessEvent::AgentsPaneReplayed {
                    mut pane,
                    pane_id,
                    epoch,
                    ok,
                    error,
                } => {
                    // Identity routing, same as the streamed events: the
                    // replay belongs to the pane its id names, wherever a
                    // rebuild moved it. The epoch gate only judges
                    // unstamped senders.
                    match either_split(&self.agents_split, &self.parked_agents_split)
                        .and_then(|split| split.pane_index_by_id(pane_id))
                    {
                        Some(resolved) => pane = resolved,
                        None if pane_id != 0 => continue,
                        None if epoch != self.agents_pane_epoch => continue,
                        None => {}
                    }
                    let target =
                        either_split_mut(&mut self.agents_split, &mut self.parked_agents_split);
                    if let Some(split) = target
                        && let Some(p) = split.panes.get_mut(pane)
                    {
                        p.rehydrating = false;
                        if !ok {
                            p.activity.apply(&SubagentEvent::Message {
                                text: format!("[replay failed] {error}"),
                            });
                        }
                    }
                    self.sync_agents_pane_mirrors();
                }
                HarnessEvent::ContextItemRecorded {
                    item_id,
                    user,
                    call_item_id,
                    tool_name,
                    compaction,
                } => {
                    // Count committed checkpoints, not lifecycle Finished
                    // notifications: nested fallbacks may emit several of
                    // those for a single applied compaction.
                    if compaction && self.telemetry.enabled() {
                        self.session_telemetry.record_compaction();
                    }
                    let owner = self
                        .active_loop_session_id
                        .as_ref()
                        .or(self.state.current_session_id.as_ref())
                        .cloned();
                    let stream_message = self
                        .stream_msg_id
                        .as_ref()
                        .filter(|(sid, _)| Some(sid) == owner.as_ref())
                        .map(|(_, mid)| mid.clone());
                    if let Some(owner) = owner
                        && let Some(session) = self.state.session_cache.get_mut(&owner)
                    {
                        let message_id = if let Some(call_id) = call_item_id {
                            session
                                .ctx_ids
                                .iter()
                                .find(|(_, ids)| ids.contains(&call_id))
                                .map(|(id, _)| id.clone())
                        } else if let Some(tool_name) = tool_name {
                            // Heuristic fallback: with parallel calls of the SAME
                            // tool this can pick the wrong message. Harmless for
                            // the model (semantic identity comes from the native
                            // call_id groups); ctx_ids here is display bookkeeping.
                            session.messages.iter().rev().find(|m| m.parts.iter().any(|p| matches!(p, Part::Tool(t) if t.tool == tool_name && t.status == ToolStatus::Running))).map(|m| m.id.clone())
                        } else if compaction {
                            session
                                .messages
                                .iter()
                                .rev()
                                .find(|m| {
                                    m.parts
                                        .iter()
                                        .any(|p| matches!(p, Part::Compaction(c) if c.is_running()))
                                })
                                .map(|m| m.id.clone())
                        } else if user {
                            session
                                .messages
                                .iter()
                                .rev()
                                .find(|m| m.role == MessageRole::User)
                                .map(|m| m.id.clone())
                        } else {
                            stream_message.or_else(|| {
                                session
                                    .messages
                                    .iter()
                                    .rev()
                                    .find(|m| m.role == MessageRole::Assistant)
                                    .map(|m| m.id.clone())
                            })
                        };
                        if let Some(message_id) = message_id {
                            let ids = session.ctx_ids.entry(message_id).or_default();
                            if !ids.contains(&item_id) {
                                ids.push(item_id);
                            }
                        }
                    }
                }
                HarnessEvent::BeginAssistant => {
                    // A new streaming attempt starts: what follows belongs to
                    // THIS attempt. ClearAssistant may later discard only the
                    // message opened here — never older transcript content.
                    self.stream_msg_id = None;
                }
                HarnessEvent::ClearAssistant => {
                    // The SDK retried a mid-stream failure: the retried
                    // response would otherwise concatenate with the partial
                    // one. Instead of REMOVING the partial message (which
                    // made the whole chat block vanish and reappear on every
                    // retry — the "blinking chat" symptom during long
                    // reasoning phases, where mid-stream resets are most
                    // likely), KEEP it staged: the retried stream's first
                    // delta clears the partial in place and re-streams into
                    // the same bubble (same id, same position, no reflow
                    // jump). Scoped to the message the attempt opened
                    // (BeginAssistant) AND to the session the attempt
                    // belongs to — a session switch during a background loop
                    // must never let a late reset reach another session's
                    // transcript. If no delta ever arrives, the terminal
                    // handlers prune the stale partial.
                    let cur_session = self.state.current_session_id.clone();
                    if let Some((sid, mid)) = self.stream_msg_id.take()
                        && cur_session.as_deref() == Some(sid.as_str())
                        && let Some(session) = self.state.current_session_mut()
                        && session.messages.iter().any(|m| m.id == mid)
                    {
                        self.cleared_stream_msg = Some((sid, mid));
                    }
                }
                HarnessEvent::Token { text } => {
                    // Whitespace-only deltas are significant MID-STREAM when
                    // they carry a newline (paragraph/line breaks): Gemini's
                    // streamGenerateContent and several OpenAI-compatible
                    // tokenizers emit standalone `"\n\n"` chunks, and
                    // dropping them glues together text that should start
                    // on a new line (the "missing line break" that only
                    // some providers show). Whitespace WITHOUT a newline
                    // stays noise (it renders identically bundled with the
                    // words around it), and a whitespace-only delta never
                    // OPENS a bubble either — so it must not churn the
                    // spinner either: flipping Working→Pondering→Working on
                    // noise deltas restarts the status glyph at frame 0 and
                    // the pondering animation looks frozen mid-stream.
                    let whitespace_only = text.trim().is_empty();
                    if whitespace_only && !text.contains('\n') {
                        continue;
                    }
                    let cur_session = self.state.current_session_id.clone();
                    // A mid-stream reset staged this attempt's message for
                    // reuse (see `ClearAssistant`). Read-only decision here:
                    // the actual take/clear happens on the FIRST REAL delta
                    // below — a leading noise chunk must neither open a
                    // bubble nor wipe the staged partial.
                    let staged_mid = if self.stream_msg_id.is_none() {
                        self.cleared_stream_msg
                            .as_ref()
                            .filter(|(sid, _)| cur_session.as_deref() == Some(sid.as_str()))
                            .map(|(_, mid)| mid.clone())
                    } else {
                        None
                    };
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    // Append to the CURRENT attempt's message only — a fresh
                    // attempt never bleeds into the previous iteration's
                    // message (its tool parts would be at risk on a reset).
                    let taken_id = self.stream_msg_id.take();
                    let mut target_id = taken_id
                        .as_ref()
                        .filter(|(sid, _)| cur_session.as_deref() == Some(sid.as_str()))
                        .map(|(_, mid)| mid.clone());
                    if whitespace_only && target_id.is_none() {
                        // Leading whitespace before any text must never open
                        // a blank bubble — and the stream id goes back
                        // untouched: a break chunk must not disturb the
                        // attempt's bookkeeping (nor the spinner: same
                        // no-churn rule as the noise guard above).
                        self.stream_msg_id = taken_id;
                        continue;
                    }
                    if target_id.is_none()
                        && let Some(mid) = staged_mid
                    {
                        match session.messages.iter_mut().find(|m| m.id == mid) {
                            Some(msg) => {
                                // The retried stream's first real delta
                                // replaces the failed partial in place: the
                                // bubble never vanishes, it re-streams.
                                msg.parts.clear();
                                self.stream_msg_id =
                                    cur_session.clone().map(|sid| (sid, mid.clone()));
                                self.cleared_stream_msg = None;
                                target_id = Some(mid);
                            }
                            None => {
                                // The staged message is gone (deleted /
                                // replaced): the stale flag must not linger.
                                self.cleared_stream_msg = None;
                            }
                        }
                    }
                    let target = target_id
                        .as_ref()
                        .and_then(|mid| session.messages.iter_mut().find(|m| m.id == *mid));
                    match target {
                        Some(msg) => {
                            self.stream_msg_id = cur_session.map(|sid| (sid, msg.id.clone()));
                            match msg.parts.last_mut() {
                                Some(Part::Text(tp)) => tp.text.push_str(&text),
                                _ => msg.parts.push(Part::Text(TextPart {
                                    text: text.clone(),
                                    synthetic: false,
                                })),
                            }
                        }
                        None => {
                            let id = format!("msg-{}", session.messages.len());
                            self.stream_msg_id = cur_session.map(|sid| (sid, id.clone()));
                            if self.telemetry.enabled() {
                                self.session_telemetry.record_message();
                            }
                            session.messages.push(Message {
                                id,
                                role: MessageRole::Assistant,
                                parts: vec![Part::Text(TextPart {
                                    text: text.clone(),
                                    synthetic: false,
                                })],
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                agent: None,
                                model: None,
                            });
                        }
                    }
                    // Streaming text means the model is generating a response:
                    // the default spinner state (also restores it after a
                    // reasoning segment). Set only after the delta actually
                    // rendered — the noise guards above `continue` first, so
                    // whitespace-only deltas never restart the status glyph
                    // at frame 0 (which froze the pondering animation
                    // mid-stream on providers that interleave noise tokens).
                    // Placed after the session borrow ends (borrow checker).
                    self.set_agent_activity(crate::types::AgentActivity::Working);
                }

                HarnessEvent::ToolCall {
                    tool,
                    input,
                    call_ref,
                } => {
                    // Telemetry aggregate: allowlisted/normalized tool name.
                    self.telemetry_tool_call(&tool);
                    // The spinner label follows the tool now running; the
                    // matching ToolResult/ToolError restores the default.
                    self.set_agent_activity(crate::types::AgentActivity::for_tool(&tool));
                    // The attempt ended — its message is now transcript
                    // history (tool parts attach below); a future reset must
                    // never reach it.
                    self.stream_msg_id = None;
                    // Start PTY tracking for bash calls
                    if tool == "bash_run" {
                        let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        self.state.right_panel.start_pty(command.to_string(), None);
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Start PTY tracking for subagent calls
                    if tool == "subagent_call" {
                        let agent = input.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = input.get("input").and_then(|v| v.as_str()).unwrap_or("");
                        // A new session of the same agent CLI supersedes the
                        // queue's previous entries inside `start_pty` (they
                        // leave the default display but stay reachable via
                        // ← navigation). Different agents coexist.
                        let cmd = format!("subagent: {agent}");
                        self.state.right_panel.start_pty(cmd, None);
                        // WINDOW BINDING (the cross-pairing fix): this
                        // call_ref is the routing identity every stream,
                        // report and notification of THIS call carries back.
                        // The echo below lands in the window bound HERE —
                        // two parallel spawns of the same CLI each bind
                        // their own window, so input1 can never pair with
                        // output2.
                        self.state
                            .right_panel
                            .bind_last_pty_call_ref(call_ref.clone());
                        // List-entry metadata: the task description (required
                        // for non-review calls — validated at dispatch — and
                        // inferred as `code-review-NN` for a review call that
                        // omitted it) plus the review flag.
                        self.state.right_panel.set_last_subagent_meta(
                            agent,
                            input.get("description").and_then(|v| v.as_str()),
                            input
                                .get("code_review")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        );
                        // Show the input message as the first lines of the
                        // dialogue: the linear chat flow — prompt, then
                        // the subagent's answer. The echo is stored
                        // VERBATIM on the window (`input_echo` field) and
                        // wears the `│` input rail at paint time; the
                        // section scrolls, so a long prompt never buries
                        // anything.
                        self.state
                            .right_panel
                            .set_last_subagent_input_echo(agent, Some(msg));
                        self.state.right_panel.scroll_to_bottom();
                    }

                    // Save a clone of input before it moves into the ToolPart
                    let input_clone = input.clone();

                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    let tool_call_id = format!(
                        "call-{}",
                        TOOL_CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    let part = Part::Tool(ToolPart {
                        tool: tool.clone(),
                        input,
                        output: None,
                        status: ToolStatus::Running,
                        // Unique per-part identity: the key that per-box
                        // expand/collapse state is keyed by. Without it all
                        // boxes of a tool type shared one key and toggled
                        // together (live parts used to be created with None).
                        tool_call_id: Some(tool_call_id.clone()),
                        is_start: true,
                        is_streaming: false,
                        cached_line_count: None,
                        lsp_notes: None,
                    });
                    match session.messages.last_mut() {
                        Some(msg) if msg.role == MessageRole::Assistant => msg.parts.push(part),
                        _ => {
                            if self.telemetry.enabled() {
                                self.session_telemetry.record_message();
                            }
                            session.messages.push(Message {
                                id: format!("msg-{}", session.messages.len()),
                                role: MessageRole::Assistant,
                                parts: vec![part],
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                agent: None,
                                model: None,
                            });
                        }
                    }
                    // Session borrow dropped; pre-create the spinner for the
                    // new tool call so the beam is visible even if ToolResult
                    // arrives before the next render (fast tools like read).
                    // Pre-create ONLY under the key the part's renderer will
                    // actually use (render_generic-managed tools pre-create
                    // under the generic key; spinner-less renderers skip — a
                    // pre-created entry under a key no renderer reads leaks
                    // one forever-Active spinner per call).
                    let display = crate::routes::session::tool_render::tool_display(&tool);
                    let part_idx = self
                        .state
                        .current_session()
                        .and_then(|s| s.messages.last())
                        .map(|m| m.parts.len().saturating_sub(1))
                        .unwrap_or(0);
                    let tool_id = crate::routes::session::tool_render::renderer_spinner_key(
                        display,
                        Some(tool_call_id.as_str()),
                        part_idx as u16,
                    );
                    if let Some(tool_id) = tool_id {
                        let text = {
                            let temp_part = ToolPart {
                                tool: tool.clone(),
                                input: input_clone,
                                output: None,
                                status: ToolStatus::Running,
                                tool_call_id: None,
                                is_start: false,
                                is_streaming: false,
                                cached_line_count: None,
                                lsp_notes: None,
                            };
                            crate::routes::session::tool_render::tool_inline_text(&temp_part)
                        };
                        let highlight = crate::routes::session::tool_render::tool_color(display)
                            .map(|c| {
                                let (r, g, b) = match c {
                                    Color::Rgb(r, g, b) => (r, g, b),
                                    _ => (128, 128, 128),
                                };
                                RGBA::from_ints(r, g, b, 255)
                            })
                            .unwrap_or(RGBA::from_ints(128, 128, 128, 255));
                        let base = self.theme.text;
                        let mut spinner = HighlightSpinner::new(&text, highlight, base);
                        spinner.set_beam_pos(0.0);
                        self.session_view
                            .tool_state
                            .tool_spinners
                            .insert(tool_id, spinner);
                    }
                }

                HarnessEvent::ToolResult { output, call_ref } => {
                    // The tool is done — back to the default working state
                    // (the model's next response or tool call re-narrows it).
                    // Before the session borrow: `current_session_mut` below
                    // holds `self.state` for the rest of the block.
                    self.set_agent_activity(crate::types::AgentActivity::Working);
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };

                    // Find and complete the running tool part, capture its name
                    let mut completed_tool_name: Option<String> = None;
                    // The sub-agent CLI of the completed `subagent_call`
                    // part (from its INPUT): routes the panel completion to
                    // the right window (see `complete_last_pty_for_agent`).
                    let mut agent_hint: Option<String> = None;
                    'find_running: for msg in session.messages.iter_mut().rev() {
                        for part in msg.parts.iter_mut().rev() {
                            if let Part::Tool(tp) = part
                                && tp.status == ToolStatus::Running
                            {
                                completed_tool_name = Some(tp.tool.clone());
                                if tp.tool == "subagent_call" {
                                    // A MISSING agent means internal: the
                                    // window was created with the same
                                    // `unwrap_or("")` (blank CLI name), so
                                    // the hint is ALWAYS Some — `Some("")`
                                    // targets THAT window exactly and can
                                    // never land on a bash window or a
                                    // sibling background turn's window.
                                    agent_hint = Some(
                                        tp.input
                                            .get("agent")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                    );
                                }
                                tp.status = ToolStatus::Completed;
                                tp.output = Some(output.clone());
                                break 'find_running;
                            }
                        }
                    }

                    // Update the right panel TODO list when a plan write
                    // completed (applied below: `session` still borrows here).
                    let todo_update = if completed_tool_name.as_deref() == Some("plan_todo_write") {
                        crate::routes::session::right_panel::types::parse_todo_output(&output)
                    } else {
                        None
                    };

                    // Deduplicate plan_todo_write: only the LAST completed one keeps its output.
                    // Previous completed plan_todo_write parts get cleared so they render
                    // inline ("☰ TODO Write") instead of as full block TODOs.
                    if completed_tool_name.as_deref() == Some("plan_todo_write") {
                        let mut found_current = false;
                        for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.tool == "plan_todo_write"
                                {
                                    if !found_current {
                                        // Skip the current (latest) plan_todo_write
                                        found_current = true;
                                    } else if tp.status == ToolStatus::Completed {
                                        // Clear output of previous completed plan_todo_write
                                        tp.output = None;
                                    }
                                }
                            }
                            // Only search the current assistant message
                            if msg.role == crate::types::MessageRole::User {
                                break;
                            }
                        }
                    }

                    if !self.state.right_panel.is_scrolled_up() {
                        self.state.right_panel.scroll_to_bottom();
                    }
                    // Harmless for subagent_call (PTY already Completed via finished:true,
                    // complete_last_pty is a no-op for non-Running sessions).
                    // Required for bash_run which only completes via ToolResult.
                    //
                    // Sub-agent guard (the leak fix, ToolResult twin): a
                    // `subagent_status` RESULT (the model's own poll) must
                    // never complete a window at all — a poll of a RUNNING
                    // task is pure JSON status, and completing the
                    // last-Running subagent window with it would both leak
                    // the JSON and mark a live turn finished. Same for a
                    // `subagent_call` result, which routes by the event's
                    // agent hint instead of the last-Running guess.
                    if completed_tool_name.as_deref() == Some("subagent_status") {
                        if let Some(todos) = todo_update {
                            self.state.right_panel.set_todos(todos);
                        }
                        continue;
                    }
                    if completed_tool_name.as_deref() == Some("subagent_call") {
                        // Per-call routing when the ref is present: the
                        // completion closes the window BOUND to the call —
                        // never a sibling's (two parallel spawns share the
                        // hint, never the ref). The routed variant falls
                        // back to the hint when the ref matches nothing.
                        self.state.right_panel.complete_last_pty_routed(
                            output.clone(),
                            call_ref.as_deref(),
                            agent_hint.as_deref(),
                        );
                        if let Some(todos) = todo_update {
                            self.state.right_panel.set_todos(todos);
                        }
                        continue;
                    }
                    // BASH-scoped completion: a main-agent tool result
                    // (`bash_run`, `plan_todo_write`, …) must never close a
                    // RUNNING background subagent window — with a parallel
                    // background turn, the subagent window IS the
                    // last-Running session, and the untargeted completion
                    // used to stamp the other tool's output into its body
                    // AND mark it Completed, so the later completion
                    // notification found no Running window and DROPPED the
                    // sub-agent's real report (the plan-JSON leak). The
                    // `subagent_call`/`subagent_status` arms above carry
                    // their own routing; everything else is a bash window.
                    self.state
                        .right_panel
                        .complete_last_bash_pty(output.clone());

                    if let Some(todos) = todo_update {
                        self.state.right_panel.set_todos(todos);
                    }
                }

                HarnessEvent::ToolError { error, call_ref } => {
                    // Telemetry aggregate: fingerprint-only representation,
                    // raw message discarded inside the accumulator.
                    self.telemetry_tool_error(&error);
                    // The failed tool is done — back to the default state.
                    // Before the session borrow (like ToolResult above): the
                    // reset must also run when the session is gone
                    // (`current_session_mut` → None) — a deleted session must
                    // not leave the label stuck on the failed tool's state.
                    self.set_agent_activity(crate::types::AgentActivity::Working);
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    // The failing tool's name routes the panel failure: a
                    // `subagent_call` whose dispatch failed must fail ITS
                    // OWN window (the agent hint comes from its input),
                    // while any other tool's error must never kill a
                    // RUNNING background subagent window (the bash-scoped
                    // variant keeps the last-Running bash choice only).
                    let mut failed_agent_hint: Option<String> = None;
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                        {
                            if tp.tool == "subagent_call" {
                                // Same normalization as the ToolResult arm:
                                // a missing agent means internal — the hint
                                // is ALWAYS Some, so the failure targets the
                                // calling subagent's own window and can never
                                // land on a bash window or a sibling turn.
                                failed_agent_hint = Some(
                                    tp.input
                                        .get("agent")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                );
                            }
                            tp.status = ToolStatus::Failed(error.clone());
                            break;
                        }
                    }
                    if let Some(hint) = failed_agent_hint.as_deref() {
                        // The hint is only set for `subagent_call`:
                        // `Some("")` matches the blank-CLI internal window
                        // directly (is_window_of); a named agent routes to
                        // its own window. Both are exact matches. With the
                        // ref present the routed variant targets the window
                        // BOUND to the failed call instead. The BASH branch
                        // below stays REF-FREE: bash windows are never
                        // bound, so a ref there could only leak into a
                        // subagent window through the fallback.
                        self.state.right_panel.fail_last_pty_routed(
                            error.clone(),
                            call_ref.as_deref(),
                            Some(hint),
                        );
                    } else {
                        // Any other tool's failure (or no Running part at
                        // all): BASH-scoped — a main-agent tool error must
                        // never kill a RUNNING background subagent window.
                        self.state.right_panel.fail_last_bash_pty(error.clone());
                    }
                }
                HarnessEvent::ToolOutput {
                    tool,
                    output,
                    finished,
                    agent,
                    call_ref,
                } => {
                    // Streaming find results (glob/grep matches) are appended to
                    // the running tool part so the chat shows a live counter.
                    // Everything else (bash, subagent) streams into the right
                    // panel PTY.
                    if matches!(tool.as_str(), "find_glob" | "find_grep") {
                        let Some(session) = self.state.current_session_mut() else {
                            continue;
                        };
                        let mut appended = false;
                        'find_part: for msg in session.messages.iter_mut().rev() {
                            for part in msg.parts.iter_mut().rev() {
                                if let Part::Tool(tp) = part
                                    && tp.status == ToolStatus::Running
                                    && tp.tool == tool
                                {
                                    let out = tp.output.get_or_insert_with(String::new);
                                    let added_lines = output.lines().count() as u32;
                                    out.push_str(&output);
                                    // Update cached line count for efficient display
                                    tp.cached_line_count = Some(
                                        tp.cached_line_count
                                            .unwrap_or(0)
                                            .saturating_add(added_lines),
                                    );
                                    appended = true;
                                    break 'find_part;
                                }
                            }
                        }
                        if !appended {
                            log::debug!(
                                "ToolOutput for {tool} with no running part; dropping chunk"
                            );
                        }
                        continue;
                    }
                    // Sub-agent routing (the leak fix): a `subagent_status`
                    // event is a BACKGROUND sub-agent's completion
                    // notification — its wrapper carries the owning agent,
                    // so resolve the hint before any window consumes the
                    // text. A `subagent_call` event's routing hint travels
                    // in the event itself. Everything else (bash) is
                    // untargeted.
                    let routing_hint: Option<String> = match tool.as_str() {
                        "subagent_call" => agent.clone(),
                        "subagent_status" => {
                            crate::routes::session::right_panel::types::subagent_notification_agent(
                                &output,
                            )
                        }
                        _ => None,
                    };
                    let hint_ref = routing_hint.as_deref();
                    // Per-call routing wins over the agent hint: the ref
                    // identifies the EXACT call (two parallel same-CLI
                    // spawns share the hint but never the ref), so its
                    // stream/report can never land in a sibling's window.
                    let call_ref_str = call_ref.as_deref();
                    // A notification is NEVER a live chunk: appending it
                    // through `update_last_pty` would double-parse it (the
                    // completion below classifies the same text again) and
                    // an UNIDENTIFIED one (malformed wrapper, no agent
                    // hint) would land verbatim in the last-Running window
                    // — possibly a bash run (the pre-fix leak shape). So
                    // `subagent_status` output only ever reaches the panel
                    // as a routed COMPLETION; without a hint it is dropped
                    // from the panel entirely (the injected user turn still
                    // renders it in the chat).
                    if tool == "subagent_status" {
                        if finished {
                            self.state.right_panel.complete_last_pty_routed(
                                output.clone(),
                                call_ref_str,
                                hint_ref,
                            );
                        }
                        continue;
                    }
                    if tool == "subagent_call" {
                        self.state.right_panel.update_last_pty_routed(
                            output.clone(),
                            call_ref_str,
                            hint_ref,
                        );
                        // Auto-follow if user is at the bottom
                        if !self.state.right_panel.is_scrolled_up() {
                            self.state.right_panel.scroll_to_bottom();
                        }
                        if finished {
                            self.state.right_panel.complete_last_pty_routed(
                                output.clone(),
                                call_ref_str,
                                hint_ref,
                            );
                        }
                    } else {
                        // Bash (and any other streaming tool): BASH-scoped —
                        // a main-agent tool's live chunks must never append
                        // into a RUNNING background subagent window (with a
                        // parallel background turn, the subagent window IS
                        // the last-Running session, and the untargeted
                        // append grafted the other tool's output into the
                        // mini-chat — the plan/stream leak family).
                        self.state.right_panel.update_last_bash_pty(output.clone());
                        // Auto-follow if user is at the bottom
                        if !self.state.right_panel.is_scrolled_up() {
                            self.state.right_panel.scroll_to_bottom();
                        }
                        if finished {
                            self.state
                                .right_panel
                                .complete_last_bash_pty(output.clone());
                        }
                    }
                }

                // Typed sub-agent activity (message text, thoughts, tool
                // calls, plans, usage) feeds the chronological mini-chat
                // timeline of the running subagent window (Phase 3a/3b) and
                // auto-follows the scroll like any other streamed output —
                // the box behaves as a small chat that climbs as content
                // arrives, unless the user scrolled away. NOTE: the `tool`
                // field is always "subagent_call" today and is intentionally
                // ignored; typed events carry no session id, so concurrent
                // subagents of the SAME agent CLI interleave into one window
                // (a pre-existing harness limitation).
                HarnessEvent::SubagentEvent {
                    call_ref, event, ..
                } => {
                    if self
                        .state
                        .right_panel
                        .update_subagent_activity(&event, call_ref.as_deref())
                        && !self.state.right_panel.is_scrolled_up()
                    {
                        self.state.right_panel.scroll_to_bottom();
                    }
                }

                HarnessEvent::ToolDiagnostics { tool, notes } => {
                    // Emitted right before the matching ToolResult, while the
                    // fs tool part is still Running: attach the findings so
                    // the renderer can draw them below the output block.
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    for part in session.messages.iter_mut().rev().flat_map(|m| &mut m.parts) {
                        if let Part::Tool(tp) = part
                            && tp.status == ToolStatus::Running
                            && tp.tool == tool
                        {
                            tp.lsp_notes = Some(notes);
                            break;
                        }
                    }
                }

                HarnessEvent::Reasoning { text } => {
                    // Empty reasoning deltas carry no visible thinking yet —
                    // flipping on them would churn the status glyph back to
                    // frame 0 (same no-churn rule as the Token noise guard).
                    if text.trim().is_empty() {
                        continue;
                    }
                    // A reasoning block is streaming: the model is thinking.
                    self.set_agent_activity(crate::types::AgentActivity::Pondering);
                    let cur_session = self.state.current_session_id.clone();
                    // Same attempt-scoping as the Token handler: reasoning
                    // belongs to the CURRENT attempt's message.
                    // A mid-stream reset staged this attempt's message for
                    // reuse: the retried stream's FIRST delta clears the
                    // failed partial in place and re-streams into the same
                    // bubble (see `ClearAssistant`).
                    let staged_reuse = if self.stream_msg_id.is_none() {
                        self.take_cleared_stream_target(cur_session.as_deref())
                    } else {
                        None
                    };
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    if let Some(mid) = staged_reuse
                        && let Some(msg) = session.messages.iter_mut().find(|m| m.id == mid)
                    {
                        msg.parts.clear();
                        self.stream_msg_id = cur_session.clone().map(|sid| (sid, mid));
                    }
                    let target = self
                        .stream_msg_id
                        .take()
                        .filter(|(sid, _)| cur_session.as_deref() == Some(sid.as_str()))
                        .and_then(|(_, mid)| session.messages.iter_mut().find(|m| m.id == mid));
                    match target {
                        Some(msg) => {
                            self.stream_msg_id = cur_session.map(|sid| (sid, msg.id.clone()));
                            msg.push_reasoning(&text);
                        }
                        None => {
                            let id = format!("msg-{}", session.messages.len());
                            self.stream_msg_id = cur_session.map(|sid| (sid, id.clone()));
                            if self.telemetry.enabled() {
                                self.session_telemetry.record_message();
                            }
                            session.messages.push(Message {
                                id,
                                role: MessageRole::Assistant,
                                parts: vec![Part::Reasoning(ReasoningPart {
                                    text: text.clone(),
                                    collapsed: true,
                                })],
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                agent: None,
                                model: None,
                            });
                        }
                    }
                }

                HarnessEvent::WeightsInstall { event } => {
                    use crate::types::InstallPart;
                    let Some(session) = self.state.current_session_mut() else {
                        continue;
                    };
                    // The running install line: the last message whose parts
                    // end in a running Part::Install (created on demand).
                    fn running_install(session: &mut crate::types::Session) -> Option<usize> {
                        session.messages.iter().rposition(
                            |m| matches!(&m.parts[..], [Part::Install(c)] if c.is_running()),
                        )
                    }
                    // The Text event's explanation message (assistant,
                    // non-synthetic, install-pair id): removed when the
                    // download fails, so the transcript never keeps prose
                    // about an install that did not happen — the agent
                    // context never received it either.
                    fn remove_install_explanation(session: &mut crate::types::Session) {
                        if let Some(pos) = session.messages.iter().rposition(|m| {
                            m.id.starts_with("msg-install-")
                                && matches!(&m.parts[..], [Part::Text(t)] if !t.synthetic)
                        }) {
                            session.messages.remove(pos);
                        }
                    }
                    match event {
                        cosh::harness::events::WeightsInstallEvent::Cached => {
                            // Cache hit: no download ran. Freeze any orphaned
                            // running install line (a crash mid-install
                            // persisted one) — nothing else will.
                            if let Some(idx) = running_install(session)
                                && let Some(Part::Install(part)) =
                                    session.messages[idx].parts.last_mut()
                            {
                                part.elapsed_ms =
                                    Some(crate::types::now_ms().saturating_sub(part.started_at));
                                part.failed = Some(true);
                            }
                        }
                        cosh::harness::events::WeightsInstallEvent::TextBegin(first) => {
                            // A running install line at this point is an
                            // ORPHAN (a crash mid-install persisted it; the
                            // restored session re-read it): freeze it so a
                            // fresh install's progress never attaches to the
                            // stale line's clock.
                            if let Some(idx) = running_install(session)
                                && let Some(Part::Install(part)) =
                                    session.messages[idx].parts.last_mut()
                            {
                                part.elapsed_ms =
                                    Some(crate::types::now_ms().saturating_sub(part.started_at));
                                part.failed = Some(true);
                            }
                            // The streamed explanation opens as a REAL
                            // assistant message with its FIRST chunk:
                            // `synthetic: false` puts it in the agent
                            // context (the harness records the same text
                            // there), and the session log persists it like
                            // any assistant turn. TextDelta appends to it.
                            if self.telemetry.enabled() {
                                self.session_telemetry.record_message();
                            }
                            session.messages.push(Message {
                                id: format!("msg-install-{}", crate::types::now_ms()),
                                role: MessageRole::Assistant,
                                parts: vec![Part::Text(TextPart {
                                    text: first,
                                    synthetic: false,
                                })],
                                created_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                agent: None,
                                model: None,
                            });
                        }
                        cosh::harness::events::WeightsInstallEvent::TextDelta(delta) => {
                            // Append to the message the TextBegin opened:
                            // the sender guarantees delta chunks only ever
                            // follow one, and the pieces concatenate back
                            // into EXACTLY the text the agent context gets.
                            if let Some(pos) = session.messages.iter().rposition(|m| {
                                m.id.starts_with("msg-install-")
                                    && matches!(&m.parts[..], [Part::Text(t)] if !t.synthetic)
                            }) && let Some(Part::Text(t)) =
                                session.messages[pos].parts.last_mut()
                            {
                                t.text.push_str(&delta);
                            }
                        }
                        cosh::harness::events::WeightsInstallEvent::Progress {
                            model,
                            bytes_done,
                            bytes_total,
                        } => {
                            let idx = match running_install(session) {
                                Some(idx) => idx,
                                None => {
                                    if self.telemetry.enabled() {
                                        self.session_telemetry.record_message();
                                    }
                                    session.messages.push(Message {
                                        id: format!("msg-install-{}", crate::types::now_ms()),
                                        role: MessageRole::Assistant,
                                        parts: vec![Part::Install(InstallPart {
                                            model: model.clone(),
                                            ..InstallPart::running()
                                        })],
                                        created_at: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as u64,
                                        agent: None,
                                        model: None,
                                    });
                                    session.messages.len() - 1
                                }
                            };
                            if let Some(Part::Install(part)) =
                                session.messages[idx].parts.last_mut()
                            {
                                if part.model.is_empty() {
                                    part.model = model;
                                }
                                part.bytes_done = bytes_done;
                                part.bytes_total = bytes_total;
                            }
                        }
                        cosh::harness::events::WeightsInstallEvent::Finished { repo, revision } => {
                            // Terminal (success): freeze the line at 100%
                            // and record the resolved SHA as the mirror pin
                            // — one revision per checkpoint, every later
                            // load cache-probes exactly this snapshot. The
                            // pin is scoped to the MIRROR: only an install
                            // of the effective mirror repo writes it (a
                            // custom repo's SHA must not poison the mirror,
                            // nor the reverse).
                            if let Some(idx) = running_install(session)
                                && let Some(Part::Install(part)) =
                                    session.messages[idx].parts.last_mut()
                            {
                                part.elapsed_ms =
                                    Some(crate::types::now_ms().saturating_sub(part.started_at));
                                part.failed = Some(false);
                            }
                            if repo == self.setup.decision.hub.effective_repo() {
                                self.setup.decision.hub.pinned_repo = repo;
                                self.setup.decision.hub.pinned_revision = revision;
                                self.setup.save();
                            }
                        }
                        cosh::harness::events::WeightsInstallEvent::Failed(reason) => {
                            // Terminal (failure): freeze the line as failed
                            // and toast the reason — the bar itself stays
                            // one line.
                            if let Some(idx) = running_install(session)
                                && let Some(Part::Install(part)) =
                                    session.messages[idx].parts.last_mut()
                            {
                                part.elapsed_ms =
                                    Some(crate::types::now_ms().saturating_sub(part.started_at));
                                part.failed = Some(true);
                            }
                            // The explanation promised an install that did
                            // not happen: pull it back out of the
                            // transcript (the agent context never received
                            // it — `weights_explanation` is only recorded
                            // on success).
                            remove_install_explanation(session);
                            self.toast_state.show(crate::ui::toast::ToastOptions {
                                title: Some("Decision model install".into()),
                                message: reason,
                                variant: crate::ui::toast::ToastVariant::Warning,
                                duration_ms: 8000,
                            });
                        }
                    }
                }

                HarnessEvent::Done {
                    context,
                    checkup_verdict,
                } => {
                    // The main-agent mirror seals its turn BEFORE the status
                    // flips: success closes the marker as Completed.
                    self.mirror_main_done(false);
                    // The decision model's audit of a heuristic guard stop:
                    // an improper stop is never silent.
                    // Record-everywhere, toast-on-disagreement: the verdict
                    // is on the terminal event for observability, but a
                    // guard stop the model AGREES with needs no toast.
                    if let Some(verdict) = checkup_verdict.filter(|v| v.contains("not-terminated"))
                    {
                        self.toast_state.show(crate::ui::toast::ToastOptions {
                            title: Some("Termination checkup".into()),
                            message: verdict,
                            variant: crate::ui::toast::ToastVariant::Info,
                            duration_ms: 6000,
                        });
                    }
                    self.state.status = SessionStatus::Idle;
                    // The loop is gone, so the "only waiting for reports"
                    // state is gone with it. The harness emits `false` when
                    // its wait ends, but a loop that ends WITHOUT entering
                    // that wait in this pass — or that dies mid-wait — never
                    // gets to; a stale flag would make the submit gate treat
                    // a genuinely idle agent as still awaiting reports.
                    self.awaiting_pane_reports = false;
                    self.agent_spinner = None;
                    self.stream_msg_id = None;
                    self.prune_unreused_cleared_stream();
                    // opencode model: the LSP engine is session-scoped (the
                    // process-wide singleton keeps servers alive between
                    // turns), so the footer keeps showing the last snapshot.
                    // It is refreshed by the next turn's opening snapshot.
                    // Safety net: a "Summarizing" line interrupted at its start
                    // must not stay running (the stopwatch would tick forever).
                    self.finalize_stale_compaction_lines();

                    // Persist the OWNING session (TUI-A), never the viewed
                    // one. Owner gone (deleted mid-run) → skip save/title.
                    let done_target = match self.active_loop_session_id.clone() {
                        Some(owner) => terminated_loop_save_target(Some(owner.as_str()), |id| {
                            self.state.session_cache.contains(id)
                        }),
                        None => self.state.current_session_id.clone(),
                    };
                    // Persist session to disk if it has valid dialog
                    if let Some(id) = done_target
                        && let Some(session) = self.state.session_cache.get_mut(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);

                        // Trigger async title generation for the first response.
                        // The session title starts as a timestamp; the LLM produces
                        // a semantic title from the first user message.
                        // Command-mode turns are LLM work and are NEVER
                        // generated here (the TUI is a plain terminal there);
                        // the flag is per-turn, so a later real agent turn in
                        // the same session still gets its semantic title.
                        if !self.title_generated && !self.command_mode_turn {
                            // Extract the first user message text.
                            let first_user = session.messages.iter().find_map(|m| {
                                if m.role == MessageRole::User {
                                    let text: String = m
                                        .parts
                                        .iter()
                                        .filter_map(|p| match p {
                                            Part::Text(t) => Some(t.text.as_str()),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    if text.is_empty() { None } else { Some(text) }
                                } else {
                                    None
                                }
                            });
                            if let Some(user_prompt) = first_user {
                                let provider = self.llm_config.provider.clone();
                                let model = self.llm_config.model.clone();
                                let base_url = self.base_url_for(&provider);
                                let session_id = id.clone();
                                let session_store = self.session_store.clone();
                                let event_tx = self.event_tx.clone();
                                self.tokio_handle.spawn(async move {
                                    let Ok(mut connector) =
                                        cosh_sdk::connector::Connector::new(&provider)
                                    else {
                                        return;
                                    };
                                    if let Some(ref m) = model {
                                        connector = connector.with_model(m);
                                    }
                                    if let Some(ref url) = base_url {
                                        connector = connector.with_base_url(url.clone());
                                    }
                                    // Cache-affinity routing: the title call
                                    // belongs to this session, so it carries
                                    // the same opaque affinity hash as the
                                    // main loop (pins it to the warm backend).
                                    connector = connector
                                        .with_session_id(super::session_affinity_id(&session_id));
                                    // Disable tools and retry for the title call —
                                    // it is a simple chat completion.
                                    connector = connector
                                        .with_tool_call_mode(
                                            cosh_sdk::connector::ToolCallMode::Native,
                                        )
                                        .with_retry(false);
                                    if let Some(title) =
                                        cosh::harness::generate_title(&connector, &user_prompt)
                                            .await
                                    {
                                        // Persist the updated title to disk.
                                        session_store.update_title(&session_id, &title);
                                        // Update the session title in memory.
                                        let _ = event_tx.send(HarnessEvent::TitleGenerated {
                                            session_id,
                                            title,
                                        });
                                    }
                                });
                                self.title_generated = true;
                            }
                        }
                        // Ensure the sidebar shows the session (must happen
                        // after the immutable borrow of session is released).
                        self.state.ensure_session_summary(&id);
                    }

                    // The loop ended: leftover "next request" messages become
                    // "next agent loop" candidates, and the first one starts a
                    // fresh loop (FIFO). If nothing is queued, the work is
                    // complete — ring the terminal bell to call the user back.
                    let started_new_loop = self.handle_loop_end(true);
                    if !started_new_loop {
                        self.trigger_bell();
                        // Send a desktop notification if the terminal
                        // window is not focused (user switched away).
                        notification::notify_done(self.terminal_focused);
                    }
                }

                HarnessEvent::UserMessageInjected { text } => {
                    self.telemetry_feature(cosh::telemetry::schema::Feature::Prompt);
                    // The running loop consumed a "next request" message:
                    // retire the in-flight marker and pop the deque head
                    // (FIFO — the harness drains the channel in order).
                    // Popping only on acknowledgment guarantees a message
                    // already handed to the loop can never be lost.
                    //
                    // The ack always belongs to the OWNING session's queue —
                    // pop from there, never from whichever session is being
                    // viewed after a mid-run switch.
                    if self.next_request_in_flight {
                        self.next_request_in_flight = false;
                        let owner = self.active_loop_session_id.clone();
                        let queues = match &owner {
                            Some(id) => self.state.pending_queues.get_mut(id),
                            None => self.state.current_pending_queues_mut(),
                        };
                        // Pop the head ONLY when it is still the acked
                        // message. A queue Delete/Edit that raced the
                        // injection already removed it (and reopened the
                        // delivery lane); popping blindly here would retire
                        // the FOLLOWING message instead.
                        if let Some(queues) = queues
                            && queues
                                .next_request
                                .front()
                                .is_some_and(|head| *head == text)
                        {
                            queues.next_request.pop_front();
                        }
                        // A delivered head shifts the edited message's home
                        // position one slot closer to the front.
                        if let Some(hint) = &mut self.edit_requeue_hint
                            && hint.queue == QueueTarget::NextRequest
                            && self.active_loop_session_id.as_deref()
                                == Some(hint.session_id.as_str())
                            && hint.index > 0
                        {
                            hint.index -= 1;
                        }
                    }
                    // Mirror the message into the owning session's history —
                    // never into whichever session happens to be selected
                    // after a mid-run switch.
                    let owner_matches = self
                        .active_loop_session_id
                        .as_ref()
                        .is_none_or(|owner| self.state.current_session_id.as_ref() == Some(owner));
                    if owner_matches && let Some(session) = self.state.current_session_mut() {
                        if self.telemetry.enabled() {
                            self.session_telemetry.record_message();
                        }
                        session.messages.push(Message {
                            id: format!("msg-{}", session.messages.len()),
                            role: MessageRole::User,
                            parts: vec![Part::Text(TextPart {
                                text,
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: None,
                        });
                        self.session_view.scroll_to_bottom();
                    }
                }

                HarnessEvent::Stopped { context } => {
                    // The main-agent mirror seals its interrupted turn as
                    // Failed, with the interruption notice as the transcript's
                    // last line (the same rule `close_turn` applies to a
                    // failed pane turn) — BEFORE the status flips.
                    self.mirror_main_message("The agent loop was stopped.");
                    self.mirror_main_done(true);
                    self.state.status = SessionStatus::Idle;
                    // The loop is gone: the "only waiting for reports" state
                    // cannot outlive it. (The harness also emits `false` when
                    // the wait ends, but a loop that dies mid-wait — a stop,
                    // an error — never gets to.)
                    self.awaiting_pane_reports = false;
                    self.agent_spinner = None;
                    self.stream_msg_id = None;
                    self.prune_unreused_cleared_stream();
                    // Session-scoped servers (see the Done handler): keep the
                    // last LSP snapshot; the next turn refreshes it.
                    self.finalize_stale_compaction_lines();
                    // Route the save by loop owner, never by the viewed
                    // session (TUI-A): deleting background owner A while
                    // viewing B must not write A's context into B. When the
                    // owner is gone (deleted mid-run) skip save + toast.
                    // `None` owner = legacy path without owner tracking:
                    // fall back to the current session to preserve behavior.
                    let save_target = match self.active_loop_session_id.clone() {
                        Some(owner) => terminated_loop_save_target(Some(owner.as_str()), |id| {
                            self.state.session_cache.contains(id)
                        }),
                        None => self.state.current_session_id.clone(),
                    };
                    if save_target.is_some() {
                        self.toast_state.show(ToastOptions {
                            title: Some("Interrupted".into()),
                            message: "Agent loop was stopped.".into(),
                            variant: ToastVariant::Warning,
                            duration_ms: 3000,
                        });
                    }

                    // Persist session to disk even when stopped (partial dialog is still valuable)
                    if let Some(id) = save_target
                        && let Some(session) = self.state.session_cache.get_mut(&id)
                        && is_valid_session(session)
                    {
                        self.session_store
                            .save_session_async_with_context(session, context);
                        self.state.ensure_session_summary(&id);
                    }

                    let started_new_loop = self.handle_loop_end(true);
                    if !started_new_loop {
                        // Send a desktop notification if the terminal
                        // window is not focused (user switched away).
                        notification::notify_stopped(self.terminal_focused);
                    }
                }
                HarnessEvent::AwaitingCoworkerReports { awaiting } => {
                    // The main loop is only waiting for pane reports: it
                    // finished its own work and is idle as far as the user is
                    // concerned. Recorded so the submit path can send a
                    // prompt straight through instead of asking which queue
                    // it should join — the wait wakes on the message.
                    self.awaiting_pane_reports = awaiting;
                }

                HarnessEvent::ContextInfo { info } => {
                    self.context_info = Some(info);
                }

                HarnessEvent::LspServers { available, servers } => {
                    // Snapshot of the LSP status for the prompt footer.
                    // Session-scoped (opencode model): the list persists
                    // across turns until a newer snapshot, the user disables
                    // LSP, or the process ends — it is never cleared by loop
                    // lifecycle events.
                    self.state.lsp_available = available;
                    self.state.lsp_servers = servers;
                }

                HarnessEvent::McpStatus { servers } => {
                    // Same session-scoped model as LSP: the footer counts
                    // persist across turns until a newer snapshot arrives.
                    self.state.mcp_count = cosh::mcp::ready_count(&servers);
                    self.state.mcp_errors = cosh::mcp::failed_count(&servers);
                }

                HarnessEvent::ContextSnapshot { context } => {
                    // Incremental persistence: the harness emits a throttled
                    // context snapshot mid-run so a crash/restart does not lose
                    // the in-flight run. Persist the session log here, mirroring
                    // the Done/Stopped handlers.
                    self.persist_incrementally(context);
                }

                HarnessEvent::LlmCompaction { event } => {
                    self.handle_llm_compaction_event(event);
                }

                HarnessEvent::LlmCompactionToken { text } => {
                    self.handle_llm_compaction_token(&text);
                }

                HarnessEvent::CompactOnDemand { outcome } => {
                    use crate::ui::toast::{ToastOptions, ToastVariant};
                    use cosh::harness::ManualCompactionOutcome as Outcome;
                    self.manual_compaction_active = false;
                    let (title, message, variant) = match outcome {
                        Outcome::Compacted => (
                            "Compacted",
                            "Session context summarized.".into(),
                            ToastVariant::Success,
                        ),
                        Outcome::NothingToCompact => (
                            "Compact",
                            "Nothing to compact yet.".into(),
                            ToastVariant::Info,
                        ),
                        Outcome::Failed => (
                            "Compact failed",
                            "The summarization call did not complete.".into(),
                            ToastVariant::Error,
                        ),
                    };
                    self.toast_state.show(ToastOptions {
                        title: Some(title.into()),
                        message,
                        variant,
                        duration_ms: 4000,
                    });
                }

                HarnessEvent::Toast { message, variant } => {
                    // Route harness notifications (context-window overflow,
                    // exhausted retries) through the existing toast system.
                    use crate::ui::toast::{ToastOptions, ToastVariant as TuiToastVariant};
                    use cosh::harness::events::ToastVariant as HarnessToastVariant;
                    let variant = match variant {
                        HarnessToastVariant::Info => TuiToastVariant::Info,
                        HarnessToastVariant::Success => TuiToastVariant::Success,
                        HarnessToastVariant::Warning => TuiToastVariant::Warning,
                        HarnessToastVariant::Error => TuiToastVariant::Error,
                    };
                    self.toast_state.show(ToastOptions {
                        title: None,
                        message,
                        variant,
                        duration_ms: 8000,
                    });
                }

                HarnessEvent::PromptCorrection { original, result } => {
                    // A cancelled correction never applies: Esc drops the
                    // flag (and aborts the task) BEFORE this event can be
                    // drained, so any response that raced the cancel is
                    // dropped here instead of overwriting the draft.
                    if !self.prompt_correction_active {
                        continue;
                    }
                    self.prompt_correction_active = false;
                    self.prompt_correction_spinner = None;
                    // The task finished on its own: drop its handle so the
                    // field keeps meaning "in-flight task" (not last task).
                    self.prompt_correction_task = None;
                    match result {
                        Ok(corrected) if corrected.trim().is_empty() => {
                            self.toast_state.show(ToastOptions {
                                title: Some("Prompt correction".into()),
                                message:
                                    "The model returned an empty correction; the prompt was kept."
                                        .into(),
                                variant: ToastVariant::Error,
                                duration_ms: 5000,
                            });
                        }
                        Ok(corrected) if self.prompt_view.input == original => {
                            self.prompt_view.replace_with_correction(corrected);
                            self.slash_menu_refresh();
                        }
                        Ok(_) => {
                            // The user edited or sent the draft while the
                            // request was in flight. Never overwrite newer
                            // input with an obsolete correction.
                        }
                        Err(error) => {
                            self.toast_state.show(ToastOptions {
                                title: Some("Prompt correction".into()),
                                message: format!("Could not correct the prompt: {error}"),
                                variant: ToastVariant::Error,
                                duration_ms: 8000,
                            });
                        }
                    }
                }

                HarnessEvent::Error {
                    message,
                    context,
                    checkup_verdict,
                } => {
                    if self.telemetry.enabled() {
                        // This event has no typed cause. Preserve Unknown rather
                        // than guessing a provider category from free-form text.
                        self.session_telemetry.record_error(
                            cosh::telemetry::schema::ErrorCategory::Unknown,
                            "harness::core",
                            None,
                            &message,
                        );
                    }
                    // The decision model's audit of a heuristic guard stop:
                    // an improper stop is never silent.
                    // Record-everywhere, toast-on-disagreement: the verdict
                    // is on the terminal event for observability, but a
                    // guard stop the model AGREES with needs no toast.
                    if let Some(verdict) = checkup_verdict.filter(|v| v.contains("not-terminated"))
                    {
                        self.toast_state.show(crate::ui::toast::ToastOptions {
                            title: Some("Termination checkup".into()),
                            message: verdict,
                            variant: crate::ui::toast::ToastVariant::Info,
                            duration_ms: 6000,
                        });
                    }
                    // Push error as an assistant message so it appears inline in the chat
                    let error_text = format!("Error: {message}");
                    // The main-agent mirror records the loop error inline
                    // (the same `Message` entry a failed pane turn shows)
                    // and seals the turn as Failed — BEFORE the status
                    // flips.
                    self.mirror_main_message(&error_text);
                    self.mirror_main_done(true);
                    self.state.status = SessionStatus::Retry {
                        message: message.clone(),
                        action: None,
                    };
                    self.agent_spinner = None;
                    self.stream_msg_id = None;
                    self.prune_unreused_cleared_stream();
                    // The loop died mid-flight, so any pending "only waiting
                    // for reports" state is gone with it. (Status becomes
                    // `Retry`, not `Idle`, so the submit gate could not fire
                    // on a stale flag anyway — but the flag must never
                    // describe a wait that no longer exists.)
                    self.awaiting_pane_reports = false;

                    if let Some(session) = self.state.current_session_mut() {
                        if self.telemetry.enabled() {
                            self.session_telemetry.record_message();
                        }
                        session.messages.push(Message {
                            id: format!("msg-err-{}", session.messages.len()),
                            role: MessageRole::Assistant,
                            parts: vec![Part::Text(TextPart {
                                text: error_text,
                                synthetic: false,
                            })],
                            created_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            agent: None,
                            model: self.llm_config.model.clone(),
                        });
                        if let Some(cosh::harness::context::ContextItem::Error { id, .. }) =
                            context.as_ref().and_then(|ctx| ctx.items.back())
                            && let Some(message) = session.messages.last()
                        {
                            session.ctx_ids.insert(message.id.clone(), vec![*id]);
                        }
                    }

                    // Persist the styled error line NOW: the loop is over, no
                    // `Done` snapshot follows, so without this save the error
                    // would vanish after a restart. Harness paths carry the
                    // snapshot with the display-only Error item already
                    // recorded (the model-facing timeline stays clean); a
                    // TUI-side failure (thread runtime, connector
                    // construction, panic) has nothing to persist into the
                    // context — the display JSONL alone carries the styled
                    // error line and the context records on disk stay
                    // untouched. Gated on `is_valid_session` exactly like
                    // Done/Stopped/snapshot: a prompt that never got a real
                    // answer is NOT dialog and must not pollute the sidebar
                    // with empty sessions.
                    if let Some(id) = self.state.current_session_id.clone() {
                        let should_save = self
                            .state
                            .session_cache
                            .get(&id)
                            .is_some_and(crate::session::store::is_valid_session);
                        if should_save && let Some(session) = self.state.session_cache.get_mut(&id)
                        {
                            match context {
                                Some(ctx) => self
                                    .session_store
                                    .save_session_async_with_context(session, ctx),
                                None => self.session_store.save_session_async(session),
                            }
                        }
                    }

                    // The run failed: leave the queues parked (promote leftover
                    // next-request messages to next-loop semantics but never
                    // auto-start — the user decides when to resend).
                    self.handle_loop_end(false);

                    // First failure of the keyless OpenCode path (missing key
                    // or auth rejection): offer the one-time free-gateway
                    // recommendation. Write-once — never offered again once answered.
                    self.maybe_offer_gateway_on_error(&message);
                }

                HarnessEvent::ModelsLoaded { models, current } => {
                    // Update cache with the freshly fetched models
                    self.update_model_cache(&models);

                    // Prepend "auto" entry
                    let auto_entry = cosh::ModelEntry {
                        provider: String::new(),
                        model: "auto".to_string(),
                    };
                    let mut models_with_auto = vec![auto_entry];
                    models_with_auto.extend(models);
                    if self.summarization_model_edit.is_some() {
                        models_with_auto
                            .retain(|entry| entry.model != "auto" && !entry.provider.is_empty());
                    }

                    let auto_current = if current == "auto" {
                        current
                    } else {
                        String::new()
                    };

                    // Update the dialog with the loaded models (and stop
                    // the loading spinner, even when the fetch found
                    // nothing, so the empty list renders as before). Look
                    // past the top of the stack: the user may have pushed
                    // the reasoning sub-dialog on top of the ModelList
                    // while the fetch was in flight. `replace()` clears
                    // the stack before showing the picker, so a single
                    // ModelList can exist: take it and move the payload in
                    // without cloning it.
                    if let Some(d) = self
                        .dialog
                        .stack
                        .iter_mut()
                        .find(|d| matches!(d.dialog_type, DialogType::ModelList { .. }))
                        && let DialogType::ModelList {
                            models: dialog_models,
                            current: dialog_current,
                            loading: dialog_loading,
                            ..
                        } = &mut d.dialog_type
                    {
                        *dialog_models = models_with_auto;
                        *dialog_current = auto_current;
                        *dialog_loading = false;
                    }
                }

                HarnessEvent::QuestionRequest { questions } => {
                    // Show the question dialog with real questions from the harness
                    self.question_dialog.show_questions(questions);
                    // Blur the prompt when questions appear (like OpenCode hides the prompt)
                    self.prompt_view.blur();
                    // Send a desktop notification if the terminal
                    // window is not focused (user switched away): the
                    // dialog blocks the loop until answered.
                    notification::notify_question(self.terminal_focused);
                }

                HarnessEvent::PermissionRequest {
                    tool,
                    description,
                    args,
                } => {
                    // Send a desktop notification if the terminal
                    // window is not focused (user switched away): the
                    // dialog blocks the loop until answered. Even when a
                    // worktree ask currently holds the box (the request
                    // parks below), the loop IS blocked on it — notify.
                    notification::notify_permission(self.terminal_focused, &tool, &description);
                    // An `agents_worktree` ask renders in this box ONLY
                    // through the App drain (`serve_agents_pane_worktree_
                    // ops`), which resolves it App-side — never by the
                    // running loop. A harness PermissionRequest arriving
                    // while such an ask is parked would otherwise
                    // OVERWRITE its box content (and its answer would go
                    // into the main loop's `perm_tx`); hold the incoming
                    // request and answer it AFTER the worktree ask is
                    // resolved (the loop stays blocked meanwhile — the
                    // permission gate is its own wait). Only ONE request
                    // can be held: the producer blocks on its own gate
                    // until the current one is answered.
                    if self.pending_worktree_confirmation.is_some() {
                        debug_assert!(
                            self.pending_permission_request.is_none(),
                            "a second permission request arrived while one was already held"
                        );
                        self.pending_permission_request = Some(
                            crate::routes::session::permission::PermissionRequest {
                                tool,
                                description,
                                args,
                            },
                        );
                        continue;
                    }
                    // Show the permission dialog with details from the harness
                    self.permission_dialog.request =
                        Some(crate::routes::session::permission::PermissionRequest {
                            tool,
                            description,
                            args,
                        });
                    self.permission_dialog.visible = true;
                    // Default to "Allow Once" (index 1)
                    self.permission_dialog.selected = 1;
                }

                HarnessEvent::TitleGenerated { session_id, title } => {
                    // Update the session title in memory.
                    if let Some(session) = self.state.session_cache.get_mut(&session_id) {
                        session.title = title.clone();
                        session.title_generated = true;
                    }
                    // Update the sidebar summary.
                    if let Some(summary) = self
                        .state
                        .session_summaries
                        .iter_mut()
                        .find(|s| s.session_id == session_id)
                    {
                        summary.title = title;
                        summary.title_generated = true;
                    }
                }
                HarnessEvent::Usage {
                    usage,
                    provider,
                    model,
                    reported_cost,
                    reported_cost_credits,
                    remaining_credits,
                } => {
                    self.record_usage(
                        usage,
                        &provider,
                        &model,
                        reported_cost,
                        reported_cost_credits,
                    );
                    // Telemetry aggregate: hashed model, allowlisted
                    // provider, saturating token counters, real cost.
                    self.telemetry_llm_usage(&provider, &model, &usage, reported_cost);
                    // The provider's remaining prepaid balance (Charm
                    // Hypercredits) — the user-facing ◆ figure in the
                    // session header. Latest request wins. Its PRESENCE is
                    // what switches the header to credits mode (only Charm
                    // reports it), so no provider-name matching happens.
                    if remaining_credits.is_some() {
                        self.hypercredit_balance = remaining_credits;
                    }
                }
            }
        }
    }

    /// Incremental persistence: called on each throttled `ContextSnapshot`
    /// event so a crash/restart mid-run resumes from the latest context
    /// instead of the session-start state. Mirrors the Done/Stopped save
    /// logic and skips sessions with no valid dialog yet.
    pub(super) fn persist_incrementally(&mut self, context: ContextManagerState) {
        if self.state.status != SessionStatus::Working && !self.manual_compaction_active {
            return;
        }
        if let Some(id) = self.state.current_session_id.clone()
            && let Some(session) = self.state.session_cache.get_mut(&id)
            && is_valid_session(session)
        {
            self.session_store
                .save_session_async_with_context(session, context);
            self.state.ensure_session_summary(&id);
        }
    }
}
