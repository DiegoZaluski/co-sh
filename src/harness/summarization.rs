//! Explicit summarizer routing, separate from the agent's auto fallback chain.

use super::*;

impl Harness {
    /// Keep nested usage and explicit model-route notices visible without
    /// mixing the subagent's context or compaction lifecycle into the parent.
    pub(super) fn forward_nested_accounting(
        event: &super::super::events::HarnessEvent,
        parent: Option<&tokio::sync::mpsc::UnboundedSender<super::super::events::HarnessEvent>>,
    ) -> bool {
        use super::super::events::HarnessEvent;
        let forward = matches!(event, HarnessEvent::Usage { .. })
            || matches!(event, HarnessEvent::Toast { message, .. }
                if message.starts_with("Summarization") || message.starts_with("Configured summarization"));
        if forward && let Some(parent) = parent {
            let _ = parent.send(event.clone());
        }
        forward
    }

    /// Empty means the active agent connector. A nonempty chain is exclusive:
    /// no implicit agent-model or built-in fallback is appended.
    #[must_use]
    pub fn with_summarization_models(mut self, models: Vec<(String, String)>) -> Self {
        self.summarization_models = models;
        self
    }

    pub(super) fn compaction_connector(&self) -> &Connector {
        self.summarization_connector
            .as_ref()
            .unwrap_or(&self.connector)
    }

    pub(super) fn compaction_model_key(&self) -> String {
        let connector = self.compaction_connector();
        let model = connector.effective_model().unwrap_or("?");
        if self.summarization_connector.is_some() {
            format!("{}/{model}", connector.provider_name().unwrap_or("?"))
        } else {
            model.to_string()
        }
    }

    fn make_summarization_connector(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Connector, String> {
        if provider.trim().is_empty() || model.trim().is_empty() || model == "auto" {
            return Err("summarization requires an explicit provider and model".into());
        }
        let mut connector = Connector::new(provider)
            .map_err(|error| error.to_string())?
            .with_model(model)
            .with_prompt_cache_ttl_1h(self.connector.prompt_cache_ttl_1h());
        if let Some(url) = self.local_base_urls.get(provider) {
            connector = connector.with_base_url(url.clone());
        }
        if let Some(key) = self.connector.prompt_cache_key() {
            connector = connector.with_prompt_cache_key(key);
        }
        if let Some(retention) = self.connector.prompt_cache_retention() {
            connector = connector.with_prompt_cache_retention(retention);
        }
        // Explicit summary models use their provider defaults for reasoning;
        // the agent's effort setting belongs to the agent, not this chain.
        Ok(connector)
    }

    pub(super) async fn llm_compact(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::super::events::HarnessEvent>,
    ) -> bool {
        if self.summarization_connector.is_some() || self.summarization_models.is_empty() {
            self.llm_compact_selected(tx).await
        } else {
            Box::pin(self.compact_with_selected_models(tx, false)).await
        }
    }

    pub(super) async fn checkpoint_context(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::super::events::HarnessEvent>,
    ) -> bool {
        if self.summarization_connector.is_some() || self.summarization_models.is_empty() {
            self.checkpoint_context_selected(tx).await
        } else {
            Box::pin(self.compact_with_selected_models(tx, true)).await
        }
    }

    async fn compact_with_selected_models(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<super::super::events::HarnessEvent>,
        checkpoint: bool,
    ) -> bool {
        use super::super::events::{HarnessEvent, ToastVariant};
        let models = self.summarization_models.clone();
        self.compaction_interrupted = false;
        let mut success = false;
        for (index, (provider, model)) in models.iter().enumerate() {
            if self
                .stop_signal
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Relaxed))
            {
                break;
            }
            let _ = tx.send(HarnessEvent::Toast {
                message: format!(
                    "Summarization: {provider}/{model} ({}/{})",
                    index + 1,
                    models.len()
                ),
                variant: ToastVariant::Info,
            });
            let connector = match self.make_summarization_connector(provider, model) {
                Ok(connector) => connector,
                Err(error) => {
                    let _ = tx.send(HarnessEvent::Toast {
                        message: format!("Summarization {provider}/{model}: {error}"),
                        variant: ToastVariant::Warning,
                    });
                    continue;
                }
            };
            self.summarization_connector = Some(connector);
            #[cfg(not(test))]
            {
                self.summarization_window = tokio::select! {
                    window = discovered_context_window(Some(model)) => window,
                    () = wait_for_stop_signal(self.stop_signal.clone()) => break,
                };
            }
            #[cfg(test)]
            {
                self.summarization_window = self.mock_summarization_windows.get(model).copied();
            }

            success = if checkpoint || self.context_manager.compaction_staging_active() {
                self.checkpoint_context_selected(tx).await
            } else {
                self.llm_compact_selected(tx).await
            };
            self.emit_compaction_snapshot(tx);
            if success
                || self.compaction_interrupted
                || self
                    .stop_signal
                    .as_ref()
                    .is_some_and(|stop| stop.load(Ordering::Relaxed))
            {
                break;
            }
        }
        self.summarization_connector = None;
        self.summarization_window = None;
        if !success
            && !self.compaction_interrupted
            && !self
                .stop_signal
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Relaxed))
        {
            let _ = tx.send(HarnessEvent::Toast {
                message: "Configured summarization models failed. Context and accepted progress are preserved; review Settings > Summarization models.".into(),
                variant: ToastVariant::Warning,
            });
        }
        success
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::events::HarnessEvent;
    use super::*;

    #[test]
    fn nested_accounting_bridge_forwards_usage_and_route_notices_but_not_context() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let usage = HarnessEvent::Usage {
            usage: Default::default(),
            provider: "chosen".into(),
            model: "summary".into(),
            reported_cost: Some(0.01),
        };
        assert!(Harness::forward_nested_accounting(&usage, Some(&tx)));
        let notice = HarnessEvent::Toast {
            message: "Summarization: chosen/summary (1/1)".into(),
            variant: crate::harness::events::ToastVariant::Info,
        };
        assert!(Harness::forward_nested_accounting(&notice, Some(&tx)));
        let snapshot = HarnessEvent::ContextSnapshot {
            context: harness().context_manager.save_state(),
        };
        assert!(!Harness::forward_nested_accounting(&snapshot, Some(&tx)));
        assert!(
            matches!(rx.try_recv(), Ok(HarnessEvent::Usage { provider, model, reported_cost: Some(cost), .. }) if provider == "chosen" && model == "summary" && cost == 0.01)
        );
        assert!(matches!(rx.try_recv(), Ok(HarnessEvent::Toast { .. })));
        assert!(rx.try_recv().is_err());
    }

    fn harness() -> Harness {
        let mut harness = Harness::new_test();
        harness.context_manager = ContextManager::new(4_000);
        harness
            .context_manager
            .add_user("Keep the API stable; verify before publishing.");
        harness
            .context_manager
            .add_assistant("Parser verified; integration tests remain open.", true);
        harness.context_manager.begin_manual_compaction();
        harness.discovered_window = Some(32_000);
        harness
    }

    fn chain() -> Vec<(String, String)> {
        vec![
            ("openai".into(), "summary-first".into()),
            ("openrouter".into(), "summary-second".into()),
        ]
    }

    #[tokio::test]
    async fn manual_summary_uses_the_selected_wire_model_local_url_and_usage_identity() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut buffer = [0u8; 4_096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + length],
                        )
                        .unwrap();
                    }
                }
            };
            assert_eq!(body["model"], "chosen-summary");
            assert!(body.get("tools").is_none());
            assert!(body.get("reasoning_effort").is_none());
            assert_eq!(body["max_tokens"], 400);
            let response = "data: {\"choices\":[{\"delta\":{\"content\":\"Checkpoint: keep the API stable; run integration tests.\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":12,\"total_tokens\":112}}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        });
        let mut harness = harness()
            .with_summarization_models(vec![("lmstudio".into(), "chosen-summary".into())])
            .with_local_base_urls(std::collections::HashMap::from([(
                "lmstudio".into(),
                format!("http://{address}/v1"),
            )]));
        harness.connector = harness.connector.with_reasoning_effort("high");
        harness
            .mock_summarization_windows
            .insert("chosen-summary".into(), 4_000);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            harness.compact_on_demand(&tx, Arc::new(AtomicBool::new(false))),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ManualCompactionOutcome::Compacted));
        server.await.unwrap();
        assert_eq!(harness.connector.reasoning_effort(), Some("high"));
        let mut usage_seen = false;
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::Usage {
                provider, model, ..
            } = event
            {
                assert_eq!(provider, "lmstudio");
                assert_eq!(model, "chosen-summary");
                usage_seen = true;
            }
        }
        assert!(usage_seen);
    }

    #[tokio::test]
    async fn legacy_resume_uses_only_selected_models_and_preserves_progress_on_overflow() {
        let mut harness = harness().with_summarization_models(chain());
        harness
            .context_manager
            .add_user(&"more evidence ".repeat(500));
        harness.context_manager.add_assistant("recent tail", true);
        harness.context_manager.begin_split(1_000);
        let request = harness.context_manager.split_next_chunk().unwrap();
        harness
            .context_manager
            .advance_split("accepted legacy prefix", request.chunk_end);
        harness
            .mock_summarization_windows
            .insert("summary-first".into(), 1_000);
        harness
            .mock_summarization_windows
            .insert("summary-second".into(), 8_000);
        harness = harness.with_mock_chats(vec![
            Err(CONTEXT_WINDOW_MARKER),
            Ok("remaining legacy evidence"),
        ]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.checkpoint_context(&tx).await);
        assert_eq!(harness.mock_compaction_models, chain());
        let view = harness
            .context_manager
            .build_messages("")
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(view.contains("accepted legacy prefix"));
        assert!(view.contains("remaining legacy evidence"));
    }

    #[test]
    fn summary_window_errors_and_cache_options_are_isolated_from_the_agent() {
        let mut harness = harness();
        harness.connector = harness
            .connector
            .with_prompt_cache_ttl_1h(true)
            .with_prompt_cache_key("session-cache")
            .with_prompt_cache_retention("24h");
        let connector = harness
            .make_summarization_connector("lmstudio", "summary")
            .unwrap();
        assert!(connector.prompt_cache_ttl_1h());
        assert_eq!(connector.prompt_cache_key(), Some("session-cache"));
        assert_eq!(connector.prompt_cache_retention(), Some("24h"));
        harness.summarization_connector = Some(connector);
        harness.remember_error_window(2_000);
        assert_eq!(harness.known_checkpoint_window(), Some(2_000));
        assert_eq!(harness.context_manager.max_tokens(), 4_000);
        assert_eq!(harness.last_context_window, None);
        harness.summarization_connector = None;
        assert_eq!(harness.known_checkpoint_window(), Some(32_000));
    }

    #[tokio::test]
    async fn reactive_one_shot_overflow_keeps_the_selected_summarizer_for_mapreduce() {
        let mut harness = harness()
            .with_summarization_models(chain())
            .with_mock_chats(vec![
                Err(CONTEXT_WINDOW_MARKER),
                Ok("map"),
                Ok("checkpoint"),
                Ok("PASS"),
            ]);
        harness
            .mock_summarization_windows
            .insert("summary-first".into(), 8_000);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.llm_compact(&tx).await);
        assert_eq!(harness.mock_compaction_models, vec![chain()[0].clone(); 4]);
        assert_eq!(harness.context_manager.max_tokens(), 4_000);
    }

    #[tokio::test]
    async fn empty_chain_uses_only_the_agent_even_with_agent_fallbacks() {
        let mut harness = harness()
            .with_fallbacks(chain())
            .with_mock_chat(Ok("checkpoint"));
        let model = harness.connector.effective_model().unwrap().to_string();
        let provider = harness.connector.provider_name().unwrap().to_string();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.llm_compact(&tx).await);
        assert_eq!(harness.mock_compaction_models, vec![(provider, model)]);
        assert_eq!(harness.fallbacks, chain());
    }

    #[tokio::test]
    async fn configured_fallbacks_are_ordered_and_never_change_the_agent() {
        let mut harness = harness()
            .with_summarization_models(chain())
            .with_mock_chats(vec![
                Err("unavailable"),
                Err("unavailable"),
                Err("unavailable"),
                Ok("checkpoint"),
            ]);
        let original = harness.connector.effective_model().unwrap().to_string();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.llm_compact(&tx).await);
        assert_eq!(
            harness.mock_compaction_models,
            vec![
                chain()[0].clone(),
                chain()[0].clone(),
                chain()[0].clone(),
                chain()[1].clone()
            ]
        );
        assert_eq!(harness.connector.effective_model(), Some(original.as_str()));
        assert_eq!(harness.context_manager.max_tokens(), 4_000);
        assert_eq!(harness.discovered_window, Some(32_000));
        assert!(harness.summarization_connector.is_none());
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::Toast { message, .. } = event
                && message.starts_with("Summarization:")
            {
                notices.push(message);
            }
        }
        assert_eq!(notices.len(), 2);
        assert!(notices[0].contains("summary-first"));
        assert!(notices[1].contains("summary-second"));
    }

    #[tokio::test]
    async fn exhausted_chain_preserves_source_and_does_not_append_an_implicit_fallback() {
        let mut harness = harness()
            .with_summarization_models(chain())
            .with_mock_chat(Err("unavailable"));
        let before = serde_json::to_value(harness.context_manager.save_state().items).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(!harness.llm_compact(&tx).await);
        assert_eq!(harness.mock_compaction_models.len(), 6);
        assert!(
            harness
                .mock_compaction_models
                .iter()
                .all(|model| chain().contains(model))
        );
        assert_eq!(
            serde_json::to_value(harness.context_manager.save_state().items).unwrap(),
            before
        );
        assert!(harness.summarization_connector.is_none());
    }

    #[tokio::test]
    async fn cancellation_never_advances_to_another_summary_model() {
        let mut harness = harness()
            .with_summarization_models(chain())
            .with_mock_chats(vec![Err(INTERRUPTED_MARKER), Ok("must not run")]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(!harness.llm_compact(&tx).await);
        assert_eq!(harness.mock_compaction_models, vec![chain()[0].clone()]);
        assert_eq!(harness.mock_chat_queue.len(), 1);
        assert!(harness.summarization_connector.is_none());
    }

    #[tokio::test]
    async fn unknown_or_auto_entries_never_resolve_to_provider_defaults() {
        let mut harness = harness()
            .with_summarization_models(vec![
                ("unknown-provider".into(), "model".into()),
                ("openai".into(), "auto".into()),
                ("openai".into(), String::new()),
            ])
            .with_mock_chat(Ok("must not run"));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(!harness.llm_compact(&tx).await);
        assert!(harness.mock_compaction_models.is_empty());
    }

    #[tokio::test]
    async fn larger_configured_model_resumes_whole_item_maps_without_resizing_the_agent() {
        let mut harness = harness().with_summarization_models(chain());
        harness.context_manager = ContextManager::new(4_000);
        harness
            .context_manager
            .add_user(&"whole evidence ".repeat(1_000));
        harness
            .context_manager
            .add_assistant("recent raw tail", true);
        harness
            .mock_summarization_windows
            .insert("summary-first".into(), 1_000);
        harness
            .mock_summarization_windows
            .insert("summary-second".into(), 8_000);
        harness = harness.with_mock_chats(vec![
            Err(CONTEXT_WINDOW_MARKER),
            Ok("map"),
            Ok("checkpoint"),
            Ok("PASS"),
        ]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.checkpoint_context(&tx).await);
        assert_eq!(
            harness.mock_compaction_models,
            vec![
                chain()[0].clone(),
                chain()[1].clone(),
                chain()[1].clone(),
                chain()[1].clone()
            ]
        );
        assert_eq!(harness.context_manager.max_tokens(), 4_000);
        assert_eq!(harness.last_context_window, None);
        assert_eq!(harness.discovered_window, Some(32_000));
    }

    #[tokio::test]
    async fn accepted_maps_survive_a_summarizer_fallback_and_all_later_stages_use_it() {
        let mut harness = harness().with_summarization_models(chain());
        for _ in 0..12 {
            harness.context_manager.add_user(&"evidence ".repeat(250));
            harness.context_manager.add_assistant("acknowledged", true);
        }
        assert!(harness.context_manager.begin_map_reduce(2_000));
        harness.context_manager.disable_parallel_mapping();
        let count = harness.context_manager.pending_map_requests().len();
        assert!(count > 1);
        harness
            .mock_summarization_windows
            .insert("summary-first".into(), 2_000);
        harness
            .mock_summarization_windows
            .insert("summary-second".into(), 8_000);
        let mut replies = vec![
            Ok("first accepted map"),
            Err("failure"),
            Err("failure"),
            Err("failure"),
        ];
        replies.extend(std::iter::repeat_n(Ok("remaining map"), count - 1));
        replies.extend([
            Ok("checkpoint"),
            Ok("restore missing constraint"),
            Ok("corrected checkpoint"),
            Ok("PASS"),
        ]);
        harness = harness.with_mock_chats(replies);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(harness.checkpoint_context(&tx).await);
        assert_eq!(harness.mock_compaction_models.len(), 4 + count - 1 + 4);
        assert!(
            harness.mock_compaction_models[4..]
                .iter()
                .all(|model| model == &chain()[1])
        );
        let mut preserved = false;
        while let Ok(event) = rx.try_recv() {
            if let HarnessEvent::ContextSnapshot { context } = event
                && let Some(staging) = context.map_reduce
                && staging.model.as_deref() == Some("openrouter/summary-second")
            {
                assert_eq!(
                    staging.segments[0].summary.as_deref(),
                    Some("first accepted map")
                );
                preserved = true;
            }
        }
        assert!(preserved);
    }
}
