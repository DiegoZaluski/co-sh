use std::collections::BTreeMap;
use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh::ModelEntry;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::Theme;

/// Visual item in the model list - either a provider header or a model
#[derive(Debug, Clone)]
enum VisualItem {
    Header(String),
    Model(ModelEntry),
}

/// List of (`key_combo`, description) for the Shortcuts dialog
/// Only non-obvious compound shortcuts — basic nav/enter/esc are excluded
const SHORTCUTS: &[(&str, &str)] = &[
    ("Tab", "Toggle mode (Build/Ask)"),
    ("PageUp/Down", "Scroll page up/down"),
    ("Home/End", "Go to start/end of input"),
    ("Ctrl+B", "Toggle sidebar"),
    ("Ctrl+C", "Copy selection / Quit"),
    ("Ctrl+T", "Toggle thinking"),
    ("Ctrl+D", "Toggle tool details"),
    ("Ctrl+G", "Toggle generic output"),
    ("Ctrl+Y", "Toggle timestamps"),
    ("Ctrl+P", "Command palette"),
    ("Ctrl+\u{2191}/\u{2193}", "Prompt history"),
    ("Ctrl+J", "Insert newline"),
    ("j/k", "Vim-style scroll (prompt empty)"),
    ("Ctrl+K", "Show keyboard shortcuts"),
];

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

#[derive(Debug, Clone)]
pub enum DialogType {
    Alert {
        message: String,
    },
    Confirm {
        message: String,
    },
    ThemeList {
        themes: Vec<String>,
        current: String,
        filter: String,
    },
    ModelList {
        models: Vec<ModelEntry>,
        current: String,
        filter: String,
    },
    ApiKeyInput {
        provider: String,
        env_var: String,
        input: String,
        cursor_pos: usize,
    },
    Shortcuts {
        scroll: usize,
    },
}

#[derive(Debug, Clone)]
pub struct DialogInstance {
    pub dialog_type: DialogType,
    pub selected: usize,
    pub cursor: Cursor,
}

pub struct DialogState {
    pub stack: Vec<DialogInstance>,
}

/// Result of a mouse event handled by the dialog system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    /// Click was not on the dialog.
    None,
    /// Click was consumed (selection changed, etc.).
    Consumed,
    /// User confirmed a selection (clicked an item or Yes/No).
    Confirmed,
    /// User dismissed the dialog (clicked "esc" or outside).
    Dismissed,
}

impl DialogState {
    pub fn new() -> Self {
        DialogState { stack: Vec::new() }
    }

    pub fn show(&mut self, dialog_type: DialogType) {
        self.stack.push(DialogInstance {
            dialog_type,
            selected: 0,
            cursor: Cursor::new(),
        });
    }

    pub fn replace(&mut self, dialog_type: DialogType) {
        self.clear();
        self.show(dialog_type);
    }

    pub fn pop(&mut self) {
        self.stack.pop();
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn visible(&self) -> bool {
        !self.stack.is_empty()
    }

    pub fn current(&self) -> Option<&DialogInstance> {
        self.stack.last()
    }

    pub fn current_mut(&mut self) -> Option<&mut DialogInstance> {
        self.stack.last_mut()
    }

    #[allow(clippy::too_many_lines, clippy::similar_names)]
    /// Handle a mouse click on the current dialog.
    /// The dialog state is modified (selection changed), but the dialog is NOT popped.
    /// Returns what action the caller should take.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect, _theme: &Theme) -> DialogAction {
        let Some(instance) = self.stack.last_mut() else {
            return DialogAction::None;
        };

        let x = mouse.x;
        let y_click = mouse.y;

        match &mut instance.dialog_type {
            DialogType::Alert { message: _ } => {
                // Click anywhere on alert → dismiss
                DialogAction::Dismissed
            }
            DialogType::Confirm { message: _ } => {
                let dialog_w = 30u16.min(area.width.saturating_sub(4)).max(16);
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_h = 7;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                let opt_yes = "Yes";
                let opt_no = "No";
                let gap: u16 = 4;
                let total_w = opt_yes.len() as u16 + gap + opt_no.len() as u16;
                let opts_x = dialog_x + dialog_w.saturating_sub(total_w) / 2;
                let opts_y = dialog_y + 4;

                // Check within dialog bounds
                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                // Check Yes
                if y_click == opts_y {
                    if x >= opts_x && x < opts_x + opt_yes.len() as u16 {
                        instance.selected = 0;
                        return DialogAction::Confirmed;
                    }
                    // Check No
                    let no_x = opts_x + opt_yes.len() as u16 + gap;
                    if x >= no_x && x < no_x + opt_no.len() as u16 {
                        instance.selected = 1;
                        return DialogAction::Confirmed;
                    }
                }

                DialogAction::Consumed
            }
            DialogType::ThemeList {
                themes,
                current: _,
                filter,
            } => {
                // Recompute dialog geometry (same as render)
                let filtered: Vec<&str> = if filter.is_empty() {
                    themes.iter().map(String::as_str).collect()
                } else {
                    let lower = filter.to_lowercase();
                    themes
                        .iter()
                        .filter(|t| t.to_lowercase().contains(&lower))
                        .map(String::as_str)
                        .collect()
                };

                let selection = if instance.selected >= filtered.len() {
                    filtered.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                let max_w = 40u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(24).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(filtered.len().max(1)).clamp(1, 10);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);

                // Check outside dialog
                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                // Check "esc" label (top-right)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                // esc is on first line (dialog_y)
                if y_click == dialog_y && x >= esc_x && x < esc_x + esc_label.len() as u16 {
                    return DialogAction::Dismissed;
                }

                // Check list items
                let list_top = dialog_y + 3;

                let scroll_offset = if selection >= max_visible {
                    selection - max_visible + 1
                } else {
                    0
                };
                let scroll_offset = scroll_offset.min(filtered.len().saturating_sub(max_visible));

                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible {
                        let item_idx = scroll_offset + row;
                        if item_idx < filtered.len() {
                            instance.selected = item_idx;
                            return DialogAction::Confirmed;
                        }
                    }
                }

                DialogAction::Consumed
            }
            DialogType::ApiKeyInput { .. } => {
                // Click outside the dialog box → dismiss
                let dialog_w = 50u16.min(area.width.saturating_sub(8)).max(30);
                let dialog_h = 7;
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }
                DialogAction::Consumed
            }
            DialogType::Shortcuts { scroll } => {
                let max_w = 46u16.min(area.width.saturating_sub(6)).max(30);
                let entries = SHORTCUTS.len();
                let max_visible = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible.min(entries).max(1).clamp(1, 20);
                let list_h = max_visible as u16;
                let dialog_h = 1 + list_h;
                let dialog_w = max_w;
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                // Click outside → dismiss
                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                // Check esc label click (title row)
                let esc_x = dialog_x
                    + dialog_w
                        .saturating_sub(4)
                        .saturating_sub("esc".len() as u16);
                if y_click == dialog_y && x >= esc_x && x < esc_x + "esc".len() as u16 {
                    return DialogAction::Dismissed;
                }

                // Scroll on click inside list area
                let list_top = dialog_y + 1;
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible {
                        // Toggle: click on upper half = scroll up, lower half = scroll down
                        if row <= max_visible / 2 {
                            *scroll = scroll.saturating_sub(1);
                        } else {
                            let max_scroll = entries.saturating_sub(max_visible);
                            *scroll = (*scroll + 1).min(max_scroll);
                        }
                    }
                }

                DialogAction::Consumed
            }
            DialogType::ModelList {
                models,
                current: _,
                filter,
            } => {
                // Group models by provider (same as render)
                let mut grouped: BTreeMap<String, Vec<&ModelEntry>> = BTreeMap::new();
                for entry in models {
                    if filter.is_empty()
                        || entry.model.to_lowercase().contains(&filter.to_lowercase())
                    {
                        grouped
                            .entry(entry.provider.clone())
                            .or_default()
                            .push(entry);
                    }
                }
                let flat_entries: Vec<&ModelEntry> = grouped.values().flatten().copied().collect();
                let selection = if instance.selected >= flat_entries.len() {
                    flat_entries.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                let max_w = 50u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(30).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let total_items = flat_entries.len() + grouped.len();
                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(total_items.max(1)).max(1);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);

                // Check outside dialog
                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                // Check "esc" label (top-right)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                if y_click == dialog_y && x >= esc_x && x < esc_x + esc_label.len() as u16 {
                    return DialogAction::Dismissed;
                }

                // Build visual items (headers + models) to map Y to model index
                let mut visual_list: Vec<(&str, bool)> = Vec::new(); // (label, is_model)
                for (provider, entries) in &grouped {
                    visual_list.push((provider.as_str(), false));
                    for entry in entries {
                        visual_list.push((&entry.model, true));
                    }
                }

                let list_top = dialog_y + 3;

                // Compute scroll offset based on visual_selection
                // Simplify: just iterate visible rows
                // We need to map the visual list to rows, accounting for scroll
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible {
                        // Compute scroll offset by finding the visual row of current selection
                        let mut model_idx = 0;
                        let mut visual_to_model = Vec::new();
                        for (_, is_model) in &visual_list {
                            if *is_model {
                                visual_to_model.push(model_idx);
                                model_idx += 1;
                            } else {
                                visual_to_model.push(usize::MAX); // header
                            }
                        }

                        let visual_selection = {
                            let mut pos = 0;
                            let mut seen = 0;
                            for (_, is_model) in &visual_list {
                                if *is_model {
                                    if seen == selection {
                                        break;
                                    }
                                    seen += 1;
                                }
                                pos += 1;
                            }
                            pos.min(visual_list.len().saturating_sub(1))
                        };

                        let scroll_offset =
                            if visual_selection >= max_visible && max_visible < visual_list.len() {
                                visual_selection.saturating_sub(max_visible - 1)
                            } else {
                                0
                            };
                        let scroll_offset =
                            scroll_offset.min(visual_list.len().saturating_sub(max_visible));

                        let vis_idx = scroll_offset + row;
                        if vis_idx < visual_list.len() && visual_to_model[vis_idx] != usize::MAX {
                            instance.selected = visual_to_model[vis_idx];
                            return DialogAction::Confirmed;
                        }
                    }
                }

                DialogAction::Consumed
            }
        }
    }

    #[allow(clippy::too_many_lines, clippy::similar_names)]
    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        theme: &Theme,
        now: SystemTime,
    ) {
        let Some(instance) = self.stack.last() else {
            return;
        };

        let dialog_w = 50.min(area.width.saturating_sub(4));
        let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

        match &instance.dialog_type {
            DialogType::Alert { message } => {
                let dialog_h = 5;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let mut bg = BoxRenderable::new();
                bg.set_background_color(Some(theme.background_element.into()));
                bg.set_border_color(Some(theme.border_active.into()));
                bg.render_self(buf, dialog_area);

                draw_text_line(
                    buf,
                    message,
                    dialog_x + 2,
                    dialog_y + 1,
                    dialog_w.saturating_sub(4),
                    Style::default().fg(rgba_color(theme.text)),
                );

                let ok_text = "[ OK ]";
                let ok_x = dialog_x + dialog_w.saturating_sub(ok_text.len() as u16) / 2;
                let ok_style = Style::default().fg(rgba_color(theme.primary));
                draw_text_line(
                    buf,
                    ok_text,
                    ok_x,
                    dialog_y + 3,
                    dialog_w.saturating_sub(2),
                    ok_style,
                );
            }
            DialogType::Confirm { message } => {
                // Box dimensions: border OUTER edge
                let dialog_w = 30u16.min(area.width.saturating_sub(4)).max(16);
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_h = 7;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                // Draw border using theme color
                let border_color = rgba_color(theme.border_active);
                let max_x = dialog_x + dialog_w - 1;
                let max_y = dialog_y + dialog_h - 1;

                // Top & bottom horizontal lines
                for x in (dialog_x + 1)..max_x {
                    if let Some(cell) = buf.cell_mut((x, dialog_y)) {
                        cell.set_char('\u{2500}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                    if let Some(cell) = buf.cell_mut((x, max_y)) {
                        cell.set_char('\u{2500}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                }

                // Left & right vertical lines
                for y in (dialog_y + 1)..max_y {
                    if let Some(cell) = buf.cell_mut((dialog_x, y)) {
                        cell.set_char('\u{2502}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                    if let Some(cell) = buf.cell_mut((max_x, y)) {
                        cell.set_char('\u{2502}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                }

                // Corners (rounded)
                if let Some(cell) = buf.cell_mut((dialog_x, dialog_y)) {
                    cell.set_char('\u{256D}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((max_x, dialog_y)) {
                    cell.set_char('\u{256E}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((dialog_x, max_y)) {
                    cell.set_char('\u{2570}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((max_x, max_y)) {
                    cell.set_char('\u{256F}');
                    cell.set_style(Style::default().fg(border_color));
                }

                // Content is INSIDE the border (1 row padding top/bottom)
                // Center the message at row dialog_y + 2
                let msg_x = dialog_x + (dialog_w.saturating_sub(message.len() as u16)) / 2;
                draw_text_line(
                    buf,
                    message,
                    msg_x,
                    dialog_y + 2,
                    dialog_w.saturating_sub(2),
                    Style::default().fg(rgba_color(theme.text)),
                );

                // Yes / No side by side, centered at row dialog_y + 4
                let opt_yes = "Yes";
                let opt_no = "No";
                let gap: u16 = 4;
                let total_w = opt_yes.len() as u16 + gap + opt_no.len() as u16;
                let opts_x = dialog_x + dialog_w.saturating_sub(total_w) / 2;
                let opts_y = dialog_y + 4;

                let yes_style = if instance.selected == 0 {
                    Style::default()
                        .fg(rgba_color(theme.primary))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(rgba_color(theme.text_muted))
                };
                draw_text_line(
                    buf,
                    opt_yes,
                    opts_x,
                    opts_y,
                    opt_yes.len() as u16,
                    yes_style,
                );

                let no_style = if instance.selected == 1 {
                    Style::default()
                        .fg(rgba_color(theme.primary))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(rgba_color(theme.text_muted))
                };
                draw_text_line(
                    buf,
                    opt_no,
                    opts_x + opt_yes.len() as u16 + gap,
                    opts_y,
                    opt_no.len() as u16,
                    no_style,
                );
            }
            DialogType::ThemeList {
                themes,
                current,
                filter,
            } => {
                // Compute filtered list (like fuzzysort in original)
                let filtered: Vec<&str> = if filter.is_empty() {
                    themes.iter().map(String::as_str).collect()
                } else {
                    let lower = filter.to_lowercase();
                    themes
                        .iter()
                        .filter(|t| t.to_lowercase().contains(&lower))
                        .map(String::as_str)
                        .collect()
                };

                let selection = if instance.selected >= filtered.len() {
                    filtered.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                // Responsive sizing: shrink with terminal, minimum 24 cols
                let max_w = 40u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(24).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                // Fit list to available height
                // Layout: 1 title + 1 filter + 1 gap + max_visible items + 1 paddingBottom = max_visible + 4
                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(filtered.len().max(1)).clamp(1, 10);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background (NO border - original DialogSelect has no border)
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Header area (paddingLeft=4, paddingRight=4 like original)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: Title (bold, like original TextAttributes.BOLD) + "esc" label (right-aligned, muted)
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Themes", header_x, dialog_y, header_w, title_style);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 1: Filter input (background_element bg to match dialog, textMuted fg)
                let bg_element = rgba_color(theme.background_element);
                for cx in header_x..header_x + header_w {
                    if let Some(cell) = buf.cell_mut((cx, dialog_y + 1)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_element));
                    }
                }

                // Use the reusable cursor component (no blur behavior; always focused)
                let cursor_state = instance.cursor.current_state(now);

                // Show "Search" when empty, otherwise show filter text + cursor
                let has_filter = !filter.is_empty();
                if has_filter {
                    draw_text_line(
                        buf,
                        filter.as_str(),
                        header_x,
                        dialog_y + 1,
                        header_w,
                        Style::default().fg(rgba_color(theme.text)).bg(bg_element),
                    );
                    // Blinking cursor at end of filter text
                    let cursor_x = header_x + filter.len() as u16;
                    if cursor_x < header_x + header_w
                        && let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1))
                    {
                        match cursor_state {
                            CursorState::On => {
                                cell.set_char('\u{2588}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.primary))
                                        .bg(bg_element),
                                );
                            }
                            CursorState::Off | CursorState::Blur => {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                } else {
                    // Show "Search" label when filter is empty
                    let search_label = "Search";
                    draw_text_line(
                        buf,
                        search_label,
                        header_x,
                        dialog_y + 1,
                        header_w,
                        Style::default()
                            .fg(rgba_color(theme.text_muted))
                            .bg(bg_element),
                    );
                    // Cursor AFTER "Search"
                    let cursor_x = header_x + search_label.len() as u16;
                    if cursor_x < header_x + header_w
                        && let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1))
                    {
                        match cursor_state {
                            CursorState::On => {
                                cell.set_char('\u{2588}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.primary))
                                        .bg(bg_element),
                                );
                            }
                            CursorState::Off | CursorState::Blur => {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                }

                // Line 2: Gap (empty, matches original gap={1})

                // Lines 3+: Theme list (paddingLeft=1, paddingRight=1 like original scrollbox)
                // After list: paddingBottom=1 (line dialog_y + 3 + max_visible)
                let list_top = dialog_y + 3;
                let list_pad = 1; // original scrollbox paddingLeft=1
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);

                if filtered.is_empty() {
                    draw_text_line(
                        buf,
                        "No matching themes",
                        list_x,
                        list_top,
                        list_w,
                        Style::default().fg(rgba_color(theme.text_muted)),
                    );
                } else {
                    let bg_element = rgba_color(theme.background_element);
                    // Scroll offset: keep selection visible
                    let scroll_offset = if selection >= max_visible {
                        selection - max_visible + 1
                    } else {
                        0
                    };
                    let scroll_offset =
                        scroll_offset.min(filtered.len().saturating_sub(max_visible));
                    for (vis_idx, &theme_name) in filtered
                        .iter()
                        .enumerate()
                        .skip(scroll_offset)
                        .take(max_visible)
                    {
                        let ry = list_top + (vis_idx - scroll_offset) as u16;
                        let is_current = theme_name == current.as_str();
                        let is_selected = vis_idx == selection;

                        // Draw full row background first
                        if is_selected {
                            for cx in list_x..list_x + list_w {
                                if let Some(cell) = buf.cell_mut((cx, ry)) {
                                    cell.set_char(' ');
                                    cell.set_style(Style::default().bg(rgba_color(theme.primary)));
                                }
                            }
                        } else {
                            // Match dialog background — NOT Color::Reset (which is terminal black)
                            for cx in list_x..list_x + list_w {
                                if let Some(cell) = buf.cell_mut((cx, ry)) {
                                    cell.set_char(' ');
                                    cell.set_style(Style::default().bg(bg_element));
                                }
                            }
                        }

                        // Indicator: ● (U+25cf) with accent color for current theme (changes with preview)
                        // For selected: use contrast color; for non-selected current: use accent
                        let (indicator_fg, indicator_ch) = if is_current {
                            if is_selected {
                                // Selected + current: ● uses contrast foreground
                                let (pr, pg, pb, _) = theme.primary.to_ints();
                                let lum = (0.299 * f32::from(pr)
                                    + 0.587 * f32::from(pg)
                                    + 0.114 * f32::from(pb))
                                    / 255.0;
                                (
                                    if lum > 0.5 {
                                        Color::Rgb(0, 0, 0)
                                    } else {
                                        Color::Rgb(255, 255, 255)
                                    },
                                    "\u{25cf}",
                                )
                            } else {
                                // Current but not selected: ● uses accent color (changes with theme preview)
                                (rgba_color(theme.accent), "\u{25cf}")
                            }
                        } else {
                            (Color::Reset, " ")
                        };

                        // Draw ● indicator with its color
                        if let Some(cell) = buf.cell_mut((list_x, ry)) {
                            cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                            cell.set_style(Style::default().fg(indicator_fg).bg(if is_selected {
                                rgba_color(theme.primary)
                            } else {
                                bg_element
                            }));
                        }
                        // Space after indicator
                        if let Some(cell) = buf.cell_mut((list_x + 1, ry)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(if is_selected {
                                rgba_color(theme.primary)
                            } else {
                                bg_element
                            }));
                        }

                        // Theme name
                        let (name_fg, name_bg) = if is_selected {
                            let (pr, pg, pb, _) = theme.primary.to_ints();
                            let lum = (0.299 * f32::from(pr)
                                + 0.587 * f32::from(pg)
                                + 0.114 * f32::from(pb))
                                / 255.0;
                            (
                                if lum > 0.5 {
                                    Color::Rgb(0, 0, 0)
                                } else {
                                    Color::Rgb(255, 255, 255)
                                },
                                rgba_color(theme.primary),
                            )
                        } else {
                            (rgba_color(theme.text), bg_element)
                        };
                        draw_text_line(
                            buf,
                            theme_name,
                            list_x + 2,
                            ry,
                            list_w.saturating_sub(2),
                            Style::default().fg(name_fg).bg(name_bg),
                        );
                    }
                }
                // Lines after list: paddingBottom=1 (already filled with background)
                // NO footer, NO separator - matching original DialogSelect
            }
            DialogType::Shortcuts { scroll } => {
                // Shortcuts overlay - scrollable list of keyboard shortcuts (no border)
                let max_w = 46u16.min(area.width.saturating_sub(6)).max(30);
                let entries = SHORTCUTS.len();
                let max_visible = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible.min(entries).max(1).clamp(1, 20);
                let list_h = max_visible as u16;
                // Title row + list rows (no border)
                let dialog_h = 1 + list_h;
                let dialog_w = max_w;
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                // Fill background with a subtly lighter shade than theme background
                let (r, g, b, _) = theme.background_element.to_ints();
                let lighten = |c: u8| c.saturating_add(5);
                let bg_color = Color::Rgb(lighten(r), lighten(g), lighten(b));
                for y in dialog_y..dialog_y + dialog_h {
                    for x in dialog_x..dialog_x + dialog_w {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Title line with inline "esc" label
                let title_text = "Keyboard Shortcuts";
                draw_text_line(
                    buf,
                    title_text,
                    dialog_x + 2,
                    dialog_y,
                    dialog_w.saturating_sub(4),
                    Style::default()
                        .fg(rgba_color(theme.text))
                        .add_modifier(Modifier::BOLD),
                );

                // "esc" label right-aligned on same line as title
                let esc_label = "esc";
                let esc_x = dialog_x
                    + dialog_w
                        .saturating_sub(4)
                        .saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y,
                    dialog_w.saturating_sub(2),
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Ensure scroll is within bounds
                let max_scroll = entries.saturating_sub(max_visible);
                let scroll = (*scroll).min(max_scroll);

                let text_color = rgba_color(theme.text);
                let accent = rgba_color(theme.primary);

                // Draw each visible shortcut
                for (i, entry) in SHORTCUTS.iter().enumerate().skip(scroll).take(max_visible) {
                    let ry = dialog_y + 1 + (i - scroll) as u16;
                    let (key_str, desc) = (entry.0, entry.1);

                    // Key column (left-aligned, accent color, fixed width)
                    let key_x = dialog_x + 2;
                    let key_w = 14u16;
                    draw_text_line(buf, key_str, key_x, ry, key_w, Style::default().fg(accent));

                    // Description column
                    let desc_x = key_x + key_w;
                    let desc_w = dialog_w.saturating_sub(2).saturating_sub(desc_x - dialog_x);
                    draw_text_line(
                        buf,
                        desc,
                        desc_x,
                        ry,
                        desc_w,
                        Style::default().fg(text_color),
                    );
                }
            }
            DialogType::ApiKeyInput {
                provider,
                env_var,
                input,
                cursor_pos,
            } => {
                // API Key input dialog - small centered box
                let dialog_w = 50u16.min(area.width.saturating_sub(8)).max(30);
                let dialog_h = 7;
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;
                let _dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background with theme background_element color
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_y..dialog_y + dialog_h {
                    for x in dialog_x..dialog_x + dialog_w {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Draw border (rounded corners via unicode)
                let border_color = rgba_color(theme.border_active);
                let max_x = dialog_x + dialog_w - 1;
                let max_y = dialog_y + dialog_h - 1;

                // Top & bottom horizontal lines
                for x in (dialog_x + 1)..max_x {
                    if let Some(cell) = buf.cell_mut((x, dialog_y)) {
                        cell.set_char('\u{2500}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                    if let Some(cell) = buf.cell_mut((x, max_y)) {
                        cell.set_char('\u{2500}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                }

                // Left & right vertical lines
                for y in (dialog_y + 1)..max_y {
                    if let Some(cell) = buf.cell_mut((dialog_x, y)) {
                        cell.set_char('\u{2502}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                    if let Some(cell) = buf.cell_mut((max_x, y)) {
                        cell.set_char('\u{2502}');
                        cell.set_style(Style::default().fg(border_color));
                    }
                }

                // Corners (rounded)
                if let Some(cell) = buf.cell_mut((dialog_x, dialog_y)) {
                    cell.set_char('\u{256D}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((max_x, dialog_y)) {
                    cell.set_char('\u{256E}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((dialog_x, max_y)) {
                    cell.set_char('\u{2570}');
                    cell.set_style(Style::default().fg(border_color));
                }
                if let Some(cell) = buf.cell_mut((max_x, max_y)) {
                    cell.set_char('\u{256F}');
                    cell.set_style(Style::default().fg(border_color));
                }

                // Content area
                let content_x = dialog_x + 2;
                let content_w = dialog_w.saturating_sub(4);

                // Title: "API Key for {provider}"
                let title = format!("API Key for {provider}");
                draw_text_line(
                    buf,
                    &title,
                    content_x,
                    dialog_y + 1,
                    content_w,
                    Style::default()
                        .fg(rgba_color(theme.text))
                        .add_modifier(Modifier::BOLD),
                );

                // Env var name
                let env_label = format!("({env_var})");
                draw_text_line(
                    buf,
                    &env_label,
                    content_x,
                    dialog_y + 2,
                    content_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Input field with masked characters (blinking cursor at cursor_pos)
                let input_x = content_x;
                let input_y = dialog_y + 4;
                let bg_element = rgba_color(theme.background_element);

                // Clear input field background
                for cx in input_x..input_x + content_w {
                    if let Some(cell) = buf.cell_mut((cx, input_y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_element));
                    }
                }

                // Draw masked input (*** characters), char by char so cursor can be positioned
                let masked: Vec<char> = input.chars().map(|_| '*').collect();

                // Cursor's terminal_focused is synced from app.rs before render
                let cursor_state = instance.cursor.current_state(now);

                // Draw each masked character
                for (i, _ch) in masked.iter().enumerate() {
                    let cx = input_x + i as u16;
                    if cx >= input_x + content_w {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, input_y)) {
                        cell.set_char('*');
                        cell.set_style(Style::default().fg(rgba_color(theme.text)).bg(bg_element));
                    }
                }

                // Draw cursor at cursor_pos
                let cursor_x = input_x + *cursor_pos as u16;
                if cursor_x < input_x + content_w
                    && let Some(cell) = buf.cell_mut((cursor_x, input_y))
                {
                    match cursor_state {
                        CursorState::On => {
                            // ON: block cursor with primary color
                            cell.set_char('\u{2588}');
                            cell.set_style(
                                Style::default()
                                    .fg(rgba_color(theme.primary))
                                    .bg(bg_element),
                            );
                        }
                        CursorState::Off | CursorState::Blur => {
                            // OFF/Blur: dimmed block cursor
                            cell.set_char('\u{2588}');
                            cell.set_style(
                                Style::default().fg(Color::Rgb(60, 60, 60)).bg(bg_element),
                            );
                        }
                    }
                }
            }
            DialogType::ModelList {
                models,
                current,
                filter,
            } => {
                // Group models by provider and filter

                let mut grouped: BTreeMap<String, Vec<&ModelEntry>> = BTreeMap::new();
                for entry in models {
                    if filter.is_empty()
                        || entry.model.to_lowercase().contains(&filter.to_lowercase())
                    {
                        grouped
                            .entry(entry.provider.clone())
                            .or_default()
                            .push(entry);
                    }
                }

                // Flatten grouped models into a single list for selection
                let flat_entries: Vec<&ModelEntry> = grouped.values().flatten().copied().collect();

                let selection = if instance.selected >= flat_entries.len() {
                    flat_entries.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                // Responsive sizing: shrink with terminal, minimum 24 cols
                let max_w = 50u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(30).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                // Calculate total items (models + provider headers)
                let total_items = flat_entries.len() + grouped.len();

                // Fit list to available height
                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(total_items.max(1)).max(1);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background (NO border - original DialogSelect has no border)
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Header area (paddingLeft=4, paddingRight=4 like original)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: Title (bold, like original TextAttributes.BOLD) + "esc" label (right-aligned, muted)
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Models", header_x, dialog_y, header_w, title_style);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 1: Filter input (background_element bg to match dialog, textMuted fg)
                let bg_element = rgba_color(theme.background_element);
                for cx in header_x..header_x + header_w {
                    if let Some(cell) = buf.cell_mut((cx, dialog_y + 1)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_element));
                    }
                }

                // Use the reusable cursor component (no blur behavior; always focused)
                let cursor_state = instance.cursor.current_state(now);

                // Show "Search" when empty, otherwise show filter text + cursor
                let has_filter = !filter.is_empty();
                if has_filter {
                    draw_text_line(
                        buf,
                        filter.as_str(),
                        header_x,
                        dialog_y + 1,
                        header_w,
                        Style::default().fg(rgba_color(theme.text)).bg(bg_element),
                    );
                    // Blinking cursor at end of filter text
                    let cursor_x = header_x + filter.len() as u16;
                    if cursor_x < header_x + header_w
                        && let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1))
                    {
                        match cursor_state {
                            CursorState::On => {
                                cell.set_char('\u{2588}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.primary))
                                        .bg(bg_element),
                                );
                            }
                            CursorState::Off | CursorState::Blur => {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                } else {
                    // Show "Search" label when filter is empty
                    let search_label = "Search";
                    draw_text_line(
                        buf,
                        search_label,
                        header_x,
                        dialog_y + 1,
                        header_w,
                        Style::default()
                            .fg(rgba_color(theme.text_muted))
                            .bg(bg_element),
                    );
                    // Cursor AFTER "Search"
                    let cursor_x = header_x + search_label.len() as u16;
                    if cursor_x < header_x + header_w
                        && let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1))
                    {
                        match cursor_state {
                            CursorState::On => {
                                cell.set_char('\u{2588}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.primary))
                                        .bg(bg_element),
                                );
                            }
                            CursorState::Off | CursorState::Blur => {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                }

                // Line 2: Gap (empty, matches original gap={1})

                // Lines 3+: Model list grouped by provider
                let list_top = dialog_y + 3;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);

                if flat_entries.is_empty() {
                    draw_text_line(
                        buf,
                        "No matching models",
                        list_x,
                        list_top,
                        list_w,
                        Style::default().fg(rgba_color(theme.text_muted)),
                    );
                } else {
                    let bg_element = rgba_color(theme.background_element);
                    let mut current_y = list_top;

                    // Build visual items: headers + models
                    // Each visual item is either a header or a model
                    // We need to map selection (model index) to visual index
                    let mut visual_items: Vec<VisualItem> = Vec::new();
                    for (provider, entries) in &grouped {
                        visual_items.push(VisualItem::Header(provider.clone()));
                        for entry in entries {
                            visual_items.push(VisualItem::Model((*entry).clone()));
                        }
                    }

                    // Map selection to visual index
                    // Selection is a model index (0..flat_entries.len())
                    // Visual index includes headers
                    let mut model_count = 0;
                    let mut visual_selection = 0;
                    for item in &visual_items {
                        match item {
                            VisualItem::Header(_) => {
                                if model_count <= selection {
                                    visual_selection += 1;
                                }
                            }
                            VisualItem::Model(_) => {
                                if model_count == selection {
                                    visual_selection += 1;
                                    break;
                                }
                                visual_selection += 1;
                                model_count += 1;
                            }
                        }
                    }

                    // Calculate scroll offset
                    let mut scroll_offset = 0;
                    if visual_selection >= max_visible && max_visible < visual_items.len() {
                        scroll_offset = visual_selection.saturating_sub(max_visible - 1);
                    }
                    scroll_offset =
                        scroll_offset.min(visual_items.len().saturating_sub(max_visible));

                    // Draw visible items
                    let mut model_index = 0;
                    for (visual_index, item) in visual_items.iter().enumerate() {
                        match item {
                            VisualItem::Header(provider) => {
                                if visual_index >= scroll_offset
                                    && (visual_index - scroll_offset) < max_visible
                                {
                                    let header_style = Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .add_modifier(Modifier::BOLD);
                                    draw_text_line(
                                        buf,
                                        provider,
                                        list_x,
                                        current_y,
                                        list_w,
                                        header_style,
                                    );
                                    current_y += 1;
                                }
                            }
                            VisualItem::Model(entry) => {
                                let is_current = entry.model == current.as_str();
                                let is_selected = model_index == selection;

                                if visual_index >= scroll_offset
                                    && (visual_index - scroll_offset) < max_visible
                                {
                                    // Draw full row background first
                                    if is_selected {
                                        for cx in list_x..list_x + list_w {
                                            if let Some(cell) = buf.cell_mut((cx, current_y)) {
                                                cell.set_char(' ');
                                                cell.set_style(
                                                    Style::default().bg(rgba_color(theme.primary)),
                                                );
                                            }
                                        }
                                    } else {
                                        for cx in list_x..list_x + list_w {
                                            if let Some(cell) = buf.cell_mut((cx, current_y)) {
                                                cell.set_char(' ');
                                                cell.set_style(Style::default().bg(bg_element));
                                            }
                                        }
                                    }

                                    // Indicator: ● (U+25cf) with accent color for current model
                                    let (indicator_fg, indicator_ch) = if is_current {
                                        if is_selected {
                                            let (pr, pg, pb, _) = theme.primary.to_ints();
                                            let lum = (0.299 * f32::from(pr)
                                                + 0.587 * f32::from(pg)
                                                + 0.114 * f32::from(pb))
                                                / 255.0;
                                            (
                                                if lum > 0.5 {
                                                    Color::Rgb(0, 0, 0)
                                                } else {
                                                    Color::Rgb(255, 255, 255)
                                                },
                                                "\u{25cf}",
                                            )
                                        } else {
                                            (rgba_color(theme.accent), "\u{25cf}")
                                        }
                                    } else {
                                        (Color::Reset, " ")
                                    };

                                    // Draw ● indicator with its color
                                    if let Some(cell) = buf.cell_mut((list_x, current_y)) {
                                        cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                                        cell.set_style(Style::default().fg(indicator_fg).bg(
                                            if is_selected {
                                                rgba_color(theme.primary)
                                            } else {
                                                bg_element
                                            },
                                        ));
                                    }
                                    // Space after indicator
                                    if let Some(cell) = buf.cell_mut((list_x + 1, current_y)) {
                                        cell.set_char(' ');
                                        cell.set_style(Style::default().bg(if is_selected {
                                            rgba_color(theme.primary)
                                        } else {
                                            bg_element
                                        }));
                                    }

                                    // Model name
                                    let (name_fg, name_bg) = if is_selected {
                                        let (pr, pg, pb, _) = theme.primary.to_ints();
                                        let lum = (0.299 * f32::from(pr)
                                            + 0.587 * f32::from(pg)
                                            + 0.114 * f32::from(pb))
                                            / 255.0;
                                        (
                                            if lum > 0.5 {
                                                Color::Rgb(0, 0, 0)
                                            } else {
                                                Color::Rgb(255, 255, 255)
                                            },
                                            rgba_color(theme.primary),
                                        )
                                    } else {
                                        (rgba_color(theme.text), bg_element)
                                    };
                                    draw_text_line(
                                        buf,
                                        &entry.model,
                                        list_x + 2,
                                        current_y,
                                        list_w.saturating_sub(2),
                                        Style::default().fg(name_fg).bg(name_bg),
                                    );

                                    current_y += 1;
                                }
                                model_index += 1;
                            }
                        }
                    }
                }
                // Lines after list: paddingBottom=1 (already filled with background)
                // NO footer, NO separator - matching original DialogSelect
            }
        }
    }
}
