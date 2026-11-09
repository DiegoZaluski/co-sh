use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::types::MouseEvent;

use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;
use crate::util::list_selection::ListSelection;

/// Per-project tool-blocklist file, stored in the directory cosh was started
/// from. Keeps one project's blocklist from leaking into another's — the
/// global scope (`setup.json`) is opt-in via the Global toggle.
const PROJECT_TOOLS_FILE: &str = ".cosh-tools.json";

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct ProjectTools {
    #[serde(default)]
    disabled: Vec<String>,
}

fn project_tools_path(cwd: &str) -> PathBuf {
    PathBuf::from(cwd).join(PROJECT_TOOLS_FILE)
}

/// Load the disabled-tool set for the selected scope.
///
/// - Global: the `setup.json` blocklist, shared by every project.
/// - Project: `<cwd>/.cosh-tools.json`. While that file does not exist yet,
///   a non-empty global blocklist keeps applying (migration: users who had
///   tools disabled before the scope existed do not silently lose them);
///   the first project-scope save takes over from there. A corrupt file
///   fails open (nothing disabled) rather than blocking the session.
#[must_use]
pub fn load_disabled_tools(
    setup: &cosh::setup::Setup,
    cwd: &str,
    global: bool,
) -> HashSet<String> {
    if global {
        return setup.tools.disabled.iter().cloned().collect();
    }
    match std::fs::read_to_string(project_tools_path(cwd)) {
        Ok(text) => serde_json::from_str::<ProjectTools>(&text)
            .map(|project| project.disabled.into_iter().collect())
            .unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if setup.tools.disabled.is_empty() {
                HashSet::new()
            } else {
                // Pre-scope global blocklist still applies until this
                // project saves a list of its own.
                setup.tools.disabled.iter().cloned().collect()
            }
        }
        Err(_) => HashSet::new(),
    }
}

/// Persist the disabled set to the scope's storage: `setup.json` for the
/// global scope, `<cwd>/.cosh-tools.json` for the project scope (the global
/// `setup.json` is left untouched so a project list never clobbers it).
pub fn save_disabled_tools(
    setup: &mut cosh::setup::Setup,
    disabled: &HashSet<String>,
    cwd: &str,
    global: bool,
) {
    if global {
        setup.tools.disabled = disabled.iter().cloned().collect();
        setup.save();
        return;
    }
    let mut names: Vec<String> = disabled.iter().cloned().collect();
    names.sort();
    let payload = ProjectTools { disabled: names };
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        // A silent failure would leave the toggle looking applied while the
        // blocklist dies with the session; the user needs to know.
        if let Err(error) = std::fs::write(project_tools_path(cwd), text) {
            log::warn!("could not persist the project tool blocklist in {cwd}: {error}");
        }
    }
}

/// One entry point for both the keyboard and the mouse toggle paths.
///
/// Toggles the row under the selection (the Global scope row flips the
/// scope; a tool row flips its entry), then keeps the view's `disabled` set
/// and the persisted storage in sync with the active scope:
///
/// - Global row: persist the scope choice itself to `setup.json` and reload
///   `disabled` from the newly selected scope's stored set — the toggle
///   survives a restart even before any tool is toggled in that scope.
/// - Tool row: persist the set to the active scope (`setup.json` when
///   global, `<cwd>/.cosh-tools.json` otherwise).
pub fn toggle_entry(
    view: &mut InternalToolsView,
    setup: &mut cosh::setup::Setup,
    cwd: &str,
) {
    let scope_row = view.selection.selected_index == GLOBAL_ROW_INDEX;
    view.toggle_current();
    if scope_row {
        setup.tools.scope = if view.global { "global" } else { "project" }.to_string();
        setup.save();
        view.disabled = load_disabled_tools(setup, cwd, view.global);
    } else {
        save_disabled_tools(setup, &view.disabled, cwd, view.global);
    }
}

/// Load the persisted tool-call mode. Unknown/missing values fall back to
/// `Native` (the default contract).
#[must_use]
pub fn load_tool_call_mode(setup: &cosh::setup::Setup) -> aiface::connector::ToolCallMode {
    match setup.tools.tool_call_mode.as_str() {
        "inline" => aiface::connector::ToolCallMode::Inline,
        _ => aiface::connector::ToolCallMode::Native,
    }
}

pub fn save_tool_call_mode(setup: &mut cosh::setup::Setup, mode: aiface::connector::ToolCallMode) {
    setup.tools.tool_call_mode = match mode {
        aiface::connector::ToolCallMode::Native => "native".to_string(),
        aiface::connector::ToolCallMode::Inline => "inline".to_string(),
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
            "subagent_status",
            "Check background sub-agent task status (read-only)",
        ),
        (
            "agents_call",
            "Send a message to a /agents coworker pane (block or background)",
        ),
        (
            "agents_worktree",
            "Move a /agents coworker into/out of a git worktree (User-bound: needs approval)",
        ),
        (
            "agents_compact",
            "Compact a /agents coworker's context ahead of the automatic trigger",
        ),
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
            "computer_windows",
            "List one app's open windows, or minimize/maximize/close/move/resize one (action = needs approval)",
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
    /// When `true` the blocklist is written to the global `setup.json` and
    /// applies to every project; when `false` (the default) it is scoped to
    /// the cwd via `.cosh-tools.json`. Rendered as the toggle row above the
    /// tool list — default OFF, hence the ✗ symbol.
    pub global: bool,
    pub changed: bool,
    /// How many rows actually fit in the current viewport. Updated on every
    /// render so that scrolling, clamping and mouse hit-testing all agree on
    /// the same value — otherwise items scroll out of view while free space
    /// below the list is wasted (same fix as the ADD Provider router).
    /// Counts the Global toggle row: index 0 is Global, 1.. are tools.
    visible_count: usize,
    /// Buffer row of the first visible list item, captured at render time.
    /// The vertical centering depends on the exact area the render used, so
    /// the mouse path must reuse it instead of re-deriving it from its own
    /// (differently sized) area.
    /// `pub(crate)` so tests can hover the same rows the render drew.
    pub(crate) list_start_y: u16,
}

/// Selection index of the Global scope toggle: it sits above every tool row.
pub const GLOBAL_ROW_INDEX: usize = 0;

/// Every selectable row: the Global toggle plus one row per tool.
fn total_rows() -> usize {
    internal_tools().len() + 1
}

impl InternalToolsView {
    pub fn new() -> Self {
        Self {
            selection: ListSelection::new(),
            disabled: HashSet::new(),
            global: false,
            changed: false,
            visible_count: 0,
            list_start_y: 0,
        }
    }

    pub fn select_next(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_next(total_rows());
    }

    pub fn select_prev(&mut self) {
        self.selection.set_visible_count(self.visible_count.max(1));
        self.selection.select_prev(total_rows());
    }

    pub fn toggle_current(&mut self) {
        if self.selection.selected_index == GLOBAL_ROW_INDEX {
            self.toggle_global();
            return;
        }
        let (name, _) = internal_tools()[self.selection.selected_index - 1];
        if !self.disabled.remove(name) {
            self.disabled.insert(name.to_string());
        }
        self.changed = true;
    }

    /// Flip the scope of the blocklist. Does NOT persist anything by itself:
    /// the caller swaps `disabled` to the newly selected scope's stored set
    /// and the next tool toggle writes there.
    pub fn toggle_global(&mut self) {
        self.global = !self.global;
        self.changed = true;
    }

    fn find_row_for_mouse(&self, mouse: &MouseEvent, area: Rect) -> Option<usize> {
        // Use the viewport the render computed, so hit-testing matches what
        // is actually drawn on screen. The mouse path receives a slightly
        // different area, and re-deriving the centered block from it shifted
        // every row by one (see the ADD Provider router's regression test).
        let count = self.visible_count.min(total_rows());
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
            // The Global row scrolls with the list like any other row, so
            // the render, the hit-test and the selection window all agree:
            // row j shows selection index scroll_offset + j.
            let idx = self.selection.scroll_offset + i;
            if idx >= total_rows() {
                break;
            }
            let item_y = list_start_y + i as u16;
            if my == item_y {
                return Some(idx);
            }
        }
        None
    }

    /// Move the visual selection to the row under the pointer WITHOUT
    /// toggling it — the same hover-follow contract as the model picker and
    /// the ADD Provider list. Returns whether the pointer hit a tool row;
    /// hovering outside the list keeps the current selection.
    pub fn follow_hover(&mut self, mouse: &MouseEvent, area: Rect) -> bool {
        match self.find_row_for_mouse(mouse, area) {
            Some(idx) => {
                self.selection.selected_index = idx;
                true
            }
            None => false,
        }
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

        draw_text_line(
            buf,
            title,
            row_x,
            start_y,
            area.right() - row_x,
            Style::default().fg(muted),
        );

        // Clamp selection and scroll before rendering
        let count = visible_items(area).min(total_rows());
        self.visible_count = count;
        self.selection.set_visible_count(count.max(1));
        self.selection.clamp(total_rows());

        // tools list aligned
        let list_start_y = start_y + 2;
        self.list_start_y = list_start_y;
        if max_w == 0 {
            return;
        }

        // "  " column for the symbol (ZERO WIDTH prefix so column 0 = symbol)
        let sym_x = row_x;
        let cols = columns();
        let name_x = row_x + cols.name_x as u16;
        let desc_x = row_x + cols.desc_x as u16;

        // One unified loop: row j on screen renders selection index
        // scroll_offset + j — index 0 is the Global scope toggle, index n ≥ 1
        // is internal_tools()[n - 1]. The Global row scrolls with the list,
        // so the render, the mouse hit-test and the selection window can
        // never disagree on which index a screen row shows.
        //
        // Global row (index 0): whether the blocklist applies to this project
        // only (default) or globally. Same ✔/✗ contract as the disable
        // toggles, read from the user's angle: default is OFF (project scope)
        // so it renders ✗; global is opt-in → ✔.
        for i in 0..count {
            let idx = self.selection.scroll_offset + i;
            if idx >= total_rows() {
                break;
            }
            let y = list_start_y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let (name, desc, symbol, sym_color) = if idx == GLOBAL_ROW_INDEX {
                (
                    "Global".to_string(),
                    "Apply the disabled tools to every project (OFF = this project only)"
                        .to_string(),
                    if self.global { "✔" } else { "✗" },
                    if self.global { Color::Green } else { Color::Red },
                )
            } else {
                let (name, desc) = internal_tools()[idx - 1];
                let enabled = !self.disabled.contains(name);
                (
                    name.to_string(),
                    desc.to_string(),
                    if enabled { "✔" } else { "✗" },
                    if enabled { Color::Green } else { Color::Red },
                )
            };
            let is_selected = idx == self.selection.selected_index;
            let row_color = if is_selected { primary } else { fg };

            // Fixed columns: symbol, name and description. Every row carries
            // all three, so the description column never shifts.
            draw_text_line(
                buf,
                symbol,
                sym_x,
                y,
                area.right() - sym_x,
                Style::default().fg(sym_color),
            );
            draw_text_line(
                buf,
                &name,
                name_x,
                y,
                area.right() - name_x,
                Style::default().fg(row_color),
            );
            draw_text_line(
                buf,
                &desc,
                desc_x,
                y,
                area.right() - desc_x,
                Style::default().fg(muted),
            );
        }
    }
}

/// Fixed column layout of the tools list, derived once from the widest
/// name/description so every row aligns: `[✔/✗] [name] [description]`. The
/// render, the centering budget and the mouse hit-test all share it.
/// The Global row's description is intentionally short enough to fit it.
struct Columns {
    name_x: usize,
    desc_x: usize,
    /// Total width a row can occupy.
    total: usize,
}

fn columns() -> Columns {
    let tools = internal_tools();
    let name_w = tools.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let desc_w = tools.iter().map(|(_, d)| d.len()).max().unwrap_or(0);
    let name_x = 2;
    let desc_x = name_x + name_w + 2;
    Columns {
        name_x,
        desc_x,
        total: desc_x + desc_w,
    }
}

/// Compute the Y position where title and list start (vertically centered).
fn content_start_y(area: Rect) -> u16 {
    let count = visible_items(area).min(total_rows());
    let content_height = count + 2; // title + blank line + items
    area.y + (area.height.saturating_sub(content_height as u16)) / 2
}

fn max_row_width() -> usize {
    columns().total
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
        // Tall viewport: the Global row plus every tool fits with room to spare.
        let area = Rect::new(0, 0, 100, 60);
        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, &theme);

        assert_eq!(
            view.visible_count,
            total_rows(),
            "viewport must fit the Global row and the whole list"
        );
        for _ in 0..total_rows() {
            view.select_next();
        }
        assert_eq!(
            view.selection.scroll_offset, 0,
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

    /// The Global row (index 0) flips the scope, persists the choice to
    /// `setup.json` and swaps `disabled` to the other scope's stored set.
    #[test]
    fn toggling_the_global_row_swaps_scopes_without_writing() {
        // The scope toggle itself calls setup.save(); pin the config dir to
        // a scratch dir so the developer's real setup.json is never touched.
        let scratch = tempfile::tempdir().unwrap();
        let previous_cfg = std::env::var("COSH_CONFIG_DIR").ok();
        unsafe { std::env::set_var("COSH_CONFIG_DIR", scratch.path()) };

        let mut setup = cosh::setup::Setup::default();
        setup.tools.disabled = vec!["web_fetch".to_string()];

        // Project scope (default): one tool disabled locally.
        let mut view = InternalToolsView::new();
        view.disabled.insert("fs_read".to_string());
        // Selection already starts on the Global row (index 0).

        toggle_entry(&mut view, &mut setup, "/definitely/not/a/real/cwd");
        assert!(view.global, "the Global row must flip the scope ON");
        assert_eq!(
            setup.tools.scope, "global",
            "the scope choice must persist to setup.json"
        );
        assert!(
            view.disabled.contains("web_fetch"),
            "the view must adopt the global scope's stored set"
        );
        assert!(
            !view.disabled.contains("fs_read"),
            "the project-only set must not leak into the global view"
        );
        assert!(
            !std::path::Path::new("/definitely/not/a/real/cwd/.cosh-tools.json").exists(),
            "toggling the scope itself must not write the project file"
        );

        // Back to project scope: the project choice persists too, and with no
        // project file on disk yet the pre-scope global blocklist applies
        // (the migration fallback).
        toggle_entry(&mut view, &mut setup, "/definitely/not/a/real/cwd");
        assert!(!view.global);
        assert_eq!(setup.tools.scope, "project");
        assert_eq!(view.disabled.iter().collect::<Vec<_>>(), ["web_fetch"]);

        match previous_cfg {
            Some(value) => unsafe { std::env::set_var("COSH_CONFIG_DIR", value) },
            None => unsafe { std::env::remove_var("COSH_CONFIG_DIR") },
        }
    }

    /// Project scope (the default): a tool toggle persists to
    /// `<cwd>/.cosh-tools.json` and leaves the global setup.json untouched;
    /// a fresh load from the same cwd reads the set back.
    #[test]
    fn project_scope_persists_to_the_cwd_and_keeps_setup_intact() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();

        let mut setup = cosh::setup::Setup::default();
        setup.tools.disabled = vec!["web_fetch".to_string()];

        // The Global row is index 0; select the FIRST TOOL (index 1 =
        // bash_run) and toggle it off.
        let mut view = InternalToolsView::new();
        view.selection.selected_index = 1;
        toggle_entry(&mut view, &mut setup, &cwd);

        assert!(view.disabled.contains("bash_run"));
        // The global blocklist is untouched by a project-scope write.
        assert_eq!(setup.tools.disabled, vec!["web_fetch".to_string()]);

        // A fresh load for the SAME cwd reads the project file.
        let reloaded = load_disabled_tools(&setup, &cwd, false);
        assert!(reloaded.contains("bash_run"), "the project file must persist");
        assert!(
            !reloaded.contains("web_fetch"),
            "the project scope must not inherit the global set once it saved its own"
        );

        // A DIFFERENT cwd does not see this project's blocklist.
        let other = tempfile::tempdir().unwrap();
        let other_set = load_disabled_tools(&setup, other.path().to_string_lossy().as_ref(), false);
        assert!(
            other_set.contains("web_fetch"),
            "another project falls back to the pre-scope global set"
        );
        assert!(
            !other_set.contains("bash_run"),
            "another project must NOT inherit this project's blocklist"
        );
    }

    /// The Global row's symbol follows the same ✔/✗ contract as the tool
    /// rows: OFF (project scope, the default) renders ✗ at the symbol column.
    #[test]
    fn global_row_renders_the_x_symbol_by_default() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        let term = Rect::new(0, 0, 100, 40);

        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(term);
        view.render(&mut buf, Rect::new(0, 1, 100, term.height - 4), &theme);

        let row_x = (100u16.saturating_sub(max_row_width() as u16)) / 2;
        let sym_cell = buf
            .cell((row_x, view.list_start_y))
            .map(|c| c.symbol().to_owned())
            .unwrap_or_default();
        assert_eq!(sym_cell, "✗", "project scope (default) must render ✗");

        view.global = true;
        view.render(&mut buf, Rect::new(0, 1, 100, term.height - 4), &theme);
        let sym_cell = buf
            .cell((row_x, view.list_start_y))
            .map(|c| c.symbol().to_owned())
            .unwrap_or_default();
        assert_eq!(sym_cell, "✔", "global scope must render ✔");
    }

    /// The hover-follow contract: a Move over a row moves the highlight and
    /// reports a hit, while a Move outside the list keeps the selection —
    /// the same behavior the model picker and the ADD Provider list give
    /// their rows. Hovering never toggles a tool.
    #[test]
    fn hover_moves_the_selection_without_toggling() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        let term = Rect::new(0, 0, 100, 30);

        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(term);
        view.render(&mut buf, Rect::new(0, 1, 100, term.height - 4), &theme);

        let max_w = max_row_width();
        let row_x = (100u16.saturating_sub(max_w as u16)) / 2;
        let mouse_area = Rect::new(0, 1, 100, term.height - 4 + 1);
        let move_to = |x: u16, y: u16| {
            MouseEvent::new(
                MouseEventType::Move,
                MouseButton::Left,
                x,
                y,
                MouseModifiers::none(),
            )
        };

        // Hover the second row: the selection follows, nothing toggles.
        assert!(view.follow_hover(&move_to(row_x + 2, view.list_start_y + 1), mouse_area));
        assert_eq!(view.selection.selected_index, 1);
        assert!(!view.changed, "a Move must never toggle the hovered tool");

        // Hovering above the list (title area) keeps the selection.
        assert!(!view.follow_hover(&move_to(row_x + 2, view.list_start_y - 1), mouse_area));
        assert_eq!(view.selection.selected_index, 1);
    }

    /// Column alignment regression: every drawn row must start its name and
    /// its description at the SAME x — the fixed-column layout, not a
    /// free-form string that drifts per row.
    #[test]
    fn columns_align_across_rows() {
        let theme = ThemeRegistry::new().themes[0].theme.clone();
        let term = Rect::new(0, 0, 100, 30);
        let render_area = Rect::new(0, 1, 100, term.height - 4);

        let mut view = InternalToolsView::new();
        let mut buf = Buffer::empty(term);
        view.render(&mut buf, render_area, &theme);

        let cols = columns();
        let row_x = render_area.x + (render_area.width.saturating_sub(max_row_width() as u16)) / 2;
        let name_x = row_x + cols.name_x as u16;
        let desc_x = row_x + cols.desc_x as u16;

        for i in 0..view.visible_count {
            let y = view.list_start_y + i as u16;
            let name = buf
                .cell((name_x, y))
                .map(|c| c.symbol().to_owned())
                .unwrap_or_default();
            assert_ne!(
                name, " ",
                "row {i}: no name drawn at the name column x={name_x}"
            );
            let desc = buf
                .cell((desc_x, y))
                .map(|c| c.symbol().to_owned())
                .unwrap_or_default();
            assert_ne!(
                desc, " ",
                "row {i}: no description drawn at the description column x={desc_x}"
            );
        }
    }
}
