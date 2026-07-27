/// Shared list-selection logic — tracks `selected_index` and `scroll_offset`
/// so the selected item stays visible. Designed for any view that renders a
/// scrollable, selectable list (e.g. sidebar, add-provider, home view).
///
/// # Usage
/// ```ignore
/// let mut sel = ListSelection::new();
/// sel.set_visible_count(20);
///
/// // Move selection (auto-adjusts scroll):
/// sel.select_next(total_items);
/// sel.select_prev(total_items);
/// sel.select_first(total_items);
/// sel.select_last(total_items);
/// ```
#[derive(Clone, Debug)]
pub struct ListSelection {
    /// Index of the currently selected item.
    pub selected_index: usize,
    /// How many items have been scrolled past (so the selected item stays visible).
    pub scroll_offset: usize,
    /// Visible item count from the last render, used internally by `adjust_scroll`.
    last_visible_count: usize,
}

impl ListSelection {
    pub const fn new() -> Self {
        Self {
            selected_index: 0,
            scroll_offset: 0,
            last_visible_count: 0,
        }
    }

    /// Store the current viewport height (call this during render).
    pub fn set_visible_count(&mut self, count: usize) {
        self.last_visible_count = count;
    }

    /// Move selection up by 1, wrapping to the last item. Scroll follows.
    pub fn select_prev(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            total - 1
        } else {
            self.selected_index - 1
        };
        self.adjust_scroll(total);
    }

    /// Move selection down by 1, wrapping to the first item. Scroll follows.
    pub fn select_next(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1) % total;
        self.adjust_scroll(total);
    }

    /// Jump selection to the first item.
    pub fn select_first(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        self.selected_index = 0;
        self.adjust_scroll(total);
    }

    /// Jump selection to the last item.
    pub fn select_last(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        self.selected_index = total - 1;
        self.adjust_scroll(total);
    }

    /// Clamp `selected_index` to valid range and ensure visibility.
    /// Call this when the item list changes (e.g. filter applied).
    pub fn clamp(&mut self, total: usize) {
        if total == 0 {
            self.selected_index = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected_index = self.selected_index.min(total - 1);
        self.adjust_scroll(total);
    }

    /// Ensure the selected item is visible by adjusting `scroll_offset`.
    fn adjust_scroll(&mut self, total: usize) {
        if total == 0 {
            self.scroll_offset = 0;
            return;
        }
        let vc = self.last_visible_count.min(total);
        if vc == 0 {
            return;
        }
        if self.selected_index >= self.scroll_offset + vc {
            self.scroll_offset = self.selected_index.saturating_sub(vc.saturating_sub(1));
        }
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_next_wraps() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(10);
        sel.select_next(5); // 0 → 1
        assert_eq!(sel.selected_index, 1);
        sel.select_next(5); // 1 → 2
        sel.select_next(5); // 2 → 3
        sel.select_next(5); // 3 → 4
        sel.select_next(5); // 4 → 0 (wraps)
        assert_eq!(sel.selected_index, 0);
    }

    #[test]
    fn test_select_prev_wraps() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(10);
        sel.select_prev(5); // 0 → 4 (wraps)
        assert_eq!(sel.selected_index, 4);
    }

    #[test]
    fn test_select_first_last() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(10);
        sel.select_next(5); // 0 → 1
        sel.select_last(5); // → 4
        assert_eq!(sel.selected_index, 4);
        sel.select_first(5); // → 0
        assert_eq!(sel.selected_index, 0);
    }

    #[test]
    fn test_empty_list() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(10);
        sel.select_next(0); // no-op
        assert_eq!(sel.selected_index, 0);
        sel.select_prev(0); // no-op
        assert_eq!(sel.selected_index, 0);
        sel.select_first(0); // no-op
        assert_eq!(sel.selected_index, 0);
        sel.select_last(0); // no-op
        assert_eq!(sel.selected_index, 0);
    }

    #[test]
    fn test_scroll_follows_selection() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(3); // only 3 items fit
        // Start at 0, scroll 3 times → should be at 3
        sel.select_next(10); // 0 → 1, scroll stays 0
        sel.select_next(10); // 1 → 2, scroll stays 0
        sel.select_next(10); // 2 → 3, scroll adjusts to 1
        assert_eq!(sel.selected_index, 3);
        assert_eq!(sel.scroll_offset, 1);
    }

    #[test]
    fn test_scroll_back_up() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(3);
        // Move down 4, then back up 1
        sel.select_next(10); // 0 → 1
        sel.select_next(10); // 1 → 2
        sel.select_next(10); // 2 → 3, scroll = 1
        sel.select_next(10); // 3 → 4, scroll = 2
        assert_eq!(sel.selected_index, 4);
        assert_eq!(sel.scroll_offset, 2);
        sel.select_prev(10); // 4 → 3, scroll stays 2? No, 3 < 2+3, so scroll stays 2
        assert_eq!(sel.selected_index, 3);
        assert_eq!(sel.scroll_offset, 2);
        sel.select_prev(10); // 3 → 2, scroll = 2 (since 2 >= 2, visible)
        assert_eq!(sel.selected_index, 2);
        assert_eq!(sel.scroll_offset, 2);
        sel.select_prev(10); // 2 → 1, scroll = 1 (1 < 2)
        assert_eq!(sel.selected_index, 1);
        assert_eq!(sel.scroll_offset, 1);
    }

    #[test]
    fn test_single_item() {
        let mut sel = ListSelection::new();
        sel.set_visible_count(10);
        sel.select_next(1); // 0 → 0 (only item)
        assert_eq!(sel.selected_index, 0);
        sel.select_prev(1); // 0 → 0
        assert_eq!(sel.selected_index, 0);
    }
}
