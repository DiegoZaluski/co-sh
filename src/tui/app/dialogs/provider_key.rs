use super::super::App;
use super::super::providers::forget_provider_api_key;
use crossterm::event::KeyCode;

use crate::ui::dialogs::DialogType;
use crate::ui::toast::{ToastOptions, ToastVariant};

impl App {
    pub(in crate::app) fn is_provider_key_choice_visible(&self) -> bool {
        self.dialog.visible()
            && matches!(
                self.dialog.current().map(|d| &d.dialog_type),
                Some(DialogType::ProviderKeyChoice { .. })
            )
    }

    /// Sovereign modal while the forget/overwrite picker is open: Left/Right
    /// (and Up/Down) switch the option, Enter resolves it, Esc cancels and
    /// every other key is swallowed.
    pub(in crate::app) fn handle_provider_key_choice_key(&mut self, key: KeyCode) -> bool {
        if !self.is_provider_key_choice_visible() {
            return false;
        }
        match key {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                if let Some(d) = self.dialog.current_mut() {
                    d.selected ^= 1;
                }
                true
            }
            KeyCode::Enter => {
                self.resolve_provider_key_choice();
                true
            }
            KeyCode::Esc => {
                self.dialog.pop();
                true
            }
            _ => true,
        }
    }

    /// Apply the currently selected option: "Forget key" pushes the standard
    /// destructive-action Confirm (same box as session delete) on top, so the
    /// keyring deletion only happens after an explicit confirmation;
    /// "Overwrite key" swaps this picker for the plain API-key input.
    pub(in crate::app) fn resolve_provider_key_choice(&mut self) {
        let Some(d) = self.dialog.current() else {
            return;
        };
        let selected = d.selected;
        let Some((provider, env_var)) = (match &d.dialog_type {
            DialogType::ProviderKeyChoice { provider, env_var } => {
                Some((provider.clone(), env_var.clone()))
            }
            _ => None,
        }) else {
            return;
        };

        if selected == 1 {
            // Overwrite: replace the picker with the regular key input.
            self.dialog.replace(DialogType::ApiKeyInput {
                provider,
                env_var,
                input: String::new(),
                cursor_pos: 0,
            });
        } else {
            // Forget: confirm first — the keyring delete is destructive and
            // irreversible (the user must re-enter the key afterwards).
            self.dialog.show(DialogType::Confirm {
                message: format!("Forget the API key for {provider}?"),
            });
        }
    }

    /// Whether the current Confirm dialog sits on top of a ProviderKeyChoice
    /// dialog — i.e. it is asking to forget a provider API key. Detected from
    /// the dialog stack so no pending state can go stale.
    fn confirm_over_provider_key_choice(&self) -> Option<(String, String)> {
        let n = self.dialog.stack.len();
        if n < 2 {
            return None;
        }
        match &self.dialog.stack[n - 2].dialog_type {
            DialogType::ProviderKeyChoice { provider, env_var } => {
                Some((provider.clone(), env_var.clone()))
            }
            _ => None,
        }
    }

    /// Resolve the "forget key" confirmation: delete the provider's key from
    /// the OS keyring, invalidate the in-process cache and the model cache,
    /// then pop both the Confirm and the picker beneath it. Returns `false`
    /// (changing nothing) when the open Confirm is not a forget-key one, so
    /// session-delete / RAG-delete / quit confirms are unaffected.
    pub(in crate::app) fn resolve_forget_key_confirmation(&mut self) -> bool {
        let Some((provider, env_var)) = self.confirm_over_provider_key_choice() else {
            return false;
        };

        match forget_provider_api_key(&env_var) {
            Ok(()) => {
                self.toast_state.show(ToastOptions {
                    title: Some("Key forgotten".to_string()),
                    message: format!("The API key for {provider} was removed from the OS keyring."),
                    variant: ToastVariant::Success,
                    duration_ms: 6000,
                });
            }
            Err(e) => {
                self.toast_state.show(ToastOptions {
                    title: Some("Key not removed".to_string()),
                    message: format!("Failed to remove the API key from the OS keyring: {e}"),
                    variant: ToastVariant::Error,
                    duration_ms: 6000,
                });
            }
        }
        // Invalidate model cache for this provider so the next dialog open
        // fetches fresh models without the removed key.
        self.model_cache.invalidate(&provider);

        // Pop the Confirm, then the forget/overwrite picker beneath it.
        self.dialog.pop();
        self.dialog.pop();
        true
    }
}
