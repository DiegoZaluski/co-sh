use super::*;

impl SessionView {
    pub(super) fn get_auto_scroll_speed(&self, mouse_y: u16) -> f64 {
        let Some((_sx, sy, _sx2, sy2)) = self.session_area else {
            return 0.0;
        };
        let relative_y = mouse_y.saturating_sub(sy);
        let height = sy2.saturating_sub(sy);

        let dist_to_top = relative_y;
        let dist_to_bottom = height.saturating_sub(relative_y);
        let min_distance = dist_to_top.min(dist_to_bottom);

        if min_distance <= 1 {
            self.auto_scroll_speed_fast
        } else if min_distance <= 2 {
            self.auto_scroll_speed_medium
        } else {
            self.auto_scroll_speed_slow
        }
    }
}

impl SessionView {
    pub fn update_auto_scroll(&mut self, mouse_x: u16, mouse_y: u16) {
        let Some((sx, _sy, sx2, _sy2)) = self.session_area else {
            self.stop_auto_scroll();
            return;
        };
        if mouse_x < sx || mouse_x >= sx2 {
            self.stop_auto_scroll();
            return;
        }

        self.auto_scroll_speed = self.get_auto_scroll_speed(mouse_y);
        let dir = self.get_auto_scroll_direction(mouse_y);
        if dir == 0 {
            self.stop_auto_scroll();
        } else if !self.is_auto_scrolling {
            self.is_auto_scrolling = true;
            self.auto_scroll_accumulator = 0.0;
        }
    }
}

impl SessionView {
    pub(super) fn handle_auto_scroll(&mut self, delta_time: f64, _total_height: i32, visible_height: i32) {
        if !self.is_auto_scrolling {
            return;
        }

        // Use accumulator pattern (like OpenCode's `handleAutoScroll`)
        let dir = if let Some((_ax, _ay, _fx, fy)) = self.drag_selection {
            self.get_auto_scroll_direction(fy)
        } else {
            0
        };

        if dir == 0 {
            self.stop_auto_scroll();
            return;
        }

        let scroll_amount = self.auto_scroll_speed * delta_time;
        self.auto_scroll_accumulator += scroll_amount * f64::from(dir);

        let int_scroll = self.auto_scroll_accumulator.trunc() as i32;
        if int_scroll != 0 {
            self.auto_scroll_accumulator -= int_scroll as f64;
            let max_scroll = (self.actual_total_height - visible_height).max(0);
            let new_scroll = (self.scroll_y + int_scroll).clamp(0, max_scroll);
            if new_scroll == self.scroll_y {
                // Already at boundary, stop
                self.stop_auto_scroll();
                return;
            }
            self.scroll_y = new_scroll;
        }
    }
}

impl SessionView {
    pub fn stop_auto_scroll(&mut self) {
        if self.is_auto_scrolling {
            self.is_auto_scrolling = false;
            self.auto_scroll_accumulator = 0.0;
            self.auto_scroll_speed = 0.0;
        }
    }
}

impl SessionView {
    const fn get_auto_scroll_direction(&self, mouse_y: u16) -> i32 {
        let Some((_sx, sy, _sx2, sy2)) = self.session_area else {
            return 0;
        };
        let relative_y = mouse_y.saturating_sub(sy);
        let height = sy2.saturating_sub(sy);
        let threshold = self.auto_scroll_threshold;

        if relative_y <= threshold && self.scroll_y > 0 {
            return -1;
        }
        if height > threshold
            && relative_y >= height.saturating_sub(threshold)
            && self.scroll_y < i32::MAX
        {
            return 1;
        }

        0
    }
}
