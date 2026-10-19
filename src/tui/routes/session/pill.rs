use super::*;

impl SessionView {
    /// Whether the pill's slide/fade animation is mid-flight (drives the
    /// live-render gate in `App::render` so the animation completes even with
    /// no other live source and no pending input events).
    pub fn pill_animating(&self) -> bool {
        self.pill.animating()
    }
}

impl SessionView {
    /// Animation progress of the pill (0 hidden … 1 settled). Exposed for
    /// tests and for callers reasoning about the live-render gate.
    pub fn pill_progress(&self) -> f32 {
        self.pill.progress()
    }
}

impl SessionView {
    /// Drop the pill's hit rect and park the animation at rest (e.g. while an
    /// inline dialog replaces the prompt). See `ScrollPill::clear`.
    pub fn clear_pill(&mut self) {
        self.pill.clear();
    }
}

impl SessionView {
    /// Draw the scroll-to-bottom pill — a small `⮟` button centered just above
    /// the session prompt box — whenever the viewport is away from the latest
    /// content, and advance its slide/opacity animation.
    ///
    /// Visibility is purely distance-based (`total_height`/`visible_height`
    /// are the frame's settled layout values): a user who is following the
    /// bottom — sticky or not — never sees it, so the normal auto-scroll
    /// behavior is untouched. `App::render` calls this after every other
    /// inline layer so the pill is never buried under the prompt, pending
    /// queues, dialogs or the slash menu.
    pub fn render_pill_overlay(
        &mut self,
        buf: &mut Buffer,
        anchor_y: u16,
        area: Rect,
        theme: &Theme,
        delta_time: f64,
    ) {
        let viewport_h = i32::from(area.height);
        let max_scroll = (self.total_height - viewport_h).max(0);
        self.pill.render(
            buf,
            self.scroll_y,
            max_scroll,
            anchor_y,
            area,
            theme,
            delta_time,
        );
    }
}
