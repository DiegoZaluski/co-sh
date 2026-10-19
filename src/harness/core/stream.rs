//! LLM streaming: one model turn end-to-end — chunk dispatch, tool-call
//! extraction funnels and mid-stream retry handling.

use std::sync::atomic::Ordering;

use tokio::time::Duration;

use cosh_sdk::connector::ChatMessage;
use cosh_sdk::connector::ConnectorError;
use cosh_sdk::extract_action::{ExtractAction, NativeToolCall, StreamAction, ToolCallData};

use super::{
    Harness, INTERRUPTED_MARKER, StreamEvent, ToolCallMode, CONTEXT_WINDOW_MARKER,
};

impl Harness {
    /// Stream a chat completion using a proper messages array (native tool-call
    /// format). This is the replacement for the old text-based
    /// [`stream_chat`](Self::stream_chat) when using the native tool API.
    ///
    /// The `system` parameter is the system context (instructions + tool
    /// definitions, no history). The `messages` array contains the
    /// conversation history with roles (user, assistant with `tool_calls`,
    /// tool with `tool_call_id`).
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start.
    pub async fn stream_chat_with_messages(
        &mut self,
        system: &str,
        messages: &[ChatMessage],
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<String, String> {
        // Reset the truncation signal and the thinking-block stash for this
        // stream before anything else. The reasoning accumulator belongs to
        // the CURRENT attempt: a reset (or a fresh stream) must not leak the
        // previous attempt's thinking into this one's context item.
        self.last_finish_reason = None;
        self.last_stream_unfinished = false;
        self.pending_thinking_blocks.clear();
        self.attempt_reasoning.clear();

        #[cfg(test)]
        if let Some(response) = self.mock_stream_queue.pop_front() {
            // Mock reasoning chunks ride the SAME accumulation + event path
            // as the real stream: emitted as `HarnessEvent::Reasoning`
            // deltas BEFORE the text tokens and accumulated on
            // `attempt_reasoning`, so tests drive the reasoning-persistence
            // contract end to end without a real provider.
            for chunk in self.mock_reasoning_queue.pop_front().unwrap_or_default() {
                self.attempt_reasoning.push_str(&chunk);
                if let Some(ref rtx) = self.reasoning_tx {
                    let _ = rtx.send(super::events::HarnessEvent::Reasoning { text: chunk });
                }
            }
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    // Simulate the SDK's mid-stream retry marker: the consumer
                    // must discard any partial content and restart.
                    if self.mock_stream_resets.pop_front() == Some(true) {
                        self.handle_mid_stream_reset(&mut extractor);
                        on_event(StreamEvent::Reset);
                        // The RETRIED segment streams its own reasoning: emit
                        // the next queue entry AFTER the marker, riding the
                        // same accumulation + event path — so a test can pin
                        // that the reset discarded the failed attempt's
                        // thinking while the retry's survives.
                        for chunk in self.mock_reasoning_queue.pop_front().unwrap_or_default() {
                            self.attempt_reasoning.push_str(&chunk);
                            if let Some(ref rtx) = self.reasoning_tx {
                                let _ = rtx.send(super::events::HarnessEvent::Reasoning {
                                    text: chunk,
                                });
                            }
                        }
                    }
                    for token in &tokens {
                        // Mid-stream stop: an interrupted mock turn must
                        // surface the INTERRUPTED_MARKER error exactly like
                        // the real stream path — never a silent `Ok("done")`
                        // that makes the loop treat the truncation as a
                        // completed turn.
                        if self
                            .stop_signal
                            .as_ref()
                            .is_some_and(|s| s.load(Ordering::Relaxed))
                        {
                            return Err(INTERRUPTED_MARKER.to_string());
                        }
                        if self.mock_stream_delay_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(self.mock_stream_delay_ms))
                                .await;
                        }
                        self.dispatch_chunk(token, None, &mut extractor, &mut on_event);
                    }
                    // Test-only: route structured native tool calls through the
                    // same funnel the real-stream branch uses (a provider
                    // emitting `tool_calls`/`tool_use`/`functionCall` parts).
                    if let Some(calls) = self.mock_native_stream_queue.pop_front() {
                        for call in calls {
                            self.process_native_tool_call(&call, &mut extractor, &mut on_event);
                        }
                    }
                    // Tests can simulate a provider-side truncation by queueing
                    // a finish_reason per stream (consumed one at a time).
                    // Match the OUTER option first: an explicitly queued
                    // `None` (EOF without a finish frame — the whitelist's
                    // silent "none" continuation path) stays testable; an
                    // unqueued mock stream ends deliberately (`stop`).
                    self.last_finish_reason = match self.mock_finish_reasons.pop_front() {
                        Some(reason) => reason,
                        None => Some("stop".to_string()),
                    };
                    // Mock-path parity with the real stream: freeze the
                    // extractor's end-of-stream state for the natural-stop
                    // gate (a parked call or unclosed JSON object = a cut).
                    self.last_stream_unfinished = extractor.has_unfinished_stream();
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        // A fresh real stream: forget any stashed context-window size from a
        // previous attempt (the mock path returns before this and sets its
        // own expectations).
        self.last_context_window = None;

        log::debug!("stream_chat_with_messages messages={}", messages.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_messages(system, messages) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        log::debug!("stream_chat_with_messages CONNECTOR_ERR={e}");
                        // A context-window overflow must reach the retry loop
                        // as the identity marker (with the window size stashed)
                        // so it is never confused with a generic error.
                        return Err(match e {
                            ConnectorError::ContextWindowExceeded {
                                window_tokens, ..
                            } => {
                                if let Some(window) = window_tokens {
                                    self.remember_error_window(window);
                                }
                                CONTEXT_WINDOW_MARKER.to_string()
                            }
                            other => other.to_string(),
                        });
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                // log::debug!("stream_chat_with_messages STOPPED during connect");
                return Err(INTERRUPTED_MARKER.to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    log::debug!("stream_chat_with_messages STOPPED by signal");
                    // Surface the interruption as the marker error — NOT as a
                    // completed turn. Falling through to `Ok("done")` here
                    // made the agent loop treat the truncated response as a
                    // finished assistant turn and CONTINUE (Done → queued
                    // loop auto-restart) instead of stopping.
                    return Err(INTERRUPTED_MARKER.to_string());
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        // log::debug!("stream_chat_with_messages STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            // Capture the stream's terminal finish_reason so run_agent_loop can
            // detect max-token truncation ("length") and continue the loop
            // instead of treating the cut-off turn as finished.
            if let Some(f) = fr {
                self.last_finish_reason = Some(f.to_owned());
            }
            // Log only the first few tokens and every 100th — logging every
            // token costs a mutex + file write per token in debug builds.
            if token_count <= 5 || token_count.is_multiple_of(100) || fr.is_some() {
                log::debug!(
                    "stream_chat_with_messages token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            // The SDK retried a mid-stream failure: drop the partial content
            // of the failed attempt — the extractor buffer, the thinking
            // blocks stashed from it, and any finish reason it carried — and
            // tell the consumer (agent loop / TUI) to discard what it
            // rendered, so the retried response restarts clean.
            if chunk.is_reset() {
                log::debug!("stream_chat_with_messages RESET after retry");
                self.handle_mid_stream_reset(&mut extractor);
                on_event(StreamEvent::Reset);
                continue;
            }
            self.dispatch_chunk(token, chunk.tool_call(), &mut extractor, &mut on_event);
            // Stream reasoning/thinking deltas to the TUI so the user sees the
            // model "think" while it works (never echoed back to the model).
            // The same deltas accumulate on `attempt_reasoning` so the loop
            // can record them with the response's context item (DISPLAY-ONLY
            // payload — the model never receives it back). Accumulated even
            // when no reasoning channel is wired, so a harness without a TUI
            // listener still persists the thinking.
            let reasoning = chunk.reasoning();
            if !reasoning.is_empty() {
                self.attempt_reasoning.push_str(reasoning);
                if let Some(ref rtx) = self.reasoning_tx {
                    let _ = rtx.send(super::events::HarnessEvent::Reasoning {
                        text: reasoning.to_string(),
                    });
                }
            }
            // Stash Claude thinking blocks (emitted at turn end when the turn
            // ended with a tool call) so `push_tool_history` can replay them
            // verbatim on the follow-up request.
            if let Some(blocks) = chunk.thinking_blocks() {
                self.pending_thinking_blocks = blocks.to_vec();
            }
        }

        self.last_failed_raw = extractor.take_last_failed_raw();
        // Freeze the extractor's end-of-stream state for the agent loop's
        // natural-stop gate: a parked tool call or an unclosed JSON object at
        // stream end proves the response was CUT, whatever `finish_reason`
        // claims.
        self.last_stream_unfinished = extractor.has_unfinished_stream();
        // Stream accounting (file log, target `cosh::harness`): what THIS
        // attempt produced — streamed token count, reasoning bytes, finish
        // reason and the cut flag. The reasoning bytes are the promotion
        // fuel: if the reasoning-only veto fires repeatedly on the same
        // turn, this line shows how much thinking gets re-sent each retry.
        log::info!(
            target: "cosh::harness",
            "stream done: tokens={} reasoning_bytes={} finish_reason={:?} cut={}",
            token_count,
            self.attempt_reasoning.len(),
            self.last_finish_reason,
            self.last_stream_unfinished,
        );

        // log::debug!("stream_chat_with_messages DONE total_tokens={token_count}");
        self.emit_usage(&mut stream).await;
        Ok("done".into())
    }

    /// Dispatch one stream chunk to the correct tool-call path, honoring the
    /// active [`ToolCallMode`]:
    /// - a native (structured) chunk always routes to the extractor's native
    ///   validation funnel ([`Self::process_native_tool_call`]), in both
    ///   modes;
    /// - text chunks are parsed for inline-JSON tool calls ONLY in
    ///   [`ToolCallMode::Inline`] — in `Native` mode (default) the text is
    ///   the model's answer and is emitted verbatim, never parsed (the crush
    ///   contract: tool calls arrive only as structured parts).
    fn dispatch_chunk(
        &mut self,
        token: &str,
        native: Option<&NativeToolCall>,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        if let Some(native) = native {
            self.process_native_tool_call(native, extractor, on_event);
        } else if self.connector.tool_call_mode() == ToolCallMode::Inline {
            self.process_stream_chunk(token, extractor, on_event);
        } else {
            on_event(StreamEvent::Token(token.to_string()));
        }
    }

    /// Process a stream chunk through the extractor, routing tool calls and
    /// yielding text via the callback.
    fn process_stream_chunk(
        &mut self,
        token: &str,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        let action = extractor.extract_stream(token);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_event(StreamEvent::Token(text));
            }
            StreamAction::ToolCall(tc) => self.route_tool_call(tc),
            StreamAction::Pending => {}
        }
    }

    /// Route a validated tool call: consume it if it is a harness tool,
    /// otherwise queue it for dispatch. Shared by the inline-JSON path
    /// ([`Self::process_stream_chunk`]) and the native structured path
    /// ([`Self::process_native_tool_call`]).
    pub(crate) fn route_tool_call(&mut self, tc: ToolCallData) {
        match self.handle_harness_tool(&tc) {
            None => {
                self.tool_issuer.push_back(tc);
            }
            Some(result) if !result.is_empty() => {
                self.push_tool_history(
                    &tc.id,
                    &tc.name,
                    &tc.arguments,
                    &tc.thought_signature,
                    &result,
                );
            }
            _ => {}
        }
    }

    /// Drop every trace of a failed stream attempt when the SDK emits a
    /// mid-stream retry marker: the extractor's half-parsed buffer, stashed
    /// thinking blocks, any tool calls already queued from the failed
    /// attempt, and the finish reason it carried. The retried response
    /// restarts from the beginning, so the failed attempt's tool calls must
    /// not be dispatched (or duplicated) alongside it.
    fn handle_mid_stream_reset(&mut self, extractor: &mut ExtractAction) {
        extractor.reset_stream_state();
        self.pending_thinking_blocks.clear();
        self.tool_issuer.clear();
        self.last_finish_reason = None;
        // The retried attempt restarts clean: the discarded partial content
        // takes its reasoning with it (the TUI drops the streaming message's
        // reasoning part on the same reset).
        self.attempt_reasoning.clear();
    }

    /// Route a tool call the provider delivered NATIVELY (structured
    /// `tool_calls`/`tool_use`/`functionCall` parts) through the extractor's
    /// native validation funnel — the same `StreamAction` dispatch as the
    /// inline-JSON path, minus the text parsing. Validation failures are
    /// counted and fed to the correction memory exactly like a failed inline
    /// call (the tool-failure message streams as text).
    fn process_native_tool_call(
        &mut self,
        native: &NativeToolCall,
        extractor: &mut ExtractAction,
        on_event: &mut dyn FnMut(StreamEvent),
    ) {
        let action = extractor.register_native_call(native);
        self.tool_extraction_failure_count += extractor.take_tool_failures();
        match action {
            StreamAction::Text(text) => {
                on_event(StreamEvent::Token(text));
            }
            StreamAction::ToolCall(tc) => self.route_tool_call(tc),
            StreamAction::Pending => {}
        }
    }

    /// Stream a chat completion, calling `on_token` with each text delta.
    ///
    /// Use this when streaming is enabled — tokens are delivered in real time
    /// via the callback. Returns `Ok("done".into())` when the stream finishes.
    ///
    /// # Errors
    ///
    /// Returns an error if the connector stream fails to start or a chunk is malformed.
    pub async fn stream_chat(
        &mut self,
        input: &str,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<String, String> {
        // Reset the truncation signal for this stream before anything else.
        self.last_finish_reason = None;

        #[cfg(test)]
        if let Some(response) = self.mock_stream_queue.pop_front() {
            match response {
                Ok(tokens) => {
                    let mut extractor = self.build_extractor();
                    for token in &tokens {
                        self.dispatch_chunk(token, None, &mut extractor, &mut on_event);
                    }
                    // Tests can simulate a provider-side truncation by queueing
                    // a finish_reason per stream (consumed one at a time).
                    // Same outer-option match as `stream_chat_with_messages`:
                    // an unqueued mock stream ends deliberately (`stop`), an
                    // explicitly queued `None` stays `None`.
                    self.last_finish_reason = match self.mock_finish_reasons.pop_front() {
                        Some(reason) => reason,
                        None => Some("stop".to_string()),
                    };
                    return Ok("done".into());
                }
                Err(msg) => {
                    return Err(msg);
                }
            }
        }

        use tokio_stream::StreamExt;

        let context = self.build_chat_context();
        log::debug!("stream_chat context_len={}", context.len());

        let mut stream = tokio::select! {
            result = self.connector.stream_chat_with_system(input, &context) => {
                match result {
                    Ok(s) => s,
                    Err(e) => {
                        // log::debug!("stream_chat CONNECTOR_ERR={e}");
                        return Err(e.to_string());
                    }
                }
            }
            _ = async {
                loop {
                    if self
                        .stop_signal
                        .as_ref()
                        .is_some_and(|s| s.load(Ordering::Relaxed))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                // log::debug!("stream_chat STOPPED during connect");
                return Err(INTERRUPTED_MARKER.to_string());
            }
        };

        let mut extractor = self.build_extractor();
        let mut token_count = 0u64;

        loop {
            {
                let stop = self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::Relaxed));
                if stop {
                    // log::debug!("stream_chat STOPPED by signal");
                    // Same contract as [`Self::stream_chat_with_messages`]:
                    // an interrupted stream must surface the marker error —
                    // the compaction caller maps it to
                    // `CompactionErr::Interrupted` (a silent `Ok` would make
                    // the truncation look like a finished summary).
                    return Err(INTERRUPTED_MARKER.to_string());
                }
            }

            let chunk = {
                let poll = tokio::select! {
                    chunk = stream.next() => chunk.map(|c| c.map_err(|e| {
                        // log::debug!("stream_chat STREAM_ERR={e}");
                        e.to_string()
                    })),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {
                        continue;
                    }
                };
                match poll {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            };
            let token = chunk.token();
            let fr = chunk.finish_reason();
            token_count += 1;
            // Capture the stream's terminal finish_reason so run_agent_loop can
            // detect max-token truncation ("length") and continue the loop
            // instead of treating the cut-off turn as finished.
            if let Some(f) = fr {
                self.last_finish_reason = Some(f.to_owned());
            }
            // Log only the first few tokens and every 100th — logging every
            // token costs a mutex + file write per token in debug builds.
            if token_count <= 5 || token_count.is_multiple_of(100) || fr.is_some() {
                log::debug!(
                    "stream_chat token#{} len={} fr={:?} first_50={:?}",
                    token_count,
                    token.len(),
                    fr,
                    &token[..token.floor_char_boundary(token.len().min(50))]
                );
            }
            // The SDK retried a mid-stream failure: drop the partial content
            // of the failed attempt and tell the consumer to do the same, so
            // the retried response restarts clean (see
            // [`Self::stream_chat_with_messages`]).
            if chunk.is_reset() {
                self.handle_mid_stream_reset(&mut extractor);
                on_event(StreamEvent::Reset);
                continue;
            }
            self.dispatch_chunk(token, chunk.tool_call(), &mut extractor, &mut on_event);
            // Stream reasoning/thinking deltas to the TUI so the user sees the
            // model "think" while it works (never echoed back to the model).
            // The same deltas accumulate on `attempt_reasoning` so the loop
            // can record them with the response's context item (DISPLAY-ONLY
            // payload — the model never receives it back). Accumulated even
            // when no reasoning channel is wired, so a harness without a TUI
            // listener still persists the thinking.
            let reasoning = chunk.reasoning();
            if !reasoning.is_empty() {
                self.attempt_reasoning.push_str(reasoning);
                if let Some(ref rtx) = self.reasoning_tx {
                    let _ = rtx.send(super::events::HarnessEvent::Reasoning {
                        text: reasoning.to_string(),
                    });
                }
            }
        }

        // Capture the last failed tool call raw JSON for correction feedback
        self.last_failed_raw = extractor.take_last_failed_raw();

        // log::debug!("stream_chat DONE total_tokens={token_count}");
        self.emit_usage(&mut stream).await;
        Ok("done".into())
    }
}
