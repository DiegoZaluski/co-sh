use super::super::{App, AppMode};
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;
use crate::ui::toast::{ToastOptions, ToastVariant};

use cosh_sdk::connector::{ZEN_PROVIDER, get_provider, has_api_key, is_local_provider};

impl App {
    pub(in crate::app) fn is_zen_gateway_dialog_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ZenFreeGateway)
            )
    }

    /// Whether sending now would hit the OpenCode Zen gateway WITHOUT an
    /// account key and before the user answered the one-time free-gateway
    /// prompt. Covers both the directly selected provider and any provider
    /// reachable through an `auto` fallback chain.
    pub(in crate::app) fn needs_zen_gateway_prompt(&self) -> bool {
        // Answered (Yes or No, persisted in setup.json) → never ask again.
        if self.setup.zen_public_opt_in().is_some() {
            return false;
        }
        // A configured account key makes the free gateway irrelevant.
        if has_api_key(ZEN_PROVIDER) {
            return false;
        }
        // Only park a send that cannot succeed anyway: EVERY candidate in
        // this send lacks credentials (local servers are skipped — they run
        // without keys). One working fallback keeps auto mode alive and the
        // prompt away.
        let candidates: Vec<String> = if self.llm_config.model.as_deref() == Some("auto") {
            self.router_view
                .fallbacks
                .iter()
                .map(|fb| fb.provider.clone())
                .collect()
        } else {
            vec![self.llm_config.provider.clone()]
        };
        !candidates.is_empty()
            && candidates.into_iter().all(|p| match get_provider(&p) {
                Some(cfg) if cfg.local => false,
                _ => !has_api_key(&p),
            })
    }

    /// Arrow navigation inside the Zen free-gateway prompt (Enter is handled
    /// by the `Confirm` action arm in `keys.rs`, Esc pops without answering).
    pub(in crate::app) fn handle_zen_gateway_dialog_key(&mut self, key: KeyCode) -> bool {
        if !self.is_zen_gateway_dialog_visible() {
            return false;
        }
        match key {
            KeyCode::Left | KeyCode::Right => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected ^= 1;
                }
                true
            }
            _ => false,
        }
    }

    /// Put a parked message back into the prompt input — used whenever the
    /// dialog closes WITHOUT an opt-in so the user never loses what they
    /// typed.
    pub(in crate::app) fn restore_pending_zen_message(&mut self) {
        if let Some(msg) = self.pending_zen_message.take() {
            self.prompt_view.input = msg;
            self.prompt_view.cursor_pos = self.prompt_view.input.len();
        }
    }

    /// Enter on the Zen free-gateway prompt: record the user's FINAL answer
    /// (write-once in setup.json — the prompt never appears again), then
    /// replay the parked message when opting in.
    pub(in crate::app) fn commit_zen_gateway_choice(&mut self) {
        let opted_in = self.dialog.current().is_some_and(|d| d.selected == 0);
        self.setup.record_zen_public_opt_in(opted_in);
        // Flip the process-wide switch immediately so the replayed message —
        // and every later connector (fallback chains, /compact, titles) —
        // inherits the answer without a restart.
        cosh_sdk::connector::set_zen_public_tier_enabled(opted_in);
        self.dialog.pop();

        let pending = self.pending_zen_message.take();
        if opted_in {
            // The prompt only ever appears after the current path FAILED
            // (no credentials, or a key/model-access error), so answering
            // Yes reroutes the session onto the free gateway's default model
            // — even when the previous provider HAS a key (a key does not
            // help against a broken/unavailable model). A running LOCAL
            // server is never overridden.
            if self.llm_config.provider != ZEN_PROVIDER
                && !is_local_provider(&self.llm_config.provider)
            {
                self.llm_config.provider = ZEN_PROVIDER.to_string();
                self.llm_config.model =
                    get_provider(ZEN_PROVIDER).map(|cfg| cfg.default_model.to_string());
                self.llm_config.reasoning = None;
            }
            match pending {
                Some(msg) => self.start_agent_loop(msg),
                None => self.toast_state.show(ToastOptions {
                    title: Some("OpenCode Zen free gateway enabled".into()),
                    message:
                        "Pick a free model and send your message. Add an API key anytime for paid models."
                            .into(),
                    variant: ToastVariant::Info,
                    duration_ms: 6000,
                }),
            }
        } else {
            self.restore_pending_zen_message();
            self.toast_state.show(ToastOptions {
                title: Some("Free gateway declined".into()),
                message:
                    "Configure an OpenCode API key (ADD Provider) to use its models. Reset providers.zen_public_opt_in in setup.json to be asked again."
                        .into(),
                variant: ToastVariant::Warning,
                duration_ms: 8000,
            });
        }
    }
    /// Offer the one-time Zen free-gateway prompt after a FAILED send whose
    /// cause the free gateway would actually fix:
    /// - a missing API key (any provider);
    /// - an auth rejection (401/403) on the selected provider;
    /// - a model-access failure (404 — model does not exist / no access).
    ///
    /// This is the zero-config onboarding path — a fresh install with no keys
    /// lands here on its first message. Transient failures (429 rate limit,
    /// 5xx, network) and context-window overflows deliberately do NOT offer:
    /// switching to the anonymous tier must never look like a way around a
    /// quota, and retryable errors resolve on their own.
    ///
    /// Guarded by the write-once opt-in state: once the user answered Yes or
    /// No — even in an earlier session — this never prompts again.
    pub(in crate::app) fn maybe_offer_zen_gateway_on_error(&mut self, error: &str) {
        if self.setup.zen_public_opt_in().is_some() || has_api_key(ZEN_PROVIDER) {
            return;
        }
        if !matches!(self.mode(), AppMode::Session) || self.dialog.visible() {
            return;
        }
        // A running local server needs no credentials and must never be
        // offered a cloud replacement because of one bad response.
        if is_local_provider(&self.llm_config.provider) {
            return;
        }
        let missing_key = error.contains("API key not set for provider:");
        let config_failure = ["HTTP 401", "HTTP 403", "HTTP 404"]
            .iter()
            .any(|marker| error.contains(marker));
        if missing_key || config_failure {
            self.dialog.show(DialogType::ZenFreeGateway);
        }
    }
}
