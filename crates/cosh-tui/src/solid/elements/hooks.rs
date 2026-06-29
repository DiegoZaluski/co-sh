use crate::core::renderer::Renderer;
use crate::core::types::Selection;
use ratatui::prelude::Backend;

/// Register a callback for terminal resize events.
pub fn on_resize<B: Backend + 'static>(
    renderer: &Renderer<B>,
    mut callback: impl FnMut(u16, u16) + 'static,
) {
    let w = renderer.config().width;
    let h = renderer.config().height;
    callback(w, h);
}

/// Register a callback for keyboard events.
pub fn _on_keyboard(
    _renderer: &impl HasKeyInput,
    _callback: impl Fn(&str) + 'static,
) {
}

/// Register a callback for paste events.
pub fn _on_paste(
    _renderer: &impl HasKeyInput,
    _callback: impl Fn(&str) + 'static,
) {
}

/// Register a callback for focus events.
pub fn _on_focus<B: Backend>(
    _renderer: &Renderer<B>,
    _callback: impl Fn() + 'static,
) {
}

/// Register a callback for blur events.
pub fn _on_blur<B: Backend>(
    _renderer: &Renderer<B>,
    _callback: impl Fn() + 'static,
) {
}

/// Register a callback for selection changes.
pub fn _on_selection<B: Backend>(
    _renderer: &Renderer<B>,
    _callback: impl Fn(&Selection) + 'static,
) {
}

/// Placeholder for animation timeline.
pub struct _Timeline;

impl Default for _Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl _Timeline {
    #[must_use]
    pub fn new() -> Self {
        _Timeline
    }
}

/// Placeholder trait for types that have key input.
pub trait HasKeyInput {
    fn on_key(&mut self, _key: &str) {}
}
