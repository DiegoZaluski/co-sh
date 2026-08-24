//! Settings router — a selectable list of configuration categories.
//!
//! Unlike the other flat-list routers (`tools`, `add_provider`), each option
//! carries an explanatory description rendered directly above it, telling
//! the user what the setting controls.
//!
//! The Hooks entry owns a sub-list: while hooks are enabled every configured
//! hook plus an "Add hook" action appears below the toggle; disabling hooks
//! hides the sub-list entirely. Activating a hook opens the shared
//! [`crate::ui::dialogs::DialogType::HookInput`] registration box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::Theme;
use crate::util::list_selection::ListSelection;
use crate::util::setup::{HookEntry, Setup};

/// The only event type implemented by the runtime engine today.
pub const HOOK_EVENT: &str = "PreToolUse";

/// Maximum characters of a hook command shown in list rows.
const COMMAND_PREVIEW_LEN: usize = 34;

// ── Catalog ─────────────────────────────────────────────────────────────────

/// One top-level entry of the Settings list. `id` keys the activation
/// behaviour; `label` is the option name; `description` explains what the
/// setting does.
struct SettingsItem {
    id: &'static str,
    label: &'static str,
    description: &'static str,
}

fn settings_items() -> &'static [SettingsItem] {
    &[SettingsItem {
        id: "hooks",
        label: "Hooks",
        description: "Run custom shell commands before every tool call",
    }]
}

fn is_enabled(item: &SettingsItem, setup: &Setup) -> bool {
    match item.id {
        "hooks" => setup.hooks.enabled,
        _ => true,
    }
}

pub fn hook_entries(setup: &Setup) -> &[HookEntry] {
    setup
        .hooks
        .events
        .get(HOOK_EVENT)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// List label for a hook: friendly name, falling back to the command like the
/// engine's own `display_name`.
fn hook_display_name(entry: &HookEntry) -> String {
    if entry.name.is_empty() {
        truncate(&entry.command, COMMAND_PREVIEW_LEN)
    } else {
        entry.name.clone()
    }
}

/// Validate one hook form submission and build its persisted entry.
///
/// Errors mirror the engine's own rules so a saved hook always runs exactly
/// as the user expects: invalid matcher regexes are silently skipped by the
/// runner, which is why they are rejected here instead.
pub fn validate_hook(
    name: &str,
    matcher: &str,
    command: &str,
    timeout: &str,
) -> Result<HookEntry, String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("Command is required.".into());
    }
    let matcher = matcher.trim().to_string();
    if !matcher.is_empty() {
        regex::Regex::new(&matcher)
            .map_err(|err| format!("Matcher is not a valid regex ({err})."))?;
    }
    let raw_timeout = timeout.trim().to_string();
    let timeout = match raw_timeout.parse::<u64>() {
        Ok(secs) => Some(secs),
        Err(_) if raw_timeout.is_empty() => None,
        Err(_) => return Err("Timeout must be a number of seconds.".into()),
    };
    Ok(HookEntry {
        name: name.trim().to_string(),
        matcher,
        command,
        timeout,
    })
}

// ── Screen model ────────────────────────────────────────────────────────────

/// A selectable row of the Settings screen. Sub-rows exist only while their
/// category is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// Top-level setting (index into `settings_items()`).
    Category(usize),
    /// Configured hook (index into `hooks.events[HOOK_EVENT]`).
    Hook(usize),
    /// Create a new hook.
    AddHook,
}

/// Every rendered line with its vertical offset from the content top.
/// Render and mouse hit-testing both consume this, so they cannot drift.
struct LayoutLine {
    y: u16,
    line: Line,
}

enum Line {
    Title,
    Blank,
    Description(&'static str),
    Category { item: usize },
    Hook { hook: usize },
    AddHook,
}

impl Line {
    /// Selectable rows in top-to-bottom order.
    fn row(&self) -> Option<SettingsRow> {
        match self {
            Line::Category { item } => Some(SettingsRow::Category(*item)),
            Line::Hook { hook } => Some(SettingsRow::Hook(*hook)),
            Line::AddHook => Some(SettingsRow::AddHook),
            _ => None,
        }
    }
}

fn build_layout(setup: &Setup) -> Vec<LayoutLine> {
    let mut lines = Vec::new();
    let mut y = 0u16;
    lines.push(LayoutLine {
        y,
        line: Line::Title,
    });
    y += 1;
    for item in 0..settings_items().len() {
        lines.push(LayoutLine {
            y,
            line: Line::Description(settings_items()[item].description),
        });
        y += 1;
        lines.push(LayoutLine {
            y,
            line: Line::Category { item },
        });
        y += 1;
    }
    if setup.hooks.enabled {
        lines.push(LayoutLine {
            y,
            line: Line::Blank,
        }); // breathing room under toggle
        y += 1;
        for hook in 0..hook_entries(setup).len() {
            lines.push(LayoutLine {
                y,
                line: Line::Hook { hook },
            });
            y += 1;
        }
        // The add action reads as the last entry of the hook list.
        lines.push(LayoutLine {
            y,
            line: Line::AddHook,
        });
    }
    lines
}

fn selectable_rows(setup: &Setup) -> Vec<SettingsRow> {
    build_layout(setup)
        .iter()
        .filter_map(|l| l.line.row())
        .collect()
}

fn content_height(setup: &Setup) -> u16 {
    build_layout(setup).len() as u16
}

/// What happened after the user activated a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    /// Persisted state changed; caller must `setup.save()`.
    ToggleSaved,
    /// Open the hook registration box for the hook at this index.
    EditHook(usize),
    /// Open the hook registration box to create a new hook.
    NewHook,
}

// ── View ────────────────────────────────────────────────────────────────────

pub struct SettingsView {
    pub selection: ListSelection,
}

impl SettingsView {
    pub const fn new() -> Self {
        Self {
            selection: ListSelection::new(),
        }
    }

    pub fn select_next(&mut self, visible_count: usize, setup: &Setup) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_next(selectable_rows(setup).len());
    }

    pub fn select_prev(&mut self, visible_count: usize, setup: &Setup) {
        self.selection.set_visible_count(visible_count);
        self.selection.select_prev(selectable_rows(setup).len());
    }

    /// Activate the row under the selection.
    pub fn activate_selected(&mut self, setup: &mut Setup) -> Option<SettingsAction> {
        let rows = selectable_rows(setup);
        let idx = self
            .selection
            .selected_index
            .min(rows.len().saturating_sub(1));
        match rows.get(idx)? {
            SettingsRow::Category(i) => {
                let item = &settings_items()[*i];
                match item.id {
                    "hooks" => {
                        setup.hooks.enabled = !setup.hooks.enabled;
                        self.selection.clamp(selectable_rows(setup).len());
                        Some(SettingsAction::ToggleSaved)
                    }
                    _ => None,
                }
            }
            SettingsRow::Hook(i) => Some(SettingsAction::EditHook(*i)),
            SettingsRow::AddHook => Some(SettingsAction::NewHook),
        }
    }

    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect, setup: &Setup) -> Option<usize> {
        self.find_row_for_mouse(mouse, area, setup)
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect, setup: &Setup) -> Option<usize> {
        let max_row_w = max_row_width(setup);
        if max_row_w == 0 {
            return None;
        }
        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        // Only option/sub rows are clickable — never titles or descriptions.
        let hit_x = mouse.x >= row_x && mouse.x < row_x + max_row_w as u16;
        if !hit_x {
            return None;
        }
        let start_y = content_start_y(area, setup);
        build_layout(setup)
            .iter()
            .find(|l| l.y as u32 + start_y as u32 == mouse.y as u32)
            .and_then(|l| l.line.row())
            .and_then(|row| selectable_rows(setup).iter().position(|r| r == &row))
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, setup: &Setup) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let start_y = content_start_y(area, setup);
        let layout = build_layout(setup);

        let max_w = max_row_width(setup);
        if max_w == 0 || area.width == 0 || area.height == 0 {
            return;
        }
        let row_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;

        // Clamp selection and scroll before rendering.
        let rows = selectable_rows(setup);
        let visible = visible_rows(area).min(rows.len());
        self.selection.set_visible_count(visible);
        self.selection.clamp(rows.len());
        let selected_idx = self.selection.selected_index;

        for line in &layout {
            let y = start_y + line.y;
            if y >= area.bottom() {
                break;
            }
            match &line.line {
                Line::Title => {
                    draw_text(buf, "Settings", row_x, y, area, Style::default().fg(muted));
                }
                Line::Blank => {}
                Line::Description(text) => {
                    draw_text(buf, text, row_x, y, area, Style::default().fg(muted));
                }
                Line::Category { item } => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::Category(ci) if ci == item));
                    let is_selected = idx == Some(selected_idx);
                    let shown = in_window(idx, visible, self.selection.scroll_offset);
                    let item = &settings_items()[*item];

                    let enabled = is_enabled(item, setup);
                    let symbol = if enabled { "✔" } else { "✗" };
                    let sym_color = if enabled { Color::Green } else { Color::Red };

                    draw_text(buf, symbol, row_x, y, area, Style::default().fg(sym_color));
                    draw_text(
                        buf,
                        item.label,
                        row_x + 2,
                        y,
                        area,
                        Style::default().fg(if is_selected && shown { primary } else { fg }),
                    );
                }
                Line::Hook { hook } => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::Hook(hi) if hi == hook));
                    let is_selected = idx == Some(selected_idx);
                    if !in_window(idx, visible, self.selection.scroll_offset) {
                        continue;
                    }
                    let entry = &hook_entries(setup)[*hook];
                    let name_line = format!("• {}", hook_display_name(entry));
                    draw_text(
                        buf,
                        &name_line,
                        row_x + 4,
                        y,
                        area,
                        Style::default().fg(if is_selected { primary } else { fg }),
                    );
                    if !entry.name.is_empty() && !entry.command.is_empty() {
                        let preview =
                            format!(" — {}", truncate(&entry.command, COMMAND_PREVIEW_LEN));
                        let cmd_x = (row_x + 4).saturating_add(name_line.chars().count() as u16);
                        draw_text(buf, &preview, cmd_x, y, area, Style::default().fg(muted));
                    }
                }
                Line::AddHook => {
                    let idx = rows.iter().position(|r| r == &SettingsRow::AddHook);
                    let is_selected = idx == Some(selected_idx);
                    if !in_window(idx, visible, self.selection.scroll_offset) {
                        continue;
                    }
                    draw_text(
                        buf,
                        "+ Add hook",
                        row_x + 4,
                        y,
                        area,
                        Style::default().fg(if is_selected { primary } else { muted }),
                    );
                }
            }
        }
    }
}

fn in_window(idx: Option<usize>, visible: usize, scroll: usize) -> bool {
    match idx {
        Some(i) => i >= scroll && i < scroll + visible,
        None => false,
    }
}

fn content_start_y(area: Rect, setup: &Setup) -> u16 {
    area.y + (area.height.saturating_sub(content_height(setup))) / 2
}

fn max_row_width(setup: &Setup) -> usize {
    let mut width = settings_items()
        .iter()
        .map(|item| item.description.len().max(2 + item.label.len())) // "✔ Hooks"
        .max()
        .unwrap_or(0);
    if setup.hooks.enabled {
        for entry in hook_entries(setup) {
            // Full drawn line: indent + "• " + name [+ " — command…"].
            let name = hook_display_name(entry);
            let mut len = 4 + 2 + name.chars().count();
            if !entry.name.is_empty() && !entry.command.is_empty() {
                len += 3 + truncate(&entry.command, COMMAND_PREVIEW_LEN)
                    .chars()
                    .count();
            }
            width = width.max(len);
        }
        width = width.max(4 + "+ Add hook".len());
    }
    width
}

fn visible_rows(area: Rect) -> usize {
    area.height.saturating_sub(3) as usize
}

fn draw_text(buf: &mut Buffer, text: &str, x: u16, y: u16, area: Rect, style: Style) {
    for (j, ch) in text.chars().enumerate() {
        let cx = x + j as u16;
        if cx >= area.right() {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn rgba_color(rgba: cosh_tui::core::lib::rgba::RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeRegistry;
    use cosh_tui::core::types::{MouseButton, MouseEventType, MouseModifiers};

    fn test_theme() -> Theme {
        ThemeRegistry::new().default_theme().clone()
    }

    fn mouse_at(x: u16, y: u16) -> MouseEvent {
        MouseEvent::new(
            MouseEventType::Move,
            MouseButton::Left,
            x,
            y,
            MouseModifiers::none(),
        )
    }

    fn setup_with_hooks(enabled: bool, hooks: &[(&str, &str)]) -> Setup {
        let mut setup = Setup::default();
        setup.hooks.enabled = enabled;
        for (name, command) in hooks {
            setup
                .hooks
                .events
                .entry(HOOK_EVENT.to_string())
                .or_default()
                .push(HookEntry {
                    name: name.to_string(),
                    matcher: String::new(),
                    command: command.to_string(),
                    timeout: None,
                });
        }
        setup
    }

    fn line_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (area.x..area.right())
            .map(|x| {
                buf.cell((x, y))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Absolute Y of a layout line for `area` given the current state.
    fn abs_y(setup: &Setup, area: Rect, pred: impl Fn(&Line) -> bool) -> u16 {
        let layout = build_layout(setup);
        let start_y = content_start_y(area, setup);
        start_y
            + layout
                .iter()
                .find(|l| pred(&l.line))
                .expect("line exists")
                .y
    }

    #[test]
    fn sub_list_follows_enabled_state() {
        let disabled = setup_with_hooks(false, &[("a", "cmd a")]);
        assert_eq!(selectable_rows(&disabled), vec![SettingsRow::Category(0)]);

        let enabled = setup_with_hooks(true, &[("a", "cmd a"), ("b", "cmd b")]);
        assert_eq!(
            selectable_rows(&enabled),
            vec![
                SettingsRow::Category(0),
                SettingsRow::Hook(0),
                SettingsRow::Hook(1),
                SettingsRow::AddHook,
            ]
        );
    }

    #[test]
    fn toggling_off_clamps_selection() {
        let mut setup = setup_with_hooks(true, &[("a", "cmd a")]);
        let mut view = SettingsView::new();
        view.selection.selected_index = 3; // Add hook
        // User navigates up to the Hooks toggle and activates it.
        for _ in 0..3 {
            view.select_prev(20, &setup);
        }

        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::ToggleSaved)
        );
        assert!(!setup.hooks.enabled);
        assert!(view.selection.selected_index < selectable_rows(&setup).len());
    }

    #[test]
    fn activating_hook_rows_requests_the_registration_box() {
        let mut setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();

        view.selection.selected_index = 1; // Hook(0)
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::EditHook(0))
        );

        view.selection.selected_index = 2; // Add hook
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::NewHook)
        );
    }

    #[test]
    fn validate_hook_rejects_invalid_fields() {
        assert_eq!(
            validate_hook("n", "", "", "").unwrap_err(),
            "Command is required."
        );
        assert!(
            validate_hook("n", "((", "exit 2", "")
                .unwrap_err()
                .starts_with("Matcher is not a valid regex"),
            "invalid matcher regex must be rejected"
        );
        assert_eq!(
            validate_hook("n", "", "exit 2", "abc").unwrap_err(),
            "Timeout must be a number of seconds."
        );
    }

    #[test]
    fn validate_hook_builds_persisted_entry() {
        let entry = validate_hook(" block rm ", "", " exit 2 ", "5").expect("valid");
        assert_eq!(entry.name, "block rm");
        assert_eq!(entry.command, "exit 2");
        assert_eq!(entry.timeout, Some(5));

        let entry = validate_hook("", "^fs_read$", "true", "").expect("valid");
        assert_eq!(entry.matcher, "^fs_read$");
        assert_eq!(entry.timeout, None);
    }

    #[test]
    fn render_shows_sublist_only_when_enabled() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 100, 24);

        // Disabled: only the toggle, no sub rows.
        let setup = setup_with_hooks(false, &[("hidden", "gone")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        let all: String = (area.y..area.bottom())
            .map(|y| format!("{}\n", line_text(&buf, area, y)))
            .collect();
        assert!(all.contains("✗ Hooks"));
        assert!(!all.contains("• hidden"));
        assert!(!all.contains("+ Add hook"));

        // Enabled: hooks and the add action appear under the toggle.
        let setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        let all: String = (area.y..area.bottom())
            .map(|y| format!("{}\n", line_text(&buf, area, y)))
            .collect();
        assert!(all.contains("✔ Hooks"));
        assert!(all.contains("• block rm — exit 2"));
        assert!(all.contains("+ Add hook"));
    }

    #[test]
    fn mouse_hits_hook_row_only_on_its_line() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 100, 24);
        let setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let hook_y = abs_y(&setup, area, |l| matches!(l, Line::Hook { hook: 0 }));
        let desc_y = abs_y(&setup, area, |l| matches!(l, Line::Description(_)));
        // row_x mirrors find_row_for_mouse's centering.
        let row_x = area.x + (area.width.saturating_sub(max_row_width(&setup) as u16)) / 2;

        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, hook_y), area, &setup),
            Some(1)
        );
        // Description line is never clickable.
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, desc_y), area, &setup),
            None
        );

        // Disabled: the list re-centers; the old hook Y hits nothing and the
        // toggle itself sits where the centered list puts it.
        let disabled = setup_with_hooks(false, &[("block rm", "exit 2")]);
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, hook_y), area, &disabled),
            None
        );
        let toggle_y = abs_y(&disabled, area, |l| matches!(l, Line::Category { item: 0 }));
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, toggle_y), area, &disabled),
            Some(0)
        );
    }
}
