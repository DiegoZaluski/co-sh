//! Demonstrate `find::glob`: the ways a target shapes a search — bare glob,
//! scoped glob, directory literal, file literal — plus `file_type` filtering,
//! `tree` formatting, and multi-target search.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example find-glob
//! ```

use cosh_tools::find::{Find, GlobCallOptions};

fn show(label: &str, result: Result<cosh_tools::find::GlobOutput, String>) {
    println!("== {label} ==");
    match result {
        Ok(out) => {
            for m in &out.matches {
                let size = match m.size_bytes {
                    Some(n) => format!("{n} bytes"),
                    None => "size n/a".to_string(),
                };
                println!("  {} ({}, {size})", m.path, m.file_type);
            }
            if let Some(note) = &out.note {
                println!("  note: {note}");
            }
            println!(
                "  scope={} total={} useless={:?} limit_reached={:?} timed_out={:?}",
                out.scope, out.total, out.useless, out.limit_reached, out.timed_out
            );
        }
        Err(e) => println!("  ERROR: {e}"),
    }
    println!();
}

// The default sort is by modification time (most recent first), which makes
// output order depend on write timestamps. Most demos pin `sort_by_mtime:
// false` so the output is stable and path-sorted; the first demo shows the
// default. Note that `size`/`mtime` metadata is only populated when
// `sort_by_mtime` is true (the default) — the sorted demos print "size n/a".
fn sorted() -> GlobCallOptions {
    GlobCallOptions {
        sort_by_mtime: Some(false),
        ..Default::default()
    }
}

fn main() {
    // A small scratch project. `Find::cwd` sets the root that path guards
    // validate against AND the base that output paths are rebased to.
    let project = std::env::temp_dir().join("cosh-find-glob-example");
    let _ = std::fs::remove_dir_all(&project);
    for dir in ["src/util", "src/bin", "tests", "docs"] {
        std::fs::create_dir_all(project.join(dir)).unwrap();
    }
    std::fs::write(project.join("src/lib.rs"), "fn lib() {}\n").unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(project.join("src/util/helper.rs"), "fn helper() {}\n").unwrap();
    std::fs::write(project.join("src/bin/tool.rs"), "fn tool() {}\n").unwrap();
    std::fs::write(project.join("tests/it.rs"), "#[test] fn t() {}\n").unwrap();
    std::fs::write(project.join("docs/guide.md"), "# Guide\n").unwrap();
    std::fs::write(project.join("README.md"), "# scratch\n").unwrap();
    std::fs::write(project.join(".gitignore"), "# build\n").unwrap();
    std::fs::write(project.join("src/.hidden.rs"), "// hidden\n").unwrap();

    let find = Find::new().cwd(&project);

    // 1. Bare glob as a target: recurses from the working directory — every
    //    `.rs` file at every depth under the project root. (Default sort:
    //    most recently modified first.)
    show(
        "bare glob target `*.rs`",
        find.glob_full(
            "",
            Some("*.rs".into()),
            None,
            GlobCallOptions::default(),
            None,
        ),
    );

    // 2. Plain directory target + bare `pattern`: SHALLOW — only the direct
    //    children of `src` named *.rs. `src/util/helper.rs` is NOT matched.
    show(
        "pattern `*.rs` over dir target `src`",
        find.glob_full("*.rs", Some("src".into()), None, sorted(), None),
    );

    // 3. Same directory target, recursive pattern: the whole tree.
    show(
        "pattern `**/*.rs` over dir target `src`",
        find.glob_full("**/*.rs", Some("src".into()), None, sorted(), None),
    );

    // 4. Scoped glob target stays shallow: `src/*.rs`, not `src/sub/*.rs`.
    show(
        "scoped glob target `src/*.rs`",
        find.glob_full("", Some("src/*.rs".into()), None, sorted(), None),
    );

    // 5. file_type filter: only directories (rendered with a trailing `/`).
    show(
        "directories only under `src`",
        find.glob_full(
            "**",
            Some("src".into()),
            None,
            GlobCallOptions {
                file_type: Some("dir".into()),
                ..sorted()
            },
            None,
        ),
    );

    // 6. tree format groups the output by depth; `formatted` carries the
    //    rendered tree.
    let tree = find
        .glob_full(
            "**/*.rs",
            Some("src".into()),
            None,
            GlobCallOptions {
                format: Some("tree".into()),
                ..sorted()
            },
            None,
        )
        .unwrap();
    println!("== tree format ==");
    println!("{}", tree.formatted);
    println!();

    // 7. Multi-target search: `paths` overrides `path`; results from both
    //    roots are merged and deduplicated.
    show(
        "multi-target `paths = [src, tests]`",
        find.glob_full(
            "*.rs",
            None,
            Some(vec!["src".into(), "tests".into()]),
            sorted(),
            None,
        ),
    );

    // 8. A missing target among several is skipped (reported in
    //    `missing_paths`), not an error.
    show(
        "one missing target",
        find.glob_full(
            "*.rs",
            None,
            Some(vec!["src".into(), "nope".into()]),
            sorted(),
            None,
        ),
    );

    // 9. Every target missing IS an error.
    show(
        "all targets missing",
        find.glob_full(
            "*.rs",
            Some("nope".into()),
            None,
            GlobCallOptions::default(),
            None,
        ),
    );

    // 10. A literal file target short-circuits the walk: the file itself is
    //     returned as the single match.
    show(
        "file literal target `src/lib.rs`",
        find.glob_full("", Some("src/lib.rs".into()), None, sorted(), None),
    );
}
