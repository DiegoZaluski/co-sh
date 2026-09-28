//! Settings router — a selectable list of configuration categories.
//!
//! Unlike the other flat-list routers (`tools`, `add_provider`), each option
//! carries an explanatory description rendered directly above it, telling
//! the user what the setting controls.
//!
//! Narrow terminals wrap those descriptions word-wise onto following rows
//! instead of clipping them at the right edge.
//!
//! The hook entries own sub-lists: while hooks are enabled every configured
//! hook plus an "Add hook" action appears below its category; disabling
//! hooks hides the sub-lists entirely. Activating a hook opens the shared
//! [`crate::ui::dialogs::DialogType::HookInput`] registration box.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
        SettingsItem {
            id: "summarization_models",
            label: "Summarization models",
            description: "Only these models, in order. Empty: use the agent model. Enter: edit; Delete: remove; Alt+Up/Down: reorder",
            event: "",
        },
        SettingsItem {
            id: "editor",
            label: "Editor",
            description: "Terminal editor for the file explorer (Ctrl+F). Enter: edit. Empty: first of nvim, vim, nano found on $PATH",
            event: "",
        },
        SettingsItem {
            id: "skill_dirs",
            label: "Skill directories",
            description: "Where the skills_* tools look for SKILL.md packs (colon-separated paths, ~ expands to $HOME). Enter: edit. Empty: ~/.skills",
            event: "",
        },
        SettingsItem {
            id: "telemetry",
            label: "Telemetry",
            description: "Share anonymous usage aggregates (no code, no paths, no prompts). On by default; disable here or via COSH_TELEMETRY=off. Takes effect on the next launch",
            event: "",
        },
        // The termination checkup only exists in binaries with the ONNX
        // runtime: no runtime in the build, no item in the TUI — the same
        // pattern the `embed` feature uses.
        #[cfg(feature = "onnx")]
        SettingsItem {
            id: "checkup_termination",
            label: "Termination checkup",
            description: "A local decision model reviews ambiguous loop stops and can continue an agent that stopped mid-task. Costs local inference per turn; the model loads at assembly time. Takes effect on the next launch",
            event: "",
        },
    ]
}

/// The current value of a CHOICE setting (rendered in place of the ✔/✗
/// switch symbol), or `None` for plain switches.
fn cache_choice_value(id: &str, setup: &Setup) -> Option<String> {
    match id {
        "summarization_models" => Some(if setup.routing.summarization_models.is_empty() {
            "same as agent".into()
        } else {
            format!("{} configured", setup.routing.summarization_models.len())
        }),
        "anthropic_cache_ttl" => Some(crate::util::setup::format_cache_duration(
            setup.cache.anthropic_ttl_min,
        )),
        "openai_cache_retention" => Some(crate::util::setup::format_cache_duration(
            setup.cache.openai_retention_min,
        )),
        "editor" => Some(if setup.editor.trim().is_empty() {
            "auto (nvim › vim › nano)".into()
        } else {
            truncate(setup.editor.trim(), COMMAND_PREVIEW_LEN)
        }),
        "skill_dirs" => Some(if setup.skills.dirs.is_empty() {
            "~/.skills".into()
        } else {
            truncate(&setup.skills.dirs.join(":"), COMMAND_PREVIEW_LEN * 2)
        }),
        #[cfg(feature = "onnx")]
        // The row renders "label: value" instead of the ✔/✗ switch, so the
        // value carries BOTH the audit state and the checkpoint identity.
        "checkup_termination" => Some(if !setup.checkup.termination.enabled {
            "off".into()
        } else {
            let checkpoint = match &setup.checkup.model {
                crate::util::setup::CheckupModel::English => "english",
                crate::util::setup::CheckupModel::Multilingual => "multilingual",
                crate::util::setup::CheckupModel::TypedDecisions => "typed-decisions",
                crate::util::setup::CheckupModel::Custom { repo, .. } => repo,
            };
            format!("on · {}", truncate(checkpoint, COMMAND_PREVIEW_LEN))
        }),
        _ => None,
    }
}

/// Each category owns its own switch, so toggling one never leaks into the
/// other event. Items WITHOUT an event (empty string) are standalone
/// switches.
fn is_enabled(item: &SettingsItem, setup: &Setup) -> bool {
    if item.event.is_empty() {
        return match item.id {
            "lsp" => setup.lsp,
            "telemetry" => setup.telemetry,
            #[cfg(feature = "onnx")]
            "checkup_termination" => setup.checkup.termination.enabled,
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

/// Word-aware wrap: breaks `text` into rows of at most `max_width` display
/// columns, splitting only on spaces (the break consumes the space). A
/// single word wider than `max_width` is hard-truncated at the boundary;
/// blank text or a zero width still yields one (empty) row so callers can
/// rely on at least one line.
fn wrap_words(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split(' ') {
        let word_width = word.width();
        if word_width > max_width {
            // Unbreakable: flush what is pending, then chunk the word
            // itself at the width boundary.
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            let mut start = 0;
            let mut taken = 0usize;
            for (i, ch) in word.char_indices() {
                let char_width = ch.width().unwrap_or(0);
                if taken + char_width > max_width {
                    rows.push(word[start..i].to_string());
                    start = i;
                    taken = char_width;
                } else {
                    taken += char_width;
                }
            }
            current = word[start..].to_string();
            current_width = taken;
        } else if current_width == 0 {
            current = word.to_string();
            current_width = word_width;
        } else if current_width + 1 + word_width <= max_width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
        } else {
            rows.push(std::mem::take(&mut current));
            current = word.to_string();
            current_width = word_width;
        }
    }
    rows.push(current);
    rows
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
    SummarizationModel(usize),
    AddSummarizationModel,
    /// Top-level setting (index into `settings_items()`).
    Category(usize),
    /// Configured hook of one event.
    Hook {
        event: &'static str,
        index: usize,
    },
    /// Create a new hook for one event.
    AddHook {
        event: &'static str,
    },
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
    SummarizationModel(usize),
    AddSummarizationModel,
    Title,
    Blank,
    /// One wrapped row of a section description; consecutive rows form a
    /// single logical description block.
    Description(String),
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
            Line::SummarizationModel(index) => Some(SettingsRow::SummarizationModel(*index)),
            Line::AddSummarizationModel => Some(SettingsRow::AddSummarizationModel),
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

/// Description of the trailing MCP section. Servers are registered through
/// the Add box (name + `command args...` or `http(s)://` endpoint); removal
/// of a misconfigured entry is a setup.json edit, disabling covers runtime.
const MCP_SECTION_DESCRIPTION: &str =
    "Connect external MCP servers for extra model tools (stdio or HTTP)";

/// Builds the vertical layout for a `width`-column terminal. Description
/// lines wrap (word-aligned) into consecutive rows at that width, so content
/// height, scroll offset and mouse hit-testing all see the same multi-row
/// blocks. Callers that only need the selection order pass `u16::MAX`, which
/// keeps every description on a single row.
fn build_layout(setup: &Setup, width: u16) -> Vec<LayoutLine> {
    let mut lines = Vec::new();
    let mut y = 0u16;
    lines.push(LayoutLine {
        y,
        line: Line::Title,
    });
    y += 1;
    for item in 0..settings_items().len() {
        for text in wrap_words(settings_items()[item].description, width as usize) {
            lines.push(LayoutLine {
                y,
                line: Line::Description(text),
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
        if settings_items()[item].id == "summarization_models" {
            for index in 0..setup.routing.summarization_models.len() {
                lines.push(LayoutLine {
                    y,
                    line: Line::SummarizationModel(index),
                });
                y += 1;
            }
            lines.push(LayoutLine {
                y,
                line: Line::AddSummarizationModel,
            });
            y += 1;
        }
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
    for text in wrap_words(MCP_SECTION_DESCRIPTION, width as usize) {
        lines.push(LayoutLine {
            y,
            line: Line::Description(text),
        });
        y += 1;
    }
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
    // Width-independent: only non-selectable description rows wrap, so the
    // selectable order (and every saved selection index) never changes with
    // the terminal width.
    build_layout(setup, u16::MAX)
        .iter()
        .filter_map(|l| l.line.row())
        .collect()
}

fn content_height(setup: &Setup, width: u16) -> u16 {
    build_layout(setup, width).len() as u16
}

/// What happened after the user activated a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    OpenSummarizationModel {
        index: Option<usize>,
    },
    /// Persisted state changed; caller must `setup.save()`.
    ToggleSaved,
    /// The LSP switch flipped; caller must `setup.save()`, flip the harness's
    /// process-wide LSP flag, and resync the app's `lsp_available` state.
    LspToggled,
    /// Telemetry consent flipped; caller must `setup.save()`. The new consent
    /// takes effect when the facade is next resolved (app restart).
    TelemetryToggled,
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
    OpenCacheInput {
        setting: &'static str,
    },
    /// Open the editor-command input box (Settings → Editor; used by the
    /// Ctrl+F file explorer). Blank = auto-detect (nvim → vim → nano).
    OpenEditorInput,
    /// Open the skill-directories input box (Settings → Skill directories;
    /// setup.json `skills.dirs`). Blank = the `~/.skills` default.
    OpenSkillsInput,
}

/// Last-typed content of the MCP registration box
/// (`DialogType::McpForm`). Held by the `App` while no dialog is open so
/// an accidental close (Esc, click outside, any dismiss) never loses
/// what was already typed: reopening the form prefills from here. The
/// draft is cleared only when a save succeeds (Enter) — or when the user
/// erases the fields themselves, which simply re-syncs blanks into it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct McpFormDraft {
    pub name: String,
    pub endpoint: String,
    pub timeout: String,
    /// The raw typed key (never persisted anywhere but the draft); masked
    /// in [`Debug`] like every other credential preview.
    pub api_key: String,
    /// Active field: 0 name · 1 endpoint · 2 timeout · 3 api key.
    pub field: usize,
    pub cursor_pos: usize,
}

impl std::fmt::Debug for McpFormDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpFormDraft")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("timeout", &self.timeout)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "…" })
            .field("field", &self.field)
            .field("cursor_pos", &self.cursor_pos)
            .finish()
    }
}

// View

pub struct SettingsView {
    pub selection: ListSelection,
}

impl SettingsView {
    /// Shared by keyboard shortcuts and the inline mouse controls.
    pub fn change_summarizer(&mut self, setup: &mut Setup, operation: &str) -> bool {
        let rows = selectable_rows(setup);
        let Some(SettingsRow::SummarizationModel(index)) = rows.get(self.selection.selected_index)
        else {
            return false;
        };
        let index = *index;
        let list = &mut setup.routing.summarization_models;
        let next = match operation {
            "remove" => {
                list.remove(index);
                index.min(list.len().saturating_sub(1))
            }
            "up" if index > 0 => {
                list.swap(index, index - 1);
                index - 1
            }
            "down" if index + 1 < list.len() => {
                list.swap(index, index + 1);
                index + 1
            }
            _ => return false,
        };
        let rows = selectable_rows(setup);
        self.selection.selected_index = rows
            .iter()
            .position(|row| *row == SettingsRow::SummarizationModel(next))
            .or_else(|| {
                rows.iter()
                    .position(|row| *row == SettingsRow::AddSummarizationModel)
            })
            .unwrap_or(0);
        self.selection.clamp(rows.len());
        true
    }

    fn viewport_offset(&self, area: Rect, setup: &Setup) -> u16 {
        let selected = selectable_rows(setup)
            .get(self.selection.selected_index)
            .copied();
        build_layout(setup, area.width)
            .iter()
            .find(|line| line.line.row() == selected && selected.is_some())
            .map_or(0, |line| {
                line.y.saturating_sub(area.height.saturating_sub(1))
            })
    }

    pub fn activate_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        setup: &mut Setup,
    ) -> Option<SettingsAction> {
        if matches!(
            selectable_rows(setup).get(self.selection.selected_index),
            Some(SettingsRow::SummarizationModel(_))
        ) {
            let controls = area.right().saturating_sub(18).max(area.x);
            if mouse.x >= controls {
                let operation = match mouse.x - controls {
                    0..=5 => "up",
                    6..=11 => "down",
                    _ => "remove",
                };
                return self
                    .change_summarizer(setup, operation)
                    .then_some(SettingsAction::ToggleSaved);
            }
        }
        self.activate_selected(setup)
    }
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
            SettingsRow::SummarizationModel(index) => {
                Some(SettingsAction::OpenSummarizationModel {
                    index: Some(*index),
                })
            }
            SettingsRow::AddSummarizationModel => {
                Some(SettingsAction::OpenSummarizationModel { index: None })
            }
            SettingsRow::Category(i) => {
                let item = &settings_items()[*i];
                if item.id == "summarization_models" {
                    return Some(SettingsAction::OpenSummarizationModel { index: None });
                }
                // The editor setting is a free-form command, not a switch:
                // open its own input box (the file explorer launches it).
                if item.id == "editor" {
                    return Some(SettingsAction::OpenEditorInput);
                }
                if item.id == "skill_dirs" {
                    return Some(SettingsAction::OpenSkillsInput);
                }
                // Choice settings (cache TTL/retention) open a duration
                // input box instead of toggling a switch — the value is a
                // free-form user choice, not a hardcoded cycle.
                //
                // The termination checkup is NOT that: its "value" is the
                // read-only resident checkpoint identity, so activation
                // toggles the audit switch instead of opening an input.
                #[cfg(feature = "onnx")]
                if item.event.is_empty() && item.id == "checkup_termination" {
                    setup.checkup.termination.enabled = !setup.checkup.termination.enabled;
                    self.selection.clamp(selectable_rows(setup).len());
                    return Some(SettingsAction::ToggleSaved);
                }
                if cache_choice_value(item.id, setup).is_some() {
                    return Some(SettingsAction::OpenCacheInput { setting: item.id });
                }
                // Standalone switches (no hook sub-list) flip their own
                // persisted field.
                if item.event.is_empty() && item.id == "lsp" {
                    setup.lsp = !setup.lsp;
                    self.selection.clamp(selectable_rows(setup).len());
                    return Some(SettingsAction::LspToggled);
                }
                // Telemetry consent: default-ON switch (opt-out, industry
                // standard). The caller persists; the change takes effect
                // on the next launch.
                if item.event.is_empty() && item.id == "telemetry" {
                    setup.telemetry = !setup.telemetry;
                    self.selection.clamp(selectable_rows(setup).len());
                    return Some(SettingsAction::TelemetryToggled);
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

    /// Remove the MCP server selected in the settings list, if any. Returns
    /// the removed entry's name so the caller can purge its keyring
    /// credential (an orphaned `mcp:<name>` entry would otherwise outlive
    /// the registration and keep a live secret for a server that no longer
    /// exists).
    pub fn remove_selected_mcp_server(&mut self, setup: &mut Setup) -> Option<String> {
        let rows = selectable_rows(setup);
        let index = match rows.get(self.selection.selected_index) {
            Some(SettingsRow::McpServer(index)) => *index,
            _ => return None,
        };
        if index >= setup.mcp.servers.len() {
            return None;
        }
        let removed = setup.mcp.servers.remove(index);
        let rows = selectable_rows(setup);
        self.selection.clamp(rows.len());
        Some(removed.name)
    }

    pub fn handle_mouse(&self, mouse: &MouseEvent, area: Rect, setup: &Setup) -> Option<usize> {
        self.find_row_for_mouse(mouse, area, setup)
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect, setup: &Setup) -> Option<usize> {
        let max_row_w = max_row_width(setup, area.width);
        if max_row_w == 0 {
            return None;
        }
        let row_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        // Only option/sub rows are clickable — never titles or descriptions.
        let hit_x = mouse.x >= row_x
            && mouse.x < area.right()
            && mouse.y >= area.y
            && mouse.y < area.bottom();
        if !hit_x {
            return None;
        }
        let start_y = content_start_y(area, setup);
        let offset = self.viewport_offset(area, setup);
        build_layout(setup, area.width)
            .iter()
            .find(|l| l.y as u32 + start_y as u32 == mouse.y as u32 + offset as u32)
            .and_then(|l| l.line.row())
            .and_then(|row| selectable_rows(setup).iter().position(|r| r == &row))
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, setup: &Setup) {
        let fg = rgba_color(theme.text);
        let muted = rgba_color(theme.text_muted);
        let primary = rgba_color(theme.primary);

        let start_y = content_start_y(area, setup);
        let layout = build_layout(setup, area.width);

        let max_w = max_row_width(setup, area.width);
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
        let offset = self.viewport_offset(area, setup);

        for line in &layout {
            let Some(y) = (start_y + line.y).checked_sub(offset) else {
                continue;
            };
            if y < area.y {
                continue;
            }
            if y >= area.bottom() {
                break;
            }
            match &line.line {
                Line::SummarizationModel(index) => {
                    let selected =
                        rows.get(selected_idx) == Some(&SettingsRow::SummarizationModel(*index));
                    let entry = &setup.routing.summarization_models[*index];
                    let controls = area.right().saturating_sub(18).max(area.x);
                    let text = truncate(
                        &format!("  {}. {}/{}", index + 1, entry.provider, entry.model),
                        controls.saturating_sub(row_x) as usize,
                    );
                    let style = Style::default().fg(if selected { primary } else { fg });
                    draw_text(buf, &text, row_x, y, area, style);
                    draw_text(buf, "[ ↑ ] [ ↓ ] [ × ]", controls, y, area, style);
                }
                Line::AddSummarizationModel => {
                    let selected =
                        rows.get(selected_idx) == Some(&SettingsRow::AddSummarizationModel);
                    draw_text(
                        buf,
                        "+ Add summarization model",
                        row_x + 2,
                        y,
                        area,
                        Style::default().fg(if selected { primary } else { muted }),
                    );
                }
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
                    let shown = true;
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
    area.y
        + (area
            .height
            .saturating_sub(content_height(setup, area.width)))
            / 2
}

fn max_row_width(setup: &Setup, width: u16) -> usize {
    // Descriptions wrap to the terminal width, so their widest rendered row
    // never exceeds it.
    let width_cap = width as usize;
    let mut width = settings_items()
        .iter()
        .map(|item| {
            let mut len = item.description.len().max(2 + item.label.len()); // "✔ PreToolUse hooks"
            // Choice rows render "label: value" at the same indent.
            if let Some(v) = cache_choice_value(item.id, setup) {
                len = len.max(2 + item.label.len() + 2 + v.len());
            }
            len.min(width_cap)
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
    width = width.max(MCP_SECTION_DESCRIPTION.len().min(width_cap));
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
    #[test]
    fn summarizer_list_reorders_edits_removes_to_default_and_scrolls_with_mouse_alignment() {
        let mut setup = Setup::default();
        setup.routing.summarization_models = (0..30)
            .map(|index| crate::util::setup::FallbackEntry {
                provider: "local".into(),
                model: format!("model-{index}"),
            })
            .collect();
        let mut view = SettingsView::new();
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|row| *row == SettingsRow::SummarizationModel(29))
            .unwrap();
        let area = Rect::new(3, 2, 90, 12);
        let mut buffer = Buffer::empty(area);
        view.render(&mut buffer, area, &test_theme(), &setup);
        let last_y = area.bottom() - 1;
        assert!(line_text(&buffer, area, last_y).contains("model-29"));
        let mouse = mouse_at(area.x + 5, last_y);
        assert_eq!(
            view.handle_mouse(&mouse, area, &setup),
            Some(view.selection.selected_index)
        );
        assert_eq!(
            view.activate_mouse(&mouse, area, &mut setup),
            Some(SettingsAction::OpenSummarizationModel { index: Some(29) })
        );
        assert!(view.change_summarizer(&mut setup, "up"));
        assert_eq!(setup.routing.summarization_models[28].model, "model-29");
        assert!(view.change_summarizer(&mut setup, "down"));
        assert_eq!(setup.routing.summarization_models[29].model, "model-29");
        let remove = mouse_at(area.right() - 3, last_y);
        assert_eq!(
            view.activate_mouse(&remove, area, &mut setup),
            Some(SettingsAction::ToggleSaved)
        );
        while !setup.routing.summarization_models.is_empty() {
            assert!(view.change_summarizer(&mut setup, "remove"));
        }
        assert_eq!(
            cache_choice_value("summarization_models", &setup).as_deref(),
            Some("same as agent")
        );
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenSummarizationModel { index: None })
        );
    }
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
        let layout = build_layout(setup, area.width);
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
                SettingsRow::AddSummarizationModel,
                SettingsRow::Category(6),
                SettingsRow::Category(7),
                SettingsRow::Category(8),
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
                SettingsRow::AddSummarizationModel,
                SettingsRow::Category(6),
                SettingsRow::Category(7),
                SettingsRow::Category(8),
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
            .position(|r| matches!(r, SettingsRow::Category(2)))
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
            .position(|r| matches!(r, SettingsRow::Category(3)))
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
                .position(|r| matches!(r, SettingsRow::Category(4)))
                .expect("lsp category row exists")
        };

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(!setup.lsp);
        assert!(!is_enabled(&settings_items()[4], &setup));

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(setup.lsp);
        assert!(is_enabled(&settings_items()[4], &setup));
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

    /// The Editor row is a free-form command setting: activating it opens
    /// the editor-command input (never a switch flip), the row renders the
    /// configured command (or the auto-detect hint), and the typed value
    /// round-trips into `setup.editor`.
    #[test]
    fn editor_setting_opens_input_and_round_trips() {
        let mut setup = Setup::default();
        let mut view = SettingsView::new();

        // Blank configured command: the row shows the fallback hint.
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(6)))
            .expect("editor category row exists");
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenEditorInput)
        );
        assert_eq!(setup.editor, "", "activation never mutates the setting");

        // The rendered row carries the auto-detect hint.
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
        assert!(all.contains("Editor: auto (nvim › vim › nano)"));

        // The choice display for a configured command shows the command.
        setup.editor = "vim -u NONE".into();
        assert_eq!(
            cache_choice_value("editor", &setup),
            Some("vim -u NONE".to_string())
        );
    }

    /// The Skill-directories row is a free-form path setting: activating it
    /// opens the skills input (never a switch flip), the row renders the
    /// configured list (or the ~/.skills default hint), and the typed value
    /// round-trips into `setup.skills.dirs`.
    #[test]
    fn skill_dirs_setting_opens_input_and_round_trips() {
        let mut setup = Setup::default();
        let mut view = SettingsView::new();

        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(7)))
            .expect("skill_dirs category row exists");
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenSkillsInput)
        );
        assert!(
            setup.skills.dirs.is_empty(),
            "activation never mutates the setting"
        );

        // The rendered row carries the default hint.
        let theme = test_theme();
        let area = Rect::new(0, 0, 120, 40);
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
        assert!(all.contains("Skill directories: ~/.skills"));

        // The choice display for a configured list joins the paths.
        setup.skills.dirs = vec!["~/a".into(), "/opt/b".into()];
        assert_eq!(
            cache_choice_value("skill_dirs", &setup),
            Some("~/a:/opt/b".to_string())
        );
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
            let layout = build_layout(setup, u16::MAX);

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
        let row_x = area.x
            + (area
                .width
                .saturating_sub(max_row_width(&setup, area.width) as u16))
                / 2;

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

    #[test]
    fn wrap_words_breaks_on_spaces_within_the_width() {
        // Short text stays on one row.
        assert_eq!(wrap_words("one two three", 20), vec!["one two three"]);
        // Breaks happen at spaces; the space is consumed by the break.
        assert_eq!(wrap_words("aaa bb cccc", 7), vec!["aaa bb", "cccc"]);
        // Continuation rows keep words whole, never splitting mid-word.
        assert_eq!(
            wrap_words("alpha beta gamma", 9),
            vec!["alpha", "beta", "gamma"]
        );
        // No trailing space leaks onto the next row.
        assert_eq!(wrap_words("aa bb", 2), vec!["aa", "bb"]);
    }

    #[test]
    fn wrap_words_hard_truncates_only_unbreakable_words() {
        // A single word longer than the limit is cut at the boundary.
        assert_eq!(wrap_words("abcdefgh", 3), vec!["abc", "def", "gh"]);
        // Pending content flushes before the oversized word starts.
        assert_eq!(
            wrap_words("hi abcdefghij", 4),
            vec!["hi", "abcd", "efgh", "ij"]
        );
        // Zero width degrades to a single empty row instead of looping.
        assert_eq!(wrap_words("anything", 0), vec![""]);
        // Multi-column characters count display width, not bytes or chars.
        assert_eq!(
            wrap_words("日本語 テスト", 4),
            vec!["日本", "語", "テス", "ト"]
        );
    }

    /// Narrow terminal: descriptions wrap word-wise onto following rows
    /// instead of being clipped at the right edge, every drawn row stays
    /// inside the width, and mouse hit-testing keeps matching the rendered
    /// (wrapped) layout.
    #[test]
    fn narrow_width_wraps_descriptions_instead_of_clipping() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 60, 40);
        let setup = Setup::default();
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        // The telemetry description exceeds 60 columns; without wrapping its
        // tail would be cut off at the right edge.
        let all: String = (area.y..area.bottom())
            .map(|y| format!("{}\n", line_text(&buf, area, y)))
            .collect();
        assert!(all.contains("COSH_TELEMETRY=off"));
        assert!(all.contains("Takes effect on the next launch"));
        // No drawn row overflows the terminal width.
        for y in area.y..area.bottom() {
            assert!(
                line_text(&buf, area, y).width() <= area.width as usize,
                "row {y} overflows the {w}-column terminal",
                w = area.width
            );
        }

        // Clicking the Language-servers toggle row (below its wrapped
        // description) still selects it: offset and start_y derive from the
        // same wrapped layout the render used.
        let lsp_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(4)))
            .expect("lsp category row exists");
        view.selection.selected_index = lsp_index;
        let layout = build_layout(&setup, area.width);
        let row_y = content_start_y(area, &setup)
            + layout
                .iter()
                .find(|l| matches!(l.line, Line::Category { item: 4 }))
                .expect("lsp layout row")
                .y;
        let row_x = area.x
            + (area
                .width
                .saturating_sub(max_row_width(&setup, area.width) as u16))
                / 2;
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 4, row_y), area, &setup),
            Some(lsp_index)
        );
    }
}
