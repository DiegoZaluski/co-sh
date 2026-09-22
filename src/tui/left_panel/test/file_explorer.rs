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

/// Build a `StatusIndex` with explicit per-file statuses (the unit-tested
/// aggregation in `explorer_status` handles the roll-up; here we only
/// need the render mapping).
fn status_index_with(entries: Vec<(PathBuf, EntryStatus)>) -> StatusIndex {
    let mut index = StatusIndex::default();
    for (path, status) in entries {
        index.insert_for_test(path, status);
    }
    index
}

/// Row content (after the tree gutter) as (char, style) pairs.
fn row_cells(buf: &Buffer, y: u16, x_start: u16, x_end: u16) -> Vec<(char, Style)> {
    (x_start..x_end)
        .map(|x| {
            let cell = &buf[(x, y)];
            (cell.symbol().chars().next().unwrap_or(' '), cell.style())
        })
        .collect()
}

#[test]
fn modified_file_renders_in_warning_color_with_lsp_error_marker() {
    let root = temp_root("status-file");
    fs::write(root.join("a.rs"), "").unwrap();
    fs::write(root.join("b.rs"), "").unwrap();

    let mut view = FileExplorerView::new(root.clone());
    let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
    let statuses = status_index_with(vec![
        (
            root.join("a.rs"),
            EntryStatus {
                git: Some(GitFileStatus::Modified),
                lsp: Some(LspFileStatus::Error),
            },
        ),
        (root.join("b.rs"), EntryStatus::default()),
    ]);

    let area = Rect::new(0, 0, 24, 6);
    let mut buf = Buffer::empty(area);
    view.render(&mut buf, area, &theme, &statuses);

    // Row 0 is a.rs (selected by default: bold warning): name in
    // warning color + a trailing ` ✗` in the error color. Row layout:
    // `├─` gutter at x=1..3, arrow x=3, space x=4, name from x=5.
    let warning = rgba_color(theme.warning);
    let error = rgba_color(theme.error);
    let text = rgba_color(theme.text);
    let row = row_cells(&buf, 2, 5, 13);
    assert_eq!(row[0].0, 'a');
    assert_eq!(row[0].1.fg, Some(warning));
    // Selected rows render bold so the selection stays distinct from
    // status-colored rows.
    assert!(
        row[0]
            .1
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(row[4].0, ' ');
    assert_eq!(row[5].0, '✗');
    assert_eq!(row[5].1.fg, Some(error));

    // Row 1 is b.rs (not selected): clean — plain text color, no marker.
    let row_b = row_cells(&buf, 3, 5, 13);
    assert_eq!(row_b[0].0, 'b');
    assert_eq!(row_b[0].1.fg, Some(text));
    assert!(row_b.iter().all(|(c, _)| *c != '✗' && *c != '⚠'));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn directory_renders_aggregated_state_before_expansion() {
    let root = temp_root("status-dir");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/err.rs"), "").unwrap();

    let mut view = FileExplorerView::new(root.clone());
    let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
    // Only the FILE has the status; the directory row must reflect it
    // through aggregation.
    let statuses = status_index_with(vec![(
        root.join("src/err.rs"),
        EntryStatus {
            git: None,
            lsp: Some(LspFileStatus::Error),
        },
    )]);

    let area = Rect::new(0, 0, 24, 6);
    let mut buf = Buffer::empty(area);
    // Aggregate the file status into `src` first (as `refresh` would).
    view.render(&mut buf, area, &theme, &statuses);

    // The `src` row (row 0, selected: bold) is collapsed, yet carries
    // the aggregated `✗` marker after its name — the pre-expansion
    // signal. Name starts at x=5 (gutter + arrow + space).
    let error = rgba_color(theme.error);
    let row = row_cells(&buf, 2, 5, 13);
    assert_eq!(row[0].0, 's');
    let marker = row.iter().find(|(c, _)| *c == '✗');
    assert!(marker.is_some(), "collapsed dir shows aggregated LSP state");
    assert_eq!(marker.unwrap().1.fg, Some(error));
    fs::remove_dir_all(&root).unwrap();
}

/// Render into a fresh buffer and return it plus the panel geometry.
/// Statuses default to "clean" — status-color behavior has its own
/// dedicated tests below.
fn rendered(view: &mut FileExplorerView, width: u16, height: u16) -> (Buffer, Rect) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let theme = crate::theme::ThemeRegistry::new().default_theme().clone();
    view.render(&mut buf, area, &theme, &StatusIndex::default());
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
