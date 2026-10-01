//! File explorer view for the left panel (Ctrl+F).
//!
//! A VS Code-style tree of the project directory, rendered inside the same
//! left panel box the session history uses. Directories expand/collapse (Enter,
//! Space, Right, Left, or a click); files open in the configured editor.
//! The tree is lazy: a directory's children are only read when it is
//! expanded, and the flattened row list is rebuilt whenever expansion or
//! the root changes. Rows carry git + LSP status colors supplied by the
//! caller (see [`super::explorer_status`]): modified/added/deleted files
//! get the diff palette, files and directories with LSP findings get a
//! trailing `✗`/`⚠` marker, and directories aggregate the states of
//! everything below them so a collapsed directory already shows its state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::explorer_status::{EntryStatus, GitFileStatus, LspFileStatus, StatusIndex};
use crate::theme::{Theme, rgba_color};
use crate::util::draw::draw_text_line;
use crate::util::list_selection::ListSelection;

/// Safety cap on the flattened row list: a pathological tree (deeply
/// nested directories, or directory symlinks expanded by the user) must
/// never be able to grow it (and memory) without bound per refresh.
const MAX_ROWS: usize = 100_000;

/// Columns shifted per Right/Left press while scrolling horizontally
/// (aligned with the 2-column indent so content lands predictably).
const H_SCROLL_STEP: usize = 4;

/// One flattened, visible row of the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Whether the directory is currently expanded (files: `false`).
    pub expanded: bool,
    /// Nesting level (0 = direct child of the root).
    pub depth: usize,
}

/// Action returned by the explorer after a key press or mouse click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerAction {
    /// Open the given file in the editor.
    OpenFile(PathBuf),
    /// No action.
    None,
}

pub struct FileExplorerView {
    /// Project root shown by the tree.
    pub root: PathBuf,
    /// Directories the user expanded.
    expanded: HashSet<PathBuf>,
    /// Flattened visible rows, rebuilt on expansion/root changes.
    entries: Vec<ExplorerEntry>,
    /// Columns the view is shifted right (horizontal scroll). Clamped to
    /// the widest visible row on every render, so collapsing a directory
    /// or shrinking the panel can never leave the view scrolled past
    /// actual content.
    h_scroll: usize,
    /// Shared list-selection state: selected_index + scroll_offset.
    pub selection: ListSelection,
}

impl FileExplorerView {
    pub fn new(root: PathBuf) -> Self {
        let mut view = Self {
            root,
            expanded: HashSet::new(),
            entries: Vec::new(),
            h_scroll: 0,
            selection: ListSelection::new(),
        };
        view.refresh();
        view
    }

    /// Number of visible rows (mirrors what the last `refresh` built).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Selected entry, if any.
    pub fn selected_entry(&self) -> Option<&ExplorerEntry> {
        self.entries.get(self.selection.selected_index)
    }

    /// Rebuild the flattened row list from the expansion state. Cheap for
    /// lazily expanded trees (only expanded directories are read).
    pub fn refresh(&mut self) {
        self.entries = Self::build_entries(&self.root, &self.expanded);
        self.selection.clamp(self.entries.len());
    }

    fn build_entries(root: &Path, expanded: &HashSet<PathBuf>) -> Vec<ExplorerEntry> {
        let mut entries = Vec::new();
        Self::walk(root, 0, expanded, &mut entries);
        entries
    }

    /// Depth-first walk of expanded directories. Directories sort before
    /// files, each group case-insensitively by name — the convention every
    /// file explorer user already knows.
    fn walk(
        dir: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
        entries: &mut Vec<ExplorerEntry>,
    ) {
        if entries.len() >= MAX_ROWS {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<(PathBuf, String, bool)> = read
            .filter_map(Result::ok)
            .filter_map(|e| {
                // Only directories and regular files are shown: opening a
                // FIFO/socket/device could block the editor (and the whole
                // TUI) indefinitely. metadata() follows symlinks, so a
                // symlink is classified by its target.
                let Ok(md) = e.metadata() else {
                    return None;
                };
                let is_dir = md.is_dir();
                if !is_dir && !md.is_file() {
                    return None;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                Some((e.path(), name, is_dir))
            })
            .collect();
        children.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
                .then_with(|| a.1.cmp(&b.1))
        });
        for (path, _, is_dir) in children {
            let is_expanded = is_dir && expanded.contains(&path);
            entries.push(ExplorerEntry {
                path: path.clone(),
                is_dir,
                expanded: is_expanded,
                depth,
            });
            if entries.len() >= MAX_ROWS {
                return;
            }
            if is_expanded {
                Self::walk(&path, depth + 1, expanded, entries);
            }
        }
    }

    /// Toggle a directory's expansion (and re-build the rows).
    pub fn toggle_dir(&mut self, path: &Path) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.refresh();
    }

    /// Move the selection up one visible row.
    pub fn select_prev(&mut self) {
        self.selection.select_prev(self.entries.len());
    }

    /// Move the selection down one visible row.
    pub fn select_next(&mut self) {
        self.selection.select_next(self.entries.len());
    }

    /// Handle a key press. `Up`/`Down` are handled by the caller (shared
    /// with the session-list left panel); everything else lives here.
    pub fn handle_key(&mut self, key: KeyCode) -> ExplorerAction {
        if !matches!(
            key,
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
        ) {
            return ExplorerAction::None;
        }
        let Some(entry) = self.selected_entry().cloned() else {
            return ExplorerAction::None;
        };
        match key {
            KeyCode::Enter | KeyCode::Char(' ') => {
                if entry.is_dir {
                    self.toggle_dir(&entry.path);
                    ExplorerAction::None
                } else {
                    ExplorerAction::OpenFile(entry.path)
                }
            }
            KeyCode::Right if entry.is_dir && !entry.expanded => {
                self.toggle_dir(&entry.path);
                ExplorerAction::None
            }
            KeyCode::Right => {
                // The directory is already expanded (or it is a file):
                // shift the view right so deeply nested content comes into
                // view. Render clamps the offset to the real overflow.
                self.h_scroll += H_SCROLL_STEP;
                ExplorerAction::None
            }
            KeyCode::Left => {
                // Scrolled? Bring the view back first — navigation
                // (collapse / jump to parent) only once fully returned.
                if self.h_scroll > 0 {
                    self.h_scroll = self.h_scroll.saturating_sub(H_SCROLL_STEP);
                    return ExplorerAction::None;
                }
                // VS Code behavior: collapse an expanded directory; a file
                // (or a collapsed dir) first jumps to its parent.
                if entry.is_dir && entry.expanded {
                    self.toggle_dir(&entry.path);
                    return ExplorerAction::None;
                }
                if let Some(parent) = entry.path.parent()
                    && parent != self.root
                    && let Some(idx) = self.entries.iter().position(|e| e.path == parent)
                {
                    self.selection.selected_index = idx;
                    self.selection.clamp(self.entries.len());
                }
                ExplorerAction::None
            }
            _ => ExplorerAction::None,
        }
    }

    /// Handle a mouse click on the explorer area. Returns an action to run.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent, area: Rect) -> ExplorerAction {
        if !Self::in_list_area(mouse, area) {
            return ExplorerAction::None;
        }
        let idx = self.selection.scroll_offset + (mouse.y - area.y - 2) as usize;
        let Some(entry) = self.entries.get(idx).cloned() else {
            return ExplorerAction::None;
        };
        self.selection.selected_index = idx;
        self.selection.clamp(self.entries.len());
        if entry.is_dir {
            self.toggle_dir(&entry.path);
            ExplorerAction::None
        } else {
            ExplorerAction::OpenFile(entry.path)
        }
    }

    /// The list starts below header + separator, like the sessions list.
    fn in_list_area(mouse: &MouseEvent, area: Rect) -> bool {
        mouse.x >= area.x
            && mouse.x < area.right()
            && mouse.y >= area.y + 2
            && mouse.y < area.bottom()
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, statuses: &StatusIndex) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let mut bg_box = BoxRenderable::new();
        bg_box.set_background_color(Some(theme.background_panel.into()));
        bg_box.render_self(buf, area);

        let content_start_y = area.y + 2;
        let visible_count = area.bottom().saturating_sub(content_start_y) as usize;
        self.selection.set_visible_count(visible_count);
        self.selection.clamp(self.entries.len());

        // Horizontal scroll: the view can only be shifted as far as the
        // widest visible row actually overflows — collapsing a directory
        // or shrinking the panel snaps the view back for free. Row width
        // = gutter (2/level) + arrow + space + name.
        let viewport_w = area.width.saturating_sub(1) as usize;
        let max_overflow = self
            .entries
            .iter()
            .skip(self.selection.scroll_offset)
            .take(visible_count)
            .map(|e| {
                let name_len = e.path.file_name().map_or(0, |n| n.to_string_lossy().len());
                // A status marker (space + glyph) rides after the name and
                // must count towards the row width or the horizontal-scroll
                // clamp would let the marker scroll away.
                let marker_len =
                    if Self::lsp_marker(&statuses.entry_status(&e.path, e.is_dir), theme).is_some()
                    {
                        2
                    } else {
                        0
                    };
                2 * e.depth + 5 + name_len + marker_len
            })
            .max()
            .unwrap_or(0)
            .saturating_sub(viewport_w);
        self.h_scroll = self.h_scroll.min(max_overflow);
        let shift = self.h_scroll as u16;

        let header_style = Style::default().fg(rgba_color(theme.text_muted));
        // `‹›` marks a horizontally shifted view; `›` marks content still
        // hidden to the right. Nothing extra when everything fits.
        let scroll_hint = if self.h_scroll > 0 {
            " \u{2039}\u{203a}"
        } else if max_overflow > 0 {
            " \u{203a}"
        } else {
            ""
        };
        draw_text_line(
            buf,
            &format!(" Explorer{scroll_hint}"),
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            header_style,
        );

        let separator_style = Style::default().fg(rgba_color(theme.border));
        if let Some(cell) = buf.cell_mut((area.x + 1, area.y + 1)) {
            cell.set_char('\u{2500}');
            cell.set_style(separator_style);
        }

        let primary_color = rgba_color(theme.primary);
        let warning_color = rgba_color(theme.warning);
        let text_color = rgba_color(theme.text);
        let mute_fg = rgba_color(theme.text_muted);
        // The selection must stand out from every row color: files may now
        // render in `warning` (modified) or a diff color, so the bold
        // modifier keeps the selected row distinct even when its status
        // color coincides with the selection tone.
        let selected_fg = Style::default()
            .fg(warning_color)
            .add_modifier(Modifier::BOLD);
        let dir_fg = Style::default().fg(primary_color);
        let file_fg = Style::default().fg(text_color);
        let guide_fg = Style::default().fg(mute_fg);

        for (i, entry) in self
            .entries
            .iter()
            .enumerate()
            .skip(self.selection.scroll_offset)
        {
            let local_idx = i - self.selection.scroll_offset;
            let y = content_start_y + local_idx as u16;
            if y >= area.bottom() {
                break;
            }
            let is_selected = i == self.selection.selected_index;

            // Tree gutter: `│ ` guide pairs for every open ancestor level,
            // then the `├─` connector into the node.
            let gutter_w = 2 * entry.depth as u16 + 2;
            Self::draw_tree_gutter(buf, area, entry.depth, y, guide_fg, shift);

            let name = entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| entry.path.display().to_string());
            // Directory nodes carry their own expand arrow; files lean on
            // the connector alone, exactly like VS Code's explorer.
            let arrow = match entry {
                ExplorerEntry {
                    is_dir: true,
                    expanded: true,
                    ..
                } => '\u{2574}', // ˄ gutter ▾ marker
                _ => ' ',
            };
            // Status-driven appearance: a file's git state colors its name
            // (VS Code-style), LSP findings add a trailing marker, and
            // directories — which aggregate everything below them — get the
            // same treatment, so a collapsed directory already shows its
            // state before it is expanded.
            let entry_status = statuses.entry_status(&entry.path, entry.is_dir);
            let name_style = if is_selected {
                selected_fg
            } else if entry.is_dir && entry_status.git.is_none() {
                dir_fg
            } else {
                Self::git_name_style(entry_status.git, theme, file_fg)
            };
            let marker = Self::lsp_marker(&entry_status, theme);
            let marker_style = marker.map_or(name_style, |(_, style)| style);
            // Row content laid out at its natural columns minus the scroll
            // offset, one char at a time: the arrow rides with the muted
            // guide color (tree chrome, like the `├─` connectors) and the
            // name carries the row style. Chars scrolled past the panel's
            // left padding or the right edge are simply not painted — no
            // column arithmetic can underflow, however deep the row.
            let clip_left = area.x as i32 + 1;
            let right = area.right() as i32;
            let base_x = area.x as i32 + 1 + gutter_w as i32;
            let name_chars = name.chars().count();
            let marker_str = marker.map_or_else(String::new, |(ch, _)| format!(" {ch}"));
            for (ci, ch) in format!("{arrow} {name}{marker_str}").chars().enumerate() {
                if ch.is_control() {
                    // Same guard as `draw_text_line`: a raw control char in
                    // the buffer would crash ratatui's diff on odd names.
                    continue;
                }
                let cx = base_x + ci as i32 - shift as i32;
                if cx < clip_left || cx >= right {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((cx as u16, y)) {
                    cell.set_char(ch);
                    cell.set_style(if ci == 0 {
                        guide_fg
                    } else if ci < 2 + name_chars {
                        name_style
                    } else {
                        marker_style
                    });
                }
            }
        }
    }

    /// Name color for one row's git state — the diff palette the transcript's
    /// diff view already uses, so "modified/added/deleted" reads the same
    /// everywhere in the TUI. Clean rows keep the caller's default style
    /// (`fallback`).
    ///
    /// Modified maps to `warning` (amber, the "uncommitted change" tone) and
    /// added/deleted map to `diff_added`/`diff_removed`, matching how VS Code
    /// distinguishes dirty (U/amber) from added (A/green) and deleted
    /// (D/red) files.
    fn git_name_style(git: Option<GitFileStatus>, theme: &Theme, fallback: Style) -> Style {
        let fg = match git {
            Some(GitFileStatus::Modified) => rgba_color(theme.warning),
            Some(GitFileStatus::Added) => rgba_color(theme.diff_added),
            Some(GitFileStatus::Deleted) => rgba_color(theme.diff_removed),
            None => return fallback,
        };
        Style::default().fg(fg)
    }

    /// Trailing LSP marker for a row: `✗` in `error` or `⚠` in `warning` —
    /// the same glyphs and colors the passive LSP notes below tool output
    /// use. `None` when the entry has no LSP findings.
    fn lsp_marker(status: &EntryStatus, theme: &Theme) -> Option<(char, Style)> {
        match status.lsp {
            Some(LspFileStatus::Error) => {
                Some(('\u{2717}', Style::default().fg(rgba_color(theme.error))))
            }
            Some(LspFileStatus::Warning) => {
                Some(('\u{26a0}', Style::default().fg(rgba_color(theme.warning))))
            }
            None => None,
        }
    }

    /// Branch gutter + connector column for one row: `│ ` pairs for every
    /// open ancestor level, then the `├─` connector into the node. Each
    /// glyph sits at its natural column minus the scroll offset, so the
    /// whole tree (guides, connectors, names) slides left together; glyphs
    /// scrolled past the panel's left padding are simply not painted.
    fn draw_tree_gutter(
        buf: &mut Buffer,
        area: Rect,
        depth: usize,
        y: u16,
        guide: Style,
        shift: u16,
    ) {
        let clip_left = area.x as i32 + 1;
        let right = area.right() as i32;
        let mut x = area.x as i32 + 1 - shift as i32;
        for _ in 0..depth {
            for ch in ['\u{2502}', ' '] {
                if x >= clip_left
                    && x < right
                    && let Some(cell) = buf.cell_mut((x as u16, y))
                {
                    cell.set_char(ch);
                    cell.set_style(guide);
                }
                x += 1;
            }
        }
        for ch in ['\u{251c}', '\u{2500}'] {
            if x >= clip_left
                && x < right
                && let Some(cell) = buf.cell_mut((x as u16, y))
            {
                cell.set_char(ch);
                cell.set_style(guide);
            }
            x += 1;
        }
    }
}

#[cfg(test)]
#[path = "test/file_explorer.rs"]
mod tests;
