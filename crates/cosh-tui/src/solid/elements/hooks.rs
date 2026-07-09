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
// TODO: wire to renderer key input once Renderer exposes a key event stream.
pub fn on_keyboard(_renderer: &impl HasKeyInput, _callback: impl Fn(&str) + 'static) {}

/// Register a callback for paste events.
// TODO: wire to renderer paste events once Renderer exposes paste support.
pub fn on_paste(_renderer: &impl HasKeyInput, _callback: impl Fn(&str) + 'static) {}

/// Register a callback for focus events.
// TODO: wire to renderer focus events once Renderer tracks focused renderable.
pub fn on_focus<B: Backend>(_renderer: &Renderer<B>, _callback: impl Fn() + 'static) {}

/// Register a callback for blur events.
// TODO: wire to renderer blur events once Renderer tracks focused renderable.
pub fn on_blur<B: Backend>(_renderer: &Renderer<B>, _callback: impl Fn() + 'static) {}

/// Register a callback for selection changes.
// TODO: wire to renderer selection events once Renderer tracks selection state.
pub fn on_selection<B: Backend>(_renderer: &Renderer<B>, _callback: impl Fn(&Selection) + 'static) {
}

/// Placeholder for animation timeline.
pub struct Timeline;

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Placeholder trait for types that have key input.
pub trait HasKeyInput {
    fn on_key(&mut self, _key: &str) {}
}
