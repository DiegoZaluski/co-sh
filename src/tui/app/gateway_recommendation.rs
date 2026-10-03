use super::{App, AppMode};

use crate::routes::session::free_gateway_recommendation::{self, GatewayRecommendationContent};
use crate::ui::toast::{ToastOptions, ToastVariant};

use cosh_sdk::connector::openai_compatible::charm_oauth::{self, CharmOAuthConfig};
use cosh_sdk::connector::{
    get_provider_env_var, has_api_key, invalidate_api_key, is_local_provider,
};

use tokio::sync::mpsc::UnboundedSender;

/// Messages from the background Charm device sign-in into the UI thread.
#[derive(Debug)]
pub(super) enum GatewaySignInEvent {
    /// The device transaction started: hold the verification URL and user
    /// code on the App so they stay VISIBLE (spinner line) for as long as
    /// the sign-in runs — a transient toast is not enough; the Charm page
    /// asks for the code after login, minutes later.
    Started { url: String, user_code: String },
    /// The token is stored in the OS keyring; the parked message can be
    /// replayed through the newly authorized gateway.
    Authorized,
    /// Sign-in failed, was denied or crashed; the parked message goes back
    /// to the prompt untouched.
    Failed(String),
}

impl App {
    /// Routes where the free-gateway recommendation dialog is rendered and
    /// takes input: Session only. Every trigger lives there — a parked
    /// message sent without credentials, or a keyless/keyed provider whose
    /// request just failed with an auth-shaped error. Home never learns the
    /// dialog exists. The shipped slim build (`--no-default-features`) has
    /// no Home: `mode()` folds to Session even with no session yet, so the
    /// offer renders inline over the empty session view. Render and input
    /// guards MUST use this — a plain `is_session` check duplicated by hand
    /// would drift.
    pub(super) fn gateway_dialog_route(&self) -> bool {
        matches!(self.mode(), AppMode::Session)
    }

    /// Whether the one-time gateway offer was already made (or deliberately
    /// skipped) on this install.
    fn gateway_offer_marker_exists() -> bool {
        directories::BaseDirs::new()
            .map(|dirs| {
                dirs.data_local_dir()
                    .join("cosh/gateway-recommendation")
                    .exists()
            })
            .unwrap_or(false)
    }

    /// Burn the one-time offer marker. Best-effort: a read-only disk just
    /// means the offer may reappear on the next launch.
    fn burn_gateway_offer_marker(&self) {
        if let Some(dirs) = directories::BaseDirs::new() {
            let marker = dirs.data_local_dir().join("cosh/gateway-recommendation");
            let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
            let _ = std::fs::write(&marker, b"");
        }
    }

    /// Which gateway recommendation to offer, if any.
    ///
    /// Two offer paths: (1) the active provider is keyless — the primary
    /// first-run scenario; (2) the provider HAS credentials but just failed
    /// with an auth-shaped error (`auth_failure`) — a broken or revoked key
    /// makes the free gateway the sensible fallback. The user's own key is
    /// NEVER touched on path 2; only the Charm flow clears its own tokens.
    pub(in crate::app) fn gateway_recommendation_content(
        &self,
        auth_failure: bool,
    ) -> Option<&'static GatewayRecommendationContent> {
        if self.charm_signing_in || self.charm_declined {
            return None;
        }
        let provider = &self.llm_config.provider;
        if is_local_provider(provider) {
            return None;
        }
        if !has_api_key(provider) || auth_failure {
            return Some(&free_gateway_recommendation::CHARM);
        }
        None
    }

    /// Put a parked message back into the prompt input — used whenever the
    /// dialog closes WITHOUT an opt-in so the user never loses what they typed.
    pub(in crate::app) fn restore_pending_gateway_message(&mut self) {
        if let Some(msg) = self.pending_gateway_message.take() {
            // One Replace group: a single Ctrl+Z undoes the restore.
            self.prompt_view.set_draft(msg);
        }
    }

    /// Enter on the free gateway recommendation dialog.
    ///
    /// Decline: record the choice (never ask again this run) and put the
    /// parked message back. Opt-in: run the Charm device sign-in in the
    /// background — the parked message replays once the token is in the
    /// keyring, or goes back untouched on failure.
    pub(in crate::app) fn commit_gateway_choice(&mut self) {
        let opted_in = self.free_gateway_dialog.selected == 0;
        if !opted_in {
            self.free_gateway_dialog.hide();
            self.charm_declined = true;
            self.restore_pending_gateway_message();
            self.toast_state.show(ToastOptions {
                title: Some("Free gateway declined".into()),
                message: "Configure a provider API key (ADD Provider) to use its models.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 8000,
            });
            return;
        }

        // Opted in: keep the box EXACTLY where it is (Connecting phase —
        // the options rows give way to a hint) and start the device flow.
        // Hiding here would visibly close and reopen it a frame later.
        self.free_gateway_dialog.begin_connecting();
        self.charm_signing_in = true;
        let tx = self.gateway_event_tx.clone();
        // The shared agent runtime: the UI thread has no ambient tokio
        // context, and the sign-in must survive any turn teardown.
        let runtime = super::agent_loop::AGENT_RUNTIME
            .get_or_init(|| tokio::runtime::Runtime::new().expect("shared agent runtime"));
        runtime.spawn(async move {
            // The inner spawn converts a panic inside the sign-in future into
            // a JoinError — and therefore a Failed event — instead of leaving
            // `charm_signing_in` stuck true and the parked message lost.
            let inner_tx = tx.clone();
            if let Err(join_error) = tokio::spawn(charm_sign_in(inner_tx)).await {
                let _ = tx.send(GatewaySignInEvent::Failed(format!(
                    "the sign-in task crashed: {join_error}"
                )));
            }
        });
    }

    /// Drain Charm sign-in outcomes from the shared runtime. Called once per
    /// frame, next to the update pump.
    pub(in crate::app) fn pump_gateway_events(&mut self) {
        // A gateway-authorized model pick resolved since the last frame
        // (commit or cancel popped the picker): the parked message can
        // finally run — with the model the user just picked, or the
        // provider default when they cancelled. Sending it any EARLIER ran
        // it under the open picker with a model the user never confirmed.
        if self.gateway_model_pick && !self.is_model_dialog_visible() {
            self.gateway_model_pick = false;
            let idle_session = matches!(self.mode(), AppMode::Session)
                && !matches!(self.state.status, crate::types::SessionStatus::Working);
            if let Some(msg) = self.pending_gateway_message.take() {
                if idle_session && !self.dialog.visible() {
                    self.toast_state.show(ToastOptions {
                        title: Some("Charm gateway authorized".into()),
                        message: "Sending your message.".into(),
                        variant: ToastVariant::Success,
                        duration_ms: 6000,
                    });
                    self.start_agent_loop(msg);
                } else {
                    // The user left the session (or a modal took over)
                    // while picking: the message goes back to the prompt,
                    // nothing fires from the wrong route.
                    self.pending_gateway_message = Some(msg);
                    self.toast_state.show(ToastOptions {
                        title: Some("Charm gateway authorized".into()),
                        message: "Resend your message to use the gateway.".into(),
                        variant: ToastVariant::Success,
                        duration_ms: 8000,
                    });
                }
            }
        }
        while let Ok(event) = self.gateway_event_rx.try_recv() {
            match event {
                GatewaySignInEvent::Started { url, user_code } => {
                    // The dialog ITSELF becomes the sign-in display: its
                    // Yes/No options give way to the device code + URL,
                    // drag-copyable, for the whole sign-in. No toast — the
                    // box already shows the URL and the code; a toast on
                    // top would only repeat them. If the user Esc'd the box
                    // away during Connecting, respect that: the flow keeps
                    // polling in the background and its outcome still
                    // reports via toast — never force the box back open.
                    if self.free_gateway_dialog.visible {
                        self.free_gateway_dialog.begin_authorizing(&url, &user_code);
                    }
                }
                GatewaySignInEvent::Authorized => {
                    self.charm_signing_in = false;
                    // Whether the user kept the sign-in box up: an Esc
                    // mid-flow asked the screen back — the outcome must not
                    // steal it again with a fresh modal one event later.
                    let box_was_open = self.free_gateway_dialog.visible;
                    self.free_gateway_dialog.hide();
                    // Route the (re)played message through the newly
                    // authorized gateway (provider default model).
                    self.llm_config.provider = "charm".into();
                    self.llm_config.model = None;
                    // Replay only from an idle Session view: a message sent
                    // while the sign-in was in flight may already own the
                    // agent loop, and start_agent_loop from another route
                    // would fire it in the wrong place.
                    let idle_session = matches!(self.mode(), AppMode::Session)
                        && !matches!(self.state.status, crate::types::SessionStatus::Working);
                    // Model picker ONLY when the user kept the box up, no
                    // other modal owns the screen, and nothing is running —
                    // `open_model_dialog` REPLACES the dialog stack, so an
                    // ungated call would clobber a theme picker / confirm
                    // the user has open, and an Esc'd sign-in must not pop
                    // a surprise modal (same principle as the `Started`
                    // gate above). Otherwise the toast carries the hint.
                    let show_models = box_was_open && !self.dialog.visible() && idle_session;
                    // A parked message + picker is the "authorized, now pick
                    // a model" moment: the message must WAIT until the pick
                    // resolves — sending it under the dialog streamed the
                    // reply with the provider default model, and the user's
                    // pick landed too late to matter.
                    self.gateway_model_pick = show_models && self.pending_gateway_message.is_some();
                    if show_models {
                        // Straight into model selection: the gateway just
                        // came online, so its models are the natural next
                        // pick — the SAME box the /models slash command
                        // opens.
                        self.open_model_dialog();
                    }
                    match self.pending_gateway_message.take() {
                        // Picker owns the moment: keep the message parked
                        // (`gateway_model_pick` replays it on resolve) and
                        // tell the user what is happening instead of
                        // silently sending with a model they never picked.
                        Some(msg) if show_models => {
                            self.pending_gateway_message = Some(msg);
                            self.toast_state.show(ToastOptions {
                                title: Some("Charm gateway authorized".into()),
                                message:
                                    "Token saved to your OS keyring. Pick a model to send your message."
                                        .into(),
                                variant: ToastVariant::Success,
                                duration_ms: 6000,
                            });
                        }
                        Some(msg) if idle_session => {
                            self.toast_state.show(ToastOptions {
                                title: Some("Charm gateway authorized".into()),
                                message: "Token saved to your OS keyring. Sending your message."
                                    .into(),
                                variant: ToastVariant::Success,
                                duration_ms: 6000,
                            });
                            self.start_agent_loop(msg);
                        }
                        pending => {
                            // No parked message (or nowhere safe to replay
                            // it): report honestly instead of claiming a
                            // send that never happened.
                            self.pending_gateway_message = pending;
                            let mut message =
                                "Token saved to your OS keyring. Resend your message to use it."
                                    .into();
                            if !show_models {
                                message = format!(
                                    "{message} Pick a model with /models when you're ready."
                                );
                            }
                            self.toast_state.show(ToastOptions {
                                title: Some("Charm gateway authorized".into()),
                                message,
                                variant: ToastVariant::Success,
                                duration_ms: 8000,
                            });
                        }
                    }
                }
                GatewaySignInEvent::Failed(message) => {
                    self.charm_signing_in = false;
                    self.free_gateway_dialog.hide();
                    self.restore_pending_gateway_message();
                    self.toast_state.show(ToastOptions {
                        title: Some("Charm sign-in failed".into()),
                        message,
                        variant: ToastVariant::Error,
                        duration_ms: 10000,
                    });
                }
            }
        }
    }

    /// Offer a free-gateway recommendation after a FAILED send.
    ///
    /// Two offer paths:
    /// 1. The FIRST failure of the install (marker not yet burned) — offer
    ///    whatever the error shape. This is the "at least once during the
    ///    app's life" guarantee: a user whose very first send fails sees the
    ///    recommendation, not just a raw error.
    /// 2. Subsequent auth-shaped failures (401/403, revoked or expired key,
    ///    exhausted quota) — a broken credential is exactly what the free
    ///    gateway fixes. Transient errors (network, 5xx) must not nag.
    ///
    /// A running local server needs no credentials and must never be offered
    /// a cloud replacement because of one bad response. The user's own key
    /// is NEVER deleted here; the Charm flow below clears only its own
    /// tokens.
    pub(in crate::app) fn maybe_offer_gateway_on_error(&mut self, error: &str) {
        if !self.gateway_dialog_route() || self.free_gateway_dialog.visible {
            return;
        }
        if is_local_provider(&self.llm_config.provider) {
            return;
        }
        let auth_failure = is_auth_shaped_error(error);
        let first_offer = !Self::gateway_offer_marker_exists();
        if !first_offer && !auth_failure {
            return;
        }
        // A sign-in still polling owns the flow: never wipe credentials or
        // re-offer underneath it. This must precede the destructive cleanup
        // below — an Esc'd-but-polling flow (the box dismissed, the device
        // transaction alive) would otherwise have its just-saved token
        // deleted by a 401 that arrived mid-poll.
        if self.charm_signing_in {
            return;
        }
        // A stale or revoked Charm token still satisfies has_api_key, which
        // would silence the recommendation forever. An auth-shaped failure
        // clears the dead credential — and the decline latch — so the offer
        // can come back. Worst case is a needless dialog on a transient
        // error, which the user can decline; the alternative is a bricked
        // flow.
        if self.llm_config.provider == "charm" && auth_failure {
            if let Some(env_var) = get_provider_env_var("charm") {
                let _ = super::providers::forget_provider_api_key(env_var);
                let _ = super::providers::forget_provider_api_key(&format!("{env_var}_REFRESH"));
                invalidate_api_key(env_var);
            }
            self.charm_declined = false;
        }
        // `first_offer` widens the keyless-only gate: the one-time offer is
        // about REACHING the user once, not about their key state.
        if let Some(content) = self.gateway_recommendation_content(auth_failure || first_offer) {
            // Burn only when the dialog actually shows — an early return
            // above (decline latch, sign-in in flight) keeps the offer due.
            self.burn_gateway_offer_marker();
            self.free_gateway_dialog.show(content);
        }
    }
}

/// Whether a failed-send error string looks like a credential failure: 401
/// (unauthorized), 403 (forbidden), revoked/expired token, or an exhausted
/// quota — all shapes a free gateway fixes. Heuristic on purpose: it only
/// widens a UX re-offer, never changes protocol behavior.
fn is_auth_shaped_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "401",
        "403 ",
        "unauthorized",
        "forbidden",
        "revoked",
        "invalid api key",
        "invalid_grant",
        "token expired",
        // Quota EXHAUSTION (billing), not a 429 rate limit — the latter is
        // transient and the free gateway does not fix it.
        "insufficient_quota",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

/// Run the Charm device sign-in end to end: start the transaction, surface
/// the verification URL + user code (manual fallback), hand the URL to the
/// system browser, poll until approved and store the resulting tokens in the
/// OS keyring. Runs on the shared agent runtime — keyring access is blocking
/// and must stay off the UI thread.
async fn charm_sign_in(tx: UnboundedSender<GatewaySignInEvent>) {
    let result = charm_sign_in_inner(&tx).await;
    let _ = tx.send(match result {
        Ok(()) => GatewaySignInEvent::Authorized,
        // OAuthError is redacted (no request URLs, no secrets), so its
        // Display form is safe to surface verbatim.
        Err(error) => GatewaySignInEvent::Failed(error),
    });
}

async fn charm_sign_in_inner(tx: &UnboundedSender<GatewaySignInEvent>) -> Result<(), String> {
    let config = CharmOAuthConfig::default();
    let auth = charm_oauth::begin(&config)
        .await
        .map_err(|error| error.to_string())?;
    // Manual fallback FIRST: the browser launch below is best-effort and may
    // never open (headless, SSH, sandboxed). The user needs the URL and the
    // device code regardless.
    let _ = tx.send(GatewaySignInEvent::Started {
        url: auth.verification_url.to_string(),
        user_code: auth.user_code.clone(),
    });
    let url = auth.verification_url.to_string();
    let _ = std::thread::Builder::new()
        .name("oauth-browser".into())
        .spawn(move || {
            let _ = webbrowser::open(url.as_str());
        });
    let tokens = charm_oauth::poll_and_exchange(auth)
        .await
        .map_err(|error| error.to_string())?;

    // Store under the same env-var name the provider table resolves, so
    // `get_api_key("charm")` picks the access token up with no other wiring.
    let env_var = get_provider_env_var("charm").unwrap_or("CHARM_API_KEY");
    if let Err(error) =
        super::providers::save_provider_api_key(env_var, tokens.access_token().expose())
    {
        return Err(format!(
            "could not store the token in the OS keyring: {error}"
        ));
    }
    // Preserve the refresh token under its own entry: the access token WILL
    // expire (and Charm's exchange may even return one born expired when it
    // omits both expiries), and this is the credential a future automatic
    // refresh — or manual recovery — needs.
    if let Some(refresh) = tokens.refresh_token() {
        let _ = super::providers::save_provider_api_key(
            &format!("{env_var}_REFRESH"),
            refresh.expose(),
        );
    }
    // Drop the cached keyring MISS so the very next send resolves the fresh
    // token instead of the remembered absence.
    invalidate_api_key(env_var);
    Ok(())
}
