use super::{App, AppMode};

use crate::routes::session::free_gateway_recommendation::GatewayRecommendationContent;
use crate::ui::toast::{ToastOptions, ToastVariant};

use cosh_sdk::connector::is_local_provider;

impl App {
    /// Placeholder: no free gateway is currently recommended.
    ///
    /// To recommend a new gateway in the future, implement the credential
    /// check here and return its content from
    /// [`gateway_recommendation_content`](Self::gateway_recommendation_content).
    pub(in crate::app) fn needs_gateway_recommendation(&self) -> bool {
        false
    }

    /// Determine which gateway recommendation content to offer.
    ///
    /// Placeholder for future gateways — currently returns `None`, so no
    /// recommendation dialog is ever shown. Add a new
    /// `GatewayRecommendationContent` const under
    /// `routes::session::free_gateway_recommendation` and return it here.
    pub(in crate::app) fn gateway_recommendation_content(
        &self,
    ) -> Option<&'static GatewayRecommendationContent> {
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

    /// Enter on the free gateway recommendation dialog: record the user's
    /// answer, then replay the parked message when opting in.
    ///
    /// Placeholder implementation — without a configured gateway there is no
    /// reroute; the choice only closes the dialog and restores the message.
    pub(in crate::app) fn commit_gateway_choice(&mut self) {
        let opted_in = self.free_gateway_dialog.selected == 0;
        self.free_gateway_dialog.hide();

        let pending = self.pending_gateway_message.take();
        if opted_in {
            match pending {
                Some(msg) => self.start_agent_loop(msg),
                None => self.toast_state.show(ToastOptions {
                    title: Some("Free gateway enabled".into()),
                    message: "Pick a model and send your message.".into(),
                    variant: ToastVariant::Info,
                    duration_ms: 6000,
                }),
            }
        } else {
            self.restore_pending_gateway_message();
            self.toast_state.show(ToastOptions {
                title: Some("Free gateway declined".into()),
                message: "Configure a provider API key (ADD Provider) to use its models.".into(),
                variant: ToastVariant::Warning,
                duration_ms: 8000,
            });
        }
    }

    /// Placeholder: offer a free-gateway recommendation after a FAILED send.
    ///
    /// Currently a no-op — wire a future gateway's error markers here once
    /// `gateway_recommendation_content()` returns `Some`.
    pub(in crate::app) fn maybe_offer_gateway_on_error(&mut self, _error: &str) {
        if !matches!(self.mode(), AppMode::Session) || self.free_gateway_dialog.visible {
            return;
        }
        // A running local server needs no credentials and must never be
        // offered a cloud replacement because of one bad response.
        if is_local_provider(&self.llm_config.provider) {
            return;
        }
        if let Some(content) = self.gateway_recommendation_content() {
            self.free_gateway_dialog.show(content);
        }
    }
}
