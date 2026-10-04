use super::{App, AppMode};

use crate::routes::session::free_gateway_recommendation::{GatewayOffer, OFFERS};
use crate::ui::toast::{ToastOptions, ToastVariant};

use cosh_sdk::connector::copilot_oauth;
use cosh_sdk::connector::openai_compatible::charm_oauth::{self, CharmOAuthConfig};
use cosh_sdk::connector::{
    get_provider_env_var, has_api_key, invalidate_api_key, is_local_provider,
};

use tokio::sync::mpsc::UnboundedSender;

/// Messages from a background gateway device sign-in into the UI thread.
/// Every variant carries the provider id it signed in for — the UI thread
/// stays stateless about WHICH flow is running (no extra App field to
/// forget to reset), and the `Authorized` arm switches the live config to
/// exactly the provider whose token just landed in the keyring.
#[derive(Debug)]
pub(super) enum GatewaySignInEvent {
    /// The device transaction started: hold the verification URL and user
    /// code on the App so they stay VISIBLE (spinner line) for as long as
    /// the sign-in runs — a transient toast is not enough; the Charm page
    /// asks for the code after login, minutes later.
    Started {
        provider: &'static str,
        url: String,
        user_code: String,
    },
    /// The token is stored in the OS keyring; the parked message can be
    /// replayed through the newly authorized gateway.
    Authorized { provider: &'static str },
    /// Sign-in failed, was denied or crashed; the parked message goes back
    /// to the prompt untouched.
    Failed {
        provider: &'static str,
        error: String,
    },
}

/// The display name of a provider id, for toasts. Known providers map to
/// their product name; unknown ones surface the raw id (never a blank).
fn provider_display(provider: &str) -> String {
    match provider {
        "charm" => "Charm".into(),
        "copilot" => "GitHub Copilot".into(),
        other => other.into(),
    }
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

    /// Whether the one-time gateway offer was already answered on this
    /// install — and, when it was, WHICH answer was given. The marker file
    /// stores the answer itself (`declined` / `authorized`): a plain
    /// existence check cannot distinguish "never ask again" from "already
    /// using the gateway, may re-offer if its credential dies".
    ///
    /// The marker is INSTALL-WIDE and provider-neutral by contract: one
    /// offer per install, whichever gateway the user picked. Authorizing
    /// GitHub Copilot mutes the first-run offer for Charm too — the tabs
    /// exist so the user picks freely at offer time, not so the offer
    /// replays once per gateway. The `gateway_died` gate re-opens the offer
    /// for ANY offered provider whose credential dies (the current
    /// provider's sign-in is the repair). One accepted edge: an install
    /// that authorized a gateway but then points a KEYLESS non-gateway
    /// provider gets raw send failures, not a re-offer — the one-time
    /// contract wins over re-prompting.
    const GATEWAY_MARKER_DECLINED: &[u8] = b"declined";
    const GATEWAY_MARKER_AUTHORIZED: &[u8] = b"authorized";

    fn gateway_offer_marker() -> Option<Vec<u8>> {
        let dirs = directories::BaseDirs::new()?;
        std::fs::read(dirs.data_local_dir().join("cosh/gateway-recommendation")).ok()
    }

    fn gateway_offer_marker_exists() -> bool {
        Self::gateway_offer_marker().is_some()
    }

    /// Burn the one-time offer marker WITH the answer that justifies it.
    /// Best-effort: a read-only disk just means the offer may reappear on
    /// the next launch.
    fn burn_gateway_offer_marker(&self, answer: &[u8]) {
        if let Some(dirs) = directories::BaseDirs::new() {
            let marker = dirs.data_local_dir().join("cosh/gateway-recommendation");
            let _ = std::fs::create_dir_all(marker.parent().unwrap_or(&marker));
            let _ = std::fs::write(&marker, answer);
        }
    }

    /// Which gateway recommendations to offer (one per tab), if any.
    ///
    /// Two offer paths: (1) the active provider is keyless — the primary
    /// first-run scenario; (2) the provider HAS credentials but just failed
    /// with an auth-shaped error (`auth_failure`) — a broken or revoked key
    /// makes the free gateway the sensible fallback. The user's own key is
    /// NEVER touched on path 2; a sign-in only replaces the tokens of the
    /// provider it signed in to.
    pub(in crate::app) fn gateway_recommendation_offers(
        &self,
        auth_failure: bool,
    ) -> Option<&'static [GatewayOffer]> {
        if self.gateway_signing_in || self.gateway_declined {
            return None;
        }
        // The one-time offer is ONE per install: an answered dialog (decline
        // or opt-in) burned the marker, and it must never come back — not
        // on the next launch, not on a later error. Auth-shaped failures
        // re-offer ONLY when the gateway credential itself is dead (cleared
        // below); a healthy install stays silent.
        if Self::gateway_offer_marker_exists() && !auth_failure {
            return None;
        }
        let provider = &self.llm_config.provider;
        if is_local_provider(provider) {
            return None;
        }
        if !has_api_key(provider) || auth_failure {
            return Some(OFFERS);
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
    /// parked message back. Opt-in: run the ACTIVE tab's device sign-in in
    /// the background — the parked message replays once the token is in the
    /// keyring, or goes back untouched on failure.
    pub(in crate::app) fn commit_gateway_choice(&mut self) {
        let opted_in = self.free_gateway_dialog.selected == 0;
        if !opted_in {
            self.free_gateway_dialog.hide();
            self.gateway_declined = true;
            // The decline is a FINAL answer, not a per-run preference: burn
            // the one-time marker so the offer never comes back after an
            // app restart (the in-memory latch alone dies with the process —
            // that regression let the box reappear on every launch).
            self.burn_gateway_offer_marker(Self::GATEWAY_MARKER_DECLINED);
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
        // the options rows give way to a hint) and start the device flow
        // for the ACTIVE tab's provider. Hiding here would visibly close
        // and reopen it a frame later.
        let Some(offer) = self.free_gateway_dialog.active_offer() else {
            // Single-content mode (no offers): nothing to dispatch on —
            // restore the message instead of silently dropping it.
            self.free_gateway_dialog.hide();
            self.restore_pending_gateway_message();
            return;
        };
        self.free_gateway_dialog.begin_connecting();
        self.gateway_signing_in = true;
        let provider = offer.provider;
        let tx = self.gateway_event_tx.clone();
        // The shared agent runtime: the UI thread has no ambient tokio
        // context, and the sign-in must survive any turn teardown.
        let runtime = super::agent_loop::AGENT_RUNTIME
            .get_or_init(|| tokio::runtime::Runtime::new().expect("shared agent runtime"));
        runtime.spawn(async move {
            // The inner spawn converts a panic inside the sign-in future into
            // a JoinError — and therefore a Failed event — instead of leaving
            // `gateway_signing_in` stuck true and the parked message lost.
            let inner_tx = tx.clone();
            if let Err(join_error) = tokio::spawn(gateway_sign_in(provider, inner_tx)).await {
                let _ = tx.send(GatewaySignInEvent::Failed {
                    provider,
                    error: format!("the sign-in task crashed: {join_error}"),
                });
            }
        });
    }

    /// Drain gateway sign-in outcomes from the shared runtime. Called once
    /// per frame, next to the update pump.
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
                        title: Some("Gateway authorized".into()),
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
                        title: Some("Gateway authorized".into()),
                        message: "Resend your message to use the gateway.".into(),
                        variant: ToastVariant::Success,
                        duration_ms: 8000,
                    });
                }
            }
        }
        while let Ok(event) = self.gateway_event_rx.try_recv() {
            match event {
                GatewaySignInEvent::Started {
                    provider: _,
                    url,
                    user_code,
                } => {
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
                GatewaySignInEvent::Authorized { provider } => {
                    self.gateway_signing_in = false;
                    // The offer was ANSWERED (opt-in → success): record it
                    // so a later boot knows the install already uses the
                    // gateway — re-offers are then reserved for a DEAD
                    // credential, never for a transient error.
                    self.burn_gateway_offer_marker(Self::GATEWAY_MARKER_AUTHORIZED);
                    // Whether the user kept the sign-in box up: an Esc
                    // mid-flow asked the screen back — the outcome must not
                    // steal it again with a fresh modal one event later.
                    let box_was_open = self.free_gateway_dialog.visible;
                    self.free_gateway_dialog.hide();
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
                    // Route the (re)played message through the newly
                    // authorized gateway (provider default model) — but
                    // ONLY when this completion actually takes over the
                    // UI right now. The sign-in runs in the background: a
                    // user who Esc'd the box and picked another model (or
                    // another session) in the meantime must keep that
                    // choice; unconditionally rewriting the live config
                    // here clobbered it and left stale reasoning applied.
                    let taking_over =
                        idle_session && (show_models || self.pending_gateway_message.is_some());
                    if taking_over {
                        self.llm_config.provider = provider.into();
                        self.llm_config.model = None;
                        // The old reasoning belongs to the previous model —
                        // a fresh gateway default starts unscoped.
                        self.llm_config.reasoning = None;
                    }
                    // A parked message + picker is the "authorized, now pick
                    // a model" moment: the message must WAIT until the pick
                    // resolves — sending it under the dialog streamed the
                    // reply with the provider default model, and the user's
                    // pick landed too late to matter.
                    self.gateway_model_pick = show_models && self.pending_gateway_message.is_some();
                    let title = format!("{} authorized", provider_display(provider));
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
                                title: Some(title.clone()),
                                message:
                                    "Token saved to your OS keyring. Pick a model to send your message."
                                        .into(),
                                variant: ToastVariant::Success,
                                duration_ms: 6000,
                            });
                        }
                        Some(msg) if idle_session => {
                            self.toast_state.show(ToastOptions {
                                title: Some(title.clone()),
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
                                title: Some(title),
                                message,
                                variant: ToastVariant::Success,
                                duration_ms: 8000,
                            });
                        }
                    }
                }
                GatewaySignInEvent::Failed { provider, error } => {
                    self.gateway_signing_in = false;
                    self.free_gateway_dialog.hide();
                    self.restore_pending_gateway_message();
                    self.toast_state.show(ToastOptions {
                        title: Some(format!("{} sign-in failed", provider_display(provider))),
                        message: error,
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
    /// 2. A DEAD gateway credential on an install already using one of the
    ///    offered gateways — a revoked/expired token is exactly what
    ///    re-running that provider's sign-in fixes, so the offer may come
    ///    back ONLY for that. Transient errors (network, 5xx) must not nag,
    ///    and heuristics NEVER delete stored credentials: a "billing"
    ///    substring in an error must not wipe a valid keyring entry (that
    ///    regression silently destroyed working Charm credentials).
    ///    Re-running the opt-in overwrites the keyring entries — that IS
    ///    the recovery path, destructive cleanup is not.
    ///
    /// A running local server needs no credentials and must never be offered
    /// a cloud replacement because of one bad response. The user's own key
    /// is NEVER deleted here; a sign-in's save step replaces only its own
    /// provider's tokens.
    pub(in crate::app) fn maybe_offer_gateway_on_error(&mut self, error: &str) {
        if !self.gateway_dialog_route() || self.free_gateway_dialog.visible {
            return;
        }
        if is_local_provider(&self.llm_config.provider) {
            return;
        }
        let auth_failure = is_auth_shaped_error(error);
        // An already-answered install stays quiet UNLESS this install uses
        // one of the offered gateways itself (marker = authorized) AND the
        // error looks like its credential died — the one state re-running
        // that sign-in fixes.
        let marker = Self::gateway_offer_marker();
        let gateway_died = marker.as_deref() == Some(Self::GATEWAY_MARKER_AUTHORIZED)
            && OFFERS
                .iter()
                .any(|offer| offer.provider == self.llm_config.provider)
            && auth_failure;
        if marker.is_some() && !gateway_died {
            return;
        }
        // A sign-in still polling owns the flow: never re-offer underneath
        // it — an Esc'd-but-polling flow (the box dismissed, the device
        // transaction alive) would otherwise get its flow disturbed by a
        // 401 that arrived mid-poll.
        if self.gateway_signing_in {
            return;
        }
        // `first_offer` widens the keyless-only gate: the one-time offer is
        // about REACHING the user once, not about their key state. A dead
        // gateway credential re-opens the offer through the same gate.
        let force_open = auth_failure && marker.is_none() || gateway_died;
        if let Some(offers) = self.gateway_recommendation_offers(force_open) {
            // The marker burns on the ANSWER (decline → `declined`, opt-in
            // success → `authorized`), never on merely showing the box: a
            // dialog closed by Esc/disk-error leaves the offer due.
            self.free_gateway_dialog.show_offers(offers);
            // A dead-credential re-offer lands on the tab of the gateway
            // that just died — the one the user needs to re-run — not on
            // whichever tab happens to be first.
            if gateway_died {
                self.free_gateway_dialog
                    .select_provider_tab(&self.llm_config.provider);
            }
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

/// Run the ACTIVE gateway's device sign-in end to end: start the
/// transaction, surface the verification URL + user code (manual fallback),
/// hand the URL to the system browser, poll until approved and store the
/// resulting tokens in the OS keyring under the provider's own env-var
/// name. Runs on the shared agent runtime — keyring access is blocking and
/// must stay off the UI thread.
async fn gateway_sign_in(provider: &'static str, tx: UnboundedSender<GatewaySignInEvent>) {
    let result = gateway_sign_in_inner(provider, &tx).await;
    let _ = tx.send(match result {
        // OAuthError is redacted (no request URLs, no secrets), so its
        // Display form is safe to surface verbatim.
        Err(error) => GatewaySignInEvent::Failed { provider, error },
        Ok(()) => GatewaySignInEvent::Authorized { provider },
    });
}

async fn gateway_sign_in_inner(
    provider: &'static str,
    tx: &UnboundedSender<GatewaySignInEvent>,
) -> Result<(), String> {
    let tokens = match provider {
        "charm" => charm_device_sign_in(tx).await?,
        "copilot" => copilot_device_sign_in(tx).await?,
        other => return Err(format!("no device sign-in flow for provider {other:?}")),
    };

    // Store under the same env-var name the provider table resolves, so
    // `get_api_key(provider)` picks the access token up with no other wiring.
    let env_var = match get_provider_env_var(provider) {
        Some(env_var) => env_var.to_string(),
        None => format!("{}_API_KEY", provider.to_uppercase()),
    };
    if let Err(error) =
        super::providers::save_provider_api_key(&env_var, tokens.access_token().expose())
    {
        return Err(format!(
            "could not store the token in the OS keyring: {error}"
        ));
    }
    // Preserve the refresh token under its own entry: the access token WILL
    // expire (and Charm's exchange may even return one born expired when it
    // omits both expiries), and this is the credential a future automatic
    // refresh — or manual recovery — needs. A failed write is not fatal
    // (the access token works now) but must not vanish silently: without
    // the refresh entry the session is doomed to a manual re-sign-in with
    // no diagnostic anywhere.
    if let Some(refresh) = tokens.refresh_token()
        && let Err(error) =
            super::providers::save_provider_api_key(&format!("{env_var}_REFRESH"), refresh.expose())
    {
        log::warn!(
            "gateway sign-in: could not store the {env_var}_REFRESH token in the OS keyring: {error}"
        );
    }
    // Drop the cached keyring MISS so the very next send resolves the fresh
    // token instead of the remembered absence.
    invalidate_api_key(&env_var);
    Ok(())
}

/// Charm's device sign-in. Charm's device protocol is its own (JSON bodies,
/// an exchange step) — the provider-specific adapter owns it; this only
/// surfaces the codes and drives the browser.
async fn charm_device_sign_in(
    tx: &UnboundedSender<GatewaySignInEvent>,
) -> Result<cosh_sdk::connector::oauth::TokenSet, String> {
    let config = CharmOAuthConfig::default();
    let auth = charm_oauth::begin(&config)
        .await
        .map_err(|error| error.to_string())?;
    announce_device_transaction(tx, "charm", auth.verification_url.as_str(), &auth.user_code);
    let tokens = charm_oauth::poll_and_exchange(auth)
        .await
        .map_err(|error| error.to_string())?;
    Ok(tokens)
}

/// GitHub Copilot's device sign-in over the SDK's generic RFC 8628 device
/// grant pinned to GitHub's endpoints (see `connector::copilot::oauth`).
async fn copilot_device_sign_in(
    tx: &UnboundedSender<GatewaySignInEvent>,
) -> Result<cosh_sdk::connector::oauth::TokenSet, String> {
    let config = copilot_oauth::CopilotOAuthConfig::default();
    let auth = config.begin().await.map_err(|error| error.to_string())?;
    announce_device_transaction(
        tx,
        "copilot",
        auth.verification_uri.as_str(),
        &auth.user_code,
    );
    copilot_oauth::poll_and_exchange(auth)
        .await
        .map_err(|error| error.to_string())
}

/// The shared visible-start of every device flow: the box shows the code +
/// URL (manual fallback FIRST — the browser launch below is best-effort and
/// may never open on headless/SSH/sandboxed setups), then the system
/// browser opens the verification page.
fn announce_device_transaction(
    tx: &UnboundedSender<GatewaySignInEvent>,
    provider: &'static str,
    verification_url: &str,
    user_code: &str,
) {
    let _ = tx.send(GatewaySignInEvent::Started {
        provider,
        url: verification_url.to_string(),
        user_code: user_code.to_string(),
    });
    let url = verification_url.to_string();
    let _ = std::thread::Builder::new()
        .name("oauth-browser".into())
        .spawn(move || {
            let _ = webbrowser::open(url.as_str());
        });
}
