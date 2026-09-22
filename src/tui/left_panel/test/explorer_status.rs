use super::*;

fn map(entries: &[(&str, &str)]) -> HashMap<PathBuf, GitFileStatus> {
    parse_porcelain(
        &entries
            .iter()
            .map(|(xy, p)| format!("{xy} {p}"))
            .collect::<Vec<_>>()
            .join("\0"),
        Path::new("/repo"),
    )
}

#[test]
fn porcelain_states_map_to_display_statuses() {
    let entries = map(&[
        ("??", "new.txt"),
        (" M", "mod.txt"),
        ("M ", "staged.txt"),
        (" D", "gone.txt"),
        ("D ", "staged-gone.txt"),
        ("A ", "added.txt"),
        ("UU", "conflict.txt"),
    ]);
    assert_eq!(
        entries[&PathBuf::from("/repo/new.txt")],
        GitFileStatus::Added
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/mod.txt")],
        GitFileStatus::Modified
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/staged.txt")],
        GitFileStatus::Modified
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/gone.txt")],
        GitFileStatus::Deleted
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/staged-gone.txt")],
        GitFileStatus::Deleted
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/added.txt")],
        GitFileStatus::Added
    );
    assert_eq!(
        entries[&PathBuf::from("/repo/conflict.txt")],
        GitFileStatus::Modified
    );
}

#[test]
fn porcelain_rename_records_skip_origin_path() {
    // `R  new.txt\0old.txt\0 M other.txt\0` — old.txt must be consumed.
    let stdout = "R  new.txt\0old.txt\0 M other.txt\0";
    let entries = parse_porcelain(stdout, Path::new("/repo"));
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[&PathBuf::from("/repo/new.txt")],
        GitFileStatus::Modified
    );
    assert!(entries.contains_key(&PathBuf::from("/repo/other.txt")));
    assert!(!entries.contains_key(&PathBuf::from("/repo/old.txt")));
}

#[test]
fn empty_and_short_records_are_skipped() {
    let entries = parse_porcelain("\0\0?? a\0", Path::new("/r"));
    assert_eq!(entries.len(), 1);
    assert!(entries.contains_key(&PathBuf::from("/r/a")));
}

#[test]
fn git_entries_are_scoped_to_the_explorer_root() {
    // A repo-root-wide snapshot scoped down to a sub-tree root: paths
    // outside the root are dropped so directory aggregation never
    // climbs past the root.
    let entries = HashMap::from([
        (PathBuf::from("/repo/pkg/a.rs"), GitFileStatus::Modified),
        (PathBuf::from("/repo/other/b.rs"), GitFileStatus::Added),
        (PathBuf::from("/elsewhere/c.rs"), GitFileStatus::Modified),
    ]);
    let scoped = scope_git_entries(entries, Path::new("/repo/pkg"));
    assert_eq!(scoped.len(), 1);
    let status = scoped[&PathBuf::from("/repo/pkg/a.rs")];
    assert_eq!(status.git, Some(GitFileStatus::Modified));
    assert_eq!(status.lsp, None);
}

#[test]
fn aggregation_rolls_states_up_to_ancestors() {
    let root = Path::new("/work");
    let mut index = StatusIndex::default();
    index.files = HashMap::from([
        (
            PathBuf::from("/work/src/err.rs"),
            EntryStatus {
                git: None,
                lsp: Some(LspFileStatus::Error),
            },
        ),
        (
            PathBuf::from("/work/src/sub/warn.rs"),
            EntryStatus {
                git: None,
                lsp: Some(LspFileStatus::Warning),
            },
        ),
        (
            PathBuf::from("/work/docs/new.md"),
            EntryStatus {
                git: Some(GitFileStatus::Added),
                lsp: None,
            },
        ),
        (PathBuf::from("/work/clean.txt"), EntryStatus::default()),
    ]);
    index.explorer_root = root.to_path_buf();
    index.aggregate();

    // src holds an error AND a (nested) warning: error wins the marker,
    // and both axes are tracked independently.
    let src = index.entry_status(&Path::new("/work/src"), true);
    assert_eq!(src.lsp, Some(LspFileStatus::Error));
    // src/sub only has the warning.
    assert_eq!(
        index.entry_status(&Path::new("/work/src/sub"), true).lsp,
        Some(LspFileStatus::Warning)
    );
    // docs aggregates the added file; the clean file contributes nothing.
    assert_eq!(
        index.entry_status(&Path::new("/work/docs"), true).git,
        Some(GitFileStatus::Added)
    );
    assert_eq!(
        index.entry_status(&Path::new("/work"), true),
        EntryStatus::default()
    );
    // The explorer root itself is not a row and is never aggregated.
    assert!(!index.dirs.contains_key(root));
}

#[test]
fn git_and_lsp_merge_per_axis() {
    // A file with both a modification and diagnostics keeps both signals.
    let mut index = StatusIndex::default();
    index.files = HashMap::from([(
        PathBuf::from("/w/a.rs"),
        EntryStatus {
            git: Some(GitFileStatus::Modified),
            lsp: Some(LspFileStatus::Warning),
        },
    )]);
    index.explorer_root = PathBuf::from("/w");
    index.aggregate();
    let status = index.entry_status(&Path::new("/w/a.rs"), false);
    assert_eq!(status.git, Some(GitFileStatus::Modified));
    assert_eq!(status.lsp, Some(LspFileStatus::Warning));
}
