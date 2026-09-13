//! File explorer view for the left sidebar (Ctrl+F).
//!
//! A VS Code-style tree of the project directory, rendered inside the same
//! sidebar box the session history uses. Directories expand/collapse (Enter,
//! Space, Right, Left, or a click); files open in the configured editor.
//! The tree is lazy: a directory's children are only read when it is
//! expanded, and the flattened row list is rebuilt whenever expansion or
//! the root changes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use cosh_tui::core::types::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::theme::{Theme, rgba_color};
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
    /// with the session-list sidebar); everything else lives here.
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

    /// The list starts below header + separator, like the session sidebar.
    fn in_list_area(mouse: &MouseEvent, area: Rect) -> bool {
        mouse.x >= area.x
            && mouse.x < area.right()
            && mouse.y >= area.y + 2
            && mouse.y < area.bottom()
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
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
            .map(|e| 2 * e.depth + 5 + e.path.file_name().map_or(0, |n| n.to_string_lossy().len()))
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
        Self::draw_text_line(
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
        // The selection must differ from BOTH row colors: files render
        // with `text` and directories with `primary`. `warning` is the
        // tone furthest from both across the themes.
        let selected_fg = Style::default().fg(warning_color);
        let normal_fg = Style::default().fg(text_color);
        let dir_fg = Style::default().fg(primary_color);
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
            let style = if is_selected { selected_fg } else { normal_fg };

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
            let name_style = if entry.is_dir && !is_selected {
                dir_fg
            } else {
                style
            };
            // Row content laid out at its natural columns minus the scroll
            // offset, one char at a time: the arrow rides with the muted
            // guide color (tree chrome, like the `├─` connectors) and the
            // name carries the row style. Chars scrolled past the panel's
            // left padding or the right edge are simply not painted — no
            // column arithmetic can underflow, however deep the row.
            let clip_left = area.x as i32 + 1;
            let right = area.right() as i32;
            let base_x = area.x as i32 + 1 + gutter_w as i32;
            for (ci, ch) in format!("{arrow} {name}").chars().enumerate() {
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
                    cell.set_style(if ci == 0 { guide_fg } else { name_style });
                }
            }
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

    /// Same cell-writer the session sidebar uses (control chars skipped, so
    /// ratatui's buffer diff never panics on odd file names).
    fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
        let Some(right) = x.checked_add(max_w) else {
            return;
        };
        for (i, ch) in text.chars().enumerate() {
            if ch.is_control() {
                continue;
            }
            let Some(cx) = x.checked_add(i as u16) else {
                break;
            };
            if cx >= right {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Unique temp dir per test (no `tempfile` dependency in this crate).
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cosh-explorer-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dirs_sort_before_files_case_insensitive() {
        let root = temp_root("sort");
        fs::create_dir(root.join("beta_dir")).unwrap();
        fs::create_dir(root.join("Alpha_dir")).unwrap();
        fs::write(root.join("zeta.txt"), "").unwrap();
        fs::write(root.join("Alpha_file.txt"), "").unwrap();

        let view = FileExplorerView::new(root.clone());
        let names: Vec<String> = (0..view.len())
            .map(|i| {
                view.entries[i]
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            ["Alpha_dir", "beta_dir", "Alpha_file.txt", "zeta.txt"]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn collapsed_dir_has_no_children_rows_until_expanded() {
        let root = temp_root("expand");
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("sub/deeper/file.txt"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        assert_eq!(view.len(), 1, "collapsed subdir shows one row");
        assert!(!view.entries[0].expanded);

        view.toggle_dir(&root.join("sub"));
        assert_eq!(view.len(), 2, "expanded subdir shows its child");
        assert_eq!(view.entries[1].depth, 1);
        assert_eq!(
            view.entries[1].path,
            root.join("sub/deeper"),
            "deeper dir stays collapsed (lazy)"
        );

        view.toggle_dir(&root.join("sub"));
        assert_eq!(view.len(), 1, "collapse hides children again");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn enter_on_file_returns_open_file_action() {
        let root = temp_root("open");
        fs::write(root.join("main.rs"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        let action = view.handle_key(KeyCode::Enter);
        assert_eq!(action, ExplorerAction::OpenFile(root.join("main.rs")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn enter_on_dir_toggles_without_action() {
        let root = temp_root("dir-toggle");
        // A non-empty dir so expansion is visible in the row count (an
        // empty dir expands to nothing extra).
        fs::create_dir_all(root.join("src/sub")).unwrap();

        let mut view = FileExplorerView::new(root.clone());
        assert_eq!(view.handle_key(KeyCode::Enter), ExplorerAction::None);
        assert_eq!(view.len(), 2, "dir expanded by Enter");
        assert_eq!(view.handle_key(KeyCode::Enter), ExplorerAction::None);
        assert_eq!(view.len(), 1, "dir collapsed by second Enter");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn left_collapses_expanded_dir_and_moves_selection_from_file_to_parent() {
        let root = temp_root("left");
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/lib.rs"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        view.toggle_dir(&root.join("pkg")); // rows: pkg, pkg/lib.rs
        assert_eq!(view.handle_key(KeyCode::Down), ExplorerAction::None);
        // Left on the nested file jumps to the parent directory row.
        assert_eq!(view.handle_key(KeyCode::Left), ExplorerAction::None);
        assert_eq!(view.selected_entry().unwrap().path, root.join("pkg"));
        // Left again collapses it.
        assert_eq!(view.handle_key(KeyCode::Left), ExplorerAction::None);
        assert_eq!(view.len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn right_expands_collapsed_dir() {
        let root = temp_root("right");
        fs::create_dir_all(root.join("a/b")).unwrap();

        let mut view = FileExplorerView::new(root.clone());
        assert_eq!(view.handle_key(KeyCode::Right), ExplorerAction::None);
        assert!(view.entries[0].expanded);
        assert_eq!(view.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unreadable_and_hidden_entries_do_not_panic() {
        let root = temp_root("hidden");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join(".git/config"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        view.toggle_dir(&root.join(".git"));
        assert!(view.len() >= 3);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_root_yields_empty_tree() {
        let view = FileExplorerView::new(temp_root("missing-root").join("nope"));
        assert!(view.is_empty());
        assert_eq!(view.selected_entry(), None);
    }

    /// Render into a fresh buffer and return it plus the panel geometry.
    fn rendered(view: &mut FileExplorerView, width: u16, height: u16) -> (Buffer, Rect) {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
        view.render(&mut buf, area, &theme);
        (buf, area)
    }

    fn cell_char(buf: &Buffer, x: u16, y: u16) -> char {
        buf[(x, y)].symbol().chars().next().unwrap_or(' ')
    }

    #[test]
    fn right_scrolls_view_when_nothing_more_to_expand() {
        let root = temp_root("hscroll");
        // Deeply nested: expanding the chain leaves the innermost rows far
        // beyond a narrow panel's right edge.
        fs::create_dir_all(root.join("l1/l2/l3/l4/l5/deep_directory_name.txt")).unwrap();
        fs::write(root.join("deep_file_name_here.rs"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        for d in ["l1", "l1/l2", "l1/l2/l3", "l1/l2/l3/l4", "l1/l2/l3/l4/l5"] {
            view.toggle_dir(&root.join(d));
        }
        // Select the deep file so its name is what we track on screen.
        for _ in 0..view.len() {
            if view
                .selected_entry()
                .unwrap()
                .path
                .ends_with("deep_file_name_here.rs")
            {
                break;
            }
            view.handle_key(KeyCode::Down);
        }
        let _before_buf = rendered(&mut view, 24, 14);
        let before = view.h_scroll;
        assert_eq!(before, 0, "freshly rendered view starts unscrolled");

        // Right on the already-expanded dir (or file) scrolls the view.
        assert_eq!(view.handle_key(KeyCode::Right), ExplorerAction::None);
        let (buf_after, _a) = rendered(&mut view, 24, 14);
        assert!(
            view.h_scroll > before,
            "Right must shift the view when content overflows"
        );
        // The header gains the shifted-view marker.
        let header: String = (1..12).map(|x| cell_char(&buf_after, x, 0)).collect();
        assert!(
            header.contains('\u{2039}'),
            "header shows ‹ after scrolling: {header}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn left_first_unscrolls_before_collapsing() {
        let root = temp_root("hscroll-left");
        fs::create_dir_all(root.join("a/b/c/d/e/target")).unwrap();
        fs::write(root.join("a/b/c/d/e/deep_file_here.txt"), "").unwrap();

        let mut view = FileExplorerView::new(root.clone());
        for d in ["a", "a/b", "a/b/c", "a/b/c/d", "a/b/c/d/e"] {
            view.toggle_dir(&root.join(d));
        }
        // Scroll right a couple of steps (on an expanded dir).
        view.handle_key(KeyCode::Right);
        view.handle_key(KeyCode::Right);
        assert!(view.h_scroll > 0);
        let expanded_before = view.entries.iter().filter(|e| e.expanded).count();

        // First Left only winds the scroll back; nothing collapses.
        assert_eq!(view.handle_key(KeyCode::Left), ExplorerAction::None);
        assert_eq!(
            view.entries.iter().filter(|e| e.expanded).count(),
            expanded_before,
            "Left must not collapse while the view is scrolled"
        );
        assert!(view.h_scroll > 0, "one step only");

        // Keep pressing until the scroll reaches 0...
        while view.h_scroll > 0 {
            view.handle_key(KeyCode::Left);
        }
        // ...then Left collapses again (existing behavior intact).
        let selected = view.selected_entry().cloned();
        view.handle_key(KeyCode::Left);
        assert!(
            view.entries.iter().filter(|e| e.expanded).count() < expanded_before
                || selected.map(|e| !e.is_dir).unwrap_or(false),
            "Left collapses / navigates once fully unscrolled"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scroll_clamps_to_widest_visible_row_and_releases_on_collapse() {
        let root = temp_root("hscroll-clamp");
        fs::create_dir_all(root.join("only/deeper/and_more")).unwrap();
        fs::write(
            root.join("only/deeper/and_more/a_very_long_file_name.txt"),
            "",
        )
        .unwrap();

        let mut view = FileExplorerView::new(root.clone());
        view.toggle_dir(&root.join("only"));
        view.toggle_dir(&root.join("only/deeper"));
        view.toggle_dir(&root.join("only/deeper/and_more"));

        // Hammer Right far beyond any real overflow: the offset must stay
        // bounded by the widest visible row, never run away.
        for _ in 0..50 {
            view.handle_key(KeyCode::Right);
        }
        let (_buf, _area) = rendered(&mut view, 20, 12);
        assert!(
            view.h_scroll < 400,
            "clamped to actual overflow: {}",
            view.h_scroll
        );

        // Collapsing the deep directory removes the overflow entirely:
        // the next render must pull the view back to 0.
        view.toggle_dir(&root.join("only/deeper/and_more"));
        let _ = rendered(&mut view, 20, 12);
        assert_eq!(view.h_scroll, 0, "view snaps back when overflow disappears");
        fs::remove_dir_all(&root).unwrap();
    }
}
