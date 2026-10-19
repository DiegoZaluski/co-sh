use super::*;

impl SessionView {
    /// Index of the first message that can intersect the viewport, found with
    /// a binary search over `prefix_y` instead of walking the whole timeline:
    /// the smallest `i` whose content bottom (`prefix_y[i] + msg_height_cache[i]`)
    /// is strictly below the viewport top (`scroll_y` in content space). The
    /// per-frame message walk then starts here and stops once `msg_top` passes
    /// `vp_bottom`, so the frame cost is O(log n + visible) instead of O(n).
    ///
    /// Messages whose estimated height is 0 occupy no content rows and can
    /// never satisfy the render overlap test (`msg_bottom > vp_top &&
    /// msg_top < vp_bottom` with `msg_bottom == msg_top`), so skipping them is
    /// safe — the walk would have skipped them anyway.
    pub(super) fn find_first_visible(&self, scroll_y: i32) -> usize {
        let n = self.msg_height_cache.len();
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let bottom = self.prefix_y[mid].saturating_add(self.msg_height_cache[mid]);
            if bottom > scroll_y {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }
}

impl SessionView {
    pub fn render_message_height(
        msg: &Message,
        max_w: u16,
        config: &TuiConfig,
        part_heights: Option<&[u16]>,
        tool_state: &ToolRenderState,
    ) -> i32 {
        let border_h: i32 = match msg.role {
            MessageRole::User => 1,
            MessageRole::Assistant => 0,
        };
        let is_error = msg.id.starts_with("msg-err-");
        let parts_h: i32 = if is_error {
            let error_text: String = msg
                .parts
                .iter()
                .filter_map(|p| {
                    if let crate::types::Part::Text(t) = p {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .filter(|ch| !ch.is_control())
                .collect();
            i32::from(cosh_tui::core::lib::unicode_util::word_wrap(&error_text, max_w).len() as u16)
        } else if let Some(heights) = part_heights {
            i32::from(heights.iter().copied().sum::<u16>())
        } else {
            msg.parts
                .iter()
                .map(|p| {
                    i32::from(Self::estimate_part_height(
                        p, max_w, config, &msg.role, tool_state,
                    ))
                })
                .sum()
        };
        let padding_bottom: i32 = match msg.role {
            MessageRole::User => 1,
            MessageRole::Assistant if is_error => 2,
            MessageRole::Assistant => 0,
        };
        border_h + parts_h + padding_bottom
    }
}

impl SessionView {
    /// Rebuild `prefix_y` from the height cache and return the total content
    /// height. The walk places message `i` at
    /// `sum(msg_height_cache[..i]) + i` (each message after the first is
    /// preceded by a 1-row gap, exactly like the render walk's `idx > 0`
    /// logic), so `prefix_y[i]` = that position and `prefix_y[n]` = the total
    /// (last message's height, no trailing gap).
    pub(super) fn rebuild_prefix_y(&mut self) -> i32 {
        self.prefix_y.clear();
        let n = self.msg_height_cache.len();
        if n == 0 {
            self.prefix_y.push(0);
            return 0;
        }
        let mut y: i32 = 0;
        self.prefix_y.push(0); // position of message 0
        for idx in 1..n {
            y += 1 + self.msg_height_cache[idx - 1];
            self.prefix_y.push(y);
        }
        y += self.msg_height_cache[n - 1]; // final height, no trailing gap
        self.prefix_y.push(y);
        y
    }
}
