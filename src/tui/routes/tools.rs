use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};
use crate::util::list_selection::ListSelection;

pub fn load_disabled_tools(setup: &crate::util::setup::Setup) -> HashSet<String> {
    setup.tools.disabled.iter().cloned().collect()
}

pub fn save_disabled_tools(setup: &mut crate::util::setup::Setup, disabled: &HashSet<String>) {
    setup.tools.disabled = disabled.iter().cloned().collect();
    setup.save();
}

/// Load the persisted tool-call mode. Unknown/missing values fall back to
/// `Native` (the default contract).
#[must_use]
pub fn load_tool_call_mode(setup: &crate::util::setup::Setup) -> cosh_sdk::connector::ToolCallMode {
    match setup.tools.tool_call_mode.as_str() {
        "inline" => cosh_sdk::connector::ToolCallMode::Inline,
        _ => cosh_sdk::connector::ToolCallMode::Native,
    }
}

pub fn save_tool_call_mode(
    setup: &mut crate::util::setup::Setup,
    mode: cosh_sdk::connector::ToolCallMode,
) {
    setup.tools.tool_call_mode = match mode {
        cosh_sdk::connector::ToolCallMode::Native => "native".to_string(),
        cosh_sdk::connector::ToolCallMode::Inline => "inline".to_string(),
    };
    setup.save();
}

fn internal_tools() -> &'static [(&'static str, &'static str)] {
    &[
        ("bash_run", "Execute shell commands"),
        ("fs_read", "Read file contents"),
        ("fs_write", "Write file contents"),
        ("fs_edit", "Edit file contents (exact string replacement)"),
        ("fs_edit_lines", "Edit file lines (line/block operations)"),
        ("fs_ast_edit", "Edit file contents (structural AST rewrite)"),
        ("fs_rollback", "Rollback file changes"),
        ("find_glob", "Find files by glob pattern"),
        ("find_grep", "Search file contents"),
        ("web_fetch", "Fetch web page content"),
        ("web_search", "Search the web"),
        ("plan_todo_write", "Write todo items"),
        ("ask_questions", "Ask the user questions"),
        #[cfg(feature = "embed")]
        (
            "recall_search",
            "Search knowledge bases for semantically similar entries",
        ),
        ("skills_list", "List available skills"),
        ("skills_read", "Read a skill"),
        ("skills_read_asset", "Read a skill asset"),
        ("skills_match_skills", "Match skills to task"),
        ("subagent_call", "Run a sub-agent for a delegated task"),
        (
            "computer_apps",
            "List desktop apps with PIDs (accessibility tree)",
        ),
        ("computer_snapshot", "Capture an app's accessibility tree"),
        (
            "computer_wait",
            "Block until an element reaches a state (visible/enabled/detached/...); read-only",
        ),
        (
            "computer_screenshot",
            "Capture screen as an image; annotate=true for boxes + selector legend (Build-safe)",
        ),
        (
            "computer_act",
            "Act AND type as one pipeline: semantic action on an element, then keyboard/wait via `then`",
        ),
        (
            "computer_control",
            "Pointer AND keyboard as one pipeline: click an element, then type/press via `then`",
        ),
        (
            "lsp",
            "Language server tools (diagnostics, definitions, references, rename, ...)",
        ),
    ]
}

pub struct InternalToolsView {
    pub selection: ListSelection,
    pub disabled: HashSet<String>,
    pub changed: bool,
    /// How many rows actually fit in the current viewport. Updated on every
    /// render so that scrolling, clamping and mouse hit-testing all agree on
    /// the same value — otherwise items scroll out of view while free space
    /// below the list is wasted (same fix as the ADD Provider router).
    visible_count: usize,
    /// Buffer row of the first visible list item, captured at render time.
    /// The vertical centering depends on the exact area the render used, so
    /// the mouse path must reuse it instead of re-deriving it from its own
    /// (differently sized) area.
    list_start_y: u16,
}

impl InternalToolsView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            disabled: HashSet::new(),
            changed: false,
            visible_count: 0,
            list_start_y: 0,
        }
    }

    pub fn select_next(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_next(internal_tools().len());
    }

    pub fn select_prev(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_prev(internal_tools().len());
    }

    pub fn toggle_current(&mut self) {
        let (name, _) = internal_tools()[self.selection.selected_index];
        if !self.disabled.remove(name) {
            self.disabled.insert(name.to_string());
        }
        self.changed = true;
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        // Use the viewport the render computed, so hit-testing matches what
        // is actually drawn on screen. The mouse path receives a slightly
        // different area, and re-deriving the centered block from it shifted
        // every row by one (see the ADD Provider router's regression test).
        let count = self.visible_count.min(internal_tools().len());
        if count == 0 {
            return None;
        }
        let my = mouse.y;
        let mx = mouse.x;
        let max_row_w = max_row_width();
        if max_row_w == 0 {
            return None;
        }

        let list_start_y = self.list_start_y;

        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let hit = mx >= row_x && mx < row_x + max_row_w as u16;
        if !hit {
            return None;
        }
        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
            if idx >= internal_tools().len() {
                break;
            }
            let item_y = list_start_y + i as u16;
            if my == item_y {
                return Some(idx);
            }
        }
        None
    }

    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        self.find_row_for_mouse(mouse, area)
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        // title
        let title = "Internal Tools";
        let start_y = content_start_y(area);

        let max_w = max_row_width();
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        let title_x = row_x;
        for (i, ch) in title.chars().enumerate() {
            let cx = title_x + i as u16;
            if cx >= area.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, start_y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(muted));
            }
        }

        // Clamp selection and scroll before rendering
        let count = visible_items(area).min(internal_tools().len());
        self.visible_count = count;
        self.selection.set_visible_count(count.max(1));
        self.selection.clamp(internal_tools().len());

        // tools list aligned
        let list_start_y = start_y + 2;
        self.list_start_y = list_start_y;
        if max_w == 0 {
            return;
        }

        // "  " column for the symbol (ZERO WIDTH prefix so column 0 = symbol)
        let sym_x = row_x;
        let name_x = row_x + 2; // symbol + 1 space

        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
            if idx >= internal_tools().len() {
                break;
            }
            let (name, desc) = internal_tools()[idx];
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let enabled = !self.disabled.contains(name);
            let symbol = if enabled { "✔" } else { "✗" };
            let sym_color = if enabled { Color::Green } else { Color::Red };
            let is_selected = idx == self.selection.selected_index;
            let row_color = if is_selected { primary } else { fg };

            // Symbol (✔ / ✗)
            let sym_ch = symbol.chars().next().unwrap();
            if let Some(cell) = buf.cell_mut((sym_x, y)) {
                cell.set_char(sym_ch);
                cell.set_style(Style::default().fg(sym_color));
            }

            // Name + " - " + description
            let rest = format!("{name} - {desc}");
            for (j, ch) in rest.chars().enumerate() {
                let cx = name_x + j as u16;
                if cx >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(row_color));
                }
            }
        }
    }
}

/// Compute the Y position where title and list start (vertically centered).
fn content_start_y(area: Rect) -> u16 {
    let count = visible_items(area).min(internal_tools().len());
    let content_height = count + 2; // title + blank line + items
    area.y + (area.height.saturating_sub(content_height as u16)) / 2
}

fn max_row_width() -> usize {
    internal_tools()
        .iter()
        .map(|(name, desc)| 2 + name.len() + 4 + desc.len()) // "✔ name - desc"
        .max()
        .unwrap_or(0)
}

const fn visible_items(area: Rect) -> usize {
    (area.height.saturating_sub(3)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            MouseEventType::Down,
            MouseButton::Left,
            x,
            y,
            MouseModifiers::none(),
        )
    }

    /// Regression: navigation must use the viewport the render computed, not
    /// a hardcoded estimate. With a stale/too-small visible count the list
    /// started scrolling while free space was still available below.
    #[test]
    fn scroll_fits_the_real_viewport() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        // Tall viewport: every tool fits with room to spare.
        let area = Rect::new(0, 0, 100, 60);
        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme);

        assert_eq!(
            view.visible_count,
            internal_tools().len(),
            "viewport must fit the whole list"
        );
        for _ in 0..internal_tools().len() {
            view.select_next();
        }
        assert_eq!(
            view.selection.scroll_offset,
            0,
            "scroll must stay at 0 while everything fits"
        );
    }

    /// Regression: `find_row_for_mouse` must reuse the list geometry the
    /// render captured instead of re-deriving it from the mouse path's area,
    /// which is a different height and would shift every hit by a row.
    #[test]
    fn mouse_hits_match_the_rendered_list_rows() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        let term = Rect::new(0, 0, 100, 30);

        // Render path (src/tui/app/render.rs): `session_area.height` is the
        // terminal height − 3 and the InternalTools branch subtracts 1 more.
        let render_area = Rect::new(0, 1, 100, term.height - 4);
        // Mouse path (src/tui/app/mouse.rs): terminal height − 4, a different
        // height than the render area.
        let mouse_area = Rect::new(0, 1, 100, term.height - 4 + 1);

        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(term);
        view.render(&mut buf, render_area, &theme);

        assert!(
            view.visible_count > 0,
            "viewport must show at least one row"
        );
        let max_w = max_row_width();
        let row_x = render_area.x + (render_area.width.saturating_sub(max_w as u16)) / 2;
        let name_x = row_x + 2;

        // Sanity: a tool really is drawn on the first list row.
        let drawn = (0..10u16).any(|dx| {
            buf.cell((name_x + dx, view.list_start_y))
                .is_some_and(|cell| cell.symbol() != " ")
        });
        assert!(
            drawn,
            "no tool drawn at first list row y={}",
            view.list_start_y
        );

        // Every drawn row must map to its own index even though the mouse
        // path hands us a differently sized area.
        for i in 0..view.visible_count.min(5) {
            let y = view.list_start_y + i as u16;
            assert_eq!(
                view.handle_mouse(&click(name_x, y), mouse_area),
                Some(i),
                "click on drawn row y={y} misaligned"
            );
        }

        // Rows just outside the drawn list must not hit anything.
        assert_eq!(
            view.handle_mouse(&click(name_x, view.list_start_y - 1), mouse_area),
            None
        );
        let below = view.list_start_y + view.visible_count as u16;
        assert_eq!(view.handle_mouse(&click(name_x, below), mouse_area), None);
    }

    /// Every entry in the Internal Tools list has a unique name so toggling
    /// one row never masks another.
    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<&str> = internal_tools().iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let len = names.len();
        names.dedup();
        assert_eq!(names.len(), len);
    }
}
