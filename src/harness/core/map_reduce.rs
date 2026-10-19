//! Hierarchical MapReduce compaction: parallel map summaries, recursive
//! reduce and the mock-error classifier shared with the summarizer tests.

use std::sync::atomic::Ordering;

#[cfg(test)]
use tokio::time::Duration;

use super::{
    CompactionErr, Harness, MAP_CONCURRENCY, MapRequest, compaction_retry_backoff,
    wait_for_stop_signal,
};
#[cfg(test)]
use super::{INTERRUPTED_MARKER, CONTEXT_WINDOW_MARKER};

pub(super) struct ParallelMapResult {
    ordinal: usize,
    result: Result<String, CompactionErr>,
}

impl Harness {
    /// Build a checkpoint from independent map summaries, recursively reduce
    /// the complete ordered set, then audit the candidate before committing.
    /// Accepted staging remains range-addressed and resumable on any failure;
    /// the committed model view is unchanged until validation succeeds.
    pub(super) async fn map_reduce_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> bool {
        use super::events::{HarnessEvent, LlmCompactionEvent};

        let model = self.compaction_model_key();
        if self.context_manager.overflow_stuck(&model) {
            self.notify_context_overflow(tx);
            return false;
        }
        let window = self
            .known_checkpoint_window()
            .unwrap_or_else(|| self.context_manager.max_tokens());
        if !self.context_manager.begin_map_reduce(window) {
            return false;
        }

        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Started,
        });
        self.context_manager
            .prepare_map_reduce_model(&model, window);
        let result = self.drive_map_reduce(tx).await;
        self.emit_compaction_snapshot(tx);
        self.compaction_generic_retries = 0;
        match &result {
            Err(CompactionErr::Other(_message)) => {
                // No toast: the frozen "Summarizing" line (Failed) already
                // reports it in the chat.
            }
            Err(CompactionErr::ContextWindow { window_tokens }) => {
                if let Some(window) = window_tokens {
                    self.remember_error_window(*window);
                }
                self.context_manager.mark_overflow(&model);
                self.notify_context_overflow(tx);
            }
            Err(CompactionErr::Incomplete(_message)) => {
                // No toast: the frozen "Summarizing" line (Failed) already
                // reports it in the chat.
            }
            Err(CompactionErr::Interrupted) => self.compaction_interrupted = true,
            Ok(_) => {}
        }
        let ok = result.is_ok_and(|committed| committed);
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: if ok {
                LlmCompactionEvent::Finished
            } else {
                LlmCompactionEvent::Failed
            },
        });
        ok
    }

    pub(super) async fn drive_map_reduce(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> Result<bool, CompactionErr> {
        use super::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};

        loop {
            if self
                .stop_signal
                .as_ref()
                .is_some_and(|signal| signal.load(Ordering::Relaxed))
            {
                return Err(CompactionErr::Interrupted);
            }
            let pending = self.context_manager.pending_map_requests();
            if !pending.is_empty() && self.context_manager.parallel_mapping_enabled() {
                self.run_parallel_maps(pending, tx).await?;
                continue;
            }
            if let Some(request) = pending.into_iter().next() {
                let (completed, total) = self.context_manager.map_progress();
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::SequentialFallback,
                        completed,
                        total,
                    },
                });
                let result = self
                    .summarize_checkpoint_request_mode(
                        &request.system,
                        &request.prompt,
                        tx,
                        false,
                        false,
                    )
                    .await;
                let summary = match result {
                    Err(CompactionErr::ContextWindow { window_tokens })
                        if self
                            .context_manager
                            .repartition_incomplete_maps(window_tokens) =>
                    {
                        self.emit_compaction_snapshot(tx);
                        continue;
                    }
                    result => result?,
                };
                if !self
                    .context_manager
                    .accept_map_summary(request.ordinal, &summary)
                {
                    return Err(CompactionErr::Other(
                        "could not accept a MapReduce segment summary".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.next_reduce_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Reducing,
                        completed: request.start,
                        total: request.total,
                    },
                });
                if request.final_group {
                    let _ = tx.send(HarnessEvent::LlmCompaction {
                        event: LlmCompactionEvent::OutputStarted,
                    });
                }
                let mut summary = self
                    .summarize_checkpoint_request_mode(
                        &request.system,
                        &request.prompt,
                        tx,
                        request.final_group,
                        request.final_group,
                    )
                    .await?;
                let reread = self.context_manager.parse_reread_request(&summary);
                if summary.trim().starts_with("REREAD:") && reread.is_empty() {
                    return Err(CompactionErr::Other(
                        "reducer requested an invalid source range".into(),
                    ));
                }
                if !reread.is_empty() {
                    let conflict = self
                        .context_manager
                        .conflict_reduce_request(&request, &reread)
                        .ok_or_else(|| {
                            CompactionErr::Other("reducer requested an invalid source range".into())
                        })?;
                    summary = self
                        .summarize_checkpoint_request_mode(
                            &conflict.system,
                            &conflict.prompt,
                            tx,
                            request.final_group,
                            request.final_group,
                        )
                        .await?;
                    if summary.trim().starts_with("REREAD:") {
                        return Err(CompactionErr::Other(
                            "reducer could not resolve a conflict after raw reread".into(),
                        ));
                    }
                }
                if !self
                    .context_manager
                    .accept_reduce_summary(&request, &summary)
                {
                    return Err(CompactionErr::Other(
                        "could not accept a MapReduce reduction".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.next_validation_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Validating,
                        completed: request.start,
                        total: request.total,
                    },
                });
                let audit = self
                    .summarize_checkpoint_request_mode(
                        &request.system,
                        &request.prompt,
                        tx,
                        false,
                        false,
                    )
                    .await?;
                if !self.context_manager.accept_validation(&request, &audit) {
                    return Err(CompactionErr::Other(
                        "could not accept a checkpoint validation result".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            if let Some(request) = self.context_manager.correction_request() {
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::Progress {
                        phase: LlmCompactionPhase::Correcting,
                        completed: 0,
                        total: 0,
                    },
                });
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::OutputStarted,
                });
                let corrected = self
                    .summarize_checkpoint_request_mode(
                        &request.system,
                        &request.prompt,
                        tx,
                        true,
                        true,
                    )
                    .await?;
                if !self.context_manager.accept_correction(&corrected) {
                    return Err(CompactionErr::Other(
                        "could not accept a corrected checkpoint".into(),
                    ));
                }
                self.emit_compaction_snapshot(tx);
                continue;
            }

            let committed = self.context_manager.commit_map_reduce();
            if committed {
                let summary = self
                    .context_manager
                    .save_state()
                    .items
                    .back()
                    .and_then(|item| {
                        if let super::context::ContextItem::Compaction { summary, .. } = item {
                            Some(summary.clone())
                        } else {
                            None
                        }
                    });
                let _ = tx.send(HarnessEvent::LlmCompaction {
                    event: LlmCompactionEvent::OutputStarted,
                });
                if let Some(text) = summary {
                    let _ = tx.send(HarnessEvent::LlmCompactionToken { text });
                }
            }
            return if committed {
                Ok(true)
            } else {
                Err(CompactionErr::Other(
                    "checkpoint could not be committed: source changed, output exceeds the budget, or reduction/validation limits were exhausted; accepted progress is preserved".into(),
                ))
            };
        }
    }

    pub(super) fn emit_compaction_snapshot(
        &self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        let _ = tx.send(super::events::HarnessEvent::ContextSnapshot {
            context: self.context_manager.save_state(),
        });
    }

    pub(super) async fn run_parallel_maps(
        &mut self,
        requests: Vec<MapRequest>,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) -> Result<(), CompactionErr> {
        use super::events::{HarnessEvent, LlmCompactionEvent, LlmCompactionPhase};

        let (completed, total) = self.context_manager.map_progress();
        let _ = tx.send(HarnessEvent::LlmCompaction {
            event: LlmCompactionEvent::Progress {
                phase: LlmCompactionPhase::Mapping,
                completed,
                total,
            },
        });

        let mut requests = requests.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..MAP_CONCURRENCY {
            let Some(request) = requests.next() else {
                break;
            };
            self.spawn_parallel_map(&mut tasks, request, tx);
        }

        let mut generic_failure = false;
        let mut incomplete_failure: Option<String> = None;
        let mut overflow_windows = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(ParallelMapResult {
                    ordinal,
                    result: Ok(summary),
                }) => {
                    if !self.context_manager.accept_map_summary(ordinal, &summary) {
                        tasks.abort_all();
                        return Err(CompactionErr::Other(
                            "could not accept a parallel MapReduce segment summary".into(),
                        ));
                    }
                    self.emit_compaction_snapshot(tx);
                    let (completed, total) = self.context_manager.map_progress();
                    let _ = tx.send(HarnessEvent::LlmCompaction {
                        event: LlmCompactionEvent::Progress {
                            phase: LlmCompactionPhase::Mapping,
                            completed,
                            total,
                        },
                    });
                }
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::Interrupted),
                    ..
                }) => {
                    tasks.abort_all();
                    return Err(CompactionErr::Interrupted);
                }
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::ContextWindow { window_tokens }),
                    ..
                }) => overflow_windows.push(window_tokens),
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::Incomplete(message)),
                    ..
                }) => incomplete_failure = Some(message),
                Ok(ParallelMapResult {
                    result: Err(CompactionErr::Other(_)),
                    ..
                })
                | Err(_) => generic_failure = true,
            }

            if incomplete_failure.is_none()
                && !generic_failure
                && overflow_windows.is_empty()
                && let Some(request) = requests.next()
            {
                self.spawn_parallel_map(&mut tasks, request, tx);
            }
        }

        if !overflow_windows.is_empty() {
            let reported_window = overflow_windows.into_iter().flatten().min();
            if !self
                .context_manager
                .repartition_incomplete_maps(reported_window)
            {
                return Err(CompactionErr::ContextWindow {
                    window_tokens: reported_window,
                });
            }
            if generic_failure {
                self.context_manager.disable_parallel_mapping();
            }
            self.emit_compaction_snapshot(tx);
            return Ok(());
        }

        if let Some(message) = incomplete_failure {
            self.context_manager.disable_parallel_mapping();
            self.emit_compaction_snapshot(tx);
            return Err(CompactionErr::Incomplete(message));
        }

        if generic_failure {
            self.context_manager.disable_parallel_mapping();
            self.emit_compaction_snapshot(tx);
            tokio::select! {
                () = tokio::time::sleep(compaction_retry_backoff(1)) => {},
                () = wait_for_stop_signal(self.stop_signal.clone()) => {
                    return Err(CompactionErr::Interrupted);
                }
            }
        }
        Ok(())
    }

    pub(super) fn spawn_parallel_map(
        &mut self,
        tasks: &mut tokio::task::JoinSet<ParallelMapResult>,
        request: MapRequest,
        tx: &tokio::sync::mpsc::UnboundedSender<super::events::HarnessEvent>,
    ) {
        let connector = self.compaction_connector().clone();
        let stop_signal = self.stop_signal.clone();
        let event_tx = tx.clone();
        let window = self.context_manager.map_reduce_window();
        #[cfg(test)]
        let mock_response = self.next_mock_chat();
        #[cfg(test)]
        let mock_delay = self.mock_map_delay_queue.pop_front().unwrap_or(0);
        tasks.spawn(async move {
            let ordinal = request.ordinal;
            #[cfg(test)]
            if let Some(response) = mock_response {
                if mock_delay > 0 {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(mock_delay)) => {},
                        () = wait_for_stop_signal(stop_signal.clone()) => {
                            return ParallelMapResult { ordinal, result: Err(CompactionErr::Interrupted) };
                        }
                    }
                }
                let result = response
                    .map_err(Self::classify_mock_compaction_error)
                    .and_then(|summary| {
                        (!summary.trim().is_empty())
                            .then_some(summary)
                            .ok_or_else(|| {
                                CompactionErr::Other("summarizer returned an empty response".into())
                            })
                    });
                return ParallelMapResult { ordinal, result };
            }
            let result = Self::summarize_map_with_connector(
                connector,
                request.system,
                request.prompt,
                stop_signal,
                event_tx,
                window,
            )
            .await;
            ParallelMapResult { ordinal, result }
        });
    }

    #[cfg(test)]
    pub(super) fn classify_mock_compaction_error(message: String) -> CompactionErr {
        match message.as_str() {
            INTERRUPTED_MARKER => CompactionErr::Interrupted,
            other if other.starts_with("summarizer response is incomplete") => {
                CompactionErr::Incomplete(message)
            }
            CONTEXT_WINDOW_MARKER => CompactionErr::ContextWindow {
                window_tokens: None,
            },
            other => other
                .strip_prefix(CONTEXT_WINDOW_MARKER)
                .and_then(|rest| rest.strip_prefix(':'))
                .and_then(|window| window.parse::<usize>().ok())
                .map_or_else(
                    || CompactionErr::Other(message),
                    |window_tokens| CompactionErr::ContextWindow {
                        window_tokens: Some(window_tokens),
                    },
                ),
        }
    }
}
