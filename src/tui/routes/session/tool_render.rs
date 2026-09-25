use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use cosh_sdk::hashline::format::{HL_FILE_PREFIX, HL_LINE_BODY_SEP};
use cosh_sdk::tree_sitter::highlight::{HighlightCategory, highlight};
use cosh_tools::question::types::QuestionOutput;
use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::{ColorInput, RGBA};
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::renderables::diff::{DiffRenderable, DiffViewMode};
use cosh_tui::core::renderables::markdown::{MarkdownRenderable, estimate_height};

use crate::component::spinner_highlight::HighlightSpinner;
use crate::routes::session::right_panel::types::{subagent_display_output, subagent_visible_body};
use crate::theme::{Theme, rgba_color};
use crate::types::{ToolPart, ToolStatus};

/// Pick the foreground color for an inline tool label based on its status.
/// Failed tools are red; completed tools are muted; running tools are normal.
fn tool_label_fg(status: &ToolStatus, theme: &Theme) -> RGBA {
    match status {
        ToolStatus::Failed(_) => theme.error,
        ToolStatus::Completed => theme.text_muted,
        ToolStatus::Running => theme.text,
    }
}

/// Return the highlight colour for a tool display name.
///
/// Tools without a dedicated colour (bash, write, edit, question, todo,
/// task) return `None` so the caller can fall back to a default.
pub(crate) fn tool_color(display: &str) -> Option<Color> {
    let (r, g, b) = match display {
        "glob" => (74, 158, 255),
        "read" => (76, 175, 80),
        "grep" => (171, 71, 188),
        "webfetch" => (38, 198, 218),
        "websearch" => (26, 188, 156),
        "skill" => (233, 30, 99),
        "recall_search" => (255, 167, 38),
        "generic" => (158, 158, 158),
        _ => return None,
    };
    Some(Color::Rgb(r, g, b))
}

/// Draws the expand/collapse hint styled as a button: highlighted with the
/// theme primary color as a marker-like background so it reads as clickable.
pub fn draw_hint_button(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, theme: &Theme) {
    let style = Style::default()
        .fg(rgba_color(theme.background))
        .bg(rgba_color(theme.primary));
    let width = (text.chars().count() as u16).saturating_add(2);
    let right = x.saturating_add(max_w).min(x.saturating_add(width));
    if right > x {
        for cx in x..right {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_style(style);
            }
        }
    }
    let inner_x = x.saturating_add(1);
    let inner_max_w = max_w.saturating_sub(2);
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let cx = inner_x + i as u16;
        if cx >= inner_x.saturating_add(inner_max_w) {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        // Skip control characters (e.g. \r progress spinners, \t, ESC/ANSI
        // bytes from raw tool output). Writing them into buffer cells makes
        // ratatui's buffer diff panic:
        //   "control character passed to cell_width without filtering"
        if ch.is_control() {
            continue;
        }
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

/// Draw a glob file row with markdown-link styling, but only over the path
/// glyphs: the leading indentation (tree/grouped formats pad rows with spaces)
/// keeps the base style so the underline starts exactly on the first letter.
fn draw_link_row(
    buf: &mut Buffer,
    text: &str,
    x: u16,
    y: u16,
    max_w: u16,
    content_style: Style,
    link_style: Style,
) {
    let indent_chars = text.chars().take_while(|c| *c == ' ').count();
    let is_link = !text.trim_end().ends_with('/');
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        if ch.is_control() {
            continue;
        }
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        let style = if is_link && i >= indent_chars {
            link_style
        } else {
            content_style
        };
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Map a file path to the tree-sitter language name used by [`highlight`]
/// (and by the markdown codeblock renderer), so tool boxes share the exact
/// same highlighting coverage as chat codeblocks.
fn lang_name_from_path(filepath: &str) -> Option<&'static str> {
    let ext = Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let ext = ext.to_ascii_lowercase();

    Some(match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" | "json" | "ts" | "tsx" | "mts" | "cts" | "php" | "lua"
        | "dart" => "javascript",
        "cs" | "c" | "h" | "cpp" | "c++" | "cxx" | "hpp" | "m" | "mm" | "objc" | "objectivec" => {
            "csharp"
        }
        "go" => "go",
        "java" | "scala" | "groovy" => "java",
        "hs" | "lhs" => "haskell",
        "swift" => "swift",
        "zig" | "zon" => "zig",
        "kt" | "kts" | "kotlin" => "kotlin",
        _ => return None,
    })
}

/// Syntax style for tool code boxes, resolved through the active [`Theme`]
/// exactly like markdown codeblocks are (see `syntax_colors`): theme colors
/// win, anything unset falls back to the surrounding text color.
fn code_highlight_style(cat: Option<HighlightCategory>, default_fg: Color, theme: &Theme) -> Style {
    let fg = match cat {
        Some(HighlightCategory::Keyword) => rgba_color(theme.syntax_keyword),
        Some(HighlightCategory::String) => rgba_color(theme.syntax_string),
        Some(HighlightCategory::Comment) => rgba_color(theme.syntax_comment),
        Some(HighlightCategory::Type) => rgba_color(theme.syntax_type),
        Some(HighlightCategory::Function) => rgba_color(theme.syntax_function),
        Some(HighlightCategory::Number) => rgba_color(theme.syntax_number),
        // Codeblocks map `Builtin` onto the operator accent.
        Some(HighlightCategory::Builtin) => rgba_color(theme.syntax_operator),
        None => default_fg,
    };
    Style::default().fg(fg)
}

fn render_inline_tool(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    text: &str,
    fg: RGBA,
    spinner: Option<&HighlightSpinner>,
) {
    if let Some(spinner) = spinner {
        spinner.render(buf, x, y, max_w);
    } else {
        draw_text_line(buf, text, x, y, max_w, Style::default().fg(rgba_color(fg)));
    }
}

pub struct ToolRenderState {
    pub expanded: HashMap<String, bool>,
    pub error_expanded: HashMap<String, bool>,
    /// Per-tool-call highlight spinners, keyed by tool call ID.
    pub tool_spinners: HashMap<String, HighlightSpinner>,
    /// Incremented on every expand/collapse toggle. Consumers use this to
    /// invalidate layout and render caches that depend on expansion state.
    pub version: u64,
}

impl ToolRenderState {
    pub fn new() -> Self {
        Self {
            expanded: HashMap::new(),
            error_expanded: HashMap::new(),
            tool_spinners: HashMap::new(),
            version: 0,
        }
    }

    pub fn toggle_expanded(&mut self, id: &str) {
        let entry = self.expanded.entry(id.to_string()).or_insert(false);
        *entry = !*entry;
        self.version = self.version.wrapping_add(1);
    }

    /// Effective expansion for an id that inherits `default` until explicitly
    /// toggled (e.g. reasoning blocks default to the global thinking mode).
    pub fn is_expanded_or(&self, id: &str, default: bool) -> bool {
        self.expanded.get(id).copied().unwrap_or(default)
    }

    /// Flip expansion relative to the id's *effective* state, so the first
    /// click on a block that starts expanded via `default` collapses it.
    pub fn toggle_with_default(&mut self, id: &str, default: bool) {
        let current = self.is_expanded_or(id, default);
        self.expanded.insert(id.to_string(), !current);
        self.version = self.version.wrapping_add(1);
    }

    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.get(id).copied().unwrap_or(false)
    }

    pub fn toggle_error(&mut self, id: &str) {
        let entry = self.error_expanded.entry(id.to_string()).or_insert(false);
        *entry = !*entry;
        self.version = self.version.wrapping_add(1);
    }

    /// Helper: convert a ratatui `Color::Rgb` to `RGBA`.
    fn rgba_from_color(color: Color) -> RGBA {
        if let Color::Rgb(r, g, b) = color {
            RGBA::from_ints(r, g, b, 255)
        } else {
            RGBA::from_ints(128, 128, 128, 255)
        }
    }

    /// Manage the lifecycle of a tool spinner.
    ///
    /// Call this on every frame for each tool part to ensure the
    /// spinner is created when the tool starts running, signalled
    /// to finish when it completes, and eventually cleaned up.
    pub fn manage_tool_spinner(
        &mut self,
        tool_id: &str,
        part: &ToolPart,
        theme: &Theme,
        is_running: bool,
    ) {
        let display = tool_display(&part.tool);

        match (is_running, self.tool_spinners.get_mut(tool_id)) {
            // Tool is running but no spinner yet — create one.
            // Beam starts ON the text (pos 0.0) so it's immediately visible.
            (true, None) => {
                let text = tool_inline_text(part);
                let highlight = tool_color(display)
                    .map(Self::rgba_from_color)
                    .unwrap_or(RGBA::from_ints(128, 128, 128, 255));
                let base = theme.text;
                let mut spinner = HighlightSpinner::new(&text, highlight, base);
                spinner.set_beam_pos(0.0);
                self.tool_spinners.insert(tool_id.to_string(), spinner);
            }
            // Tool is running but spinner is idle — replace with fresh active.
            (true, Some(spinner)) if spinner.is_idle() => {
                let text = tool_inline_text(part);
                let highlight = tool_color(display)
                    .map(Self::rgba_from_color)
                    .unwrap_or(RGBA::from_ints(128, 128, 128, 255));
                let base = theme.text;
                let mut s = HighlightSpinner::new(&text, highlight, base);
                s.set_beam_pos(0.0);
                *spinner = s;
            }
            // Tool completed without ever being seen as Running — no spinner.
            // New tools get their spinner created directly in the ToolCall event
            // handler (app.rs), not here. Historical tools should never show one.
            (false, None) => {}
            // Tool is no longer running — signal the spinner to finish its
            // sweep at a visible speed. Covers both Completed and Failed.
            (false, Some(spinner)) if spinner.is_active() => {
                spinner.with_speed(0.05);
                spinner.finish();
            }
            _ => {}
        }
    }

    /// Advance all active / finishing spinners, scaled by `delta_secs`.
    pub fn advance_tool_spinners(&mut self, delta_secs: f64) {
        for spinner in self.tool_spinners.values_mut() {
            if !spinner.is_idle() {
                spinner.advance(delta_secs);
            }
        }
    }

    /// Drop spinners that have finished (phase Idle). Idle spinners are never
    /// rendered — every render path filters with `!is_idle()` — so keeping
    /// them would grow the map by one entry per tool call the session ever
    /// made. Dropping them bounds the map by the number of tools currently
    /// animating, and a re-created spinner is indistinguishable.
    pub fn sweep_idle_spinners(&mut self) {
        self.tool_spinners.retain(|_, s| !s.is_idle());
    }
}

impl Default for ToolRenderState {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared context for rendering a tool part into the buffer.
///
/// Bundles the buffer region, layout limits, and render state so the
/// per-tool render functions don't need long positional argument lists.
pub struct ToolRenderCtx<'a> {
    pub buf: &'a mut Buffer,
    pub x: u16,
    pub y: u16,
    pub line_h: &'a mut u16,
    pub max_w: u16,
    pub state: &'a mut ToolRenderState,
    pub theme: &'a Theme,
}

const TOOL_DISPLAYS: &[&str] = &[
    "bash",
    "glob",
    "read",
    "grep",
    "webfetch",
    "websearch",
    "write",
    "edit",
    "task",
    "apply_patch",
    "question",
    "skill",
];

pub fn tool_display(tool: &str) -> &str {
    if TOOL_DISPLAYS.contains(&tool) {
        return tool;
    }
    match tool {
        "bash_run" => "bash",
        "fs_read" => "read",
        "fs_write" => "write",
        "fs_edit" | "fs_edit_lines" | "fs_ast_edit" => "edit",
        "find_glob" => "glob",
        "find_grep" => "grep",
        "web_fetch" => "webfetch",
        "web_search" => "websearch",
        "ask_questions" => "question",
        "skills_list" | "skills_read" | "skills_read_asset" | "skills_match_skills" => "skill",
        "plan_todo_write" => "todo",
        "recall_search" => "recall_search",
        _ => "generic",
    }
}

/// Generate the visible inline text for a tool part.
pub(crate) fn tool_inline_text(part: &ToolPart) -> String {
    match tool_display(&part.tool) {
        "bash" => {
            let cmd = input_value(&part.input, "command").unwrap_or_default();
            if cmd.is_empty() || matches!(part.status, ToolStatus::Running) {
                "Writing command".to_string()
            } else {
                cmd
            }
        }
        "write" => {
            let fp = input_filepath(&part.input).unwrap_or_default();
            format!("Write {fp}")
        }
        "edit" => {
            let fp = input_filepath(&part.input).unwrap_or_default();
            format!("Edit {fp}")
        }
        "glob" => {
            let pat = input_value(&part.input, "pattern").unwrap_or_default();
            let path = input_value(&part.input, "path");
            if let Some(p) = path {
                format!("Glob \"{pat}\" in {p}")
            } else {
                format!("Glob \"{pat}\"")
            }
        }
        "read" => {
            let fp = input_filepath(&part.input).unwrap_or_default();
            format!("Read {fp}")
        }
        "grep" => {
            let pat = input_value(&part.input, "pattern").unwrap_or_default();
            let path = input_value(&part.input, "path");
            if let Some(p) = path {
                format!("Grep \"{pat}\" in {p}")
            } else {
                format!("Grep \"{pat}\"")
            }
        }
        "webfetch" => {
            let url = input_value(&part.input, "url").unwrap_or_default();
            format!("WebFetch {url}")
        }
        "websearch" => {
            let q = input_value(&part.input, "query").unwrap_or_default();
            let provider = input_value(&part.input, "provider");
            let label = web_search_provider_label(provider.as_deref());
            format!("{label} \"{q}\"")
        }
        "task" => {
            let desc = input_value(&part.input, "description").unwrap_or_default();
            if desc.is_empty() {
                "Delegating".to_string()
            } else {
                desc
            }
        }
        "question" => "Asking questions".to_string(),
        "todo" => {
            if matches!(part.status, ToolStatus::Running)
                || part.output.as_deref().unwrap_or("").trim().is_empty()
            {
                format!("Writing {}", part.tool)
            } else {
                match part.tool.as_str() {
                    "plan_todo_write" => "TODO Write".to_string(),
                    _ => "\u{2630} TODO".to_string(),
                }
            }
        }
        "recall_search" => part.tool.clone(),
        _ => {
            if matches!(part.status, ToolStatus::Completed) {
                part.tool.clone()
            } else {
                format!("Writing {}", part.tool)
            }
        }
    }
}

pub fn web_search_provider_label(provider: Option<&str>) -> &str {
    match provider {
        Some("parallel") => "Parallel Web Search",
        Some("exa") => "Exa Web Search",
        _ => "Web Search",
    }
}

pub(crate) fn input_value(input: &serde_json::Value, key: &str) -> Option<String> {
    input.get(key).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

/// Extract a filepath from tool input, supporting opencode-style
/// ({"filePath": "..."}), cosh flat style ({"path": "..."}, the advertised
/// fs_write/fs_edit shape) and the legacy batch form
/// ({"targets": [{"path": "...", ...}]}).
pub(crate) fn input_filepath(input: &serde_json::Value) -> Option<String> {
    if let Some(fp) = input_value(input, "filePath") {
        return Some(fp);
    }
    if let Some(p) = input_value(input, "path") {
        return Some(p);
    }
    input
        .get("targets")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
        .and_then(|t| input_value(t, "path"))
}

/// Extract content from tool input, supporting opencode-style
/// ({"content": "..."}), cosh flat style ({"content": "..."} at the top
/// level, the advertised fs_write shape) and the legacy batch form
/// ({"targets": [{"text": "...", ...}]}).
pub(crate) fn input_content(input: &serde_json::Value) -> Option<String> {
    if let Some(c) = input_value(input, "content") {
        return Some(c);
    }
    input
        .get("targets")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
        .and_then(|t| input_value(t, "text"))
}

/// True when a completed write carries warnings (e.g. missing file_hash or
/// hash mismatch). In that case the write did NOT modify the file and the
/// renderer falls back to the label-only view — shared by the renderer,
/// the height estimate and `tool_is_block` so all three agree.
pub(crate) fn write_has_warnings(part: &ToolPart) -> bool {
    part.output
        .as_deref()
        .and_then(|o| serde_json::from_str::<serde_json::Value>(o).ok())
        .and_then(|v| v.as_array()?.first()?.get("warnings")?.as_str().map(|_| ()))
        .is_some()
}

/// Number of content rows the write box draws, or `None` when `render_write`
/// falls back to the inline label. Mirrors the renderer's predicate exactly:
/// completed + no warnings + non-empty `input.content` (NOT `output` — the
/// output of a successful write is a short JSON receipt, unrelated in size
/// to the previewed content).
pub(crate) fn write_box_lines(part: &ToolPart) -> Option<u16> {
    if !matches!(part.status, ToolStatus::Completed) || write_has_warnings(part) {
        return None;
    }
    let content = input_content(&part.input).unwrap_or_default();
    if content.is_empty() {
        return None;
    }
    const MAX_LINES: usize = 20;
    Some(content.lines().count().min(MAX_LINES) as u16)
}

/// The unified diff `render_edit` draws for a completed edit, or `None` when
/// it falls back to the inline label. Shared by the renderer, the height
/// estimate and `tool_is_block` so all three agree.
pub(crate) fn edit_diff_content(part: &ToolPart) -> Option<String> {
    if !matches!(part.status, ToolStatus::Completed) {
        return None;
    }
    let raw_output = part.output.as_deref().unwrap_or("");
    if raw_output.is_empty() {
        return None;
    }
    if looks_like_unified_diff(raw_output) {
        Some(raw_output.to_string())
    } else {
        extract_diff_from_json(raw_output)
    }
}

/// Heuristic: does the output string look like a unified diff?
pub(crate) fn looks_like_unified_diff(output: &str) -> bool {
    output.starts_with("--- ") || output.starts_with("diff --git ")
}

/// Try to extract a unified diff string from a JSON tool output.
/// Handles cosh-style `[{..., "diff": "..."}, ...]`.
pub(crate) fn extract_diff_from_json(output: &str) -> Option<String> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(output) else {
        return None;
    };
    match val {
        serde_json::Value::Array(ref arr) => {
            let diffs: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("diff")?.as_str())
                .filter(|d| looks_like_unified_diff(d))
                .collect();
            if diffs.is_empty() {
                None
            } else {
                Some(diffs.join("\n"))
            }
        }
        serde_json::Value::Object(ref obj) => {
            let d = obj.get("diff")?.as_str()?;
            if looks_like_unified_diff(d) {
                Some(d.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Spinner-map key for ONE tool part. Keyed by the part's unique
/// `tool_call_id` when present: `part_idx` alone is unique only WITHIN a
/// message, so two same-tool calls in different messages shared one key and
/// the older row's finished spinner re-animated when its twin call started
/// (the map entry flipped back to Active and both rows rendered the beam).
/// The positional fallback is legacy only — production parts always carry a
/// `call-N` id (live ones from the ToolCall handler, loaded ones from
/// `ensure_tool_call_ids`).
pub fn spinner_key(display: &str, tool_call_id: Option<&str>, part_idx: u16) -> String {
    match tool_call_id {
        Some(id) => format!("{display}:{id}"),
        None => format!("{display}_{part_idx}"),
    }
}

/// Whether the ToolCall event handler should PRE-create a spinner for this
/// display. Only the renderers that manage a spinner under the tool's OWN
/// display key benefit from pre-creation (the beam must be visible even if
/// the tool finishes before the next render). The remaining renderers either
/// draw no spinner at all (`write`/`edit`/`question`/`todo`) or manage a
/// `generic`-keyed one themselves in [`render_generic`] — for those a
/// pre-created entry under the tool's own display would never be consumed
/// and, with call-id keys, would leak one forever-Active spinner per call
/// for the whole app run (invisible, but scanned by
/// `advance_tool_spinners` every frame).
pub fn tool_pre_creates_spinner(display: &str) -> bool {
    matches!(
        display,
        "bash" | "glob" | "read" | "grep" | "webfetch" | "websearch" | "task"
    )
}

/// The spinner-map key a tool part's RENDERER will actually use, or `None`
/// when its renderer draws no spinner at all. Mirrors `dispatch_tool`'s
/// mapping: the seven inline renderers manage their own display's key,
/// `write`/`edit`/`question`/`todo` draw none, and everything else falls
/// through to [`render_generic`], which manages a `generic`-keyed spinner.
/// Both the ToolCall handler (pre-creation) and the cache-bypass check in
/// the session renderer must ask THIS — asking for the display directly
/// would miss generic-managed spinners, and pre-creating under a key no
/// renderer reads leaks one forever-Active entry per call.
pub fn renderer_spinner_key(
    display: &str,
    tool_call_id: Option<&str>,
    part_idx: u16,
) -> Option<String> {
    let mut key = String::new();
    renderer_spinner_key_into(&mut key, display, tool_call_id, part_idx).then_some(key)
}

/// Write the key used by a tool renderer into a reusable buffer.
///
/// This keeps the cache-bypass check allocation-free while sharing the exact
/// key mapping with [`renderer_spinner_key`].
pub fn renderer_spinner_key_into(
    key: &mut String,
    display: &str,
    tool_call_id: Option<&str>,
    part_idx: u16,
) -> bool {
    if matches!(display, "write" | "edit" | "question" | "todo") {
        return false;
    }
    let key_display = if tool_pre_creates_spinner(display) {
        display
    } else {
        "generic"
    };
    key.clear();
    key.push_str(key_display);
    match tool_call_id {
        Some(id) => {
            key.push(':');
            key.push_str(id);
        }
        None => {
            key.push('_');
            let _ = write!(key, "{part_idx}");
        }
    }
    true
}

pub fn render_shell(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let command = input_value(&part.input, "command").unwrap_or_default();
    let output = part.output.as_deref().unwrap_or("").trim().to_string();
    let id = part.tool_call_id.as_deref().unwrap_or("shell");
    let is_running = matches!(part.status, ToolStatus::Running);
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let tool_id = spinner_key("bash", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());

    if output.is_empty() {
        let pending = "Writing command";
        let label = if is_completed { &command } else { pending };
        let fg = tool_label_fg(&part.status, ctx.theme);
        *ctx.line_h = 1;
        render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, label, fg, spinner);
    } else {
        let expanded = ctx.state.is_expanded(id);
        let collapsed = crate::util::scroll::collapse_tool_output(&output, 10, 800);
        let display = if expanded || !collapsed.overflow {
            &output
        } else {
            &collapsed.output
        };

        let title = format!("$ {command}");
        let lines = display.lines().count() as u16 + u16::from(collapsed.overflow);
        // 1 blank row of internal padding above the title (the bottom-padding
        // row is the last row of the box), matching the Glob box.
        let area = Rect::new(
            ctx.x,
            ctx.y,
            ctx.max_w.saturating_add(3),
            lines + 2 + TOOL_BOX_PAD_V,
        );
        *ctx.line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(ctx.theme.background_panel.into()));
        border_box.set_border_color(Some(ctx.theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(ctx.buf, area);

        let x_off = ctx.x + 3;
        let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
        draw_text_line(
            ctx.buf,
            &title,
            x_off,
            ctx.y + TOOL_BOX_PAD_V,
            ctx.max_w.saturating_sub(3),
            title_style,
        );

        let content_style = Style::default().fg(rgba_color(ctx.theme.text));
        for (i, line) in display.lines().enumerate() {
            let ly = ctx.y + TOOL_BOX_PAD_V + 1 + i as u16;
            if ly >= area.bottom() {
                break;
            }
            draw_text_line(
                ctx.buf,
                line,
                x_off,
                ly,
                ctx.max_w.saturating_sub(3),
                content_style,
            );
        }
        if collapsed.overflow {
            let hint_y = ctx.y + TOOL_BOX_PAD_V + 1 + display.lines().count() as u16;
            let hint = if expanded {
                "Click to collapse"
            } else {
                "Click to expand"
            };
            draw_hint_button(
                ctx.buf,
                hint,
                x_off,
                hint_y,
                ctx.max_w.saturating_sub(3),
                ctx.theme,
            );
        }
    }
}

/// Parameters for drawing a syntax-highlighted code block with line numbers.
struct CodeBlockSpec<'a> {
    content: &'a str,
    lang: Option<&'a str>,
    default_fg: Color,
    ln_fg: Color,
    max_lines: u16,
    theme: &'a Theme,
}

/// Draw a code block with line numbers (matching opencode's `<line_number>` wrapper).
fn draw_highlighted_code_with_ln(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    spec: CodeBlockSpec<'_>,
) -> u16 {
    let CodeBlockSpec {
        content,
        lang,
        default_fg,
        ln_fg,
        max_lines,
        theme,
    } = spec;
    let ln_count = content.lines().count().min(max_lines as usize);
    if ln_count == 0 {
        return 0;
    }
    let ln_width = ((ln_count as f64).log10().floor() as u16 + 1).max(2);

    let Some(lang) = lang else {
        let mut lines_drawn = 0u16;
        for (i, line) in content.lines().enumerate() {
            if lines_drawn >= max_lines {
                break;
            }
            let ly = y + lines_drawn;
            let ln_text = format!("{:>w$} ", i + 1, w = ln_width as usize);
            draw_text_line(buf, &ln_text, x, ly, max_w, Style::default().fg(ln_fg));
            draw_text_line(
                buf,
                line,
                x + ln_width + 1,
                ly,
                max_w.saturating_sub(ln_width + 1),
                Style::default().fg(default_fg),
            );
            lines_drawn += 1;
        }
        return lines_drawn;
    };

    let spans = highlight(content, lang);
    let mut cat_map: Vec<Option<HighlightCategory>> = vec![None; content.len()];
    if let Some(ref spans) = spans {
        for span in spans {
            let end = span.end.min(content.len());
            for item in cat_map[span.start..end].iter_mut() {
                *item = Some(span.category);
            }
        }
    }

    let mut lines_drawn = 0u16;
    let mut byte_offset = 0usize;
    let mut y_pos = y;

    for (i, line) in content.lines().enumerate() {
        if lines_drawn >= max_lines {
            break;
        }
        if i > 0 {
            y_pos += 1;
        }

        let mut x_pos = x;

        let ln_text = format!("{:>w$}", i + 1, w = ln_width as usize);
        for ch in ln_text.chars() {
            if let Some(cell) = buf.cell_mut((x_pos, y_pos)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(ln_fg));
            }
            x_pos += 1;
        }

        if x_pos < x + max_w {
            if let Some(cell) = buf.cell_mut((x_pos, y_pos)) {
                cell.set_char(' ');
                cell.set_style(Style::default().fg(ln_fg));
            }
            x_pos += 1;
        }

        for (ci, ch) in line.char_indices() {
            // Skip control characters (same reason as draw_text_line).
            if ch.is_control() {
                continue;
            }
            if x_pos >= x + max_w {
                break;
            }
            let byte_pos = byte_offset + ci;
            let cat = cat_map.get(byte_pos).copied().flatten();
            let style = code_highlight_style(cat, default_fg, theme);
            if let Some(cell) = buf.cell_mut((x_pos, y_pos)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            x_pos += 1;
        }

        while x_pos < x + max_w {
            if let Some(cell) = buf.cell_mut((x_pos, y_pos)) {
                cell.set_style(Style::default().fg(default_fg));
            }
            x_pos += 1;
        }

        byte_offset += line.len() + 1;
        lines_drawn += 1;
    }
    lines_drawn
}

pub fn render_write(ctx: &mut ToolRenderCtx, part: &ToolPart) {
    let filepath = input_filepath(&part.input).unwrap_or_default();
    let content = input_content(&part.input).unwrap_or_default();

    if let Some(display_lines) = write_box_lines(part) {
        let max_lines = 20u16;
        // 1 blank row of internal padding above the title (the bottom-padding
        // row is the last row of the box), matching the Glob box.
        let area = Rect::new(
            ctx.x,
            ctx.y,
            ctx.max_w.saturating_add(3),
            display_lines + 2 + TOOL_BOX_PAD_V,
        );
        *ctx.line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(ctx.theme.background_panel.into()));
        border_box.set_border_color(Some(ctx.theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(ctx.buf, area);

        let title = format!("# Wrote {filepath}");
        let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
        draw_text_line(
            ctx.buf,
            &title,
            ctx.x + 3,
            ctx.y + TOOL_BOX_PAD_V,
            ctx.max_w.saturating_sub(3),
            title_style,
        );

        let max_w_inner = ctx.max_w.saturating_sub(3);
        let default_fg = rgba_color(ctx.theme.text);
        let ln_fg = rgba_color(ctx.theme.text_muted);
        let lang = lang_name_from_path(&filepath);
        draw_highlighted_code_with_ln(
            ctx.buf,
            ctx.x + 3,
            ctx.y + 1 + TOOL_BOX_PAD_V,
            max_w_inner,
            CodeBlockSpec {
                content: &content,
                lang,
                default_fg,
                ln_fg,
                max_lines,
                theme: ctx.theme,
            },
        );
    } else {
        let label = format!("Write {filepath}");
        let fg = tool_label_fg(&part.status, ctx.theme);
        *ctx.line_h = 1;
        render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, None);
    }
}

pub fn render_edit(ctx: &mut ToolRenderCtx, part: &ToolPart) {
    let filepath = input_filepath(&part.input).unwrap_or_default();

    let diff_content = edit_diff_content(part);

    if let Some(ref diff_content) = diff_content {
        let diff_lines = diff_content.lines().count() as u16;
        let area = Rect::new(
            ctx.x,
            ctx.y,
            ctx.max_w.saturating_add(3),
            diff_lines.min(30) + 3,
        );
        *ctx.line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(ctx.theme.background_panel.into()));
        border_box.set_border_color(Some(ctx.theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(ctx.buf, area);

        let title = filepath.clone();
        let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
        draw_text_line(
            ctx.buf,
            &title,
            ctx.x + 3,
            ctx.y + 1,
            ctx.max_w.saturating_sub(3),
            title_style,
        );

        let diff_area = Rect::new(
            ctx.x + 3,
            ctx.y + 2,
            ctx.max_w.saturating_sub(3),
            diff_lines.min(30),
        );
        let mut diff = DiffRenderable::new(Some(diff_content.clone()));
        diff.set_show_line_numbers(true);
        diff.set_added_bg(ctx.theme.diff_added_bg);
        diff.set_removed_bg(ctx.theme.diff_removed_bg);
        diff.set_context_bg(ctx.theme.diff_context_bg);
        diff.set_added_sign_color(ctx.theme.diff_highlight_added);
        diff.set_removed_sign_color(ctx.theme.diff_highlight_removed);
        diff.set_hunk_header_fg(ctx.theme.diff_hunk_header);
        diff.set_line_number_fg(ctx.theme.diff_line_number);
        diff.set_added_line_number_bg(ctx.theme.diff_added_line_number_bg);
        diff.set_removed_line_number_bg(ctx.theme.diff_removed_line_number_bg);
        // Syntax-highlight the code body with the same palette chat
        // codeblocks use; the language comes from the edited file path.
        diff.set_code_lang(lang_name_from_path(&filepath).map(str::to_string));
        diff.set_syntax_colors(crate::util::markdown::syntax_colors(ctx.theme));
        diff.set_code_default_fg(ctx.theme.text);
        if ctx.max_w >= 100 {
            diff.set_view_mode(DiffViewMode::Split);
        }
        diff.render_self(ctx.buf, diff_area);
    } else {
        let label = format!("Edit {filepath}");
        let fg = tool_label_fg(&part.status, ctx.theme);
        *ctx.line_h = 1;
        render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, None);
    }
}

/// Live/result summary for the streaming search tools (glob/grep).
///
/// Returns the label suffix (live match counter while running; match count +
/// flags when completed) and whether the result carries a warning flag
/// (timed out / limit reached) that deserves the warning color.
///
/// The completed output JSON is parsed ONCE and both the suffix and the
/// warning flag are derived from that single parse.
fn search_tool_summary(part: &ToolPart) -> (Option<String>, bool) {
    let is_running = matches!(part.status, ToolStatus::Running);
    let output = part.output.as_deref().unwrap_or("");
    if is_running {
        // Use cached line count for O(1) lookup instead of O(n) lines().count()
        let count = part.cached_line_count.unwrap_or_else(|| {
            // Fallback to O(n) count if cache not available (shouldn't happen in normal flow)
            output.lines().count() as u32
        });
        return ((count > 0).then(|| format!(" — {count} matches")), false);
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(output) else {
        return (None, false);
    };
    let timed_out = json
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit_reached = json
        .get("limit_reached")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let warning = timed_out || limit_reached;
    let mut parts: Vec<String> = Vec::new();
    if let Some(count) = json
        .get("matches")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
    {
        parts.push(format!("{count} matches"));
    }
    if timed_out {
        parts.push("timed out, partial".to_string());
    }
    if limit_reached {
        parts.push("limit reached".to_string());
    }
    if json
        .get("useless")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        parts.push("no matches".to_string());
    }
    if parts.is_empty() {
        return (None, warning);
    }
    (Some(format!(" — {}", parts.join(", "))), warning)
}

/// Extract the display-ready glob output text from a completed tool part.
///
/// Prefers the tool's `formatted` field (flat/grouped/tree layout produced by
/// the tool layer from the `format` argument); falls back to joining the raw
/// `matches` paths one per line. Returns `None` when there is no listable
/// output (still running, empty, or not JSON).
pub(crate) fn glob_block_text(part: &ToolPart) -> Option<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(part.output.as_deref()?) else {
        return None;
    };
    if let Some(formatted) = json
        .get("formatted")
        .and_then(|f| f.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(formatted.to_string());
    }
    json.get("matches")
        .and_then(|m| m.as_array())
        .and_then(|list| {
            let rows: Vec<String> = list
                .iter()
                .filter_map(|e| e.get("path").and_then(|p| p.as_str()))
                .map(String::from)
                .collect();
            (!rows.is_empty()).then(|| rows.join("\n"))
        })
}

/// Extract the file content a completed read surfaced, as clean code lines.
///
/// `fs_read` returns a JSON array of read results whose `content` is
/// hashline-formatted: a `¶path#hash` header line, numbered `N| text` body
/// lines, and trailing bracketed notices (`[…N lines elided…]`, truncation
/// hints). This strips the header and notices and removes the `N| ` prefixes
/// so the read box shows exactly the code the model read (mirrors
/// `glob_block_text`). Multiple results are joined with a blank line; `None`
/// when nothing readable remains.
pub(crate) fn read_block_text(part: &ToolPart) -> Option<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(part.output.as_deref()?) else {
        return None;
    };
    let results = json.as_array()?;
    let mut blocks: Vec<String> = Vec::new();
    for result in results {
        let content = result.get("content")?.as_str()?;
        let mut code: Vec<&str> = Vec::new();
        for line in content.lines() {
            if line.starts_with(HL_FILE_PREFIX) {
                continue; // hashline header `¶path#hash`
            }
            // Numbered hashline: `N| text` → keep `text`. The bare `…` elision
            // marker is kept too (it is part of what the model read). Bracketed
            // notices and any other unnumbered line are dropped.
            if let Some(body) = strip_numbered_prefix(line) {
                code.push(body);
            } else if line.trim() == "…" {
                code.push("…");
            }
        }
        if !code.is_empty() {
            blocks.push(code.join("\n"));
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

/// Split a hashline `N| text` line into its body, or `None` when the line is
/// not numbered (the separator is [`HL_LINE_BODY_SEP`]).
fn strip_numbered_prefix(line: &str) -> Option<&str> {
    let (num, rest) = line.split_once(HL_LINE_BODY_SEP)?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(rest)
}

/// Status glyph prefix and warning flag for a completed search-tool result.
struct GlobStatus<'a> {
    glyph: &'a str,
    warning: bool,
}

fn glob_status(part: &ToolPart) -> Option<GlobStatus<'_>> {
    if !matches!(part.status, ToolStatus::Completed) {
        return None;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(part.output.as_deref()?) else {
        return None;
    };
    let warning = json
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || json
            .get("limit_reached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let glyph = if warning {
        "\u{26A0}"
    } else if json
        .get("useless")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        "\u{2715}"
    } else if json
        .get("matches")
        .and_then(|m| m.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        "\u{2713}"
    } else {
        ""
    };
    Some(GlobStatus { glyph, warning })
}

/// Render a glob tool. Completed globs with results draw an expandable block:
/// the labelled header plus the file list (grouped/flat per the tool's
/// `format`), collapsed to a fixed preview until toggled. Running globs keep
/// the lightweight single-line spinner label.
/// Vertical padding inside the tool boxes (Glob, bash, write): 1 blank row
/// above the title and 1 below the last content row (mirrors the Summarizing
/// box).
const TOOL_BOX_PAD_V: u16 = 1;

pub fn render_glob(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let pattern = input_value(&part.input, "pattern").unwrap_or_default();
    let path = input_value(&part.input, "path");
    let is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let mut label = format!("Glob \"{pattern}\"");
    if let Some(p) = path {
        let _ = write!(label, " in {p}");
    }
    let status = is_completed.then(|| glob_status(part)).flatten();
    if let Some(status) = &status
        && !status.glyph.is_empty()
    {
        let _ = write!(label, " {glyph}", glyph = status.glyph);
    }

    let tool_id = spinner_key("glob", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());

    let fg = if matches!(part.status, ToolStatus::Failed(_)) {
        ctx.theme.error
    } else if is_completed {
        if status.as_ref().is_some_and(|s| s.warning) {
            ctx.theme.warning
        } else {
            ctx.theme.text_muted
        }
    } else {
        ctx.theme.text
    };

    // Only completed globs with listable output render the expandable block.
    let Some(body) = glob_block_text(part) else {
        *ctx.line_h = 1;
        render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
        return;
    };

    let id = part.tool_call_id.as_deref().unwrap_or("glob");
    let expanded = ctx.state.is_expanded(id);
    let collapsed = crate::util::scroll::collapse_tool_output(&body, 10, 800);
    let display = if expanded || !collapsed.overflow {
        &body
    } else {
        &collapsed.output
    };

    let lines = display.lines().count().max(1) as u16 + u16::from(collapsed.overflow);
    // 1 blank row of internal padding above the title (the bottom-padding
    // row is the last row of the box), matching the Summarizing box.
    let area = Rect::new(
        ctx.x,
        ctx.y,
        ctx.max_w.saturating_add(3),
        lines + 2 + TOOL_BOX_PAD_V,
    );
    *ctx.line_h = area.height;

    let mut border_box = BoxRenderable::new();
    border_box.set_background_color(Some(ctx.theme.background_panel.into()));
    border_box.set_border_color(Some(ctx.theme.background.into()));
    border_box.set_border_sides(BorderSidesConfig {
        left: true,
        top: false,
        right: false,
        bottom: false,
    });
    border_box.set_custom_border_chars(BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '┃',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '┃',
        right_t: ' ',
        cross: ' ',
    });
    border_box.render_self(ctx.buf, area);

    let x_off = ctx.x + 3;
    let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
    draw_text_line(
        ctx.buf,
        &label,
        x_off,
        ctx.y + TOOL_BOX_PAD_V,
        ctx.max_w.saturating_sub(3),
        title_style,
    );

    let content_style = Style::default().fg(rgba_color(ctx.theme.text));
    let link_style = Style::default()
        .fg(rgba_color(ctx.theme.markdown_link))
        .add_modifier(Modifier::UNDERLINED);
    for (i, line) in display.lines().enumerate() {
        let ly = ctx.y + TOOL_BOX_PAD_V + 1 + i as u16;
        if ly >= area.bottom() {
            break;
        }
        // Emulate the chat's markdown link styling for file rows: the underline
        // starts on the first path glyph (leading grouped/tree indentation is
        // exempt). Directory headers (ending in `/`) stay plain.
        draw_link_row(
            ctx.buf,
            line,
            x_off,
            ly,
            ctx.max_w.saturating_sub(3),
            content_style,
            link_style,
        );
    }
    if collapsed.overflow {
        let hint_y = ctx.y + TOOL_BOX_PAD_V + 1 + display.lines().count() as u16;
        let hint = if expanded {
            "Click to collapse"
        } else {
            "Click to expand"
        };
        draw_hint_button(
            ctx.buf,
            hint,
            x_off,
            hint_y,
            ctx.max_w.saturating_sub(3),
            ctx.theme,
        );
    }
}

pub fn render_read(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let filepath = input_filepath(&part.input).unwrap_or_default();
    let is_running = matches!(part.status, ToolStatus::Running);
    let is_completed = matches!(part.status, ToolStatus::Completed);

    let tool_id = spinner_key("read", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());

    // Completed reads with parseable content draw a code box styled like the
    // Write box. Large reads collapse to a preview (like the Glob/bash box):
    // the first lines with a "…" marker and a "Click to expand" hint; clicking
    // toggles the full file the model read.
    if is_completed && let Some(code) = read_block_text(part) {
        let id = part.tool_call_id.as_deref().unwrap_or("read");
        let expanded = ctx.state.is_expanded(id);
        let collapsed = crate::util::scroll::collapse_tool_output(&code, 10, 800);
        let display = if expanded || !collapsed.overflow {
            &code
        } else {
            &collapsed.output
        };
        let content_lines = display.lines().count().max(1) as u16;
        let lines = content_lines + u16::from(collapsed.overflow);
        // 1 blank row of internal padding above the title (the bottom-padding
        // row is the last row of the box), matching the Write box.
        let area = Rect::new(
            ctx.x,
            ctx.y,
            ctx.max_w.saturating_add(3),
            lines + 2 + TOOL_BOX_PAD_V,
        );
        *ctx.line_h = area.height;

        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(ctx.theme.background_panel.into()));
        border_box.set_border_color(Some(ctx.theme.background.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(BorderCharacters {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: '┃',
            top_t: ' ',
            bottom_t: ' ',
            left_t: '┃',
            right_t: ' ',
            cross: ' ',
        });
        border_box.render_self(ctx.buf, area);

        let title = format!("# Read {filepath}");
        let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
        draw_text_line(
            ctx.buf,
            &title,
            ctx.x + 3,
            ctx.y + TOOL_BOX_PAD_V,
            ctx.max_w.saturating_sub(3),
            title_style,
        );

        let max_w_inner = ctx.max_w.saturating_sub(3);
        let default_fg = rgba_color(ctx.theme.text);
        let ln_fg = rgba_color(ctx.theme.text_muted);
        let lang = lang_name_from_path(&filepath);
        draw_highlighted_code_with_ln(
            ctx.buf,
            ctx.x + 3,
            ctx.y + 1 + TOOL_BOX_PAD_V,
            max_w_inner,
            CodeBlockSpec {
                content: display,
                lang,
                default_fg,
                ln_fg,
                max_lines: content_lines,
                theme: ctx.theme,
            },
        );
        if collapsed.overflow {
            let hint_y = ctx.y + TOOL_BOX_PAD_V + 1 + content_lines;
            let hint = if expanded {
                "Click to collapse"
            } else {
                "Click to expand"
            };
            draw_hint_button(
                ctx.buf,
                hint,
                ctx.x + 3,
                hint_y,
                ctx.max_w.saturating_sub(3),
                ctx.theme,
            );
        }
        return;
    }

    // Running/failed/no-code reads keep the lightweight single-line label.
    let label = format!("Read {filepath}");
    let fg = tool_label_fg(&part.status, ctx.theme);
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
}

pub fn render_grep(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let pattern = input_value(&part.input, "pattern").unwrap_or_default();
    let path = input_value(&part.input, "path");
    let is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let mut label = format!("Grep \"{pattern}\"");
    if let Some(p) = path {
        let _ = write!(label, " in {p}");
    }
    let (summary, has_warning) = search_tool_summary(part);
    if let Some(summary) = summary {
        label.push_str(&summary);
    }

    let fg = if matches!(part.status, ToolStatus::Failed(_)) {
        ctx.theme.error
    } else if is_completed {
        if has_warning {
            ctx.theme.warning
        } else {
            ctx.theme.text_muted
        }
    } else {
        ctx.theme.text
    };
    let tool_id = spinner_key("grep", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
}

pub fn render_webfetch(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let url = input_value(&part.input, "url").unwrap_or_default();
    let _is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let label = format!("WebFetch {url}");
    let fg = tool_label_fg(&part.status, ctx.theme);
    let tool_id = spinner_key("webfetch", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
}

pub fn render_websearch(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let query = input_value(&part.input, "query").unwrap_or_default();
    let provider = input_value(&part.input, "provider");
    let _is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let provider_label = web_search_provider_label(provider.as_deref());
    let label = format!("{provider_label} \"{query}\"");
    let fg = tool_label_fg(&part.status, ctx.theme);
    let tool_id = spinner_key("websearch", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
}

pub fn render_task(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let description = input_value(&part.input, "description").unwrap_or_default();
    let _is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let content = if description.is_empty() {
        "Delegating".to_string()
    } else {
        description
    };

    let fg = tool_label_fg(&part.status, ctx.theme);
    let tool_id = spinner_key("task", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &content, fg, spinner);
}

/// Markdown summary of a completed `ask_questions` call: one heading per
/// question with the user's answer emphasized below it. This is what the chat
/// renders in place of the old one-line "Asking questions" label, so the
/// user can re-read what was asked and answered. `None` while the tool is
/// running/failed or when the output isn't parseable `QuestionOutput` JSON.
pub fn question_markdown(part: &ToolPart) -> Option<String> {
    if !matches!(part.status, ToolStatus::Completed) {
        return None;
    }
    let output = part.output.as_deref()?;
    let parsed = serde_json::from_str::<QuestionOutput>(output).ok()?;

    let mut md = String::new();
    for (qi, q) in parsed.questions.iter().enumerate() {
        // Correlate the answer to its question by id; fall back to position.
        let answer = parsed
            .answers
            .iter()
            .find(|a| a.id == q.id)
            .or_else(|| parsed.answers.get(qi))
            .and_then(|a| {
                a.answer
                    .clone()
                    .or_else(|| a.selected.as_ref().map(|s| s.join(", ")))
            })
            .unwrap_or_else(|| "no answer".to_string());
        if !md.is_empty() {
            md.push_str("\n\n");
        }
        // Trim the answer: a trailing space would break the emphasis
        // delimiters (pulldown_cmark's flanking rules) and leak the raw
        // asterisks onto the screen.
        md.push_str(&format!(
            "# {}\n\n***{}***",
            q.question.trim(),
            answer.trim()
        ));
    }
    (!md.is_empty()).then_some(md)
}

pub fn render_question_tool(ctx: &mut ToolRenderCtx, part: &ToolPart) {
    // Completed: render the Q&A summary as plain markdown on the chat
    // background — visually identical to an assistant text part, no box.
    if let Some(md_text) = question_markdown(part) {
        let body_h = estimate_height(&md_text, ctx.max_w).max(1);
        *ctx.line_h = body_h;
        let mut md = MarkdownRenderable::new(Some(md_text));
        md.set_fg(Some(ColorInput::RGBA(ctx.theme.text)));
        md.set_bg(Some(ColorInput::RGBA(ctx.theme.background)));
        md.render_self(ctx.buf, Rect::new(ctx.x, ctx.y, ctx.max_w, body_h));
        return;
    }

    let label = "Asking questions".to_string();
    let fg = tool_label_fg(&part.status, ctx.theme);
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, None);
}

/// Parse the tool output JSON and return formatted display lines for TODO items.
/// Shared by both `render_todo` and `estimate_part_height` so the line count
/// is consistent between rendering and height estimation.
///
/// Matches OpenCode's inline TodoWrite style: flat list of items with
/// bracket status symbols and no group headers or IDs.
/// The copy/selection body for a tool part: exactly what the renderer draws
/// for its type (formatted TODO list, extracted diff, code preview), never
/// the raw JSON schema stored in `output`. Returns `None` when the tool
/// renders no body on screen.
pub fn tool_copy_text(part: &ToolPart) -> Option<String> {
    let output = part.output.as_deref().unwrap_or("").trim();
    match tool_display(&part.tool) {
        "todo" => (!output.is_empty()).then(|| format_todo_output(output).join("\n")),
        // The Q&A markdown summary is exactly what's rendered on screen.
        "question" => question_markdown(part),
        "edit" => {
            if output.is_empty() || !matches!(part.status, ToolStatus::Completed) {
                return None;
            }
            let diff = if looks_like_unified_diff(output) {
                Some(output.to_string())
            } else {
                extract_diff_from_json(output)
            };
            // Mirror render_edit's 30-line cap.
            diff.map(|d| d.lines().take(30).collect::<Vec<_>>().join("\n"))
        }
        "read" => read_block_text(part),
        "write" => {
            // The screen shows the content from the tool *input*, not the
            // output; a warnings payload means nothing was written and the
            // renderer falls back to the label-only view (same predicate as
            // the renderer via `write_box_lines`).
            write_box_lines(part)?;
            let content = input_content(&part.input).unwrap_or_default();
            (!content.is_empty()).then(|| content.lines().take(20).collect::<Vec<_>>().join("\n"))
        }
        // NOTE: keyed on the RAW tool name — tool_display("subagent_call")
        // falls through to "generic", so a display-keyed arm never matches.
        _ if part.tool == "subagent_call" => {
            // The chat never DRAWS the subagent body (render_generic keeps
            // to a one-line label), but drag-selection over the part still
            // copies this text. `part.output` is the persisted
            // SubAgentCallOutput envelope (or a plain report on the
            // internal path) whose first line is the CONSUMED severity
            // header — copying it would leak `<!-- severity: ... -->` into
            // the user's clipboard. Unwrap the envelope and strip the
            // header, mirroring exactly what the box displays
            // (subagent_display_output → subagent_visible_body).
            let report = subagent_display_output(output.to_string());
            let visible = subagent_visible_body(&report).trim().to_string();
            (!visible.is_empty()).then_some(visible)
        }
        _ => (!output.is_empty()).then(|| output.to_string()),
    }
}

pub fn format_todo_output(output: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    lines.push("# Todos".to_string());

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output) {
        let items = json
            .get("list")
            .and_then(|l| l.get("items"))
            .and_then(|i| i.as_array());

        if let Some(items) = items {
            for item in items {
                let desc = item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("?");
                let symbol = match status {
                    "completed" => "\u{2713}",
                    "in_progress" => "\u{25CF}",
                    "cancelled" => "\u{2717}",
                    _ => " ",
                };
                lines.push(format!("[{symbol}] {desc}"));
            }
        }

        if let Some(nags) = json.get("nags").and_then(|n| n.as_array()) {
            for nag in nags {
                if let Some(msg) = nag.get("message").and_then(|m| m.as_str()) {
                    lines.push(format!("  \u{26A0} {msg}"));
                }
            }
        }

        let has_items = items.is_some_and(|i| !i.is_empty());
        if !has_items
            && json
                .get("nags")
                .and_then(|n| n.as_array())
                .is_none_or(|n| n.is_empty())
        {
            lines.push("  (empty)".to_string());
        }
    } else {
        for line in output.lines() {
            lines.push(line.to_string());
        }
    }

    lines
}

pub fn render_todo(ctx: &mut ToolRenderCtx, part: &ToolPart) {
    let tool_name: &str = &part.tool;
    let output = part.output.as_deref().unwrap_or("").trim();
    let is_running = matches!(part.status, ToolStatus::Running);

    if is_running || output.is_empty() {
        let is_failed = matches!(part.status, ToolStatus::Failed(_));
        if is_failed {
            let label = match tool_name {
                "plan_todo_write" => "TODO Write".to_string(),
                _ => tool_name.to_string(),
            };
            let style = Style::default().fg(rgba_color(ctx.theme.text_muted));
            *ctx.line_h = 1;
            draw_text_line(ctx.buf, &label, ctx.x, ctx.y, ctx.max_w, style);
        } else {
            *ctx.line_h = 1;
        }
        return;
    }

    let formatted = format_todo_output(output);
    if formatted.is_empty() {
        *ctx.line_h = 1;
        return;
    }
    let total_lines = formatted.len() as u16;
    let box_h = total_lines.saturating_add(2);
    *ctx.line_h = box_h;

    let area_w = ctx.max_w.saturating_add(3);
    let area = Rect::new(ctx.x, ctx.y, area_w, box_h);

    let bg_style = Style::default().bg(rgba_color(ctx.theme.background_panel));
    let border_style = Style::default()
        .fg(rgba_color(ctx.theme.background))
        .bg(rgba_color(ctx.theme.background_panel));
    for ly in ctx.y..ctx.y + box_h {
        if let Some(cell) = ctx.buf.cell_mut((ctx.x, ly)) {
            cell.set_char('\u{2503}');
            cell.set_style(border_style);
        }
        for lx in ctx.x + 1..ctx.x + area_w {
            if let Some(cell) = ctx.buf.cell_mut((lx, ly)) {
                cell.set_char(' ');
                cell.set_style(bg_style);
            }
        }
    }

    let title_style = Style::default().fg(rgba_color(ctx.theme.text_muted));
    let content_x = ctx.x + 3;
    let content_w = ctx.max_w.saturating_sub(3);
    draw_text_line(
        ctx.buf,
        &formatted[0],
        content_x,
        ctx.y + 1,
        content_w,
        title_style,
    );

    let item_style = Style::default().fg(rgba_color(ctx.theme.text));
    let content_bottom = area.bottom().saturating_sub(1);
    for (i, line) in formatted.iter().enumerate().skip(1) {
        let ly = ctx.y + 1 + i as u16;
        if ly >= content_bottom {
            break;
        }
        draw_text_line(ctx.buf, line, content_x, ly, content_w, item_style);
    }
}

pub fn render_generic(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let tool_name = &part.tool;
    let is_completed = matches!(part.status, ToolStatus::Completed);
    let is_running = matches!(part.status, ToolStatus::Running);

    let label = if is_completed {
        tool_name.clone()
    } else {
        format!("Writing {tool_name}")
    };
    let fg = tool_label_fg(&part.status, ctx.theme);
    let tool_id = spinner_key("generic", part.tool_call_id.as_deref(), part_idx);
    ctx.state
        .manage_tool_spinner(&tool_id, part, ctx.theme, is_running);
    let spinner = ctx
        .state
        .tool_spinners
        .get(&tool_id)
        .filter(|s| !s.is_idle());
    *ctx.line_h = 1;
    render_inline_tool(ctx.buf, ctx.x, ctx.y, ctx.max_w, &label, fg, spinner);
}

pub fn dispatch_tool(ctx: &mut ToolRenderCtx, part: &ToolPart, part_idx: u16) {
    let display = tool_display(&part.tool);
    match display {
        "bash" => render_shell(ctx, part, part_idx),
        "glob" => render_glob(ctx, part, part_idx),
        "read" => render_read(ctx, part, part_idx),
        "grep" => render_grep(ctx, part, part_idx),
        "webfetch" => render_webfetch(ctx, part, part_idx),
        "websearch" => render_websearch(ctx, part, part_idx),
        "write" => render_write(ctx, part),
        "edit" => render_edit(ctx, part),
        "task" => render_task(ctx, part, part_idx),
        "question" => render_question_tool(ctx, part),
        "todo" => render_todo(ctx, part),
        _ => render_generic(ctx, part, part_idx),
    }
}
