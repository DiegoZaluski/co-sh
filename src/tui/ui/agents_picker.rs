use std::time::SystemTime;

use cosh_tui::core::types::{MouseButton, MouseEvent, MouseEventType};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::{Theme, contrast_color, rgba_color};
use crate::util::draw::draw_text_line_compact;
use crate::util::list_selection::ListSelection;

/// One row of the picker: a fixed listing entry plus the user's mark. The
/// synthetic `cosh` daemon entry and every ACP harness share this shape so
/// keyboard, mouse and render can treat them uniformly.
#[derive(Clone, Debug)]
pub struct AgentEntry {
    pub name: String,
    /// All required binaries found in PATH. Rendered dim when `false`; the
    /// mark is still allowed (the router re-checks before spawning, so a
    /// stale detection must not lock the user out).
    pub installed: bool,
    /// The user's multi-select mark. Toggleable; survives navigation AND
    /// filter edits (narrowing the list never discards a mark).
    pub selected: bool,
    /// How many instances of this CLI to spawn when the pick confirms —
    /// typed digits land in THIS field while the row is marked, so "two
    /// Claude panes" is a single pick. The string is the raw typed digits
    /// (`""` = the `DEFAULT_INSTANCES` placeholder shown in italic); it is
    /// parsed at confirm time.
    pub instances: String,
}

/// The default instance count a pick carries when the user leaves the
/// count input untouched: ONE instance — typing a count is an explicit
/// multi-instance gesture, not the default.
pub const DEFAULT_INSTANCES: usize = 1;

/// The MAXIMUM instance count a pick may request — the pane-grid ceiling
/// for ONE agent CLI. The single knob for the limit: the count input
/// rejects any digit that would push the parsed count past it, the confirm
/// parse clamps to it, and the persisted pre-mark path clamps too. Bump
/// the constant to widen the limit; nothing else needs to change.
pub const MAX_INSTANCES: usize = 9;

/// Instances confirmed for ONE agent CLI: the pane grid receives `n`
/// identical panes, each with its own independent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPick {
    pub name: String,
    /// How many panes of `name` to create (>= 1, sanity-clamped).
    pub instances: usize,
}

impl AgentEntry {
    /// The parsed instance count: the typed digits when valid, the DEFAULT
    /// placeholder otherwise (empty input = one instance). The result never
    /// exceeds `MAX_INSTANCES` — the count input already rejects
    /// overflowing digits, so this clamp only guards persisted or
    /// programmatic values typed under a SMALLER previous limit.
    #[must_use]
    pub fn parsed_instances(&self) -> usize {
        match self.instances.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(crate::ui::agents_picker::MAX_INSTANCES),
            _ => DEFAULT_INSTANCES,
        }
    }

    /// The count input's display text: the typed digits when present, the
    /// INFORMATIVE placeholder when untouched. The `[1-9]` bracket form is
    /// the classic editable-slot glyph: brackets say "type here", the range
    /// inside gives min AND max in one cluster (the min doubles as the
    /// default — Enter on an untouched row confirms `DEFAULT_INSTANCES`,
    /// so there is no "type 1" redundancy). Both bounds read from the
    /// constants, so raising the ceiling re-labels every row with no
    /// further change.
    #[must_use]
    pub fn instances_display(&self) -> String {
        if self.instances.is_empty() {
            format!("[{DEFAULT_INSTANCES}-{MAX_INSTANCES}]")
        } else {
            self.instances.clone()
        }
    }
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
    /// Enter confirmed. Carries the marked agents WITH their instance
    /// counts in listing order, `cosh` first when marked.
    Confirmed(Vec<AgentPick>),
    /// Esc: close and discard the marks.
    Cancelled,
}

/// Central multi-select dialog for the `/agents` command: pick which agent
/// CLIs may receive delegated work.
///
/// The first entry is always the synthetic `cosh` daemon — the top-level
/// agent that delegates to the ACP harnesses below it. The remaining entries
/// mirror [`cosh_tools::subagent::acp::ACP_AGENTS`] in registry order.
///
/// Layout parity with `/models`: the box rows are title / Search input / gap
/// / listing (dialog height = visible rows + 4) and the listing height
/// follows the number of entries up to the terminal budget, so the box can
/// grow instead of sitting at a fixed low height.
///
/// Input contract (same split as [`crate::ui::slash_menu::SlashMenu`]): the
/// App feeds raw key/mouse events through [`AgentsPicker::handle_key`] and
/// [`AgentsPicker::handle_mouse`], and consumes the returned
/// [`AgentsPickerAction`] — the component keeps no knowledge of App focus.
pub struct AgentsPicker {
    pub visible: bool,
    pub entries: Vec<AgentEntry>,
    /// The `/models`-style search input at the top of the box. Filters by
    /// agent name, case-insensitive. Marks are NOT touched by the filter,
    /// clearing it brings every mark back.
    pub filter: String,
    /// Blinking caret for the search input (same reusable component the
    /// dialog filters use). The App syncs `terminal_focused` before
    /// rendering, like it does for every dialog cursor.
    pub cursor: Cursor,
    /// Cursor + scroll state within the FILTERED listing. `selected_index`
    /// is what render highlights and what Space toggles, so keyboard, mouse
    /// and render stay in lockstep. Typing resets it to 0 (the filter
    /// changing invalidates the previous position).
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
            installed: true,
            selected: false,
            instances: String::new(),
        }];
        entries.extend(cosh_tools::subagent::acp::ACP_AGENTS.iter().map(|agent| {
            AgentEntry {
                name: agent.name.to_string(),
                installed: installed.contains(&agent.name),
                selected: false,
                instances: String::new(),
            }
        }));
        Self {
            visible: false,
            entries,
            filter: String::new(),
            cursor: Cursor::new(),
            selection: ListSelection::new(),
        }
    }

    pub fn open(&mut self) {
        self.visible = true;
        // A reopen always starts from a clean slate: no filter, cursor back
        // at the top (the search caret shows right away).
        self.filter.clear();
        self.selection.selected_index = 0;
        self.cursor.note_activity();
        self.selection.clamp(self.filtered_indices().len());
    }

    pub fn close(&mut self) {
        self.visible = false;
        // Marks live only for the session the picker is open: Esc discards
        // and confirm hands them to the App via the action payload, so a
        // reopen always starts from a clean slate. The filter follows the
        // same rule — it never leaks between opens.
        for entry in &mut self.entries {
            entry.selected = false;
        }
        self.filter.clear();
        self.selection.selected_index = 0;
    }

    /// Indices into `entries` that survive the current search filter, in
    /// listing order. The single source of truth for the filtered view —
    /// keyboard, mouse and render all read the same list so a click can
    /// never land on a row the renderer did not draw.
    fn filtered_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry_matches_filter(entry, &self.filter))
            .map(|(index, _)| index)
            .collect()
    }

    /// Names of the marked entries in listing order (`cosh` first). Marks
    /// survive filter edits, so a narrowed view still confirms everything.
    #[must_use]
    pub fn selected_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// The marked entries WITH their instance counts, in listing order —
    /// the confirm payload. Each marked row expands to ONE pick; the split
    /// grid is the one that multiplies it into `instances` panes.
    #[must_use]
    pub fn selected_picks(&self) -> Vec<AgentPick> {
        self.entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| AgentPick {
                name: entry.name.clone(),
                instances: entry.parsed_instances(),
            })
            .collect()
    }

    /// The entry under the cursor, if any — the instance-count input's
    /// target. EVERY row owns the qualifier digits (the count field is a
    /// per-row input the user types into directly, marker or not): typing
    /// digits on a row selects it with that count in one gesture.
    fn entry_at_cursor(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        filtered.get(self.selection.selected_index).copied()
    }

    fn toggle_at_cursor(&mut self) -> AgentsPickerAction {
        let filtered = self.filtered_indices();
        match filtered.get(self.selection.selected_index) {
            Some(&index) => {
                self.entries[index].selected = !self.entries[index].selected;
                AgentsPickerAction::Toggled
            }
            // Nothing under the cursor (empty filter result): inert.
            None => AgentsPickerAction::Consumed,
        }
    }

    /// Add a character to the search filter — the same contract the /models
    /// filter follows: an edit resets the selection to the top of the
    /// narrowed list so the highlight always lands on a row that exists.
    fn push_filter(&mut self, ch: char) {
        self.filter.push(ch);
        // The edit invalidates the previous position: restart from the top
        // of the narrowed list (the same contract the /models filter uses),
        // and clamp so the selection bookkeeping stays consistent with
        // `pop_filter`/`open`.
        self.selection.selected_index = 0;
        self.selection.clamp(self.filtered_indices().len());
        self.cursor.note_activity();
    }

    fn pop_filter(&mut self) {
        self.filter.pop();
        self.selection.selected_index = 0;
        self.cursor.note_activity();
        self.selection.clamp(self.filtered_indices().len());
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
            let picks = self.selected_picks();
            self.close();
            AgentsPickerAction::Confirmed(picks)
        }
    }

    /// Keyboard entry point. Esc cancels and discards; printable characters
    /// edit the search filter (Space excluded — Space toggles the mark) —
    /// EXCEPT digits while the cursor sits on a MARKED row: those fill the
    /// row's instance-count field ("how many panes of this CLI"), the
    /// Backspace sibling that pops it. A filter edit resets the cursor to
    /// the top of the narrowed list.
    pub fn handle_key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> AgentsPickerAction {
        if !self.visible {
            return AgentsPickerAction::None;
        }
        let total = self.filtered_indices().len();
        match (key, modifiers) {
            (KeyCode::Esc, _) => {
                self.close();
                AgentsPickerAction::Cancelled
            }
            (KeyCode::Enter, _) => self.confirm(),
            // Space must win over the generic Char arm: typing a space into
            // the search would otherwise toggle rows invisibly (agent names
            // never contain spaces, so the filter loses nothing).
            (KeyCode::Char(' '), _) => self.toggle_at_cursor(),
            // Digits edit the instance count of the row UNDER THE CURSOR —
            // marker or not (crossterm marks SHIFT with the modifier, and
            // shift+digit is not a symbol the count cares about). Typing a
            // count is itself the selection gesture: the row auto-marks so
            // "claude, 2, Enter" is a complete pick without touching Space.
            (KeyCode::Char(ch), KeyModifiers::NONE) if ch.is_ascii_digit() => {
                if let Some(index) = self.entry_at_cursor() {
                    let count = &mut self.entries[index].instances;
                    // The count input enforces the MAX_INSTANCES ceiling.
                    // Appending first: a digit that keeps the count inside
                    // the ceiling extends the number ("2" -> "8"). A digit
                    // that would OVERFLOW ("3" -> "32") is not discarded —
                    // it REPLACES the field (the replace gesture). That is
                    // what makes the count freely editable: "3" then "2"
                    // reads 2 (down), "3" then "9" reads 9 (up to the
                    // ceiling), instead of leaving the field stuck at its
                    // old value forever. The ceiling still holds: the
                    // replace digit itself must parse inside it ("9" with
                    // the ceiling at 9 is accepted, never 99). Raising
                    // MAX_INSTANCES automatically admits longer numbers
                    // (the append path stays bounded by the ceiling's
                    // digits); `count` only ever holds digits (this same
                    // guard mints it), so the parses cannot fail.
                    let candidate = format!("{count}{ch}");
                    let parsed = candidate.parse::<usize>().ok();
                    let replacement = ch.to_digit(10).map(|d| d as usize);
                    if parsed.is_some_and(|n| n <= MAX_INSTANCES) {
                        count.push(ch);
                        self.entries[index].selected = true;
                        self.cursor.note_activity();
                    } else if replacement.is_some_and(|n| n <= MAX_INSTANCES) {
                        *count = ch.to_string();
                        self.entries[index].selected = true;
                        self.cursor.note_activity();
                    }
                    // Both out of range (e.g. a lone "0" stays the default
                    // placeholder): the keypress is a no-op — the field
                    // keeps whatever the user had.
                }
                AgentsPickerAction::Consumed
            }
            // Plain typing (SHIFT for uppercase included — crossterm marks
            // shifted letters with SHIFT) lands in the search field.
            (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.push_filter(ch);
                AgentsPickerAction::Consumed
            }
            // Backspace pops the row's count input FIRST (the field owns the
            // key while it has content), THEN falls back to the filter.
            (KeyCode::Backspace, _) => {
                // The row under the cursor owns Backspace FIRST (its count
                // field pops while it has content — marker or not), THEN
                // the filter (the classic search-erase).
                if let Some(index) = self.entry_at_cursor()
                    && !self.entries[index].instances.is_empty()
                {
                    self.entries[index].instances.pop();
                    self.cursor.note_activity();
                    return AgentsPickerAction::Consumed;
                }
                self.pop_filter();
                AgentsPickerAction::Consumed
            }
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
    /// a click on a row INSIDE the box toggles it, wheel moves the cursor,
    /// and a click OFF the box dismisses the modal — the same outside-click
    /// contract the other dialog boxes use. The window math is shared with
    /// render via [`Self::row_window`], so a click can only ever land on a
    /// row that is drawn.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> AgentsPickerAction {
        if !self.visible {
            return AgentsPickerAction::None;
        }
        let box_area = self.dialog_box(area);
        let inside_box = mouse.x >= box_area.x
            && mouse.x < box_area.x + box_area.width
            && mouse.y >= box_area.y
            && mouse.y < box_area.y + box_area.height;
        let Some(row) = self.row_at(mouse.x, mouse.y, area) else {
            return match mouse.event_type {
                // Wheel navigates from anywhere while the picker is open.
                MouseEventType::ScrollUp => {
                    self.selection.select_prev(self.filtered_indices().len());
                    AgentsPickerAction::Consumed
                }
                MouseEventType::ScrollDown => {
                    self.selection.select_next(self.filtered_indices().len());
                    AgentsPickerAction::Consumed
                }
                // Click OFF the box dismisses the picker (left button — the
                // dismissal gate the other dialogs use; marks are discarded
                // by close). The dismiss lands on the PRESS, which also
                // anchors releases: an Up that lands outside can only be
                // seen here when its press started INSIDE the box (an
                // outside press already closed the picker, so the Up never
                // reaches this arm) — a fat-finger overshoot release must
                // not act as a dismiss click, the free-gateway dialog's
                // drag-ownership rule.
                MouseEventType::Down
                    if !inside_box && mouse.button == MouseButton::Left =>
                {
                    self.close();
                    AgentsPickerAction::Cancelled
                }
                MouseEventType::Up
                    if !inside_box && mouse.button == MouseButton::Left =>
                {
                    // Release of a press that started inside the box:
                    // inert, the picker stays open with its marks.
                    AgentsPickerAction::Consumed
                }
                // Other buttons, and presses inside the box but not on a
                // list row (title, search row, gap, bottom padding): inert,
                // the picker stays open.
                MouseEventType::Down | MouseEventType::Up => AgentsPickerAction::Consumed,
                // Move stays None so the App's hover tracking keeps working
                // around the modal.
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
                self.selection.select_prev(self.filtered_indices().len());
                AgentsPickerAction::Consumed
            }
            MouseEventType::ScrollDown => {
                self.selection.select_next(self.filtered_indices().len());
                AgentsPickerAction::Consumed
            }
            MouseEventType::Up if mouse.button == MouseButton::Left => {
                self.selection.selected_index = row;
                self.toggle_at_cursor()
            }
            _ => AgentsPickerAction::Consumed,
        }
    }

    /// The `(start, len)` slice of the FILTERED listing shown in the visible
    /// window, scrolled so the cursor row stays visible. The window size is
    /// the box's dynamic row budget (same clamp as the /models dialog), so
    /// the list only scrolls when the box is genuinely too small. Render and
    /// mouse hit-testing both call this — one source of truth, like the
    /// slash menu.
    fn row_window(&self, area: Rect) -> (usize, usize) {
        let visible = self.dialog_box(area).height.saturating_sub(4) as usize;
        let total = self.filtered_indices().len();
        let len = visible.min(total);
        if len == 0 {
            return (0, 0);
        }
        let pos = self.selection.selected_index.min(total - 1);
        // Scroll only once the cursor falls past the last visible row, the
        // same rule the model list dialog uses.
        let start = if pos >= len { pos - len + 1 } else { 0 };
        (start, len)
    }

    /// Centered dialog box for a full-screen `area` — the single source of
    /// geometry for render and mouse hit-testing, so a click can never land
    /// on a row the renderer did not draw. Sizing parity with /models:
    /// width clamps at 50/30 and the height grows with the filtered listing
    /// up to the terminal budget (visible rows + title + search + gap +
    /// bottom padding), so a 9-entry registry no longer sits in a box sized
    /// for 6.
    fn dialog_box(&self, area: Rect) -> Rect {
        // Responsive width: the same anchors the model dialog uses.
        let max_w = 50u16.min(area.width.saturating_sub(4));
        let width = max_w.max(30).min(area.width.saturating_sub(2));

        // Dynamic height: fit the whole filtered listing when the terminal
        // allows it, otherwise clamp to the available budget like /models.
        let max_visible_height = area.height.saturating_sub(4) as usize;
        let total = self.filtered_indices().len();
        let visible = max_visible_height.min(total.max(1)).max(1);
        let height = (visible as u16) + 4;

        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        Rect::new(x, y, width, height)
    }

    /// Which listing row (0-based within the visible window) sits at
    /// terminal point (`x`, `y`), as an index within the FILTERED list. The
    /// hit-test covers BOTH axes: a click beside the box on the same row
    /// band must NOT toggle a row — only a cell the renderer actually drew
    /// does. Rows are drawn between the gap row and the bottom padding row.
    fn row_at(&self, x: u16, y: u16, area: Rect) -> Option<usize> {
        let (start, len) = self.row_window(area);
        let box_area = self.dialog_box(area);
        if x < box_area.x || x >= box_area.x + box_area.width {
            return None; // beside the box on the same row band
        }
        let list_top = box_area.y + 3; // title + search + gap
        let list_bottom = box_area.y + box_area.height - 1; // bottom padding
        if y < list_top || y >= list_bottom {
            return None;
        }
        let row = (y - list_top) as usize;
        (row < len).then_some(start + row)
    }

    /// Render the centered dialog into `buf`. `area` is the FULL terminal
    /// area: the box is centered inside it, and mouse hit-testing in
    /// [`Self::handle_mouse`] expects the same value. `now` drives the
    /// search caret blink.
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: SystemTime) {
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

        // Title band: the dialogs' header contract — paddingLeft 4, bold
        // title, and a muted "esc" tucked into the header's right edge (no
        // shortcut billboard along the bottom — space/enter are the obvious
        // actions).
        let header_pad = 4;
        let header_x = box_area.x + header_pad;
        let header_w = box_area.width.saturating_sub(header_pad * 2);
        draw_text_line_compact(
            buf,
            "Select agents",
            header_x,
            box_area.y,
            box_area.width.saturating_sub(4),
            Style::default().fg(text).bg(fill).add_modifier(Modifier::BOLD),
        );
        let esc_hint = "esc";
        draw_text_line_compact(
            buf,
            esc_hint,
            header_x + header_w.saturating_sub(esc_hint.len() as u16),
            box_area.y,
            esc_hint.len() as u16,
            Style::default().fg(muted).bg(fill),
        );

        // Search row — the exact component the /models dialog renders: a
        // "Search" placeholder while the filter is empty, otherwise the
        // filter text, with the shared blinking caret on top of either.
        let cursor_state = self.cursor.current_state(now);
        for cx in header_x..header_x + header_w {
            if let Some(cell) = buf.cell_mut((cx, box_area.y + 1)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(fill));
            }
        }
        let search_x = header_x;
        let search_w = header_w;
        let caret = |cell: &mut ratatui::buffer::Cell| match cursor_state {
            CursorState::On => {
                cell.set_char('\u{2588}');
                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(fill));
            }
            CursorState::Off | CursorState::Blur => {
                // Semi-transparent cursor with medium shade character
                cell.set_char('\u{2592}');
                cell.set_style(Style::default().fg(muted).bg(fill));
            }
        };
        if self.filter.is_empty() {
            draw_text_line_compact(
                buf,
                "Search",
                search_x,
                box_area.y + 1,
                search_w,
                Style::default().fg(muted).bg(fill),
            );
            // Caret parked over the first character of the placeholder.
            if let Some(cell) = buf.cell_mut((search_x, box_area.y + 1)) {
                caret(cell);
            }
        } else {
            draw_text_line_compact(
                buf,
                self.filter.as_str(),
                search_x,
                box_area.y + 1,
                search_w,
                Style::default().fg(text).bg(fill),
            );
            // Blinking caret at the end of the filter text.
            let caret_x = search_x + self.filter.chars().count() as u16;
            if caret_x < search_x + search_w
                && let Some(cell) = buf.cell_mut((caret_x, box_area.y + 1))
            {
                caret(cell);
            }
        }

        // Gap row stays empty (the model dialog's gap row).

        let (start, len) = self.row_window(area);
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            let list_top = box_area.y + 3;
            let list_x = box_area.x + 1;
            draw_text_line_compact(
                buf,
                "No matching agents",
                list_x,
                list_top,
                box_area.width.saturating_sub(2),
                Style::default().fg(muted).bg(fill),
            );
            return;
        }
        for row in 0..len {
            let entry = &self.entries[filtered[start + row]];
            let yy = box_area.y + 3 + row as u16;
            let is_cursor = start + row == self.selection.selected_index;

            // Row treatment follows the theme/model dialogs: the CURSOR row
            // fills with `theme.primary` and renders in its contrast
            // foreground; every other row stays on the element fill.
            let (row_bg, fg) = if is_cursor {
                (rgba_color(theme.primary), contrast_color(theme.primary))
            } else {
                (fill, text)
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
            // glyph, so marked/unmarked rows don't jitter horizontally, and
            // spans the rest of the row: only the agent name is shown today,
            // the repo name is a future addition to this column.
            let name_style = if !entry.installed {
                Style::default().fg(muted).bg(row_bg)
            } else if is_cursor {
                Style::default().fg(fg).add_modifier(Modifier::BOLD).bg(row_bg)
            } else {
                Style::default().fg(text).bg(row_bg)
            };
            let name_x = box_area.x + 4;
            // The instance-count column — the former install-hint slot,
            // right-aligned against the border. EVERY row owns it: the
            // count field is a per-row input the user types into directly
            // (digits land on the row under the cursor and auto-mark it).
            // The untouched rows show the `×[1-9]` placeholder — brackets
            // mark the slot as TYPABLE, the range gives min AND max (the
            // min doubles as the default: Enter without typing keeps 1) —
            // in ITALIC muted, LIT UP when the cursor sits on the row so
            // the typable slot announces itself where the user is looking.
            // Typed digits paint in plain row text.
            {
                let counter = format!("×{}", entry.instances_display());
                let count_len = counter.chars().count() as u16;
                let count_x = box_area.x + box_area.width - 2 - count_len;
                let count_style = if entry.instances.is_empty() {
                    let placeholder_fg = if is_cursor { fg } else { muted };
                    Style::default()
                        .fg(placeholder_fg)
                        .add_modifier(Modifier::ITALIC)
                        .bg(row_bg)
                } else if is_cursor {
                    Style::default().fg(fg).bg(row_bg)
                } else {
                    Style::default().fg(text).bg(row_bg)
                };
                draw_text_line_compact(buf, &counter, count_x, yy, count_len, count_style);
                draw_text_line_compact(
                    buf,
                    &entry.name,
                    name_x,
                    yy,
                    count_x.saturating_sub(1 + name_x),
                    name_style,
                );
            }
        }
    }
}

/// Whether an entry matches the picker's search filter: the filter matches
/// the agent name, the same contract the /models filter uses in spirit
/// (visible text only — the rows carry nothing beyond the name, so matching
/// an invisible column would produce confusing empty-looking results).
/// Case-insensitive substring match.
fn entry_matches_filter(entry: &AgentEntry, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    entry.name.to_lowercase().contains(&filter.to_lowercase())
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

    /// Restricted registry so filter tests are deterministic regardless of
    /// the real agent registry contents.
    fn small_picker() -> AgentsPicker {
        let mut picker = AgentsPicker::new();
        let entry = |name: &str| AgentEntry {
            name: name.to_string(),
            installed: true,
            selected: false,
            instances: String::new(),
        };
        picker.entries = vec![entry("cosh"), entry("gemini"), entry("goose"), entry("kilo")];
        picker.open();
        picker
    }

    /// Same geometry the tests render into: full-screen area. Kept in sync
    /// with `render`'s sizing on purpose — if the layout constants drift,
    /// these tests fail loudly instead of hit-testing rows that no longer
    /// exist.
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
    fn digits_on_a_marked_row_fill_the_instance_field_not_the_filter() {
        let mut marked = picker();
        // Mark cosh (cursor row 0), then type digits.
        marked.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        marked.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        // An over-ceiling digit is NOT discarded: "2" then "5" would read
        // 25 > MAX_INSTANCES as an append, so the 5 REPLACES the field (the
        // correction gesture) — and it must NOT leak into the filter either:
        // the marked row owns the qualifier slot.
        marked.handle_key(KeyCode::Char('5'), KeyModifiers::NONE);

        assert_eq!(marked.entries[0].instances, "5", "5 replaced the stuck 2");
        // The filter stayed clear: the digits were NOT search text.
        assert!(marked.filter.is_empty());

        // The confirm payload carries the parsed count and NEVER the
        // default: parse happens at confirm time, not render time.
        let action = marked.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            action,
            AgentsPickerAction::Confirmed(vec![AgentPick {
                name: "cosh".to_string(),
                instances: 5,
            }])
        );
    }

    #[test]
    fn a_smaller_digit_after_a_larger_count_corrects_instead_of_sticking() {
        // The bug this guards: "3" then "2" used to be discarded as an
        // overflowing append (32 > ceiling), leaving the field stuck at 3
        // forever. The replace gesture makes the count freely editable:
        // typing a SMALLER digit corrects the field down.
        let mut corrected = picker();
        corrected.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        corrected.handle_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert_eq!(corrected.entries[0].instances, "3");

        corrected.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(
            corrected.entries[0].instances, "2",
            "the smaller digit REPLACED the stuck count"
        );
        assert_eq!(corrected.entries[0].parsed_instances(), 2);

        // And up again: "2" then "9" corrects to the ceiling.
        corrected.handle_key(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(corrected.entries[0].instances, "9");
        assert_eq!(corrected.entries[0].parsed_instances(), MAX_INSTANCES);
    }

    #[test]
    fn digits_on_an_unmarked_row_edit_the_count_and_auto_mark() {
        let mut picker = picker();
        // Cursor on cosh but UNMARKED: the count field is a per-row input
        // the user types into directly — a digit fills the field AND marks
        // the row, making "claude, 2, Enter" a complete pick without
        // touching Space. (The filter is search text for LETTERS only.)
        picker.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);

        assert_eq!(picker.entries[0].instances, "2");
        assert!(picker.entries[0].selected, "typing a count is the pick");
        assert!(picker.filter.is_empty(), "digits never reach the filter");
    }

    #[test]
    fn instance_count_untouched_confirms_the_default() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        let action = picker.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let AgentsPickerAction::Confirmed(picks) = action else {
            panic!("expected a confirm");
        };
        assert_eq!(picks[0].instances, DEFAULT_INSTANCES);
        // And the display shows the bracketed placeholder, not an empty
        // cell: brackets mark the slot TYPABLE, the range gives min=1 (the
        // default Enter keeps) and max=the ceiling.
        assert_eq!(
            picker.entries[0].instances_display(),
            format!("[{DEFAULT_INSTANCES}-{MAX_INSTANCES}]"),
            "the placeholder marks the field TYPABLE and shows the range"
        );
    }

    #[test]
    fn backspace_pops_the_instance_field_before_the_filter() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(picker.entries[0].instances, "2");

        // Spills into the field first (order 2 -> empty)…
        picker.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(picker.entries[0].instances.is_empty());
        // …then keeps popping the empty field (no-op)…
        picker.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(picker.entries[0].instances.is_empty());
        // …and only when the field is drained does Backspace reach the
        // filter.
        picker.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(picker.filter.is_empty(), "filter popped last");
    }

    #[test]
    fn instance_count_never_exceeds_max_instances() {
        // The MAX_INSTANCES ceiling is enforced PROGRESSIVELY at the input:
        // with the limit at 9, "99" is unreachable ("9" then "9" reads 99 >
        // 9) — the field can only ever hold a value the ceiling admits,
        // never one the parse would have to clamp away.
        let mut capped = picker();
        capped.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        capped.handle_key(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(capped.entries[0].instances, "9", "9 IS the ceiling");
        capped.handle_key(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(
            capped.entries[0].instances, "9",
            "99 exceeds the ceiling: the second digit was rejected"
        );
        assert_eq!(capped.entries[0].parsed_instances(), MAX_INSTANCES);

        // Anything above the ceiling is rejected the same way — with the
        // ceiling at 9 every single digit parses, so the guard shows on the
        // second keystroke (9 then 4 would read 94 > 9).
        capped.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        capped.handle_key(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(capped.entries[0].instances, "4");
        capped.handle_key(KeyCode::Char('4'), KeyModifiers::NONE);
        assert_eq!(capped.entries[0].instances, "4", "44 exceeds the ceiling");

        // A leading zero parses as 0 -> sanity-clamped to the default
        // (the split never spawns zero panes of a picked agent).
        let mut zero_picker = picker();
        zero_picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        zero_picker.handle_key(KeyCode::Char('0'), KeyModifiers::NONE);
        assert_eq!(
            zero_picker.entries[0].parsed_instances(),
            DEFAULT_INSTANCES
        );
    }

    #[test]
    fn enter_confirms_marked_entries_and_closes() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        let action = picker.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(
            action,
            AgentsPickerAction::Confirmed(vec![AgentPick {
                name: "cosh".to_string(),
                instances: DEFAULT_INSTANCES,
            }])
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
            AgentsPickerAction::Confirmed(vec![AgentPick {
                name: "cosh".to_string(),
                instances: DEFAULT_INSTANCES,
            }])
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
        // Full registry: 9 entries → box height = 9+4 = 13, y = (40-13)/2 =
        // 13; list rows start at +3 → y = 16, 17, … Click y = 17 → row 1.
        let action = picker.handle_mouse(&mouse(MouseEventType::Up, 30, 17), area);

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
        let total = picker.filtered_indices().len();
        // Selecting far past the end clamps instead of panicking.
        picker.selection.selected_index = total + 5;
        picker.selection.clamp(total);
        assert_eq!(picker.selection.selected_index, total - 1);

        // Empty listing stays inert.
        picker.entries.clear();
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
    fn render_draws_box_title_search_and_indicators() {
        let mut picker = picker();
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE); // mark cosh
        let area = picker_area();
        let theme = ThemeRegistry::new().default_theme().clone();
        let mut buf = Buffer::empty(area);
        picker.render(&mut buf, area, &theme, SystemTime::now());

        // Full registry: 9 entries → box 50×13 centered in 100×40 →
        // x = 25, y = 13. Title row = 13, search row = 14, first list
        // row = 16.
        let title: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 13)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(title.contains("Select agents"), "title row: {title}");
        // The dialogs' header hint: a muted "esc", not a shortcut billboard.
        assert!(title.contains("esc"), "esc hint on the header row: {title}");

        let search_row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 14)).map(|c| c.symbol().to_string()))
            .collect();
        // The placeholder shows as "earch": the blinking caret replaces the
        // placeholder's first glyph, exactly like the /models dialog.
        assert!(
            search_row.contains("earch"),
            "placeholder on the search row: {search_row}"
        );

        let first_row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 16)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(first_row.contains('●'), "marked ● on cursor row: {first_row}");
        assert!(first_row.contains("cosh"), "agent name on cursor row: {first_row}");

        let second_row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 17)).map(|c| c.symbol().to_string()))
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

    #[test]
    fn typing_edits_the_search_filter_and_narrows_the_list() {
        let mut picker = small_picker();
        assert_eq!(picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE), AgentsPickerAction::Consumed);
        assert_eq!(picker.filter, "g");
        // gemini + goose match by name; cosh and kilo do not.
        let filtered = picker.filtered_indices();
        assert_eq!(filtered, vec![1, 2]); // gemini, goose
        assert_eq!(picker.selection.selected_index, 0); // reset on filter edit

        // Backspace reverts to the full listing.
        picker.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(picker.filter.is_empty());
        assert_eq!(picker.filtered_indices().len(), 4);
    }

    #[test]
    fn filter_edits_never_touch_marks() {
        let mut picker = small_picker();
        // Mark goose (it is entry index 2 → Down ×2, since cosh starts under
        // the cursor), then narrow away and back: the mark survives because
        // the filter only reorders the view.
        for _ in 0..2 {
            picker.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Backspace, KeyModifiers::NONE);

        assert!(picker.entries
            .iter()
            .find(|entry| entry.name == "goose")
            .is_some_and(|entry| entry.selected));
    }

    #[test]
    fn space_toggles_instead_of_typing_into_the_search() {
        let mut picker = small_picker();
        assert_eq!(picker.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), AgentsPickerAction::Toggled);
        assert!(picker.filter.is_empty());
        assert!(picker.entries[0].selected);
    }

    #[test]
    fn uppercase_typing_reaches_the_filter() {
        let mut picker = small_picker();
        picker.handle_key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(picker.filter, "G");
        assert_eq!(picker.filtered_indices().len(), 2);
    }

    #[test]
    fn box_grows_with_the_listing_not_fixed_low() {
        let mut picker = small_picker();
        let full = picker.dialog_box(picker_area());
        // 4 entries → 4 visible rows → height 8 (rows + title + search +
        // gap + padding).
        assert_eq!(full.height, 8);
        assert_eq!(full.width, 50, "models width clamp: 50 when the terminal allows");

        // Narrow the listing and the box shrinks with it.
        picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let narrowed = picker.dialog_box(picker_area());
        assert_eq!(narrowed.height, 6, "2 matches: height = 2 + 4");
    }

    #[test]
    fn box_respects_the_terminal_height_clamp_like_models() {
        let picker = picker(); // full registry: 9 entries
        // 10-row terminal: the /models clamp gives ≤ 6 list rows → box 10.
        let short = Rect::new(0, 0, 100, 10);
        let box_rect = picker.dialog_box(short);
        assert_eq!(box_rect.height, 10);
        assert_eq!(box_rect.width, 50);
        // Window shows at most the clamped budget.
        let (start, len) = picker.row_window(short);
        assert_eq!(len, 6);
        assert_eq!(start, 0);
    }

    #[test]
    fn filter_resets_between_opens() {
        let mut picker = small_picker();
        picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        picker.handle_key(KeyCode::Esc, KeyModifiers::NONE); // close
        picker.open();
        assert!(picker.filter.is_empty());
        assert_eq!(picker.filtered_indices().len(), 4);
        assert_eq!(picker.selection.selected_index, 0);
    }

    #[test]
    fn click_on_a_padding_row_is_consumed_not_toggled() {
        let mut picker = small_picker();
        picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let area = picker_area();
        // 'g' matches gemini+goose → box = 50×6, y = (40-6)/2 = 17; list rows
        // at 20..21, bottom pad 22. A click on the (empty) pad row must not
        // toggle anything.
        let action = picker.handle_mouse(&mouse(MouseEventType::Up, 30, 22), area);
        assert_eq!(action, AgentsPickerAction::Consumed);
        assert!(picker.selected_names().is_empty());
    }

    #[test]
    fn click_outside_the_box_closes_the_picker() {
        let mut picker = small_picker();
        let area = picker_area();
        // The common dialog contract (slash menu / dialogs): a left click
        // OFF the box dismisses the modal. The dismiss lands on the PRESS.
        let action = picker.handle_mouse(&mouse(MouseEventType::Down, 5, 5), area);
        assert_eq!(action, AgentsPickerAction::Cancelled);
        assert!(!picker.visible);
    }

    #[test]
    fn release_outside_without_an_outside_press_stays_open() {
        let mut picker = small_picker();
        let area = picker_area();
        // Anchoring falls out of the dismiss-on-press model: an Up outside
        // the box with no outside press having closed first (e.g. a press
        // that started inside) is an overshoot release, NOT a dismiss
        // click.
        let action = picker.handle_mouse(&mouse(MouseEventType::Up, 5, 5), area);
        assert_eq!(action, AgentsPickerAction::Consumed);
        assert!(picker.visible);
    }

    #[test]
    fn click_beside_the_box_on_the_same_row_band_never_toggles() {
        let mut picker = small_picker();
        let area = picker_area();
        // THE regression this run fixed: the hit-test used to scope by row
        // band only, so a click BESIDE the box on the same y (here x = 80,
        // well right of the 50-wide box centered at x = 25) toggled a row.
        // The x-range check makes this an outside click → dismiss.
        // 4 entries → box 50×8 centered: x = 25..75, y = 16..24.
        let action = picker.handle_mouse(&mouse(MouseEventType::Down, 80, 20), area);
        assert_eq!(action, AgentsPickerAction::Cancelled);
        assert!(!picker.visible);
        assert!(picker.selected_names().is_empty());
        assert_eq!(picker.filtered_indices().len(), 4);
    }

    #[test]
    fn press_inside_the_box_that_overshoots_on_release_stays_open() {
        let mut picker = small_picker();
        let area = picker_area();
        // Drag-ownership rule (the free-gateway dialog's contract): a press
        // INSIDE the box must not let its release act as a dismiss click
        // when the pointer slips past the box edge mid-press — a fat-finger
        // overshoot must not silently discard every mark.
        // Down on a row (x = 30, y = 20 is the box's row band), release
        // outside at x = 5.
        picker.handle_mouse(&mouse(MouseEventType::Down, 30, 20), area);
        let action = picker.handle_mouse(&mouse(MouseEventType::Up, 5, 20), area);
        assert_eq!(action, AgentsPickerAction::Consumed);
        assert!(picker.visible);
    }

    #[test]
    fn right_button_outside_does_not_dismiss() {
        let mut picker = small_picker();
        let area = picker_area();
        // Dismissal is a LEFT-button contract, like the other dialogs; a
        // right/middle click off the box is inert (falls to the generic
        // Down|Up → Consumed arm, so the modal stays open and the marks
        // survive unchanged).
        let mut right_click = mouse(MouseEventType::Down, 5, 5);
        right_click.button = MouseButton::Right;
        let action = picker.handle_mouse(&right_click, area);
        assert_eq!(action, AgentsPickerAction::Consumed);
        assert!(picker.visible);
    }

    #[test]
    fn render_draws_the_search_caret_and_placeholder() {
        let mut picker = small_picker();
        let area = picker_area();
        let theme = ThemeRegistry::new().default_theme().clone();
        let mut buf = Buffer::empty(area);
        picker.render(&mut buf, area, &theme, SystemTime::now());

        // Empty filter: "Search" placeholder with the caret over its first
        // character (box: 50×8, x = 25, y = 16 → search row y = 17). The
        // caret literally REPLACES the placeholder's first glyph, like the
        // /models dialog does.
        let cell = buf.cell((25 + 4, 17)).unwrap();
        let symbol = cell.symbol().to_string();
        assert!(
            symbol == "\u{2588}" || symbol == "\u{2592}",
            "caret over the placeholder: {symbol}"
        );

        // Typed filter: the caret parks right after the text. The narrower
        // listing shrinks the box (4 → 2 rows: height 8 → 6), so the search
        // row is re-derived from the CURRENT geometry instead of hardcoded.
        picker.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        let mut buf = Buffer::empty(area);
        picker.render(&mut buf, area, &theme, SystemTime::now());
        let search_row = picker.dialog_box(area).y + 1;
        let text_row: String = (25..75)
            .filter_map(|x| buf.cell((x, search_row)).map(|c| c.symbol().to_string()))
            .collect();
        let trimmed = text_row.trim_start();
        assert!(
            trimmed.starts_with('g') && trimmed.len() > 1,
            "filter text + caret on the search row: {text_row:?}"
        );
    }

    #[test]
    fn no_matching_agents_row_renders_the_empty_message() {
        let mut picker = small_picker();
        for ch in "zzz".chars() {
            picker.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let area = picker_area();
        let theme = ThemeRegistry::new().default_theme().clone();
        let mut buf = Buffer::empty(area);
        picker.render(&mut buf, area, &theme, SystemTime::now());

        // Zero matches → box shrinks to 1 visible row; the empty message
        // takes the list_top row (box y = 17 → list_top = 20).
        let list_top: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 20)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(list_top.contains("No matching agents"), "empty message row: {list_top}");
    }
}
