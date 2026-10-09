use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::theme::{Theme, contrast_color, rgba_color};
use crate::util::draw::draw_text_line_compact;
use crate::util::list_selection::ListSelection;

/// Rows visible at once inside the picker's box. The list scrolls through
/// [`ListSelection`] when the registry grows past this.
const MAX_ROWS: usize = 6;

/// One row of the picker: a fixed listing entry plus the user's mark. The
/// synthetic `cosh` daemon entry and every ACP harness share this shape so
/// keyboard, mouse and render can treat them uniformly.
#[derive(Clone, Debug)]
pub struct AgentEntry {
    pub name: String,
    pub desc: String,
    /// Full static install hint from the registry. Shown (truncated) in the
    /// muted description column when the entry is not installed.
    pub install_hint: String,
    /// All required binaries found in PATH. Rendered dim with the install
    /// hint when `false`; the mark is still allowed (the router re-checks
    /// before spawning, so a stale detection must not lock the user out).
    pub installed: bool,
    /// The user's multi-select mark. Toggleable; survives navigation.
    pub selected: bool,
}

/// What a key or mouse event asked the picker to do. The App routes on this
/// enum; the picker itself never owns focus logic beyond its `visible` flag,
/// so the caller decides whether to reopen or dismiss surrounding UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsPickerAction {
    /// Not visible, or the event is meaningless to the picker: let the event
    /// fall through to the rest of the App.
    None,
    /// Handled but with no visible effect (e.g. Enter with nothing marked):
    /// stop propagation without closing or confirming.
    Consumed,
    /// A mark toggled under the cursor; the picker stays open.
    Toggled,
    /// Enter confirmed. Carries the marked names in listing order, `cosh`
    /// first.
    Confirmed(Vec<String>),
    /// Esc: close and discard the marks.
    Cancelled,
}

/// Central multi-select dialog for the future `/agents` command: pick which
/// agent CLIs may receive delegated work.
///
/// The first entry is always the synthetic `cosh` daemon — the top-level
/// agent that delegates to the ACP harnesses below it. The remaining entries
/// mirror [`cosh_tools::subagent::acp::ACP_AGENTS`] in registry order.
///
/// Input contract (same split as [`crate::ui::slash_menu::SlashMenu`]): the
/// App feeds raw key/mouse events through [`AgentsPicker::handle_key`] and
/// [`AgentsPicker::handle_mouse`], and consumes the returned
/// [`AgentsPickerAction`] — the component keeps no knowledge of App focus.
pub struct AgentsPicker {
    pub visible: bool,
    pub entries: Vec<AgentEntry>,
    /// Cursor + scroll state; `selected_index` is what render highlights and
    /// what Space/Enter toggle, so keyboard and mouse stay in lockstep.
    pub selection: ListSelection,
}

impl Default for AgentsPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentsPicker {
    /// Build the picker from the crate's agent registry: the synthetic `cosh`
    /// daemon entry on top, then every registered ACP harness with its
    /// installation status from [`cosh_tools::subagent::acp::detect_installed`]
    /// (cached, so repeated opens are free).
    #[must_use]
    pub fn new() -> Self {
        let installed = cosh_tools::subagent::acp::detect_installed();
        let mut entries = vec![AgentEntry {
            name: "cosh".to_string(),
            desc: "cosh daemon — delegates to the agents below".to_string(),
            install_hint: String::new(),
            installed: true,
            selected: false,
        }];
        entries.extend(cosh_tools::subagent::acp::ACP_AGENTS.iter().map(|agent| {
            AgentEntry {
                name: agent.name.to_string(),
                desc: first_sentence(agent.install_hint),
                install_hint: agent.install_hint.to_string(),
                installed: installed.contains(&agent.name),
                selected: false,
            }
        }));
        Self {
            visible: false,
            entries,
            selection: ListSelection::new(),
        }
    }

    pub fn open(&mut self) {
        self.visible = true;
        self.selection.clamp(self.entries.len());
    }

    pub fn close(&mut self) {
        self.visible = false;
        // Marks live only for the session the picker is open: Esc discards
        // and confirm hands them to the App via the action payload, so a
        // reopen always starts from a clean slate.
        for entry in &mut self.entries {
            entry.selected = false;
        }
    }

    /// Names of the marked entries in listing order (`cosh` first).
    #[must_use]
    pub fn selected_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.name.clone())
            .collect()
    }

    fn toggle_at_cursor(&mut self) -> AgentsPickerAction {
        let index = self.selection.selected_index;
        match self.entries.get_mut(index) {
            Some(entry) => {
                entry.selected = !entry.selected;
                AgentsPickerAction::Toggled
            }
            None => AgentsPickerAction::Consumed,
        }
    }

    /// Confirm and close, or refuse to confirm while nothing is marked.
    /// Enter NEVER toggles: toggling is Space's job alone, so unmarking a
    /// row can never be mistaken for a confirm (an Enter that flipped a
    /// mark off would otherwise close the dialog mid-edit).
    fn confirm(&mut self) -> AgentsPickerAction {
        if self.selected_names().is_empty() {
            // Nothing to confirm: the dialog stays open, like the model
            // dialog refusing an empty selection.
            AgentsPickerAction::Consumed
        } else {
            let names = self.selected_names();
            self.close();
            AgentsPickerAction::Confirmed(names)
        }
    }

    /// Keyboard entry point. Esc cancels and discards; Space toggles the
    /// mark; Enter confirms (and ONLY confirms — it never flips a mark).
    /// Movement reuses [`ListSelection`] so the scroll window follows the
    /// cursor.
    pub fn handle_key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> AgentsPickerAction {
        if !self.visible {
            return AgentsPickerAction::None;
        }
        let total = self.entries.len();
        match (key, modifiers) {
            (KeyCode::Esc, _) => {
                self.close();
                AgentsPickerAction::Cancelled
            }
            (KeyCode::Enter, _) => self.confirm(),
            (KeyCode::Char(' '), _) => self.toggle_at_cursor(),
            (KeyCode::Up, _) => {
                self.selection.select_prev(total);
                AgentsPickerAction::Consumed
            }
            (KeyCode::Down, _) => {
                self.selection.select_next(total);
                AgentsPickerAction::Consumed
            }
            (KeyCode::Home, _) => {
                self.selection.select_first(total);
                AgentsPickerAction::Consumed
            }
            (KeyCode::End, _) => {
                self.selection.select_last(total);
                AgentsPickerAction::Consumed
            }
            _ => AgentsPickerAction::Consumed,
        }
    }

    /// Mouse entry point over the same `area` passed to [`Self::render`]:
    /// click toggles the row under the pointer, wheel moves the cursor. The
    /// window math is shared with render via [`Self::row_window`], so a click
    /// always lands on the row that is drawn.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> AgentsPickerAction {
        if !self.visible {
            return AgentsPickerAction::None;
        }
        let Some(row) = self.row_at(mouse.y, area) else {
            // Outside the list body: clicks OFF the box are still consumed —
            // the picker is a modal, so a click behind it must never move
            // split-pane focus or select transcript text. Wheel navigates
            // from anywhere while the picker is open; Move stays None so
            // the App's hover tracking keeps working around the modal.
            return match mouse.event_type {
                MouseEventType::ScrollUp => {
                    self.selection.select_prev(self.entries.len());
                    AgentsPickerAction::Consumed
                }
                MouseEventType::ScrollDown => {
                    self.selection.select_next(self.entries.len());
                    AgentsPickerAction::Consumed
                }
                MouseEventType::Down | MouseEventType::Up => AgentsPickerAction::Consumed,
                _ => AgentsPickerAction::None,
            };
        };

        match mouse.event_type {
            MouseEventType::Move | MouseEventType::Drag | MouseEventType::Down => {
                // Cursor follow (the model picker's hover contract): the
                // highlight sticks to the pointer as it sweeps the list,
                // long before any release event reaches the dispatch —
                // without this the cursor could never follow the mouse.
                self.selection.selected_index = row;
                AgentsPickerAction::Consumed
            }
            MouseEventType::ScrollUp => {
                self.selection.select_prev(self.entries.len());
                AgentsPickerAction::Consumed
            }
            MouseEventType::ScrollDown => {
                self.selection.select_next(self.entries.len());
                AgentsPickerAction::Consumed
            }
            MouseEventType::Up if mouse.button == MouseButton::Left => {
                self.selection.selected_index = row;
                self.toggle_at_cursor()
            }
            _ => AgentsPickerAction::Consumed,
        }
    }

    /// The `(start, len)` slice of entries shown in the MAX_ROWS window,
    /// scrolled so the cursor row stays visible. Render and mouse hit-testing
    /// both call this — one source of truth, like the slash menu.
    fn row_window(&self) -> (usize, usize) {
        let len = MAX_ROWS.min(self.entries.len());
        if len == 0 {
            return (0, 0);
        }
        let pos = self.selection.selected_index.min(self.entries.len() - 1);
        // Scroll only once the cursor falls past the last visible row, the
        // same rule the model list dialog uses.
        let start = if pos >= len { pos - len + 1 } else { 0 };
        (start, len)
    }

    /// Centered dialog box for a full-screen `area` — the single source of
    /// geometry for render and mouse hit-testing, so a click can never land
    /// on a row the renderer did not draw.
    fn dialog_box(&self, area: Rect) -> Rect {
        let width = 56.min(area.width.saturating_sub(2)).max(20);
        // 1 padding + title + MAX_ROWS rows + 1 padding (borderless).
        let height = (MAX_ROWS as u16 + 3).min(area.height.saturating_sub(2)).max(6);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        Rect::new(x, y, width, height)
    }

    /// Which list row (0-based within the visible window) sits at terminal
    /// row `y`, if any. Rows are drawn between the title band and the
    /// bottom padding row.
    fn row_at(&self, y: u16, area: Rect) -> Option<usize> {
        let (start, len) = self.row_window();
        let box_area = self.dialog_box(area);
        let list_top = box_area.y + 2; // padding + title
        let list_bottom = box_area.y + box_area.height - 1; // bottom padding
        if y < list_top || y >= list_bottom {
            return None;
        }
        let row = (y - list_top) as usize;
        (row < len).then_some(start + row)
    }

    /// Render the centered dialog into `buf`. `area` is the FULL terminal
    /// area: the box is centered inside it, and mouse hit-testing in
    /// [`Self::handle_mouse`] expects the same value.
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }
        let box_area = self.dialog_box(area);

        let fill = rgba_color(theme.background_element);
        let text = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);

        // Solid background panel — the same bare floating surface the other
        // dialogs use (`clear_dialog_surface` semantics: spaces on the fill,
        // every modifier stripped, cells opted out of the diff pipeline so
        // the frame redraws as a unit). NO border characters: the TUI's
        // boxes are borderless floating surfaces.
        for yy in box_area.y..box_area.y + box_area.height {
            for xx in box_area.x..box_area.x + box_area.width {
                if let Some(cell) = buf.cell_mut((xx, yy)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(fill).remove_modifier(Modifier::all()));
                    cell.set_diff_option(CellDiffOption::None);
                }
            }
        }

        draw_text_line_compact(
            buf,
            "Select agents",
            box_area.x + 2,
            box_area.y + 1,
            box_area.width.saturating_sub(4),
            Style::default().fg(text).bg(fill).add_modifier(Modifier::BOLD),
        );
        // The single contextual hint, in the dialogs' language: a muted
        // "esc" tucked into the header's right edge (no shortcut billboard
        // along the bottom — space/enter are the obvious actions).
        let esc_hint = "esc";
        draw_text_line_compact(
            buf,
            esc_hint,
            box_area.x + box_area.width - 2 - esc_hint.len() as u16,
            box_area.y + 1,
            esc_hint.len() as u16,
            Style::default().fg(muted).bg(fill),
        );

        let (start, len) = self.row_window();
        for row in 0..len {
            let entry = &self.entries[start + row];
            let yy = box_area.y + 2 + row as u16;
            let is_cursor = start + row == self.selection.selected_index;

            // Row treatment follows the theme/model dialogs: the CURSOR row
            // fills with `theme.primary` and renders in its contrast
            // foreground; every other row stays on the element fill.
            let (row_bg, fg) = if is_cursor {
                (rgba_color(theme.primary), contrast_color(theme.primary))
            } else {
                (fill, rgba_color(theme.text))
            };
            for cx in box_area.x + 1..box_area.x + box_area.width - 1 {
                if let Some(cell) = buf.cell_mut((cx, yy)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(row_bg));
                }
            }

            // Mark indicator: the dialogs' ● (filled = marked, accent;
            // hollow outline = unmarked, muted). One column, aligned.
            let (indicator, indicator_fg) = if entry.selected {
                ('●', rgba_color(theme.accent))
            } else {
                ('○', muted)
            };
            if let Some(cell) = buf.cell_mut((box_area.x + 2, yy)) {
                cell.set_char(indicator);
                cell.set_style(Style::default().fg(indicator_fg).bg(row_bg));
            }

            // The name column stays aligned regardless of the indicator
            // glyph, so marked/unmarked rows don't jitter horizontally.
            let name_style = if !entry.installed {
                Style::default().fg(muted).bg(row_bg)
            } else if is_cursor {
                Style::default().fg(fg).add_modifier(Modifier::BOLD).bg(row_bg)
            } else {
                Style::default().fg(text).bg(row_bg)
            };
            draw_text_line_compact(buf, &entry.name, box_area.x + 4, yy, 14, name_style);

            let desc = if entry.installed {
                entry.desc.clone()
            } else {
                first_sentence(&entry.install_hint)
            };
            let desc_style = Style::default().fg(muted).bg(row_bg);
            let desc_x = box_area.x + 19;
            draw_text_line_compact(
                buf,
                &desc,
                desc_x,
                yy,
                box_area.x + box_area.width - 2 - desc_x,
                desc_style,
            );
        }
    }
}

/// First sentence of a hint/description, for the muted list column: keeps
/// long npm/curl instructions from overflowing the fixed dialog width.
fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.find(". ") {
        Some(idx) => trimmed[..idx].to_string(),
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;

    fn picker() -> AgentsPicker {
        let mut picker = AgentsPicker::new();
        picker.open();
        picker
    }

    fn mouse(event_type: MouseEventType, x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            event_type,
            MouseButton::Left,
            x,
            y,
            cosh_tui::core::types::MouseModifiers::none(),
        )
    }

    /// Same geometry the tests render into: full-screen area, box centered
    /// at 56×(MAX_ROWS+4). Kept in sync with `render`'s sizing on purpose —
    /// if the layout constants drift, these tests fail loudly instead of
    /// hit-testing rows that no longer exist.
    fn picker_area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    #[test]
    fn toggle_preserves_listing_order() {
        let mut picker = picker();
        // Mark 3rd, then 1st: the confirmed list must still be in listing
        // order, not in the order the user happened to toggle.
        picker.handle_key(KeyCode::Down, KeyModifiers::NONE);
        picker.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), AgentsPickerAction::Toggled);
        picker.handle_key(KeyCode::Up, KeyModifiers::NONE);
        picker.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), AgentsPickerAction::Toggled);

        assert_eq!(picker.selected_names()[0], "cosh");
        assert_eq!(picker.selected_names()[1], picker.entries[2].name);
    }

    #[test]
    fn enter_confirms_marked_entries_and_closes() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        let action = picker.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(
            action,
            AgentsPickerAction::Confirmed(vec!["cosh".to_string()])
        );
        assert!(!picker.visible);
    }

    #[test]
    fn enter_never_toggles_it_only_confirms() {
        let mut picker = picker();
        // Enter on an unmarked list flips NOTHING: toggling is Space's job,
        // so unmarking a row can never be mistaken for a confirm.
        let action = picker.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(action, AgentsPickerAction::Consumed);
        assert!(picker.visible);
        assert!(!picker.entries[0].selected);

        // Mark, unmark, then Enter: the unmark was honored and the picker
        // refuses to confirm an empty selection (stays open).
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(picker.selected_names().is_empty());
        assert_eq!(
            picker.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            AgentsPickerAction::Consumed
        );
        assert!(picker.visible);

        // With something marked, Enter confirms and closes.
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            picker.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            AgentsPickerAction::Confirmed(vec!["cosh".to_string()])
        );
        assert!(!picker.visible);
    }

    #[test]
    fn esc_cancels_and_discards_marks() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Down, KeyModifiers::NONE);
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);

        assert_eq!(picker.handle_key(KeyCode::Esc, KeyModifiers::NONE), AgentsPickerAction::Cancelled);
        assert!(!picker.visible);
        assert!(picker.selected_names().is_empty());
    }

    #[test]
    fn mouse_click_toggles_the_clicked_row() {
        let mut picker = picker();
        let area = picker_area();
        // Box top = (40 - 10) / 2 = 15; list rows start at +2 → y = 17, 18, …
        let action = picker.handle_mouse(&mouse(MouseEventType::Up, 30, 18), area);

        assert_eq!(action, AgentsPickerAction::Toggled);
        assert!(picker.entries[1].selected);
        assert_eq!(picker.selection.selected_index, 1);
    }

    #[test]
    fn mouse_wheel_navigates_with_clamp_semantics() {
        let mut picker = picker();
        let area = picker_area();
        let at_row = picker.entries.len() - 1; // a row that is on screen

        picker.handle_mouse(&mouse(MouseEventType::ScrollDown, 30, 17), area);
        assert_eq!(picker.selection.selected_index, 1);
        picker.handle_mouse(&mouse(MouseEventType::ScrollUp, 30, 17), area);
        assert_eq!(picker.selection.selected_index, 0);
        // Wrapped up from 0 must land on the last entry (ListSelection wrap).
        picker.handle_mouse(&mouse(MouseEventType::ScrollUp, 30, 17), area);
        assert_eq!(picker.selection.selected_index, at_row);
    }

    #[test]
    fn navigation_clamps_to_list_bounds() {
        let mut picker = picker();
        let total = picker.entries.len();
        // Selecting far past the end clamps instead of panicking.
        picker.selection.selected_index = total + 5;
        picker.selection.clamp(total);
        assert_eq!(picker.selection.selected_index, total - 1);

        // Empty list stays inert.
        picker.entries.clear();
        picker.selection.clamp(0);
        assert_eq!(picker.handle_key(KeyCode::Down, KeyModifiers::NONE), AgentsPickerAction::Consumed);
        assert_eq!(picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), AgentsPickerAction::Consumed);
    }

    #[test]
    fn events_when_hidden_are_passed_through() {
        let mut picker = AgentsPicker::new();
        assert_eq!(picker.handle_key(KeyCode::Esc, KeyModifiers::NONE), AgentsPickerAction::None);
        assert_eq!(
            picker.handle_mouse(&mouse(MouseEventType::Up, 0, 0), picker_area()),
            AgentsPickerAction::None
        );
    }

    #[test]
    fn render_draws_box_title_and_indicators() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE); // mark cosh
        let area = picker_area();
        let theme = ThemeRegistry::new().default_theme().clone();
        let mut buf = Buffer::empty(area);
        picker.render(&mut buf, area, &theme);

        // Box: 56×10 centered in 100×40 → y = 15. Title row = +1 = 16,
        // first list row = +2 = 17.
        let title: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 16)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(title.contains("Select agents"), "title row: {title}");
        // The dialogs' header hint: a muted "esc", not a shortcut billboard.
        assert!(title.contains("esc"), "esc hint on the header row: {title}");

        let first_row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 17)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(first_row.contains('●'), "marked ● on cursor row: {first_row}");
        assert!(first_row.contains("cosh"), "agent name on cursor row: {first_row}");

        let second_row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 18)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(second_row.contains('○'), "unmarked ○ on the next row: {second_row}");
    }

    #[test]
    fn cosh_entry_leads_and_registry_entries_follow() {
        let picker = AgentsPicker::new();
        assert_eq!(picker.entries[0].name, "cosh");
        assert!(picker.entries[0].installed);
        assert!(picker.entries[1..]
            .iter()
            .enumerate()
            .all(|(i, entry)| entry.name == cosh_tools::subagent::acp::ACP_AGENTS[i].name));
    }
}
