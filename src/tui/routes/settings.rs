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
use cosh::setup::{HookEntry, Setup};

/// Event section keys inside `setup.json` (`hooks.events`).
pub const PRE_TOOL_USE_EVENT: &str = "PreToolUse";
pub const POST_TOOL_USE_EVENT: &str = "PostToolUse";

/// Maximum characters of a hook command shown in list rows.
const COMMAND_PREVIEW_LEN: usize = 34;

/// Horizontal breathing room between a section's contour and its content,
/// on each side (columns). The contour itself has no side walls — only the
/// closed `╭…╮` / `╰…╯` top and bottom rows — so this padding is what keeps
/// the content from touching the drawn margins.
const BOX_PAD: usize = 2;

/// Reorder/remove controls drawn on summarizer-model rows. Render and the
/// mouse hit zones must agree on this width.
const SUMMARIZER_CONTROLS: &str = "[ ↑ ] [ ↓ ] [ × ]";

// Catalog

/// One top-level entry of the Settings list. `id` keys dialogs and tests;
/// `label` is the option name; `description` explains what the setting
/// controls; `kind` drives BOTH rendering and activation.
struct SettingsItem {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    kind: SettingKind,
}

/// What an item renders as and does on activation. Keeping the behaviour in
/// the catalog means adding a setting is ONE struct literal — never edits
/// scattered across render, enable-check and activation dispatch.
#[derive(PartialEq, Eq)]
enum SettingKind {
    /// Hook category: a toggle plus (while enabled) the event's hook list.
    HookList(&'static str),
    /// Fallback-model list with its Add/edit sub-rows.
    ModelList,
    /// Registered MCP servers with their Add action.
    ServerList,
    /// Boolean switch flipped in place on activation.
    Switch(SwitchAction),
    /// Value row rendered as "label: value"; activation opens an input box.
    Choice(ChoiceInput),
}

#[derive(PartialEq, Eq)]
enum SwitchAction {
    /// Flip `setup.lsp`; the caller resyncs the harness's process-wide flag.
    Lsp,
    /// Flip telemetry consent; effective on the next launch.
    Telemetry,
    /// Flip the item's own persisted field and report
    /// [`SettingsAction::ToggleSaved`].
    Persist,
}

#[derive(PartialEq, Eq)]
enum ChoiceInput {
    /// Cache-duration input keyed by the Settings-item id
    /// ("anthropic_cache_ttl" / "openai_cache_retention").
    Cache(&'static str),
    /// Editor-command input (the file explorer launches it).
    Editor,
    /// Skill-directories input (`setup.skills.dirs`).
    Skills,
    /// Checkup model picker (`setup.decision.model`: english | multilingual
    /// | typed-decisions — custom checkpoints stay in setup.json).
    #[cfg(feature = "onnx")]
    CheckupModel,
    /// Checkup min-confidence input (`setup.decision.termination
    /// .min_confidence`), a 0–1 float.
    #[cfg(feature = "onnx")]
    CheckupMinConfidence,
}

/// A bordered group of related settings. Sections order from most-touched
/// (per-session knobs) to rarely-changed infrastructure.
struct SettingsSection {
    title: &'static str,
    items: &'static [SettingsItem],
}

const AUTOMATION: SettingsSection = SettingsSection {
    title: "Automation",
    items: &[
        SettingsItem {
            id: "summarization_models",
            label: "Summarization models",
            description: "Fallback models used for context summarization",
            kind: SettingKind::ModelList,
        },
        SettingsItem {
            id: "hooks",
            label: "PreToolUse hooks",
            description: "Shell commands run before every tool call",
            kind: SettingKind::HookList(PRE_TOOL_USE_EVENT),
        },
        SettingsItem {
            id: "post_tool_use_hooks",
            label: "PostToolUse hooks",
            description: "Shell commands run after every successful tool call",
            kind: SettingKind::HookList(POST_TOOL_USE_EVENT),
        },
        // The termination checkup only exists in binaries with the ONNX
        // runtime: no runtime in the build, no item in the TUI — the same
        // pattern the `embed` feature uses.
        #[cfg(feature = "onnx")]
        SettingsItem {
            id: "checkup_termination",
            label: "Termination checkup",
            description: "A local model reviews ambiguous loop stops and may resume the agent (local inference per turn)",
            kind: SettingKind::Switch(SwitchAction::Persist),
        },
        // The audit's resident model: which checkpoint the loader picks at
        // harness assembly. Custom checkpoints stay in setup.json.
        #[cfg(feature = "onnx")]
        SettingsItem {
            id: "checkup_model",
            label: "Checkup model",
            description: "Checkpoint used by the decision models (english | multilingual | typed-decisions)",
            kind: SettingKind::Choice(ChoiceInput::CheckupModel),
        },
        // The confidence floor: how sure a "not finished" verdict must be
        // before it can veto the loop's end.
        #[cfg(feature = "onnx")]
        SettingsItem {
            id: "checkup_min_confidence",
            label: "Checkup min confidence",
            description: "Confidence floor for a veto: 0 always continues, 1 never does (default 60%)",
            kind: SettingKind::Choice(ChoiceInput::CheckupMinConfidence),
        },
    ],
};

const MODELS_AND_CACHING: SettingsSection = SettingsSection {
    title: "Models & caching",
    items: &[
        SettingsItem {
            id: "anthropic_cache_ttl",
            label: "Anthropic cache TTL",
            description: "Prompt-cache lifetime for Anthropic (1h writes cost 2×)",
            kind: SettingKind::Choice(ChoiceInput::Cache("anthropic_cache_ttl")),
        },
        SettingsItem {
            id: "openai_cache_retention",
            label: "OpenAI cache retention",
            description: "How long OpenAI may keep your prompt cache",
            kind: SettingKind::Choice(ChoiceInput::Cache("openai_cache_retention")),
        },
    ],
};

const ENVIRONMENT: SettingsSection = SettingsSection {
    title: "Environment",
    items: &[
        SettingsItem {
            id: "editor",
            label: "Editor",
            description: "Terminal editor opened by the file explorer (Ctrl+F)",
            kind: SettingKind::Choice(ChoiceInput::Editor),
        },
        SettingsItem {
            id: "skill_dirs",
            label: "Skill directories",
            description: "Where the skills_* tools look for SKILL.md packs",
            kind: SettingKind::Choice(ChoiceInput::Skills),
        },
        SettingsItem {
            id: "lsp",
            label: "Language servers",
            description: "Language servers powering diagnostics, hover and symbols",
            kind: SettingKind::Switch(SwitchAction::Lsp),
        },
    ],
};

// External capability sources (stdio/HTTP tool servers today; plugins, ACP
// and similar integrations would land here). Kept semantically separate
// from Environment (local workspace tooling) so future items have a home.
const INTEGRATIONS: SettingsSection = SettingsSection {
    title: "Integrations",
    items: &[SettingsItem {
        id: "mcp",
        label: "MCP servers",
        description: "External tool servers connected over stdio or HTTP",
        kind: SettingKind::ServerList,
    }],
};

const GENERAL: SettingsSection = SettingsSection {
    title: "General",
    items: &[SettingsItem {
        id: "telemetry",
        label: "Telemetry",
        description: "Anonymous usage aggregates; disable here or with COSH_TELEMETRY=off. Applies next launch",
        kind: SettingKind::Switch(SwitchAction::Telemetry),
    }],
};

fn sections() -> &'static [SettingsSection] {
    &[
        AUTOMATION,
        MODELS_AND_CACHING,
        ENVIRONMENT,
        INTEGRATIONS,
        GENERAL,
    ]
}

/// Flat catalog in display order: (global index, item). Row indices in
/// [`SettingsRow::Category`] and activation dispatch are these indexes.
fn all_items() -> impl Iterator<Item = (usize, &'static SettingsItem)> {
    sections()
        .iter()
        .flat_map(|section| section.items.iter())
        .enumerate()
}

/// Catalog item by flat display index (the `Category(row)` index).
fn item_at(index: usize) -> &'static SettingsItem {
    all_items()
        .find(|(i, _)| *i == index)
        .map(|(_, item)| item)
        .expect("category index within the catalog")
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
        "anthropic_cache_ttl" => Some(cosh::setup::format_cache_duration(
            setup.cache.anthropic_ttl_min,
        )),
        "openai_cache_retention" => Some(cosh::setup::format_cache_duration(
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
        // The MCP category row renders as a count, never a ✔/✗ switch:
        // toggling belongs to the individual server rows below it.
        "mcp" => Some(format!("{} configured", setup.mcp.servers.len())),
        #[cfg(feature = "onnx")]
        // The row renders "label: value" instead of the ✔/✗ switch, so the
        // value carries BOTH the audit state and the checkpoint identity.
        "checkup_termination" => Some(if !setup.decision.termination.enabled {
            "off".into()
        } else {
            format!(
                "on · {}",
                truncate(setup.decision.model.kind_name(), COMMAND_PREVIEW_LEN)
            )
        }),
        #[cfg(feature = "onnx")]
        "checkup_model" => Some(setup.decision.model.kind_name().to_string()),
        #[cfg(feature = "onnx")]
        "checkup_min_confidence" => Some(format!(
            "{:.0}%",
            setup.decision.termination.min_confidence * 100.0
        )),
        _ => None,
    }
}

/// Each hook category owns its own switch, so toggling one never leaks
/// into the other event.
fn is_enabled(item: &SettingsItem, setup: &Setup) -> bool {
    match item.kind {
        SettingKind::HookList(event) => setup.hooks.is_event_enabled(event),
        SettingKind::Switch(SwitchAction::Lsp) => setup.lsp,
        SettingKind::Switch(SwitchAction::Telemetry) => setup.telemetry,
        #[cfg(feature = "onnx")]
        // The checkup renders as a choice ("off" / "on · checkpoint") but
        // its ON/OFF state is the switch itself.
        SettingKind::Switch(SwitchAction::Persist) => setup.decision.termination.enabled,
        _ => false,
    }
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
    /// Top-level setting (index into the flat catalog `all_items()`).
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
    /// Section contour: the opening `╭─ Title ───…` row or the closing
    /// `╰────…` row of one [`SettingsSection`]. Never selectable.
    SectionBorder {
        title: &'static str,
        /// `true` = opening border (carries the title), `false` = closing.
        top: bool,
    },
    Blank,
    /// One wrapped row of a section description; consecutive rows form a
    /// single logical description block.
    Description(String),
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
            Line::Title | Line::SectionBorder { .. } | Line::Blank | Line::Description(_) => None,
        }
    }
}

/// Builds the vertical layout for a `width`-column terminal. Description
/// lines wrap (word-aligned) into consecutive rows at that width, so content
/// height, scroll offset and mouse hit-testing all see the same multi-row
/// blocks. Callers that only need the selection order pass `u16::MAX`, which
/// keeps every description on a single row.
fn build_layout(setup: &Setup, width: u16) -> Vec<LayoutLine> {
    // Descriptions wrap inside the box's inner width, leaving room for the
    // contour padding on both sides (the `u16::MAX` selection-order call
    // keeps single rows either way).
    let inner_width = width.saturating_sub(2 * BOX_PAD as u16) as usize;
    let mut lines = Vec::new();
    let mut y = 0u16;
    lines.push(LayoutLine {
        y,
        line: Line::Title,
    });
    y += 1;
    // `SettingsRow::Category` indices are the item's position in the flat
    // display order — the same enumeration `all_items()` produces.
    let mut item_index = 0usize;
    let section_count = sections().len();
    for (section_index, section) in sections().iter().enumerate() {
        lines.push(LayoutLine {
            y,
            line: Line::SectionBorder {
                title: section.title,
                top: true,
            },
        });
        y += 1;
        for (item_pos, item) in section.items.iter().enumerate() {
            let index = item_index;
            item_index += 1;
            // Breathing room between siblings inside the box (not before
            // the first, which sits right under the opening border).
            if item_pos > 0 {
                lines.push(LayoutLine {
                    y,
                    line: Line::Blank,
                });
                y += 1;
            }
            for text in wrap_words(item.description, inner_width) {
                lines.push(LayoutLine {
                    y,
                    line: Line::Description(text),
                });
                y += 1;
            }
            lines.push(LayoutLine {
                y,
                line: Line::Category { item: index },
            });
            y += 1;
            match item.kind {
                SettingKind::ModelList => {
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
                SettingKind::HookList(event) => {
                    // The hook sub-list exists only while its category is
                    // enabled.
                    if is_enabled(item, setup) {
                        lines.push(LayoutLine {
                            y,
                            line: Line::Blank,
                        }); // breathing room
                        y += 1;
                        for hook in 0..hook_entries(setup, event).len() {
                            lines.push(LayoutLine {
                                y,
                                line: Line::Hook { event, hook },
                            });
                            y += 1;
                        }
                        // The add action reads as the last entry of the hook list.
                        lines.push(LayoutLine {
                            y,
                            line: Line::AddHook { event },
                        });
                        y += 1;
                    }
                }
                // Always visible, like an enabled hook list: one toggle row
                // per registered server plus the Add action.
                SettingKind::ServerList => {
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
                    y += 1;
                }
                // Switches and choices are a single toggle row.
                _ => {}
            }
        }
        lines.push(LayoutLine {
            y,
            line: Line::SectionBorder {
                title: section.title,
                top: false,
            },
        });
        y += 1;
        // One blank between boxes; none after the last (it would only
        // inflate content_height and skew the vertical centering).
        if section_index + 1 < section_count {
            lines.push(LayoutLine {
                y,
                line: Line::Blank,
            });
            y += 1;
        }
    }
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
    /// Open the checkup-model picker (`english` / `multilingual` /
    /// `typed-decisions`). Custom checkpoints stay setup.json-only.
    #[cfg(feature = "onnx")]
    OpenCheckupModelDialog,
    /// Open the checkup min-confidence input (a 0–1 float; the persisted
    /// value is clamped to that range).
    #[cfg(feature = "onnx")]
    OpenCheckupMinConfidenceInput,
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

    /// Scroll offset keeping the selected row visible while framing whole
    /// section blocks: the viewport pins the selected block's closing
    /// border (`╰…╯`) at the bottom edge, so scrolling slides the entire
    /// box up instead of cutting it in half. A block taller than the
    /// viewport falls back to pinning the selected row itself — the only
    /// way to keep it on screen.
    fn viewport_offset(&self, area: Rect, setup: &Setup) -> u16 {
        let layout = build_layout(setup, area.width);
        let selected = selectable_rows(setup)
            .get(self.selection.selected_index)
            .copied();
        let Some(sel_line) = layout
            .iter()
            .find(|line| line.line.row() == selected && selected.is_some())
        else {
            return 0;
        };
        // Rows the render can actually draw: content starts at `start_y`
        // (vertical centering when everything fits) and runs to the bottom.
        let start_y = content_start_y(area, setup);
        let window = area.bottom().saturating_sub(start_y).max(1);
        // The selected row's enclosing block: the last opening border at or
        // above it and the first closing border at or below it (the layout
        // always closes every box it opens).
        let mut top_y = 0u16;
        for line in &layout {
            if line.y > sel_line.y {
                break;
            }
            if let Line::SectionBorder { top: true, .. } = line.line {
                top_y = line.y;
            }
        }
        let bottom_y = layout
            .iter()
            .skip_while(|line| line.y < sel_line.y)
            .find(|line| matches!(line.line, Line::SectionBorder { top: false, .. }))
            .map_or(sel_line.y, |line| line.y);
        if bottom_y.saturating_sub(top_y) + 1 <= window {
            // Whole block fits: pin its closing border to the last visible
            // row (0 while the content still fits without scrolling).
            bottom_y.saturating_sub(window - 1)
        } else {
            sel_line.y.saturating_sub(window - 1)
        }
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
            // Same geometry the render uses: controls hug the box's right
            // inner edge, so the hit zones stay under the drawn buttons.
            let max_row_w = max_row_width(setup, area.width) as u16;
            let row_x = area.x + (area.width.saturating_sub(max_row_w)) / 2 + BOX_PAD as u16;
            let controls = (row_x + max_row_w)
                .saturating_sub(2 * BOX_PAD as u16 + SUMMARIZER_CONTROLS.width() as u16)
                .max(row_x);
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
                let item = all_items()
                    .find(|(index, _)| index == i)
                    .map(|(_, item)| item)
                    .expect("category index within the catalog");
                match &item.kind {
                    SettingKind::ModelList => {
                        return Some(SettingsAction::OpenSummarizationModel { index: None });
                    }
                    // Value rows open their own input box instead of
                    // toggling a switch — the value is a free-form user
                    // choice, not a hardcoded cycle.
                    SettingKind::Choice(ChoiceInput::Cache(setting)) => {
                        return Some(SettingsAction::OpenCacheInput { setting });
                    }
                    SettingKind::Choice(ChoiceInput::Editor) => {
                        return Some(SettingsAction::OpenEditorInput);
                    }
                    SettingKind::Choice(ChoiceInput::Skills) => {
                        return Some(SettingsAction::OpenSkillsInput);
                    }
                    #[cfg(feature = "onnx")]
                    SettingKind::Choice(ChoiceInput::CheckupModel) => {
                        return Some(SettingsAction::OpenCheckupModelDialog);
                    }
                    #[cfg(feature = "onnx")]
                    SettingKind::Choice(ChoiceInput::CheckupMinConfidence) => {
                        return Some(SettingsAction::OpenCheckupMinConfidenceInput);
                    }
                    SettingKind::Switch(SwitchAction::Lsp) => {
                        setup.lsp = !setup.lsp;
                        self.selection.clamp(selectable_rows(setup).len());
                        return Some(SettingsAction::LspToggled);
                    }
                    // Telemetry consent: default-ON switch (opt-out,
                    // industry standard). The caller persists; the change
                    // takes effect on the next launch.
                    SettingKind::Switch(SwitchAction::Telemetry) => {
                        setup.telemetry = !setup.telemetry;
                        self.selection.clamp(selectable_rows(setup).len());
                        return Some(SettingsAction::TelemetryToggled);
                    }
                    SettingKind::Switch(SwitchAction::Persist) => {
                        setup.decision.termination.enabled = !setup.decision.termination.enabled;
                        self.selection.clamp(selectable_rows(setup).len());
                        return Some(SettingsAction::ToggleSaved);
                    }
                    // Each hook category flips only its own event's switch.
                    SettingKind::HookList(event) => match *event {
                        PRE_TOOL_USE_EVENT => {
                            setup.hooks.pre_tool_use_enabled = !setup.hooks.pre_tool_use_enabled;
                        }
                        POST_TOOL_USE_EVENT => {
                            setup.hooks.post_tool_use_enabled = !setup.hooks.post_tool_use_enabled;
                        }
                        _ => {}
                    },
                    // The MCP category row itself just opens the wizard;
                    // toggling belongs to the individual server rows.
                    SettingKind::ServerList => {
                        return Some(SettingsAction::OpenMcpForm);
                    }
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
        // Same geometry the render uses: content rows start BOX_PAD columns
        // inside the contour's left edge.
        let box_x = area.x + (area.width.saturating_sub(max_row_w as u16)) / 2;
        let row_x = box_x + BOX_PAD as u16;
        // Only option/sub rows are clickable — never titles or descriptions.
        let hit_x = mouse.x >= row_x
            && mouse.x < box_x + max_row_w as u16
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
        // `box_x` is the contour's left edge; every content row starts
        // `BOX_PAD` columns in, so nothing touches the drawn margins.
        let box_x = area.x + (area.width.saturating_sub(max_w as u16)) / 2;
        let row_x = box_x + BOX_PAD as u16;

        // Clamp selection and scroll before rendering.
        let rows = selectable_rows(setup);
        let visible = visible_rows(area).min(rows.len());
        self.selection.set_visible_count(visible);
        self.selection.clamp(rows.len());
        let selected_idx = self.selection.selected_index;
        let offset = self.viewport_offset(area, setup);

        let mut inside_box = false;
        for line in &layout {
            // Track which side of the contour we are on BEFORE the
            // visibility checks: when scrolling hides a box's top border,
            // its visible rows below must still get walls.
            if let Line::SectionBorder { top, .. } = &line.line {
                inside_box = *top;
            }
            let Some(y) = (start_y + line.y).checked_sub(offset) else {
                continue;
            };
            if y < area.y {
                continue;
            }
            if y >= area.bottom() {
                break;
            }
            // Side walls complete the contour: every row strictly between
            // the box's top and bottom borders extends the ╭/╰ and ╮/╯
            // corners into `│` pillars. Drawn BEFORE the content so nothing
            // can overwrite them (content starts BOX_PAD columns in, and
            // early-`continue` arms still keep their walls).
            if inside_box && !matches!(line.line, Line::SectionBorder { .. }) {
                let wall = Style::default().fg(muted);
                draw_text(buf, "│", box_x, y, area, wall);
                draw_text(buf, "│", box_x + max_w as u16 - 1, y, area, wall);
            }
            match &line.line {
                Line::SummarizationModel(index) => {
                    let selected =
                        rows.get(selected_idx) == Some(&SettingsRow::SummarizationModel(*index));
                    let entry = &setup.routing.summarization_models[*index];
                    // Controls hug the box's right inner edge, inside the
                    // contour — never outside it.
                    let controls = (box_x + max_w as u16)
                        .saturating_sub(SUMMARIZER_CONTROLS.width() as u16 + BOX_PAD as u16)
                        .max(row_x);
                    let text = truncate(
                        &format!("  {}. {}/{}", index + 1, entry.provider, entry.model),
                        controls.saturating_sub(row_x) as usize,
                    );
                    let style = Style::default().fg(if selected { primary } else { fg });
                    draw_text(buf, &text, row_x, y, area, style);
                    draw_text(buf, SUMMARIZER_CONTROLS, controls, y, area, style);
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
                Line::SectionBorder { title, top } => {
                    // The contour spans the same centered width the rows
                    // use, so the boxes and their content share margins.
                    // Corners close on both ends: `╭─ Title ───… ─╮` and
                    // `╰─────… ─╯`.
                    if *top {
                        let head = format!("╭─ {} ", title);
                        let dashes = max_w.saturating_sub(head.width() + 1); // 1 = closing corner
                        draw_text(
                            buf,
                            &format!("{head}{}╮", "─".repeat(dashes)),
                            box_x,
                            y,
                            area,
                            Style::default().fg(muted),
                        );
                        // The section title pops above the border glyph.
                        draw_text(buf, title, box_x + 3, y, area, Style::default().fg(fg));
                    } else {
                        draw_text(
                            buf,
                            &format!("╰{}╯", "─".repeat(max_w.saturating_sub(2))),
                            box_x,
                            y,
                            area,
                            Style::default().fg(muted),
                        );
                    }
                }
                Line::Category { item } => {
                    let idx = rows
                        .iter()
                        .position(|r| matches!(r, SettingsRow::Category(ci) if ci == item));
                    let is_selected = idx == Some(selected_idx);
                    let item = item_at(*item);

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
                            Style::default().fg(if is_selected { primary } else { fg }),
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
                        Style::default().fg(if is_selected { primary } else { fg }),
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
    // Descriptions wrap to the box's inner width, so their widest rendered
    // row never exceeds it.
    let width_cap = width.saturating_sub(2 * BOX_PAD as u16) as usize;
    let mut width = all_items()
        .map(|(_, item)| {
            // Display width, not byte length: values can carry
            // multi-byte glyphs (×, ›) that skew centering otherwise.
            let mut len = item.description.width().max(2 + item.label.width()); // "✔ PreToolUse hooks"
            // Choice rows render "label: value" at the same indent.
            if let Some(v) = cache_choice_value(item.id, setup) {
                len = len.max(2 + item.label.width() + 2 + v.width());
            }
            len.min(width_cap)
        })
        .max()
        .unwrap_or(0);
    for (_, item) in all_items() {
        if let SettingKind::HookList(event) = item.kind {
            if !setup.hooks.is_event_enabled(event) {
                continue;
            }
            for entry in hook_entries(setup, event) {
                // Full drawn line: indent + "• " + name [+ " — command…"].
                // Capped like the descriptions, so a very long hook name
                // cannot push the contour past a tiny terminal.
                let name = hook_display_name(entry);
                let mut len = 4 + 2 + name.chars().count();
                if !entry.name.is_empty() && !entry.command.is_empty() {
                    len += 3 + truncate(&entry.command, COMMAND_PREVIEW_LEN)
                        .chars()
                        .count();
                }
                width = width.max(len.min(width_cap));
            }
            width = width.max(4 + "+ Add hook".len());
        }
    }
    // MCP server rows (indented like hook sub-rows).
    for entry in &setup.mcp.servers {
        // Full drawn line: indent + "✔ " + name + " — target…".
        let len =
            4 + 1 + entry.name.chars().count() + 3 + mcp_server_preview(entry).chars().count();
        width = width.max(len.min(width_cap));
    }
    width = width.max(4 + "+ Add server".len());
    // The contour must enclose the padded content: plus the breathing room
    // on each side, so `╭…╮` extends past the widest drawn row.
    width + 2 * BOX_PAD
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
            .map(|index| cosh::setup::FallbackEntry {
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

    /// Flat catalog index of a setting id. Tests must not hardcode row
    /// indices: the `onnx` feature adds the checkup mid-list and any
    /// reordering would silently retarget the assertions.
    fn catalog_index(id: &str) -> usize {
        all_items()
            .find(|(_, item)| item.id == id)
            .map(|(index, _)| index)
            .unwrap_or_else(|| panic!("setting {id} exists in the catalog"))
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

    /// Scroll frames whole section blocks: when the selection sits in a
    /// section that requires scrolling, the viewport pins that block's
    /// closing border on the last visible row, so the box is never cut in
    /// half at the bottom (the old behavior pinned the selected row and
    /// clipped the block below it). A block taller than the viewport falls
    /// back to pinning the selected row itself.
    #[test]
    fn scroll_frames_whole_section_blocks() {
        let setup = Setup::default();
        let area = Rect::new(3, 2, 90, 12);

        // Selection in the LAST section (General): scrolling must be
        // required and the whole General block must fit in the window.
        let mut view = SettingsView::new();
        let telemetry_row = selectable_rows(&setup)
            .into_iter()
            .find(|row| matches!(row, SettingsRow::Category(index) if item_at(*index).id == "telemetry"))
            .unwrap();
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|row| *row == telemetry_row)
            .unwrap();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &test_theme(), &setup);

        let layout = build_layout(&setup, area.width);
        let start_y = content_start_y(area, &setup);
        let offset = view.viewport_offset(area, &setup);
        // The selected row's block bounds in layout coordinates.
        let sel_y = layout
            .iter()
            .find(|l| l.line.row() == Some(telemetry_row))
            .unwrap()
            .y;
        let block_top = layout
            .iter()
            .take_while(|l| l.y <= sel_y)
            .filter_map(|l| match l.line {
                Line::SectionBorder { top: true, .. } => Some(l.y),
                _ => None,
            })
            .last()
            .unwrap();
        let block_bottom = layout
            .iter()
            .filter(|l| l.y >= sel_y)
            .find_map(|l| match l.line {
                Line::SectionBorder { top: false, .. } => Some(l.y),
                _ => None,
            })
            .unwrap();
        // The block's closing border sits on the last visible row...
        assert!(
            line_text(&buf, area, area.bottom() - 1).starts_with('╰'),
            "closing border must be pinned at the viewport bottom, got {:?}",
            line_text(&buf, area, area.bottom() - 1)
        );
        // ...and the opening border is inside the window: the whole block
        // is framed, nothing cut at the bottom edge.
        let top_screen_y = (start_y + block_top).saturating_sub(offset);
        assert!(
            top_screen_y >= area.y && top_screen_y < area.bottom(),
            "opening border must stay visible (top_screen_y = {top_screen_y})"
        );
        assert!(block_bottom >= sel_y, "layout closes every box");

        // Fallback: a block taller than the viewport (30 summarization
        // models inside the Automation box) pins the selected row instead —
        // the row stays visible even though the block cannot be framed.
        let mut setup = Setup::default();
        setup.routing.summarization_models = (0..30)
            .map(|index| cosh::setup::FallbackEntry {
                provider: "local".into(),
                model: format!("model-{index}"),
            })
            .collect();
        let mut view = SettingsView::new();
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|row| *row == SettingsRow::SummarizationModel(29))
            .unwrap();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &test_theme(), &setup);
        assert!(
            line_text(&buf, area, area.bottom() - 1).contains("model-29"),
            "oversized block falls back to pinning the selected row"
        );
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
        // Expected rows are built from the catalog by id (never hardcoded
        // indices): the `onnx` feature inserts the checkup item mid-list.
        let cat = |id: &str| SettingsRow::Category(catalog_index(id));
        let cfg_rows = |rows: Vec<SettingsRow>| {
            #[cfg(feature = "onnx")]
            // The checkup trio sits right after the PostToolUse item's
            // whole sub-list (catalog order: summarization, pre, post,
            // termination, model, min-confidence). With hooks enabled the
            // AddHook sub-row follows the toggle, so anchor on it;
            // otherwise anchor on the toggle itself. The position is
            // looked up, never hardcoded.
            {
                let after_post = rows
                    .iter()
                    .position(|r| matches!(r, SettingsRow::AddHook { event: POST_TOOL_USE_EVENT }))
                    .or_else(|| {
                        rows.iter().position(
                            |r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("post_tool_use_hooks")),
                        )
                    })
                    .expect("post-tool row exists")
                    + 1;
                rows.insert(after_post, cat("checkup_min_confidence"));
                rows.insert(after_post, cat("checkup_model"));
                rows.insert(after_post, cat("checkup_termination"));
            }
            rows
        };

        // Flat catalog order: summarization (+ Add action), hooks, then the
        // cache/environment/integrations/general items. MCP's Add action
        // sits inside its Integrations box, BEFORE the General items.
        let disabled = setup_with_hooks(false, &[("a", "cmd a")]);
        assert_eq!(
            selectable_rows(&disabled),
            cfg_rows(vec![
                cat("summarization_models"),
                SettingsRow::AddSummarizationModel,
                cat("hooks"),
                cat("post_tool_use_hooks"),
                cat("anthropic_cache_ttl"),
                cat("openai_cache_retention"),
                cat("editor"),
                cat("skill_dirs"),
                cat("lsp"),
                cat("mcp"),
                SettingsRow::AddMcpServer,
                cat("telemetry"),
            ])
        );

        let enabled = setup_with_hooks(true, &[("a", "cmd a"), ("b", "cmd b")]);
        assert_eq!(
            selectable_rows(&enabled),
            cfg_rows(vec![
                cat("summarization_models"),
                SettingsRow::AddSummarizationModel,
                cat("hooks"),
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
                cat("post_tool_use_hooks"),
                SettingsRow::AddHook {
                    event: POST_TOOL_USE_EVENT
                },
                cat("anthropic_cache_ttl"),
                cat("openai_cache_retention"),
                cat("editor"),
                cat("skill_dirs"),
                cat("lsp"),
                cat("mcp"),
                SettingsRow::AddMcpServer,
                cat("telemetry"),
            ])
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

    /// The MCP section appends server toggles plus the Add action inside
    /// its Integrations box; toggling flips only the server's own switch.
    #[test]
    fn mcp_rows_toggle_and_open_the_wizard() {
        let setup = setup_with_mcp_server("docs", true);
        assert!(selectable_rows(&setup).contains(&SettingsRow::McpServer(0)));
        // The category row opens the wizard; the Add action is the last
        // selectable row of the Integrations box.
        assert!(selectable_rows(&setup).contains(&SettingsRow::AddMcpServer));

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
            .position(|r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("anthropic_cache_ttl")))
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
            .position(|r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("openai_cache_retention")))
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
                .position(|r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("lsp")))
                .expect("lsp category row exists")
        };

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(!setup.lsp);
        assert!(!is_enabled(item_at(catalog_index("lsp")), &setup));

        view.selection.selected_index = lsp_row(&setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::LspToggled)
        );
        assert!(setup.lsp);
        assert!(is_enabled(item_at(catalog_index("lsp")), &setup));
    }

    #[test]
    fn toggling_off_clamps_selection() {
        let mut setup = setup_with_hooks(true, &[("a", "cmd a")]);
        let mut view = SettingsView::new();
        // Start on the pre-tool Add-hook row and walk up to the hook
        // toggle (row identity, not a fixed step count — the layout above
        // the Automation items may change without retargeting this test).
        view.selection.selected_index = selectable_rows(&setup)
            .iter()
            .position(|r| {
                matches!(
                    r,
                    SettingsRow::AddHook {
                        event: PRE_TOOL_USE_EVENT
                    }
                )
            })
            .expect("pre-tool Add-hook row exists");
        let hook_toggle = SettingsRow::Category(catalog_index("hooks"));
        for _ in 0..10 {
            if selectable_rows(&setup).get(view.selection.selected_index) == Some(&hook_toggle) {
                break;
            }
            view.select_prev(20, &setup);
        }
        assert_eq!(
            selectable_rows(&setup).get(view.selection.selected_index),
            Some(&hook_toggle),
            "navigation must land on the hook toggle"
        );

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
            .position(|r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("editor")))
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

    /// The two checkup config rows exposed alongside the termination
    /// switch: the model row opens the kind picker (never mutates), the
    /// min-confidence row opens the floor input, and both rows render
    /// their current value (`kind_name` / percentage).
    #[cfg(feature = "onnx")]
    #[test]
    fn checkup_config_rows_open_their_pickers_and_render_values() {
        let mut setup = Setup::default();
        let mut view = SettingsView::new();

        let row = |id: &str, view: &mut SettingsView, setup: &Setup| {
            view.selection.selected_index = selectable_rows(setup)
                .iter()
                .position(|r| matches!(r, SettingsRow::Category(x) if *x == catalog_index(id)))
                .unwrap_or_else(|| panic!("{id} category row exists"));
        };

        // Model row: activation opens the picker, never the switch flip.
        row("checkup_model", &mut view, &setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenCheckupModelDialog)
        );
        assert!(
            matches!(setup.decision.model, cosh::setup::DecisionModel::English),
            "activation never mutates the setting"
        );
        assert_eq!(
            cache_choice_value("checkup_model", &setup),
            Some("english".to_string())
        );

        // Min-confidence row: activation opens the floor input.
        row("checkup_min_confidence", &mut view, &setup);
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenCheckupMinConfidenceInput)
        );
        assert_eq!(
            cache_choice_value("checkup_min_confidence", &setup),
            Some("60%".to_string())
        );

        // The values track the persisted config.
        setup.decision.model = cosh::setup::DecisionModel::Multilingual;
        setup.decision.termination.min_confidence = 0.75;
        assert_eq!(
            cache_choice_value("checkup_model", &setup),
            Some("multilingual".to_string())
        );
        assert_eq!(
            cache_choice_value("checkup_min_confidence", &setup),
            Some("75%".to_string())
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
            .position(
                |r| matches!(r, SettingsRow::Category(x) if *x == catalog_index("skill_dirs")),
            )
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
        let rows = |setup: &Setup| selectable_rows(setup);

        view.selection.selected_index = rows(&setup)
            .iter()
            .position(|r| {
                matches!(
                    r,
                    SettingsRow::Hook {
                        event: PRE_TOOL_USE_EVENT,
                        index: 0
                    }
                )
            })
            .expect("pre-tool hook row exists"); // Pre-tool hook
        assert_eq!(
            view.activate_selected(&mut setup),
            Some(SettingsAction::OpenHookForm {
                event: PRE_TOOL_USE_EVENT,
                index: Some(0)
            })
        );

        view.selection.selected_index = rows(&setup)
            .iter()
            .position(|r| {
                matches!(
                    r,
                    SettingsRow::AddHook {
                        event: PRE_TOOL_USE_EVENT
                    }
                )
            })
            .expect("pre-tool Add-hook row exists"); // Add hook (pre)
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

            // Every item's description is either the FIRST content of its
            // box (preceded by the opening border) or a later sibling
            // (preceded by exactly one blank line). This holds regardless
            // of whether the previous item ended in a toggle, a hook row
            // or an "+ Add hook" entry.
            let mut saw_a_description = false;
            for line in &layout {
                if matches!(line.line, Line::Description(_)) {
                    saw_a_description = true;
                    // Predecessor check via the sequential-y invariant:
                    // layout[line.y - 1] is the row right above this one.
                    let preceded_by = if line.y >= 1 {
                        Some(&layout[(line.y - 1) as usize].line)
                    } else {
                        None
                    };
                    assert!(
                        matches!(
                            preceded_by,
                            Some(Line::Blank) | Some(Line::SectionBorder { top: true, .. })
                        ),
                        "description at y={} must follow a blank line or an opening border",
                        line.y
                    );
                } else if matches!(
                    line.line,
                    Line::SectionBorder {
                        top: true,
                        title: _
                    }
                ) {
                    saw_a_description = false;
                }
            }
            assert!(saw_a_description, "layout has descriptions to check");
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

        let hook_row = selectable_rows(&setup)
            .iter()
            .position(|r| {
                matches!(
                    r,
                    SettingsRow::Hook {
                        event: PRE_TOOL_USE_EVENT,
                        index: 0
                    }
                )
            })
            .expect("pre-tool hook row exists");
        assert_eq!(
            view.handle_mouse(&mouse_at(row_x + 6, hook_y), area, &setup),
            Some(hook_row)
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
        // Tall enough that even the last section (General) is on screen:
        // the section contours add two border rows plus a blank per group.
        let area = Rect::new(0, 0, 60, 64);
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
        assert!(all.contains("Applies next launch"));
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
        let lsp_catalog = catalog_index("lsp");
        let lsp_index = selectable_rows(&setup)
            .iter()
            .position(|r| matches!(r, SettingsRow::Category(x) if *x == lsp_catalog))
            .expect("lsp category row exists");
        view.selection.selected_index = lsp_index;
        let layout = build_layout(&setup, area.width);
        let row_y = content_start_y(area, &setup)
            + layout
                .iter()
                .find(|l| matches!(l.line, Line::Category { item } if item == lsp_catalog))
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

    /// The contour must be fully closed: every box's top border starts
    /// with `╭` and ends with `╮`, the bottom border with `╰`/`╯`, and
    /// every row between them carries `│` pillars on both frame columns
    /// (walls exist even on blank and early-`continue` content rows).
    #[test]
    fn section_contours_are_fully_closed() {
        let theme = test_theme();
        let area = Rect::new(0, 0, 80, 64);
        let setup = setup_with_hooks(true, &[("block rm", "exit 2")]);
        let mut view = SettingsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);

        let max_w = max_row_width(&setup, area.width);
        let box_x = area.x + (area.width - max_w as u16) / 2;
        let left = box_x;
        let right = box_x + max_w as u16 - 1;
        let start_y = content_start_y(area, &setup);
        // Corners are read per-cell, not from the trimmed row text: the box
        // is centered, so leading blanks are legitimate and a starts_with
        // would misfire.
        let cell = |x: u16, y: u16| {
            buf.cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        };

        let mut inside = false;
        let mut boxes_seen = 0usize;
        for line in build_layout(&setup, area.width) {
            let y = start_y + line.y;
            match line.line {
                Line::SectionBorder { top: true, .. } => {
                    assert_eq!(cell(left, y), "╭", "open corner missing at y={y}");
                    assert_eq!(cell(right, y), "╮", "open corner missing at y={y}");
                    inside = true;
                    boxes_seen += 1;
                }
                Line::SectionBorder { top: false, .. } => {
                    assert_eq!(cell(left, y), "╰", "close corner missing at y={y}");
                    assert_eq!(cell(right, y), "╯", "close corner missing at y={y}");
                    inside = false;
                }
                _ if inside => {
                    assert_eq!(cell(left, y), "│", "left wall missing at y={y}");
                    assert_eq!(cell(right, y), "│", "right wall missing at y={y}");
                }
                _ => {}
            }
        }
        // Every catalog section must have produced a closed box.
        assert_eq!(boxes_seen, sections().len());
    }

    /// Visual dump of the rendered screen (run with --nocapture). Ignored
    /// harness for eyeballing the section contours and padding — keep it
    /// when tweaking BOX_PAD or the border geometry.
    #[test]
    #[ignore]
    fn preview_screen() {
        let theme = test_theme();
        let mut setup = setup_with_hooks(true, &[("block rm", "exit 2"), ("b", "cmd b")]);
        setup.routing.summarization_models = vec![cosh::setup::FallbackEntry {
            provider: "local".into(),
            model: "qwen2.5-coder:7b".into(),
        }];
        let mut view = SettingsView::new();
        let area = Rect::new(0, 0, 80, 46);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme, &setup);
        for y in area.y..area.bottom() {
            println!("{:3}│{}│", y, line_text(&buf, area, y));
        }
    }
}
