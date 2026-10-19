//! Tool dispatch: dequeue the next pending call and run it through the
//! three-tier dispatch (cosh tools → MCP servers).

use cosh_tools::subagent::types::SubAgentCallInput;

use super::{Harness, Mode, background, wait_for_stop_signal};
use crate::harness::tools::{Tools, is_tool_disabled};

impl Harness {
    /// Dequeue and dispatch the next pending tool call through the
    /// three-tier dispatch: cosh tools → MCP servers.
    ///
    /// Returns the text content of the tool response on success.
    /// Returns an error if the queue is empty, the tool is not found,
    /// arguments are malformed, or the execution fails.
    ///
    /// # Errors
    ///
    /// Returns an error if the queue is empty, the tool is unknown,
    /// arguments are not a JSON object, or the execution fails.
    pub async fn dispatch_next(&mut self) -> Result<String, String> {
        let (tool_name, args_map) = {
            let tc = self
                .tool_issuer
                .front()
                .ok_or_else(|| "no pending tool calls".to_string())?;

            let serde_json::Value::Object(ref args_map) = tc.arguments else {
                let name = tc.name.clone();
                self.tool_issuer.pop_front();
                return Err(self.enrich_with_schema_hint(
                    &name,
                    "tool arguments must be a JSON object".to_string(),
                ));
            };

            (tc.name.clone(), args_map.clone())
        };

        // A tool the user disabled on the Internal Tools screen is never
        // dispatched, even if the model hallucinates a call (its schema was
        // already hidden). Group prefixes (e.g. `lsp`) expand to their tools.
        if is_tool_disabled(&tool_name, &self.disabled_tools) {
            self.tool_issuer.pop_front();
            return Err(format!("tool '{tool_name}' is disabled"));
        }

        // The merged `subagent_call` tool has TWO dispatch paths behind one
        // visible definition: `agent` present → external CLI (the CoshTools
        // arm below); `agent` omitted or empty → INTERNAL sub-agent (a nested
        // harness that reuses this connector, starts with an empty context,
        // runs in Yolo mode, persists nothing, and reports only its final
        // answer). The internal path needs this harness's connector and stop
        // signal, so it is routed here, at harness level, before Tier 1.
        if tool_name == "subagent_call" {
            // Nested harnesses run FOREGROUND-ONLY (Claude Code's
            // non-interactive nesting rule): a background task spawned
            // inside a sub-agent could outlive the nested loop that
            // requested it, and its completion notification has nowhere
            // coherent to land. Rejected for BOTH dispatch paths before
            // any parsing, so the model gets one consistent refusal.
            let run_in_background = args_map
                .get("run_in_background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if run_in_background && (self.nested || self.panes_requester != super::panes::MAIN) {
                self.tool_issuer.pop_front();
                return Err(
                    "run_in_background is not available in this loop: background \
                     sub-agent spawns are disabled here — the completion \
                     notification would have nowhere coherent to land. Run the \
                     task in the foreground (omit run_in_background) instead."
                        .to_string(),
                );
            }
            let agent_empty = args_map
                .get("agent")
                .and_then(|v| v.as_str())
                .is_none_or(|a| a.trim().is_empty());
            if agent_empty {
                // Pop BEFORE any fallible step: dispatch_next consumes the
                // item on both success and error, so the caller never
                // re-dispatches a failed internal call.
                self.tool_issuer.pop_front();
                let input: SubAgentCallInput =
                    serde_json::from_value(serde_json::Value::Object(args_map.clone()))
                        .map_err(|e| e.to_string())?;
                let raw_input = args_map
                    .get("input")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let call_input = self
                    .cosh_tools
                    .as_ref()
                    .ok_or_else(|| "internal sub-agent unavailable".to_string())?
                    .resolve_subagent_input(raw_input)?;
                // Review tasks carry the severity contract (same as the
                // external path): appended AFTER the stored-input resolution
                // so a retry never double-appends it. The same review
                // decision is kept for the report side: the internal turn's
                // final report gets the header ENFORCED in
                // `run_internal_subagent`.
                let is_review =
                    cosh_tools::subagent::severity::is_review_task(&call_input, input.code_review);
                // Right-panel list entry description (same contract as the
                // external path): explicit values pass through; a code-review
                // call may omit it (inferred `code-review-NN`), non-review
                // calls must carry one. The RESOLVED value is display-only —
                // the TUI derives the entry from the raw input — so this
                // call exists to VALIDATE.
                if let Some(tools) = self.cosh_tools.as_ref() {
                    // The issuer slot was ALREADY popped above ("Pop BEFORE
                    // any fallible step"): a validation failure here must
                    // NOT pop again, or the next queued tool call would be
                    // silently dropped.
                    tools.resolve_subagent_description(input.description.clone(), is_review)?;
                }
                let call_input = cosh_tools::subagent::severity::with_severity_contract(
                    &call_input,
                    input.code_review,
                );
                // Background mode (opt-in): register the task BEFORE the
                // spawn so `subagent_status` already reports it as
                // `running` when the acknowledgment below reaches the
                // model, then return the spawn receipt immediately. The
                // foreground path is untouched (task_id = None).
                let task_id = input.run_in_background.then(|| {
                    background::register("internal", &call_input, self.active_call_ref.clone())
                });
                return self
                    .run_internal_subagent(
                        call_input,
                        is_review,
                        task_id,
                        self.active_call_ref.clone(),
                    )
                    .await;
            }
        }

        // The /agents pane tool (`agents_call`): enqueue the message in the
        // pane registry and either await the pane's report (wait mode,
        // raced against the stop signal so Esc unblocks the tool) or
        // return the receipt immediately (background mode — the report
        // arrives later as an automated notification in THIS loop's drain;
        // the other loop never sees it). The queue itself IS the race
        // handling: a pane busy with the user's turn (or an earlier
        // request) receives the message after its current work — the
        // registry holds it, the App dispatches it when the pane frees.
        if tool_name == "agents_call" {
            self.tool_issuer.pop_front();
            let pane = args_map
                .get("pane")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let input = args_map
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if pane.is_empty() || input.trim().is_empty() {
                return Err(
                    "`agents_call` requires a non-empty `pane` and `input`.".to_string()
                );
            }
            // Pane-call guard (defense in depth): `with_own_pane_id` strips
            // the tool from every pane loop, so this arm is unreachable from
            // a pane — but if a pane ever reaches it anyway (a re-added
            // tool, a stale harness), refuse BEFORE the registry is
            // touched: a pane must never queue work into the pane set.
            // NO pop here — the arm already popped this call at its top,
            // and every error path after that point pops nothing.
            if self.panes_own_id.is_some() {
                return Err(
                    "`agents_call` is not available to a pane: pane agents execute their \
                     delegated task and report back — the coordinating agent owns the panes."
                        .to_string(),
                );
            }
            // Self-call guard (deadlock): a MANAGER loop runs INSIDE its own
            // pane, so asking that same pane to run a turn would queue behind
            // the turn making the call — the `wait` could never resolve and
            // the background busy-guard would spin forever. This refuses
            // BOTH modes for the pane's OWN id; the cross-pane `Wait` case
            // (a manager targeting ANOTHER cosh pane) is refused inside
            // `panes::enqueue` with a pointer to background mode. Sibling
            // cosh panes reached in background stay addressable.
            if self.panes_requester == super::panes::MANAGER
                && self.panes_own_id.as_deref() == Some(pane.as_str())
            {
                return Err(format!(
                    "`agents_call` cannot target this pane's own session ('{pane}'): the manager is already running in it, so the request could never be served. Address a different pane."
                ));
            }
            let background = args_map
                .get("run_in_background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mode = if background {
                super::panes::ReplyMode::Background
            } else {
                super::panes::ReplyMode::Wait
            };
            let handle = match super::panes::enqueue(&pane, &input, self.panes_requester, mode) {
                Ok(handle) => handle,
                Err(error) => return Err(error),
            };
            if let Some(rx) = handle.receiver {
                // Wait mode: the report IS the tool result. Esc unblocks:
                // the stop wait wins the race and the tool returns the stop
                // error (the request is cancelled for THIS requester — a
                // pane already working on it finishes its turn normally,
                // the report simply is not delivered to a dead caller).
                return tokio::select! {
                    answer = rx => {
                        answer.unwrap_or_else(
                            |_| Err("the pane request was cancelled".to_string()),
                        )
                    }
                    () = wait_for_stop_signal(self.stop_signal.clone()) => {
                        super::panes::cancel_wait(&handle.request_id);
                        Err("the agents_call wait was interrupted (stop requested)".to_string())
                    }
                };
            }
            return Ok(format!(
                "Message sent to pane '{pane}' in the background (request {}). \
                 Its report arrives as an [automated notification] when the pane's turn ends.",
                handle.request_id
            ));
        }

        // Test-only tools have no real dispatch implementation; return a
        // canned, deterministic success so tests (e.g. loop detection) can
        // drive repeated identical (tool, args, result) interactions.
        #[cfg(test)]
        if let Some(schema) = self.test_tools.iter().find(|t| t.name == tool_name) {
            self.tool_issuer.pop_front();
            return Ok(format!("ok:{}", schema.name));
        }

        // Tier 1: cosh tools
        let mut dispatched: Option<String> = None;
        // Drop any image payload abandoned by an earlier dispatch (permission
        // hook halt, user stop, MCP fallback): it can never be attached to
        // the result it belonged to anymore, so carrying it forward would
        // risk leaking an old screenshot onto a later tool result.
        self.pending_images.clear();
        let mut images = Vec::new();
        if let Some(ref cosh) = self.cosh_tools {
            // Clear any stale staged payload BEFORE dispatching: if a previous
            // screenshot was staged but its caller bailed out early (permission
            // hook halt, user stop), this reset guarantees the bytes never
            // surface on an unrelated tool result.
            let _ = cosh.take_pending_images();
            let args = serde_json::Value::Object(args_map.clone());
            match cosh.dispatch(&tool_name, args).await {
                Ok(result) => {
                    self.tool_issuer.pop_front();
                    images = cosh.take_pending_images();
                    dispatched = Some(result);
                }
                Err(err) if err.starts_with("unknown cosh tool") => {}
                Err(err) => {
                    self.tool_issuer.pop_front();
                    // Drop any staged payload: the error string returned to
                    // the caller has no image channel, and a stale PNG must
                    // never leak onto a later tool result.
                    let _ = cosh.take_pending_images();
                    return Err(self.enrich_with_schema_hint(&tool_name, err));
                }
            }
        }
        self.pending_images = images;
        if let Some(result) = dispatched {
            // Any plan tool may have changed the TODO list; mirror the
            // authoritative Plan state into the protected TODO block so the
            // model always sees the up-to-date plan.
            if tool_name.starts_with("plan_") {
                self.sync_todo_context();
            }

            return Ok(result);
        }

        // Tier 2: MCP servers (managed). Hidden in Ask mode, like the
        // header: the planning agent must not reach tools of unknown safety.
        // The harness-level "no server found" message is kept stable.
        if !matches!(self.mode, Mode::Ask) && self.mcp.owner_of(&tool_name).is_some() {
            self.tool_issuer.pop_front();
            if self.disabled_tools.contains(&tool_name) {
                return Err(format!("tool '{tool_name}' is disabled"));
            }
            return match self.mcp.call_tool(&tool_name, args_map).await {
                Ok(result) => Ok(crate::mcp::result_to_text(&result)),
                Err(crate::mcp::McpError::UnknownTool(_)) => {
                    Err(self.unknown_tool_message(&tool_name))
                }
                Err(err) => Err(self.enrich_with_schema_hint(&tool_name, err.to_string())),
            };
        }

        self.tool_issuer.pop_front();
        Err(self.unknown_tool_message(&tool_name))
    }
}
