use std::collections::BTreeMap;
use std::time::SystemTime;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh::ModelEntry;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;

use crate::component::cursor::{Cursor, CursorState};
use crate::theme::{Theme, rgba_color};

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
    ("Ctrl+B", "Toggle sidebar (session history)"),
    ("Ctrl+C", "Toggle conceal (hide assistant text)"),
    ("Ctrl+T", "Toggle thinking (show/hide reasoning)"),
    ("Ctrl+D", "Toggle tool details (show/hide completed)"),
    ("Ctrl+G", "Toggle generic tool output"),
    ("Ctrl+\u{2191}/\u{2193}", "Prompt history"),
    ("\u{2190}/\u{2192}", "Right panel history (focused slot)"),
    ("Alt+\u{2190}/\u{2192}", "Switch agent queue (right panel)"),
    ("Shift+B", "Previous agent queue (panel focused)"),
    ("Shift+N", "Next agent queue (panel focused)"),
];

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

/// Borderless text prompt panel: bold title with an "esc" hint on the first
/// content row, the input line with the blinking cursor, and an "enter
/// submit" hint at the bottom — one blank padding row above and below.
fn render_rename_session_dialog(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    now: SystemTime,
    cursor: &Cursor,
    input: &str,
    cursor_pos: usize,
) {
    let dialog_w = 50u16.min(area.width.saturating_sub(8)).max(30);
    let dialog_h = 8; // 1 padding + header + gap + input + gap + hint + 1 padding
    let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
    let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

    // Solid background panel — the same color as the history sidebar, a bare
    // floating surface with no border characters.
    let bg_color = rgba_color(theme.background_panel);
    for y in dialog_y..dialog_y + dialog_h {
        for x in dialog_x..dialog_x + dialog_w {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(
                    Style::default()
                        .bg(bg_color)
                        .remove_modifier(Modifier::all()),
                );
                cell.set_diff_option(CellDiffOption::None);
            }
        }
    }

    let content_x = dialog_x + 2;
    let content_w = dialog_w.saturating_sub(4);

    // Header row (below the top padding): bold title left, muted "esc" right.
    let header_y = dialog_y + 1;
    draw_text_line(
        buf,
        "Rename Session",
        content_x,
        header_y,
        content_w,
        Style::default()
            .fg(rgba_color(theme.text))
            .add_modifier(Modifier::BOLD),
    );
    let esc_hint = "esc";
    draw_text_line(
        buf,
        esc_hint,
        dialog_x + dialog_w - 2 - esc_hint.len() as u16,
        header_y,
        esc_hint.len() as u16,
        Style::default().fg(rgba_color(theme.text_muted)),
    );

    // Input row with the blinking block cursor, one blank row below the
    // header.
    let input_y = dialog_y + 3;
    let cursor_state = cursor.current_state(now);
    let display_chars: Vec<char> = input.chars().collect();
    for (i, ch) in display_chars.iter().enumerate() {
        let cx = content_x + i as u16;
        if cx >= content_x + content_w {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, input_y)) {
            cell.set_char(*ch);
            cell.set_style(Style::default().fg(rgba_color(theme.text)).bg(bg_color));
        }
    }
    let cursor_x = content_x + cursor_pos.min(input.len()) as u16;
    if cursor_x < content_x + content_w
        && let Some(cell) = buf.cell_mut((cursor_x, input_y))
    {
        match cursor_state {
            CursorState::On => {
                cell.set_char('\u{2588}');
                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg_color));
            }
            CursorState::Off | CursorState::Blur => {
                cell.set_char('\u{2592}');
                cell.set_style(Style::default().fg(Color::Rgb(60, 60, 60)).bg(bg_color));
            }
        }
    }

    // Bottom hint row ("enter" highlighted + muted "submit"), one blank
    // padding row above the panel's bottom edge.
    let submit_y = dialog_y + dialog_h - 2;
    let enter_hint = "enter";
    draw_text_line(
        buf,
        enter_hint,
        content_x,
        submit_y,
        content_w,
        Style::default().fg(rgba_color(theme.text)),
    );
    draw_text_line(
        buf,
        " submit",
        content_x + enter_hint.len() as u16,
        submit_y,
        content_w.saturating_sub(enter_hint.len() as u16),
        Style::default().fg(rgba_color(theme.text_muted)),
    );
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
    /// Sub-dialog shown after picking a model that supports configurable
    /// reasoning: choose `default` / `low` / `medium` / `high`. Pushed on
    /// top of the ModelList; `model`/`provider` carry the picked model.
    ReasoningList {
        model: String,
        provider: String,
        levels: Vec<String>,
        current: String,
    },
    /// Compact two-option picker for the tool-call delivery mode: `native`
    /// (structured parts, default) or `inline` (JSON parsed from text). The
    /// two paths are mutually exclusive at the request level.
    ToolCallList {
        current: String,
    },
    ApiKeyInput {
        provider: String,
        env_var: String,
        input: String,
        cursor_pos: usize,
    },
    /// Base URL entry for a LOCAL provider (no API key). Unmasked input saved
    /// to setup.json instead of the keyring.
    LocalUrlInput {
        provider: String,
        input: String,
        cursor_pos: usize,
    },
    /// Rename the current session: a text prompt prefilled with the current
    /// title, Enter applies, Esc cancels.
    RenameSession {
        input: String,
        cursor_pos: usize,
    },
    /// Create/edit one hook inside a single registration box (same visual
    /// language as the API-key / server-URL inputs). Four labeled fields
    /// share the text-input mechanics: Up/Down switch fields, Enter saves,
    /// Esc cancels.
    HookInput {
        /// Lifecycle event the hook belongs to (`PreToolUse`/`PostToolUse`).
        event: &'static str,
        /// Index of the hook being edited, or `None` when creating one.
        editing_index: Option<usize>,
        name: String,
        matcher: String,
        command: String,
        timeout: String,
        /// Active field: 0 name · 1 matcher · 2 command · 3 timeout.
        field: usize,
        cursor_pos: usize,
    },
    /// Per-message action picker shown when clicking a user message in the
    /// transcript (port of opencode's "Message Actions" dialog): Copy always
    /// available, Revert/Fork only for the last user message. `message_id`
    /// identifies the clicked message.
    MessageActions {
        message_id: String,
        /// Truncated text of the clicked message shown as context.
        preview: String,
        /// Whether this is the last user message in the session (only the
        /// last user message can be reverted/forked — the context item log is
        /// truncated at the message boundary).
        is_last_user_message: bool,
    },

    /// Action picker for a PENDING queued message (the rows above the
    /// prompt): Edit, Delete, Copy. `queue` + `index` identify the clicked
    /// message inside its `VecDeque`.
    QueueActions {
        queue: crate::routes::session::queue_choice::QueueTarget,
        index: usize,
        /// Truncated text of the clicked message shown as context.
        preview: String,
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
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
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

    pub const fn visible(&self) -> bool {
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
            DialogType::ReasoningList { levels, .. } => {
                // Compact list dialog (no filter): same geometry rules as the
                // theme list but sized to the (tiny) level count.
                let max_w = 40u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = levels.len().clamp(1, 10);
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

                // Click on a level row confirms it.
                let list_top = dialog_y + 3;
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible && row < levels.len() {
                        instance.selected = row;
                        return DialogAction::Confirmed;
                    }
                }

                DialogAction::Consumed
            }
            DialogType::MessageActions {
                is_last_user_message,
                ..
            } => {
                // Same compact geometry as the ReasoningList (3 items for last
                // user message, 1 item for older messages).
                let max_w = 64u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = if *is_last_user_message {
                    3usize
                } else {
                    1usize
                };
                let dialog_h = (max_visible + 5) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);

                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                if y_click == dialog_y + 1 && x >= esc_x && x < esc_x + esc_label.len() as u16 {
                    return DialogAction::Dismissed;
                }

                let list_top = dialog_y + 4;
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible {
                        instance.selected = row;
                        return DialogAction::Confirmed;
                    }
                }

                DialogAction::Consumed
            }
            DialogType::QueueActions { .. } => {
                // Identical compact geometry to the render arm (3 items plus
                // 1 line of top padding).
                let max_w = 64u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = 3usize;
                let dialog_h = (max_visible + 5) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);

                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                if y_click == dialog_y + 1 && x >= esc_x && x < esc_x + esc_label.len() as u16 {
                    return DialogAction::Dismissed;
                }

                let list_top = dialog_y + 4;
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < max_visible {
                        instance.selected = row;
                        return DialogAction::Confirmed;
                    }
                }

                DialogAction::Consumed
            }
            DialogType::ToolCallList { current: _ } => {
                // Two-option list with long descriptions: wide enough for
                // "inline  —  JSON written in the text, parsed locally".
                let max_w = 60u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = 2usize;
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);

                if x < dialog_x
                    || x >= dialog_x + dialog_w
                    || y_click < dialog_y
                    || y_click >= dialog_y + dialog_h
                {
                    return DialogAction::Dismissed;
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                if y_click == dialog_y && x >= esc_x && x < esc_x + esc_label.len() as u16 {
                    return DialogAction::Dismissed;
                }

                // Click on a row confirms that option.
                let list_top = dialog_y + 3;
                if y_click >= list_top {
                    let row = (y_click - list_top) as usize;
                    if row < 2 {
                        instance.selected = row;
                        return DialogAction::Confirmed;
                    }
                }

                DialogAction::Consumed
            }
            DialogType::ApiKeyInput { .. } | DialogType::LocalUrlInput { .. } => {
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
            DialogType::HookInput {
                name,
                matcher,
                command,
                timeout,
                field,
                cursor_pos,
                ..
            } => {
                let values = [
                    name.as_str(),
                    matcher.as_str(),
                    command.as_str(),
                    timeout.as_str(),
                ];
                let (dialog_x, dialog_y, dialog_w, dialog_h, ..) = hook_input_metrics(area, values);
                let inside = x >= dialog_x
                    && x < dialog_x + dialog_w
                    && y_click >= dialog_y
                    && y_click < dialog_y + dialog_h;
                if !inside {
                    // Click outside the registration panel → dismiss (same
                    // rule as the other text-input dialogs).
                    return DialogAction::Dismissed;
                }
                // Inside: labels focus the field, value rows also move the
                // insertion point; padding just consumes the click.
                if let Some((clicked_field, char_idx)) =
                    hook_input_hit_field(area, values, x, y_click)
                {
                    *field = clicked_field;
                    *cursor_pos = match char_idx {
                        Some(ci) => byte_at_char(values[clicked_field], ci),
                        None => values[clicked_field].len(),
                    };
                }
                DialogAction::Consumed
            }
            DialogType::RenameSession { .. } => {
                // Click outside the borderless prompt panel → dismiss
                // (matches render_rename_session_dialog geometry).
                let dialog_w = 50u16.min(area.width.saturating_sub(8)).max(30);
                let dialog_h = 8;
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
                let max_w = 59u16.min(area.width.saturating_sub(6)).max(30);
                let entries = SHORTCUTS.len();
                let max_visible = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible.min(entries).max(1).clamp(1, 20);
                let list_h = max_visible as u16;
                let dialog_h = 1 + list_h + 2;
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

                // Scroll on click inside list area
                let list_top = dialog_y + 2;
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
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: SystemTime) {
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

                // Fill interior with theme background
                let bg_color = rgba_color(theme.background);
                for y in dialog_y..dialog_y + dialog_h {
                    for x in dialog_x..dialog_x + dialog_w {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

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
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
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
                                // Semi-transparent cursor with medium shade character
                                cell.set_char('\u{2592}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .bg(bg_element),
                                );
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
                    // Cursor positioned over the first character of the placeholder
                    if let Some(cell) = buf.cell_mut((header_x, dialog_y + 1)) {
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
                                // Semi-transparent cursor with medium shade character
                                cell.set_char('\u{2592}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .bg(bg_element),
                                );
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
                let max_w = 59u16.min(area.width.saturating_sub(6)).max(30);
                let entries = SHORTCUTS.len();
                let max_visible = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible.min(entries).max(1).clamp(1, 20);
                let list_h = max_visible as u16;
                // Title row + list rows + padding top/bottom (no border)
                let dialog_h = 1 + list_h + 2;
                let dialog_w = max_w;
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;
                let dialog_y = area.y + area.height.saturating_sub(dialog_h) / 2;

                // Fill background with the same panel color as the left
                // sidebar, so the dialog matches the app's left panel.
                let bg_color = rgba_color(theme.background_panel);
                for y in dialog_y..dialog_y + dialog_h {
                    for x in dialog_x..dialog_x + dialog_w {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

                // Title line
                let title_text = "Keyboard Shortcuts";
                draw_text_line(
                    buf,
                    title_text,
                    dialog_x + 2,
                    dialog_y + 1,
                    dialog_w.saturating_sub(4),
                    Style::default()
                        .fg(rgba_color(theme.text))
                        .add_modifier(Modifier::BOLD),
                );

                // Ensure scroll is within bounds
                let max_scroll = entries.saturating_sub(max_visible);
                let scroll = (*scroll).min(max_scroll);

                let text_color = rgba_color(theme.text);
                let accent = rgba_color(theme.primary);

                // Draw each visible shortcut
                for (i, entry) in SHORTCUTS.iter().enumerate().skip(scroll).take(max_visible) {
                    let ry = dialog_y + 2 + (i - scroll) as u16;
                    let (key_str, desc) = (entry.0, entry.1);

                    // Key column (left-aligned, accent color, fixed width)
                    let key_x = dialog_x + 2;
                    let key_w = 15u16;
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
                render_text_input_dialog(
                    buf,
                    area,
                    theme,
                    now,
                    &instance.cursor,
                    &format!("API Key for {provider}"),
                    &format!("({env_var})"),
                    true,
                    input,
                    *cursor_pos,
                );
            }
            DialogType::LocalUrlInput {
                provider,
                input,
                cursor_pos,
            } => {
                render_text_input_dialog(
                    buf,
                    area,
                    theme,
                    now,
                    &instance.cursor,
                    &format!("Server URL for {provider}"),
                    "e.g. http://127.0.0.1:8080",
                    false,
                    input,
                    *cursor_pos,
                );
            }
            DialogType::RenameSession { input, cursor_pos } => {
                render_rename_session_dialog(
                    buf,
                    area,
                    theme,
                    now,
                    &instance.cursor,
                    input,
                    *cursor_pos,
                );
            }
            DialogType::HookInput {
                event,
                name,
                matcher,
                command,
                timeout,
                field,
                cursor_pos,
                editing_index,
            } => {
                let title = if editing_index.is_some() {
                    "Edit Hook"
                } else {
                    "Add Hook"
                };
                render_hook_form_dialog(
                    buf,
                    area,
                    theme,
                    now,
                    &instance.cursor,
                    title,
                    event,
                    [name, matcher, command, timeout],
                    *field,
                    *cursor_pos,
                );
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
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
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
                                // Semi-transparent cursor with medium shade character
                                cell.set_char('\u{2592}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .bg(bg_element),
                                );
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
                    // Cursor positioned over the first character of the placeholder
                    if let Some(cell) = buf.cell_mut((header_x, dialog_y + 1)) {
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
                                // Semi-transparent cursor with medium shade character
                                cell.set_char('\u{2592}');
                                cell.set_style(
                                    Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .bg(bg_element),
                                );
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

                                    // Model name (with special styling for "auto")
                                    let is_auto = entry.model == "auto";
                                    let display_name = if is_auto {
                                        "★ auto  (fallback chain)"
                                    } else {
                                        &entry.model
                                    };
                                    let (name_fg, name_bg) = if is_auto && !is_selected {
                                        (rgba_color(theme.accent), bg_element)
                                    } else if is_selected {
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
                                        display_name,
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
            DialogType::ReasoningList {
                model,
                provider: _,
                levels,
                current,
            } => {
                let selection = if instance.selected >= levels.len() {
                    levels.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                // Compact dialog, sized to the small level list.
                let max_w = 40u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = levels.len().clamp(1, 10);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background (same borderless style as the other lists).
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: Title + "esc" label.
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Reasoning", header_x, dialog_y, header_w, title_style);
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

                // Line 1: the picked model (muted, may be long — truncate).
                let model_style = Style::default().fg(rgba_color(theme.text_muted));
                draw_text_line(buf, model, header_x, dialog_y + 1, header_w, model_style);

                // Line 2: gap.

                // Lines 3+: levels.
                let list_top = dialog_y + 3;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);
                let bg_element = rgba_color(theme.background_element);

                for (idx, level) in levels.iter().enumerate() {
                    if idx >= max_visible {
                        break;
                    }
                    let y = list_top + idx as u16;
                    let is_selected = idx == selection;
                    let is_current = level == current;
                    let sel_bg = rgba_color(theme.primary);

                    // Full row background.
                    for cx in list_x..list_x + list_w {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(if is_selected {
                                sel_bg
                            } else {
                                bg_element
                            }));
                        }
                    }

                    // Current-level indicator ●; selected indicator (space).
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
                    if let Some(cell) = buf.cell_mut((list_x, y)) {
                        cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                        cell.set_style(Style::default().fg(indicator_fg).bg(if is_selected {
                            sel_bg
                        } else {
                            bg_element
                        }));
                    }
                    if let Some(cell) = buf.cell_mut((list_x + 1, y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(if is_selected {
                            sel_bg
                        } else {
                            bg_element
                        }));
                    }

                    let label = if level == "default" {
                        "default  (model default)".to_string()
                    } else {
                        level.clone()
                    };
                    let (name_fg, name_bg) = if is_selected {
                        let (pr, pg, pb, _) = theme.primary.to_ints();
                        let lum =
                            (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb))
                                / 255.0;
                        (
                            if lum > 0.5 {
                                Color::Rgb(0, 0, 0)
                            } else {
                                Color::Rgb(255, 255, 255)
                            },
                            sel_bg,
                        )
                    } else {
                        (rgba_color(theme.text), bg_element)
                    };
                    draw_text_line(
                        buf,
                        &label,
                        list_x + 2,
                        y,
                        list_w.saturating_sub(2),
                        Style::default().fg(name_fg).bg(name_bg),
                    );
                }
            }
            DialogType::MessageActions {
                message_id: _,
                preview,
                is_last_user_message,
            } => {
                let selection = if *is_last_user_message {
                    instance.selected.min(2)
                } else {
                    instance.selected.min(0)
                };
                let max_w = 64u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let items = if *is_last_user_message {
                    3usize
                } else {
                    1usize
                };
                let dialog_h = (items + 5) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Solid fill in the same color as the history sidebar panel —
                // a bare floating surface with no border characters, like the
                // rename dialog.
                let bg_color = rgba_color(theme.background_panel);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: padding top.

                // Line 1: bold title + "esc" hint (right-aligned, muted).
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(
                    buf,
                    "Message Actions",
                    header_x,
                    dialog_y + 1,
                    header_w,
                    title_style,
                );
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y + 1,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 2: clicked message preview (muted, truncated).
                draw_text_line(
                    buf,
                    preview,
                    header_x,
                    dialog_y + 2,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 3: gap.

                // Lines 4+: the actions. The selected row is marked
                // with the project's 🞴 indicator and primary color instead
                // of a background swap. Only the last user message can be
                // reverted/forked (the transcript is truncated at that point).
                let list_top = dialog_y + 4;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);
                let options = if *is_last_user_message {
                    vec![
                        ("Revert", "Restore prompt, drop later messages"),
                        ("Copy", "Copy message text to clipboard"),
                        ("Fork", "Branch a new session from here"),
                    ]
                } else {
                    vec![("Copy", "Copy message text to clipboard")]
                };

                for (idx, (name, desc)) in options.iter().enumerate() {
                    let y = list_top + idx as u16;
                    let is_selected = idx == selection;

                    for cx in list_x..list_x + list_w {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }

                    let name_style = if is_selected {
                        Style::default()
                            .fg(rgba_color(theme.primary))
                            .add_modifier(Modifier::BOLD)
                            .bg(bg_color)
                    } else {
                        Style::default().fg(rgba_color(theme.text)).bg(bg_color)
                    };

                    let indicator = if is_selected { "🞴 " } else { "   " };
                    let indicator_x = list_x;
                    draw_text_line(buf, indicator, indicator_x, y, 3, name_style);

                    let name_x = list_x + 3;
                    draw_text_line(buf, name, name_x, y, list_w - 3, name_style);

                    let desc_x = list_x + 15;
                    if desc_x < list_x + list_w {
                        draw_text_line(
                            buf,
                            desc,
                            desc_x,
                            y,
                            list_x + list_w - desc_x,
                            Style::default()
                                .fg(rgba_color(theme.text_muted))
                                .bg(bg_color),
                        );
                    }
                }
            }
            DialogType::QueueActions { preview, .. } => {
                let selection = instance.selected.min(2);
                let max_w = 64u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let items = 3usize;
                // +1 line of content padding at the top, matching the blank
                // line already left below the last option.
                let dialog_h = (items + 5) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Same surface as the left sidebar panel so the box reads as
                // part of the panel family.
                let bg_color = rgba_color(theme.background_panel);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: bold title + "esc" hint (right-aligned, muted).
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(
                    buf,
                    "Queue Actions",
                    header_x,
                    dialog_y + 1,
                    header_w,
                    title_style,
                );
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y + 1,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 2: clicked message preview (muted, truncated).
                draw_text_line(
                    buf,
                    preview,
                    header_x,
                    dialog_y + 2,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 3: gap.

                // Lines 4+: the three actions. The selected row is marked
                // with the project's 🞴 indicator instead of a color swap.
                let list_top = dialog_y + 4;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);
                let options = [
                    ("Edit", "Load the message into the prompt"),
                    ("Delete", "Remove it from the queue"),
                    ("Copy", "Copy message text to clipboard"),
                ];

                for (idx, (name, desc)) in options.iter().enumerate() {
                    let y = list_top + idx as u16;
                    let is_selected = idx == selection;

                    for cx in list_x..list_x + list_w {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }

                    let name_style = if is_selected {
                        Style::default()
                            .fg(rgba_color(theme.primary))
                            .add_modifier(Modifier::BOLD)
                            .bg(bg_color)
                    } else {
                        Style::default().fg(rgba_color(theme.text)).bg(bg_color)
                    };

                    // Indicator takes 2 cols, like the sidebar rows.
                    let ind = if is_selected { "🞴 " } else { "   " };
                    draw_text_line(buf, ind, list_x, y, 3, name_style);

                    let name_x = list_x + 3;
                    draw_text_line(buf, name, name_x, y, list_w - 3, name_style);

                    let desc_x = list_x + 15;
                    if desc_x < list_x + list_w {
                        draw_text_line(
                            buf,
                            desc,
                            desc_x,
                            y,
                            list_x + list_w - desc_x,
                            Style::default()
                                .fg(rgba_color(theme.text_muted))
                                .bg(bg_color),
                        );
                    }
                }
            }
            DialogType::ToolCallList { current } => {
                let selection = if instance.selected >= 2 {
                    1
                } else {
                    instance.selected
                };

                // Same wide geometry as the mouse handler so clicks and the
                // painted rows line up.
                let max_w = 60u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(28).min(area.width.saturating_sub(2));
                let dialog_x = area.x + area.width.saturating_sub(dialog_w) / 2;

                let max_visible = 2usize;
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area
                    .y
                    .saturating_add((area.height.saturating_sub(dialog_h)) / 2);
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(
                                Style::default()
                                    .bg(bg_color)
                                    .remove_modifier(Modifier::all()),
                            );
                            cell.set_diff_option(CellDiffOption::None);
                        }
                    }
                }

                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Tool calls", header_x, dialog_y, header_w, title_style);
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

                // Line 1: explanation (muted).
                draw_text_line(
                    buf,
                    "How the model calls tools",
                    header_x,
                    dialog_y + 1,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                let list_top = dialog_y + 3;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);
                let bg_element = rgba_color(theme.background_element);
                let options = [
                    ("native", "Structured function calls (default)"),
                    ("inline", "JSON written in the text, parsed locally"),
                ];

                for (idx, (name, desc)) in options.iter().enumerate() {
                    let y = list_top + idx as u16;
                    let is_selected = idx == selection;
                    let is_current = *name == current;
                    let sel_bg = rgba_color(theme.primary);

                    for cx in list_x..list_x + list_w {
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(if is_selected {
                                sel_bg
                            } else {
                                bg_element
                            }));
                        }
                    }

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
                    if let Some(cell) = buf.cell_mut((list_x, y)) {
                        cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                        cell.set_style(Style::default().fg(indicator_fg).bg(if is_selected {
                            sel_bg
                        } else {
                            bg_element
                        }));
                    }
                    if let Some(cell) = buf.cell_mut((list_x + 1, y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(if is_selected {
                            sel_bg
                        } else {
                            bg_element
                        }));
                    }

                    let (name_fg, name_bg) = if is_selected {
                        let (pr, pg, pb, _) = theme.primary.to_ints();
                        let lum =
                            (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb))
                                / 255.0;
                        (
                            if lum > 0.5 {
                                Color::Rgb(0, 0, 0)
                            } else {
                                Color::Rgb(255, 255, 255)
                            },
                            sel_bg,
                        )
                    } else {
                        (rgba_color(theme.text), bg_element)
                    };
                    let label = format!("{name}  —  {desc}");
                    draw_text_line(
                        buf,
                        &label,
                        list_x + 2,
                        y,
                        list_w.saturating_sub(2),
                        Style::default().fg(name_fg).bg(name_bg),
                    );
                }
            }
        }
    }
}

/// Shared box for the single-line text input dialogs (API key for cloud
/// providers, server URL for local providers). `masked` draws the input as
/// `*` (sensitive); `subtitle` shows the env var or an URL example.
#[allow(clippy::too_many_arguments)]
fn render_text_input_dialog(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    now: SystemTime,
    cursor: &Cursor,
    title: &str,
    subtitle: &str,
    masked: bool,
    input: &str,
    cursor_pos: usize,
) {
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
                cell.set_style(
                    Style::default()
                        .bg(bg_color)
                        .remove_modifier(Modifier::all()),
                );
                cell.set_diff_option(CellDiffOption::None);
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

    // Title
    draw_text_line(
        buf,
        title,
        content_x,
        dialog_y + 1,
        content_w,
        Style::default()
            .fg(rgba_color(theme.text))
            .add_modifier(Modifier::BOLD),
    );

    // Subtitle (env var or URL example)
    draw_text_line(
        buf,
        subtitle,
        content_x,
        dialog_y + 2,
        content_w,
        Style::default().fg(rgba_color(theme.text_muted)),
    );

    // Input field (blinking cursor at cursor_pos)
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

    // Cursor's terminal_focused is synced from app.rs before render
    let cursor_state = cursor.current_state(now);

    let display_chars: Vec<char> = if masked {
        input.chars().map(|_| '*').collect()
    } else {
        input.chars().collect()
    };

    // Draw each character (masked for keys, plain for URLs)
    for (i, ch) in display_chars.iter().enumerate() {
        let cx = input_x + i as u16;
        if cx >= input_x + content_w {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, input_y)) {
            cell.set_char(*ch);
            cell.set_style(Style::default().fg(rgba_color(theme.text)).bg(bg_element));
        }
    }

    // Draw cursor at cursor_pos
    let cursor_x = input_x + cursor_pos as u16;
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
                // OFF/Blur: semi-transparent cursor
                cell.set_char('\u{2592}');
                cell.set_style(Style::default().fg(Color::Rgb(60, 60, 60)).bg(bg_element));
            }
        }
    }
}
// ── Hook registration panel ─────────────────────────────────────────────

const HOOK_FIELDS: usize = 4;

fn hook_field_label(field: usize) -> &'static str {
    match field {
        0 => "Name",
        1 => "Matcher",
        2 => "Command",
        _ => "Timeout",
    }
}

fn hook_field_hint(field: usize) -> &'static str {
    match field {
        0 => "shown in this list · optional",
        1 => "regex on tool name · empty = all tools",
        2 => "shell command run for each matched tool call",
        _ => "seconds before it is killed · empty = 30",
    }
}

/// Panel width — matches the other text-input dialogs.
pub(crate) fn hook_input_dialog_w(area: Rect) -> u16 {
    56u16.min(area.width.saturating_sub(8)).max(30)
}

pub(crate) const fn hook_input_content_w(panel_w: u16) -> u16 {
    panel_w.saturating_sub(4)
}

/// Rows a value occupies when hard-wrapped at `cols` chars. Char-based on
/// purpose: identical to `word_ops::move_visual_line`, so cursor math and
/// rendering can never disagree.
fn hook_field_row_count(value: &str, cols: usize) -> usize {
    value.chars().count().div_ceil(cols.max(1)).max(1)
}

/// Per-field vertical geometry, offsets relative to the panel top.
pub(crate) struct HookFieldGeometry {
    /// Row of the highlighted label.
    pub(crate) label_y: u16,
    /// First row of the (possibly wrapped) value.
    pub(crate) value_y: u16,
    /// How many rows the wrapped value occupies.
    pub(crate) rows: usize,
}

pub(crate) fn hook_field_geometries(
    dialog_y: u16,
    fields: [&str; HOOK_FIELDS],
    cols: usize,
) -> Vec<HookFieldGeometry> {
    let mut out = Vec::with_capacity(HOOK_FIELDS);
    let mut y = dialog_y + 4; // top pad · title · subtitle · gap
    for value in fields {
        let rows = hook_field_row_count(value, cols);
        // label · margin · value… · margin (before the next label)
        out.push(HookFieldGeometry {
            label_y: y,
            value_y: y + 2,
            rows,
        });
        y += 3 + rows as u16;
    }
    out
}

/// Shared geometry for the hook registration panel:
/// `(x, y, w, h, content_x, content_w)`. Mirrors [`render_hook_form_dialog`].
pub(crate) fn hook_input_metrics(
    area: Rect,
    fields: [&str; HOOK_FIELDS],
) -> (u16, u16, u16, u16, u16, usize) {
    let w = hook_input_dialog_w(area);
    let cols = hook_input_content_w(w) as usize;
    let field_rows: u16 = fields
        .iter()
        .map(|v| 3 + hook_field_row_count(v, cols) as u16) // label + margins + values
        .sum();
    // top pad · title · subtitle · gap · fields · bottom pad (footer removed)
    let h = 5 + field_rows;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    (x, y, w, h, x + 2, cols)
}

/// WCAG relative luminance — decides which text family stays readable.
fn relative_luminance(c: RGBA) -> f32 {
    let channel = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b, _) = c.to_ints();
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Marker text color adapted to the THEME, not just the highlight: dark
/// themes (cosh) get near-white, light themes (sakura) near-black, so the
/// label keeps contrast both against the primary swatch and the panel.
fn hook_marker_fg(theme: &Theme) -> Color {
    if relative_luminance(theme.background) < 0.5 {
        Color::Rgb(245, 245, 245)
    } else {
        Color::Rgb(30, 30, 30)
    }
}

/// Map a click position to `(field, char index)` — labels focus the field,
/// value rows also position the cursor. `None` when the click lands on
/// padding inside the panel; the caller decides outside-panel dismissal.
fn hook_input_hit_field(
    area: Rect,
    fields: [&str; HOOK_FIELDS],
    x: u16,
    y: u16,
) -> Option<(usize, Option<usize>)> {
    let (dialog_x, dialog_y, dialog_w, dialog_h, content_x, cols) =
        hook_input_metrics(area, fields);
    let inside_panel =
        x >= dialog_x && x < dialog_x + dialog_w && y >= dialog_y && y < dialog_y + dialog_h;
    if !inside_panel {
        return None;
    }
    for (field, g) in hook_field_geometries(dialog_y, fields, cols)
        .into_iter()
        .enumerate()
    {
        if y == g.label_y {
            return Some((field, None));
        }
        if y >= g.value_y && y < g.value_y + g.rows as u16 {
            let line = (y - g.value_y) as usize;
            let col = (x.saturating_sub(content_x)) as usize;
            let char_idx = (line * cols + col).min(fields[field].chars().count());
            return Some((field, Some(char_idx)));
        }
    }
    None
}

/// Byte offset of the `char_idx`-th character, clamped to the end.
fn byte_at_char(value: &str, char_idx: usize) -> usize {
    value
        .char_indices()
        .nth(char_idx)
        .map_or(value.len(), |(i, _)| i)
}

/// Registration panel for creating/editing a PreToolUse hook — borderless,
/// filled with the sidebar's `background_panel` like the rename prompt.
/// Every field label wears a primary-color marker highlight (text tone
/// adapts to the theme), values keep breathing margins around them, wrap
/// onto extra rows instead of running off-panel, and clicking a label or
/// value focuses that field.
#[allow(clippy::too_many_arguments)]
fn render_hook_form_dialog(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    now: SystemTime,
    cursor: &Cursor,
    title: &str,
    event: &'static str,
    fields: [&str; HOOK_FIELDS],
    active_field: usize,
    cursor_pos: usize,
) {
    let (dialog_x, dialog_y, dialog_w, dialog_h, content_x, cols) =
        hook_input_metrics(area, fields);
    let content_w = cols as u16;
    let geoms = hook_field_geometries(dialog_y, fields, cols);

    // Solid background panel — same color as the history sidebar, a bare
    // floating surface with no border characters (rename-prompt style).
    let bg_color = rgba_color(theme.background_panel);
    for y in dialog_y..dialog_y + dialog_h {
        for x in dialog_x..dialog_x + dialog_w {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(
                    Style::default()
                        .bg(bg_color)
                        .remove_modifier(Modifier::all()),
                );
                cell.set_diff_option(CellDiffOption::None);
            }
        }
    }

    // Header row: bold title left, muted "esc" right (rename-prompt style).
    let header_y = dialog_y + 1;
    draw_text_line(
        buf,
        title,
        content_x,
        header_y,
        content_w,
        Style::default()
            .fg(rgba_color(theme.text))
            .add_modifier(Modifier::BOLD),
    );
    let esc_hint = "esc";
    draw_text_line(
        buf,
        esc_hint,
        dialog_x + dialog_w - 2 - esc_hint.len() as u16,
        header_y,
        esc_hint.len() as u16,
        Style::default().fg(rgba_color(theme.text_muted)),
    );

    // Subtitle row (muted).
    draw_text_line(
        buf,
        &format!(
            "{event} · runs {} each tool call",
            if event == "PostToolUse" {
                "after"
            } else {
                "before"
            }
        ),
        content_x,
        dialog_y + 2,
        content_w,
        Style::default().fg(rgba_color(theme.text_muted)),
    );

    // Field blocks: marker-highlighted label, margin, wrapped value, margin.
    let cursor_state = cursor.current_state(now);
    let marker_fg = hook_marker_fg(theme);
    for (field, value) in fields.iter().enumerate() {
        let g = &geoms[field];
        let focused = field == active_field;
        let label = hook_field_label(field);

        // Marker-highlighted label: " Name " drawn on the theme's primary —
        // focused fields go bold so the active one stands out.
        let marked = format!(" {label} ");
        let mut mark_style = Style::default().fg(marker_fg).bg(rgba_color(theme.primary));
        if focused {
            mark_style = mark_style.add_modifier(Modifier::BOLD);
        }
        for (i, ch) in marked.chars().enumerate() {
            let cx = content_x + i as u16;
            if cx >= content_x + content_w {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, g.label_y)) {
                cell.set_char(ch);
                cell.set_style(mark_style);
            }
        }
        let hint_x = content_x + marked.chars().count() as u16 + 1;
        draw_text_line(
            buf,
            hook_field_hint(field),
            hint_x,
            g.label_y,
            content_w.saturating_sub(marked.chars().count() as u16 + 1),
            Style::default().fg(rgba_color(theme.text_muted)),
        );

        // Wrapped value rows with the blinking block cursor on the line that
        // holds the insertion point.
        let char_before = value[..cursor_pos.min(value.len())].chars().count();
        let total_chars = value.chars().count();
        for r in 0..g.rows {
            let start_char = r * cols;
            let end_char = ((r + 1) * cols).min(total_chars);
            let row_y = g.value_y + r as u16;
            let line: String = value
                .chars()
                .skip(start_char)
                .take(end_char - start_char)
                .collect();
            for (i, ch) in line.chars().enumerate() {
                let cx = content_x + i as u16;
                if cx >= content_x + content_w {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, row_y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(rgba_color(theme.text)).bg(bg_color));
                }
            }
            if focused && char_before >= start_char && char_before <= end_char {
                let col = char_before - start_char;
                let cx = content_x + col.min(cols.saturating_sub(1)) as u16;
                if cx < content_x + content_w
                    && let Some(cell) = buf.cell_mut((cx, row_y))
                {
                    match cursor_state {
                        CursorState::On => {
                            cell.set_char('\u{2588}');
                            cell.set_style(
                                Style::default().fg(rgba_color(theme.primary)).bg(bg_color),
                            );
                        }
                        CursorState::Off | CursorState::Blur => {
                            cell.set_char('\u{2592}');
                            cell.set_style(
                                Style::default().fg(Color::Rgb(60, 60, 60)).bg(bg_color),
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod hook_panel_tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};
    #[test]
    fn marker_fg_adapts_to_theme_background() {
        let registry = ThemeRegistry::new();
        let cosh = registry
            .themes
            .iter()
            .find(|t| t.name == "cosh")
            .expect("cosh theme")
            .theme
            .clone();
        let sakura = registry
            .themes
            .iter()
            .find(|t| t.name == "sakura")
            .expect("sakura theme")
            .theme
            .clone();

        // Dark background → light marker text; light background → dark.
        assert!(
            relative_luminance(cosh.background) < 0.5,
            "cosh is a dark theme"
        );
        assert!(matches!(hook_marker_fg(&cosh), Color::Rgb(245, 245, 245)));
        assert!(
            relative_luminance(sakura.background) >= 0.5,
            "sakura is a light theme"
        );
        assert!(matches!(hook_marker_fg(&sakura), Color::Rgb(30, 30, 30)));
    }

    #[test]
    fn hit_field_maps_labels_and_value_rows() {
        let area = Rect::new(0, 0, 80, 30);
        let values = ["block rm", "", "exit 2", ""];
        let (dialog_x, dialog_y, _, _, content_x, cols) = hook_input_metrics(area, values);
        let geoms = hook_field_geometries(dialog_y, values, cols);

        // Label row focuses the field without moving the cursor.
        assert_eq!(
            hook_input_hit_field(area, values, content_x + 2, geoms[0].label_y),
            Some((0, None))
        );
        // Value row positions the cursor at the clicked character.
        let col_of_x = 3usize; // third char of "exit 2"
        assert_eq!(
            hook_input_hit_field(area, values, content_x + col_of_x as u16, geoms[2].value_y),
            Some((2, Some(col_of_x)))
        );
        // Padding inside the panel is not a field.
        assert_eq!(
            hook_input_hit_field(area, values, dialog_x + 1, dialog_y + 1),
            None
        );
    }

    #[test]
    fn dialog_click_focuses_field_and_moves_cursor() {
        let mut state = DialogState::new();
        state.show(DialogType::HookInput {
            event: crate::routes::settings::PRE_TOOL_USE_EVENT,
            editing_index: None,
            name: String::new(),
            matcher: String::new(),
            command: "exit 2".into(),
            timeout: String::new(),
            field: 0,
            cursor_pos: 0,
        });
        let area = Rect::new(0, 0, 80, 30);
        let values = ["", "", "exit 2", ""];
        let (_dialog_x, dialog_y, _, _, content_x, cols) = hook_input_metrics(area, values);

        // Click the Command value row at the 'i' column.
        let cmd_geom_row = hook_field_geometries(dialog_y, values, cols)[2].value_y;
        let click = MouseEvent::new(
            MouseEventType::Down,
            MouseButton::Left,
            content_x + 2,
            cmd_geom_row,
            MouseModifiers::none(),
        );
        let theme = ThemeRegistry::new().default_theme().clone();
        assert_eq!(
            state.handle_mouse(&click, area, &theme),
            DialogAction::Consumed
        );
        assert!(
            matches!(
                state.current().map(|d| &d.dialog_type),
                Some(DialogType::HookInput { field, cursor_pos, .. })
                    if *field == 2 && *cursor_pos == 2
            ),
            "click must focus Command and park the cursor on 'i'"
        );

        // Outside the panel dismisses.
        let outside = MouseEvent::new(
            MouseEventType::Down,
            MouseButton::Left,
            1,
            1,
            MouseModifiers::none(),
        );
        assert_eq!(
            state.handle_mouse(&outside, area, &theme),
            DialogAction::Dismissed
        );
    }

    #[test]
    fn margins_and_wrap_grow_the_panel() {
        let area = Rect::new(0, 0, 80, 60);
        let single = ["", "", "", ""];
        let (_, _, _, h_single, _, cols) = hook_input_metrics(area, single);

        // Every field carries label + top/bottom margins.
        assert_eq!(h_single as usize, 5 + 4 * (3 + 1));

        // A value one char past `cols` adds exactly one wrapped row (+margin).
        let long = format!("a{}", "b".repeat(cols));
        let wrapped = ["", "", long.as_str(), ""];
        let (_, _, _, h_wrapped, _, _) = hook_input_metrics(area, wrapped);
        assert_eq!(h_wrapped, h_single + 1);
    }
}
