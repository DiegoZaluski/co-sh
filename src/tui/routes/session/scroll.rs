use super::*;

impl SessionView {
    // ── Scroll-to-bottom pill tuning lives in `component::scroll_pill` ──────────

    pub fn new() -> Self {
        Self {
            scroll_y: 0,
            tool_state: ToolRenderState::new(),
            text_regions: Vec::new(),
            drag_selection: None,
            is_auto_scrolling: false,
            auto_scroll_speed: 0.0,
            auto_scroll_accumulator: 0.0,
            session_area: None,
            auto_scroll_threshold: 3,
            auto_scroll_speed_slow: 6.0,
            auto_scroll_speed_medium: 36.0,
            auto_scroll_speed_fast: 72.0,
            total_height: 0,
            visible_height: 0,
            actual_total_height: 0,
            mouse_down_scroll_y: 0,
            selection_anchor_content_y: 0,
            selection_focus_content_y: 0,
            has_manual_scroll: false,
            is_sticky_bottom: true,
            last_content_height: 0,
            cached_total_height: 0,
            is_applying_sticky_scroll: false,
            pill: ScrollPill::new(),
            scroll_accumulator_y: 0.0,
            scroll_accel: 3.0,
            msg_height_cache: Vec::new(),
            prefix_y: Vec::new(),
            part_heights_cache: Vec::new(),
            cache_max_w: 0,
            cache_config_token: 0,
            last_msg_change_token: 0,
            msg_change_tokens: Vec::new(),
            prev_mutable_msgs: Vec::new(),
            text_regions_gen: 0,
            msg_cache_tokens: Vec::new(),
            msg_cache_cells: Vec::new(),
            msg_cache_w: Vec::new(),
            msg_cache_h: Vec::new(),
            msg_cache_text_regions: Vec::new(),
            scratch: None,
            spinner_key_scratch: String::new(),
            render_frame: 0,
            msg_cache_last_used: Vec::new(),
            msg_cache_bytes: 0,
            streaming_msg: None,
            last_streamed_msg: None,
            width_cache_snapshot: None,
            hovered_msg_idx: None,
            pending_message_action: None,
            last_session_id: None,
            last_tool_state_version: 0,
        }
    }
}

impl SessionView {
    // ── Scroll control (port of OpenCode's ScrollBox) ──────────────────────────

    /// Accelerated scroll (for mouse wheel). Multiplies delta by `scroll_accel`.
    /// Uses fractional accumulator for smooth scrolling.
    /// Mirrors OpenCode's `onMouseEvent` for scroll type.
    pub fn scroll_by(&mut self, delta: f64) {
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);

        let scroll_amount = delta * self.scroll_accel;
        self.scroll_accumulator_y += scroll_amount;
        let int_scroll = self.scroll_accumulator_y.trunc() as i32;
        if int_scroll != 0 {
            self.scroll_accumulator_y -= int_scroll as f64;
            self.scroll_y = (self.scroll_y + int_scroll).clamp(0, max_scroll);
        }

        self.sync_manual_scroll_state();
    }
}

impl SessionView {
    /// Raw scroll (for keyboard / programmatic). No acceleration, uses accumulator.
    /// Mirrors OpenCode's `scrollBy` + `handleKeyPress` pattern.
    pub fn scroll_by_raw(&mut self, delta: f64) {
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);

        self.scroll_accumulator_y += delta;
        let int_scroll = self.scroll_accumulator_y.trunc() as i32;
        if int_scroll != 0 {
            self.scroll_accumulator_y -= int_scroll as f64;
            self.scroll_y = (self.scroll_y + int_scroll).clamp(0, max_scroll);
        }

        self.sync_manual_scroll_state();
    }
}

impl SessionView {
    /// Reset the scroll accumulator (called after keyboard scroll, like OpenCode).
    pub fn reset_scroll_accumulator(&mut self) {
        self.scroll_accumulator_y = 0.0;
    }
}

impl SessionView {
    /// Absolute scroll to position. Mirrors OpenCode's `scrollTo`.
    pub fn scroll_to(&mut self, position: i32) {
        let max_scroll = (self.actual_total_height - self.visible_height).max(0);
        self.scroll_y = position.clamp(0, max_scroll);
        self.scroll_accumulator_y = 0.0;
        self.sync_manual_scroll_state();
    }
}

impl SessionView {
    /// Scroll to bottom. Mirrors OpenCode's `toBottom()`:
    ///   `scroll.scrollTo(scroll.scrollHeight)`
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
        self.scroll_y = max_scroll;
        self.scroll_accumulator_y = 0.0;
        self.has_manual_scroll = false;
        self.is_sticky_bottom = true;
    }
}

impl SessionView {
    /// Jump straight back to the latest content and re-engage the sticky
    /// auto-follow — the action behind the scroll-to-bottom pill (and the
    /// keyboard ScrollToBottom binding). Delegates to [`Self::scroll_to_bottom`]:
    /// aiming at the cached bottom avoids scroll jumping mid-stream, and
    /// clearing `has_manual_scroll` is what makes `recalculate_bar_props`
    /// keep following the agent's output again.
    pub fn jump_to_live(&mut self) {
        self.scroll_to_bottom();
    }
}

impl SessionView {
    /// Sync manual scroll state. Mirrors OpenCode's `syncManualScrollState()`.
    /// Sets `hasManualScroll = hasScrollableContent && !isAtStickyPosition()`.
    /// The `is_applying_sticky_scroll` guard prevents this from being called
    /// during `applyStickyStart`/`recalculateBarProps`.
    pub(super) fn sync_manual_scroll_state(&mut self) {
        if self.is_applying_sticky_scroll {
            return;
        }

        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
        let has_scrollable_content = max_scroll > 1;

        self.has_manual_scroll = has_scrollable_content && !self.is_at_sticky_position();

        self.update_sticky_state();
    }
}

impl SessionView {
    /// Update sticky state flags. Mirrors OpenCode's `updateStickyState()`.
    pub(super) fn update_sticky_state(&mut self) {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);

        if self.scroll_y <= 0 {
            self.is_sticky_bottom = false;
        } else if self.scroll_y >= max_scroll {
            self.is_sticky_bottom = true;
        } else {
            self.is_sticky_bottom = self.scroll_y >= max_scroll;
        }
    }
}

impl SessionView {
    /// Check if at sticky position. Mirrors OpenCode's `isAtStickyPosition()`.
    /// For "bottom": `scrollTop >= maxScrollTop` (accepts >=, not strict equality).
    pub(super) fn is_at_sticky_position(&self) -> bool {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);

        // stickyStart = "bottom"
        if max_scroll <= 0 {
            return true;
        }
        self.scroll_y >= max_scroll
    }
}

impl SessionView {
    /// Check if at sticky re-engage point. Mirrors OpenCode's `isAtStickyReengagePoint()`.
    /// For "bottom": `maxScrollTop > 0 && scrollTop >= maxScrollTop - 1`
    pub fn is_at_bottom(&self) -> bool {
        let max_scroll = (self.cached_total_height - self.visible_height).max(0);
        if max_scroll <= 0 {
            return true;
        }
        self.scroll_y >= max_scroll.saturating_sub(1)
    }
}

impl SessionView {
    /// Recalculate bar props (called when content/viewport size changes).
    /// Mirrors OpenCode's `recalculateBarProps()` which:
    /// 1. Sets `is_applying_sticky_scroll = true`
    /// 2. If `!hasManualScroll` → `applyStickyStart(stickyStart)`
    /// 3. If `hasManualScroll && isAtStickyReengagePoint()` → re-engage
    /// 4. Updates `last_content_height`
    pub(super) fn recalculate_bar_props(&mut self, total_height: i32, visible_height: i32) {
        let was_applying = self.is_applying_sticky_scroll;
        self.is_applying_sticky_scroll = true;

        // Use cached_total_height for max_scroll during streaming to prevent
        // scroll jumping. The cached height is the stable source of truth for
        // layout calculations, while actual_total_height is only used for
        // scrollbar sizing and may fluctuate slightly during rendering.
        let new_max_scroll = (total_height - visible_height).max(0);

        if !self.has_manual_scroll {
            // No manual scroll → apply sticky start
            self.scroll_y = new_max_scroll;
            self.is_sticky_bottom = true;
        } else if self.is_at_bottom() && new_max_scroll > 0 {
            // User scrolled back to bottom during streaming → re-engage sticky
            self.has_manual_scroll = false;
            self.scroll_y = new_max_scroll;
            self.is_sticky_bottom = true;
        }

        self.is_applying_sticky_scroll = was_applying;
    }
}
