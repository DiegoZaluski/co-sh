//! Settings router — a selectable list of configuration categories.
//!
//! Unlike the other flat-list routers (`tools`, `add_provider`), each option
//! carries an explanatory description rendered directly above it, telling
//! the user what the setting controls.
//!
//! The hook entries own sub-lists: while hooks are enabled every configured
//! hook plus an "Add hook" action appears below its category; disabling
//! hooks hides the sub-lists entirely. Activating a hook opens the shared
//! [`crate::ui::dialogs::DialogType::HookInput`] registration box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};
use crate::util::list_selection::ListSelection;
use crate::util::setup::{HookEntry, Setup};

/// Event section keys inside `setup.json` (`hooks.events`).
pub const PRE_TOOL_USE_EVENT: &str = "PreToolUse";
pub const POST_TOOL_USE_EVENT: &str = "PostToolUse";

/// Maximum characters of a hook command shown in list rows.
const COMMAND_PREVIEW_LEN: usize = 34;

// Catalog

/// One top-level entry of the Settings list. `id` keys the activation
/// behaviour; `label` is the option name; `description` explains what the
/// setting controls; `event` selects which hook list the entry manages.
struct SettingsItem {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    event: &'static str,
}

fn settings_items() -> &'static [SettingsItem] {
    &[
        SettingsItem {
            id: "hooks",
            label: "PreToolUse hooks",
            description: "Run custom shell commands before every tool call",
            event: PRE_TOOL_USE_EVENT,
        },
        SettingsItem {
            id: "post_tool_use_hooks",
            label: "PostToolUse hooks",
            description: "Run custom shell commands after every successful tool call",
            event: POST_TOOL_USE_EVENT,
        },
        SettingsItem {
            id: "zen_free_gateway",
            label: "OpenCode Zen free gateway",
            description: "Use OpenCode's free models without login when no API key is set",
            event: "",
        },
        SettingsItem {
            id: "anthropic_cache_ttl",
            label: "Anthropic cache TTL",
            description: "How long Anthropic's prompt cache survives between turns (1h costs 2x per write but rides out long tool calls)",
            event: "",
        },
        SettingsItem {
            id: "openai_cache_retention",
            label: "OpenAI cache retention",
            description: "How long OpenAI may keep your prompt cache (24h helps resumed sessions; older models ignore it)",
            event: "",
        },
        SettingsItem {
            id: "lsp",
            label: "Language servers",
            description: "Run language servers for diagnostics, hover, symbols and related `lsp_*` tools",
            event: "",
        },
    ]
}

/// The current value of a CHOICE setting (rendered in place of the ✔/✗
/// switch symbol), or `None` for plain switches.
fn cache_choice_value(id: &str, setup: &Setup) -> Option<String> {
    match id {
        "anthropic_cache_ttl" => Some(crate::util::setup::format_cache_duration(
            setup.cache.anthropic_ttl_min,
        )),
        "openai_cache_retention" => Some(crate::util::setup::format_cache_duration(
            setup.cache.openai_retention_min,
        )),
        _ => None,
    }
}

/// Each category owns its own switch, so toggling one never leaks into the
/// other event. Items WITHOUT an event (empty string) are standalone
/// switches — currently only the OpenCode Zen free gateway, which reads its
/// state from the persisted one-time prompt answer.
fn is_enabled(item: &SettingsItem, setup: &Setup) -> bool {
    if item.event.is_empty() {
        return match item.id {
            "zen_free_gateway" => setup.zen_public_opt_in() == Some(true),
            "lsp" => setup.lsp,
            _ => false,
        };
    }
    setup.hooks.is_event_enabled(item.event)
}

pub fn hook_entries<'a>(setup: &'a Setup, event: &str) -> &'a [HookEntry] {
    setup
        .hooks
        .events
        .get(event)
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

// Screen model

/// A selectable row of the Settings screen. Sub-rows exist only while their
/// category is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// Top-level setting (index into `settings_items()`).
    Category(usize),
    /// Configured hook of one event.
    Hook { event: &'static str, index: usize },
    /// Create a new hook for one event.
    AddHook { event: &'static str },
    /// Registered MCP server (index into `setup.mcp.servers`).
    McpServer(usize),
    /// Register a new MCP server.
    AddMcpServer,
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
    /// A non-selectable URL rendered under a description (e.g. the Zen free
    /// gateway's terms).
    Link(&'static str),
    Category {
        item: usize,
    },
    Hook {
        event: &'static str,
        hook: usize,
    },
    AddHook {
        event: &'static str,
    },
    McpServer {
        index: usize,
    },
    AddMcpServer,
}

impl Line {
    /// Selectable rows in top-to-bottom order.
    fn row(&self) -> Option<SettingsRow> {
        match self {
            Line::Category { item } => Some(SettingsRow::Category(*item)),
            Line::Hook { event, hook } => Some(SettingsRow::Hook {
                event,
                index: *hook,
            }),
            Line::AddHook { event } => Some(SettingsRow::AddHook { event }),
            Line::McpServer { index } => Some(SettingsRow::McpServer(*index)),
            Line::AddMcpServer => Some(SettingsRow::AddMcpServer),
            Line::Title | Line::Blank | Line::Description(_) | Line::Link(_) => None,
        }
    }
}

/// Terms shown under the Zen free-gateway description.
const ZEN_TERMS_LINK: &str = "https://opencode.ai/legal/terms-of-service";

/// Description of the trailing MCP section. Servers are registered through
/// the Add box (name + `command args...` or `http(s)://` endpoint); removal
/// of a misconfigured entry is a setup.json edit, disabling covers runtime.
const MCP_SECTION_DESCRIPTION: &str =
    "Connect external MCP servers for extra model tools (stdio or HTTP)";

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
        // The Zen free gateway carries its terms right below the
        // description, above the toggle.
        if settings_items()[item].id == "zen_free_gateway" {
            lines.push(LayoutLine {
                y,
                line: Line::Link(ZEN_TERMS_LINK),
            });
            y += 1;
        }
        lines.push(LayoutLine {
            y,
            line: Line::Category { item },
        });
        y += 1;
        // Only hook categories own sub-lists; standalone switches (empty
        // event) are a single toggle row.
        let manages_hooks = !settings_items()[item].event.is_empty();
        if manages_hooks && is_enabled(&settings_items()[item], setup) {
            lines.push(LayoutLine {
                y,
                line: Line::Blank,
            }); // breathing room
            y += 1;
            for hook in 0..hook_entries(setup, settings_items()[item].event).len() {
                lines.push(LayoutLine {
                    y,
                    line: Line::Hook {
                        event: settings_items()[item].event,
                        hook,
                    },
                });
                y += 1;
            }
            // The add action reads as the last entry of the hook list.
            lines.push(LayoutLine {
                y,
                line: Line::AddHook {
                    event: settings_items()[item].event,
                },
            });
            y += 1;
        }
        // Breathing room BELOW every section, so standalone switches (no
        // hook sub-list) never sit glued to the next section's description.
        // The last section needs no trailing blank: it would only inflate
        // content_height and skew the vertical centering.
        if item + 1 < settings_items().len() {
            lines.push(LayoutLine {
                y,
                line: Line::Blank,
            });
            y += 1;
        }
    }
    // Trailing MCP section (always visible, like an enabled hook list):
    // one toggle row per registered server plus the Add action. Separated
    // from the last category by the same breathing room sections enjoy.
    lines.push(LayoutLine {
        y,
        line: Line::Blank,
    });
    y += 1;
    lines.push(LayoutLine {
        y,
        line: Line::Description(MCP_SECTION_DESCRIPTION),
    });
    y += 1;
    for index in 0..setup.mcp.servers.len() {
        lines.push(LayoutLine {
            y,
            line: Line::McpServer { index },
        });
        y += 1;
    }
    lines.push(LayoutLine {
        y,
        line: Line::AddMcpServer,
    });
    lines
}

/// One-line target preview for an MCP server row: `command args...` for
/// stdio, the URL for HTTP.
fn mcp_server_preview(entry: &cosh::mcp::McpServerEntry) -> String {
    use cosh::mcp::McpTransport;
    let target = match &entry.transport {
        McpTransport::Stdio(stdio) => {
            let mut parts = vec![stdio.command.clone()];
            parts.extend(stdio.args.iter().cloned());
            parts.join(" ")
        }
        McpTransport::Http(http) => http.url.clone(),
    };
    truncate(&target, COMMAND_PREVIEW_LEN)
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
    /// The Zen free-gateway switch flipped; caller must `setup.save()` AND
    /// resync the SDK's process-wide tier flag
    /// ([`cosh_sdk::connector::set_zen_public_tier_enabled`]) so connectors
    /// built from now on honor the new state.
    ZenGatewayToggled,
    /// The LSP switch flipped; caller must `setup.save()`, flip the harness's
    /// process-wide LSP flag, and resync the app's `lsp_available` state.
    LspToggled,
    /// Open the registration box: blank when `index` is `None`.
    OpenHookForm {
        event: &'static str,
        index: Option<usize>,
    },
    /// An MCP server was enabled/disabled; caller must `setup.save()`.
    /// Takes effect on the next agent loop (the manager connects at boot).
    McpToggled,
    /// Open the MCP registration wizard (name → endpoint → timeout).
    OpenMcpForm,
    /// Open the cache-duration input box for `setting` (a Settings-item id:
    /// "anthropic_cache_ttl" / "openai_cache_retention").
    OpenCacheInput { setting: &'static str },
}

// View

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
                // Choice settings (cache TTL/retention) open a duration
                // input box instead of toggling a switch — the value is a
                // free-form user choice, not a hardcoded cycle.
                if cache_choice_value(item.id, setup).is_some() {
                    return Some(SettingsAction::OpenCacheInput { setting: item.id });
                }
                // Standalone switches (no hook sub-list) flip their own
                // persisted field.
                if item.event.is_empty() && item.id == "zen_free_gateway" {
                    let enabled = setup.zen_public_opt_in() == Some(true);
                    setup.providers.zen_public_opt_in = Some(!enabled);
                    self.selection.clamp(selectable_rows(setup).len());
                    return Some(SettingsAction::ZenGatewayToggled);
                }
                if item.event.is_empty() && item.id == "lsp" {
                    setup.lsp = !setup.lsp;
                    self.selection.clamp(selectable_rows(setup).len());
                    return Some(SettingsAction::LspToggled);
                }
                // Each category flips only its own event's switch.
                match item.event {
                    PRE_TOOL_USE_EVENT => {
                        setup.hooks.pre_tool_use_enabled = !setup.hooks.pre_tool_use_enabled;
                    }
                    POST_TOOL_USE_EVENT => {
                        setup.hooks.post_tool_use_enabled = !setup.hooks.post_tool_use_enabled;
                    }
                    _ => {}
                }
                self.selection.clamp(selectable_rows(setup).len());
                Some(SettingsAction::ToggleSaved)
            }
            SettingsRow::Hook { event, index } => Some(SettingsAction::OpenHookForm {
                event,
                index: Some(*index),
            }),
            SettingsRow::AddHook { event } => {
                Some(SettingsAction::OpenHookForm { event, index: None })
            }
            SettingsRow::McpServer(index) => {
                if let Some(server) = setup.mcp.servers.get_mut(*index) {
                    server.enabled = !server.enabled;
                }
                self.selection.clamp(selectable_rows(setup).len());
                Some(SettingsAction::McpToggled)
            }
            SettingsRow::AddMcpServer => Some(SettingsAction::OpenMcpForm),
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
                Line::Link(text) => {
                    // Same accent color the dialog uses for its terms link.
                    draw_text(buf, text, row_x, y, area, Style::default().fg(primary));
                }
                Line::Category { item } => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::Category(ci) if ci == item));
                    let is_selected = idx == Some(selected_idx);
                    let shown = in_window(idx, visible, self.selection.scroll_offset);
                    let item = &settings_items()[*item];

                    // Choice rows render "label: value" instead of the
                    // ✔/✗ switch symbol.
                    if let Some(value) = cache_choice_value(item.id, setup) {
                        let label = format!("{}: {}", item.label, value);
                        draw_text(
                            buf,
                            &label,
                            row_x + 2,
                            y,
                            area,
                            Style::default().fg(if is_selected && shown { primary } else { fg }),
                        );
                        continue;
                    }

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
                Line::Hook { event, hook } => {
                    let idx = rows.iter().position(
                        |r| matches!(r, SettingsRow::Hook { event: e, index: hi } if *e == *event && hi == hook),
                    );
                    let is_selected = idx == Some(selected_idx);
                    if !in_window(idx, visible, self.selection.scroll_offset) {
                        continue;
                    }
                    let entry = &hook_entries(setup, event)[*hook];
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
                Line::AddHook { event } => {
                    let idx = rows.iter().position(
                        |r| matches!(r, SettingsRow::AddHook { event: e } if *e == *event),
                    );
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
                Line::McpServer { index } => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::McpServer(i) if i == index));
                    let is_selected = idx == Some(selected_idx);
                    if !in_window(idx, visible, self.selection.scroll_offset) {
                        continue;
                    }
                    let Some(entry) = setup.mcp.servers.get(*index) else {
                        continue;
                    };
                    let symbol = if entry.enabled { "✔" } else { "✗" };
                    let sym_color = if entry.enabled {
                        Color::Green
                    } else {
                        Color::Red
                    };
                    draw_text(
                        buf,
                        symbol,
                        row_x + 2,
                        y,
                        area,
                        Style::default().fg(sym_color),
                    );
                    draw_text(
                        buf,
                        &entry.name,
                        row_x + 4,
                        y,
                        area,
                        Style::default().fg(if is_selected { primary } else { fg }),
                    );
                    let preview = format!(" — {}", mcp_server_preview(entry));
                    let name_x = (row_x + 4).saturating_add(entry.name.chars().count() as u16);
                    draw_text(buf, &preview, name_x, y, area, Style::default().fg(muted));
                }
                Line::AddMcpServer => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::AddMcpServer));
                    let is_selected = idx == Some(selected_idx);
                    if !in_window(idx, visible, self.selection.scroll_offset) {
                        continue;
                    }
                    draw_text(
                        buf,
                        "+ Add server",
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
        .map(|item| {
            let mut len = item.description.len().max(2 + item.label.len()); // "✔ PreToolUse hooks"
            // Choice rows render "label: value" at the same indent.
            if let Some(v) = cache_choice_value(item.id, setup) {
                len = len.max(2 + item.label.len() + 2 + v.len());
            }
            if item.id == "zen_free_gateway" {
                len = len.max(ZEN_TERMS_LINK.len());
            }
            len
        })
        .max()
        .unwrap_or(0);
    for item in settings_items() {
        if item.event.is_empty() || !setup.hooks.is_event_enabled(item.event) {
            continue;
        }
        for entry in hook_entries(setup, item.event) {
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
    // MCP section: description plus one toggle row per server and the Add
    // action (indented like hook sub-rows).
    width = width.max(MCP_SECTION_DESCRIPTION.len());
    for entry in &setup.mcp.servers {
        // Full drawn line: indent + "✔ " + name + " — target…".
        let len =
            4 + 1 + entry.name.chars().count() + 3 + mcp_server_preview(entry).chars().count();
        width = width.max(len);
    }
    width = width.max(4 + "+ Add server".len());
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

    fn setup_with_hooks(enabled: bool, pre: &[(&str, &str)]) -> Setup {
        let mut setup = Setup::default();
        setup.hooks.pre_tool_use_enabled = enabled;
        setup.hooks.post_tool_use_enabled = enabled;
        for (name, command) in pre {
            setup
                .hooks
                .events
                .entry(PRE_TOOL_USE_EVENT.to_string())
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

    /// Absolute Y of a layout line matching `pred`.
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
    fn sub_lists_follow_enabled_state_per_event() {
        let disabled = setup_with_hooks(false, &[("a", "cmd a")]);
        assert_eq!(
            selectable_rows(&disabled),
            vec![
                SettingsRow::Category(0),
                SettingsRow::Category(1),
                SettingsRow::Category(2),
                SettingsRow::Category(3),
                SettingsRow::Category(4),
                SettingsRow::Category(5),
                SettingsRow::AddMcpServer,
            ]
        );

        let enabled = setup_with_hooks(true, &[("a", "cmd a"), ("b", "cmd b")]);
        assert_eq!(
            selectable_rows(&enabled),
            vec![
                SettingsRow::Category(0),
                SettingsRow::Hook {
                    event: PRE_TOOL_USE_EVENT,
                    index: 0
                },
                SettingsRow::Hook {
                    event: PRE_TOOL_USE_EVENT,
                    index: 1
                },
                SettingsRow::AddHook {
                    event: PRE_TOOL_USE_EVENT
                },
                SettingsRow::Category(1),
                SettingsRow::AddHook {
                    event: POST_TOOL_USE_EVENT
                },
                SettingsRow::Category(2),
                SettingsRow::Category(3),
                SettingsRow::Category(4),
                SettingsRow::Category(5),
                SettingsRow::AddMcpServer,
            ]
        );
    }

    fn setup_with_mcp_server(name: &str, enabled: bool) -> Setup {
        let mut setup = Setup::default();
        setup.mcp.servers.push(cosh::mcp::McpServerEntry {
            name: name.to_string(),
            transport: cosh::mcp::McpTransport::Stdio(cosh::mcp::StdioTransport {
                command: "srv".to_string(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            }),
            enabled,
        });
        setup
    }

    /// The MCP section appends server toggles plus the Add action after
    /// every category; toggling flips only the server's own switch.
    #[test]
    fn mcp_rows_toggle_and_open_the_wizard() {
        let setup = setup_with_mcp_server("docs", true);
        assert_eq!(
            selectable_rows(&setup).last(),
            Some(&SettingsRow::AddMcpServer)
        );
        assert!(selectable_rows(&setup).contains(&SettingsRow::McpServer(0)));

        let mut setup = setup;
        let mut view = SettingsView::new();
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::McpServer(0)))
            .unwrap();
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::McpToggled)
        );
        assert!(!setup.mcp.servers[0].enabled);

        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::AddMcpServer))
            .unwrap();
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenMcpForm)
        );
    }

    /// The cache TTL/retention rows are CHOICE settings: activation opens
    /// the duration input box (no value changes in place), and the rendered
    /// label carries the currently configured duration.
    #[test]
    fn cache_choices_open_the_duration_input() {
        let mut setup = Setup::default();
        assert_eq!(
            cache_choice_value("anthropic_cache_ttl", &setup),
            Some("default".to_string())
        );

        let mut view = SettingsView::new();
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(3)))
            .unwrap();
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenCacheInput {
                setting: "anthropic_cache_ttl"
            })
        );
        // Activation NEVER mutates the persisted value by itself.
        assert_eq!(setup.cache.anthropic_ttl_min, 0);

        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(4)))
            .unwrap();
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenCacheInput {
                setting: "openai_cache_retention"
            })
        );
        assert_eq!(setup.cache.openai_retention_min, 0);

        // The rendered row carries the configured value ("label: value").
        setup.cache.anthropic_ttl_min = 90;
        setup.cache.openai_retention_min = 1440;
        let theme = test_theme();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        let all: String = (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<String>();
        assert!(all.contains("Anthropic cache TTL: 1h30m"));
        assert!(all.contains("OpenAI cache retention: 24h"));
        assert!(!all.contains("✗ Anthropic cache TTL"));
    }

    /// The Zen free-gateway row flips the persisted answer BOTH ways from
    /// the settings screen — unlike the one-time prompt, this is the manual
    /// control — and reports the dedicated action so callers resync the SDK.
    #[test]
    fn zen_free_gateway_toggle_round_trip() {
        let mut setup = Setup::default();
        assert_eq!(setup.zen_public_opt_in(), None);
        let mut view = SettingsView::new();

        // Find the row wherever it sits in the selectable order.
        let zen_row = |setup: &Setup| {
            selectable_rows(setup)
                .iter()
                .position(|r| matches!(r, SettingsRow::Category(2)))
                .expect("zen gateway category row exists")
        };

        view.selection.selected_index = zen_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::ZenGatewayToggled)
        );
        assert_eq!(setup.zen_public_opt_in(), Some(true));
        assert!(is_enabled(&settings_items()[2], &setup));

        // Second activation turns it OFF (manual override of any answer).
        view.selection.selected_index = zen_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::ZenGatewayToggled)
        );
        assert_eq!(setup.zen_public_opt_in(), Some(false));
        assert!(!is_enabled(&settings_items()[2], &setup));
    }

    /// The LSP row is a standalone switch: activating it flips
    /// `setup.lsp` and reports the dedicated action so callers resync
    /// the harness's process-wide LSP flag.
    #[test]
    fn lsp_toggle_round_trip() {
        let mut setup = Setup::default();
        assert!(setup.lsp);
        let mut view = SettingsView::new();

        let lsp_row = |setup: &Setup| {
            selectable_rows(setup)
                .iter()
                .position(|r| matches!(r, SettingsRow::Category(5)))
                .expect("lsp category row exists")
        };

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(!setup.lsp);
        assert!(!is_enabled(&settings_items()[5], &setup));

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(setup.lsp);
        assert!(is_enabled(&settings_items()[5], &setup));
    }

    #[test]
    fn toggling_off_clamps_selection() {
        let mut setup = setup_with_hooks(true, &[("a", "cmd a")]);
        let mut view = SettingsView::new();
        view.selection.selected_index = 3; // Add hook (pre)
        // User navigates up to the Hooks toggle and activates it.
        for _ in 0..3 {
            view.select_prev(20, &setup);
        }

        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::ToggleSaved)
        );
        assert!(!setup.hooks.pre_tool_use_enabled);
        assert!(view.selection.selected_index < selectable_rows(&setup).len());
    }

    #[test]
    fn activating_hook_rows_requests_the_registration_box() {
        let mut setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();

        view.selection.selected_index = 1; // Pre-tool hook
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenHookForm {
                event: PRE_TOOL_USE_EVENT,
                index: Some(0)
            })
        );

        view.selection.selected_index = 2; // Add hook (pre)
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenHookForm {
                event: PRE_TOOL_USE_EVENT,
                index: None
            })
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

    /// VERIFICATION + REGRESSION: every section must end with one blank line
    /// before the next section's description. Standalone switches (no hook
    /// sub-list) used to be glued together because the gap only existed
    /// inside the enabled-hook branch. Also asserts the layout's `y` values
    /// are strictly sequential — a dropped `y += 1` elsewhere (e.g. after
    /// the AddHook row) would make two lines share a row and silently eat a
    /// gap while every successor check still passed.
    #[test]
    fn sections_are_separated_by_a_blank_line() {
        // Both a no-hook layout and one with populated hook sub-lists so
        // hook sections exercise their full Category→Blank→Hook→AddHook→Blank
        // sequence.
        let setups = [
            Setup::default(),
            setup_with_hooks(true, &[("block rm", "exit 2"), ("b", "cmd b")]),
            setup_with_mcp_server("docs", true),
        ];
        for setup in &setups {
            let layout = build_layout(setup);

            // Strictly sequential y: no duplicate rows, no skipped rows.
            for (i, line) in layout.iter().enumerate() {
                assert_eq!(
                    line.y, i as u16,
                    "layout y must be strictly sequential at index {i}"
                );
            }

            // A section always STARTS with its description, so every
            // Description (except the first, which follows the title) must
            // be preceded by exactly one blank line — this holds regardless
            // of whether the previous section ended in a toggle, a hook row
            // or an "+ Add hook" entry.
            let mut first_description = true;
            for line in &layout {
                if matches!(line.line, Line::Description(_)) {
                    if first_description {
                        first_description = false;
                        continue;
                    }
                    // Predecessor check via the sequential-y invariant:
                    // layout[line.y - 1] is the row right above this one.
                    assert!(
                        line.y >= 1 && matches!(layout[(line.y - 1) as usize].line, Line::Blank),
                        "description at y={} must follow a blank line",
                        line.y
                    );
                }
            }
        }
    }

    #[test]
    fn render_shows_sublists_only_when_enabled() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 100, 30);

        // Disabled: only toggles, no sub rows.
        let setup = setup_with_hooks(false, &[("hidden", "gone")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        let all: String = (area.y..area.bottom())
            .map(|y| format!("{}\n", line_text(&buf, area, y)))
            .collect();
        assert!(all.contains("✗ PreToolUse hooks"));
        assert!(all.contains("✗ PostToolUse hooks"));
        // The Zen gateway row carries its terms link under the description.
        assert!(all.contains("✗ OpenCode Zen free gateway"));
        assert!(all.contains("https://opencode.ai/legal/terms-of-service"));
        assert!(!all.contains("• hidden"));
        assert!(!all.contains("+ Add hook"));

        // Enabled: hooks and the add action appear under each toggle.
        let setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        let all: String = (area.y..area.bottom())
            .map(|y| format!("{}\n", line_text(&buf, area, y)))
            .collect();
        assert!(all.contains("✔ PreToolUse hooks"));
        assert!(all.contains("• block rm — exit 2"));
        assert!(all.contains("+ Add hook"));
        assert!(all.contains("PostToolUse hooks"));
    }

    #[test]
    fn mouse_hits_pre_hook_row_only_on_its_line() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 100, 40);
        let setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let hook_y = abs_y(&setup, area, |l| {
            matches!(
                l,
                Line::Hook {
                    event: PRE_TOOL_USE_EVENT,
                    hook: 0
                }
            )
        });
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

        // Disabled: the list re-centers; whatever sits under the old hook Y
        // must be a category row — never a phantom hook/add entry.
        let disabled = setup_with_hooks(false, &[("block rm", "exit 2")]);
        let res = view.handle_mouse(&mouse_at(row_x + 6, hook_y), area, &disabled);
        assert!(
            res.is_none()
                || matches!(res, Some(i) if matches!(
                    selectable_rows(&disabled)[i],
                    SettingsRow::Category(_)
                )),
            "disabled layout must not expose hook/add rows, got {res:?}"
        );
        let toggle_y = abs_y(&disabled, area, |l| matches!(l, Line::Category { item: 0 }));
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, toggle_y), area, &disabled),
            Some(0)
        );
    }
}
