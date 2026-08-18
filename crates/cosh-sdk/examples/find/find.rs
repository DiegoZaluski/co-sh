//! Demonstrate the `find` module: glob name search, grep content search,
//! output modes, pattern helpers, the shared scan cache, cancellation, and
//! the error paths.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example find
//! ```

use cosh_sdk::find::task::{AbortReason, AbortToken, CancelToken};
use cosh_sdk::find::{
    FileType, GlobOptions, GlobResult, GrepOptions, GrepOutputMode, GrepResult, build_glob_pattern,
    glob, grep,
};
use std::path::Path;

fn show_glob(label: &str, result: Result<GlobResult, String>) {
    println!("== {label} ==");
    match result {
        Ok(out) => {
            for m in &out.matches {
                let kind = match m.file_type {
                    FileType::File => "file",
                    FileType::Dir => "dir",
                    FileType::Symlink => "symlink",
                };
                let size = m.size.map(|s| format!(", {s} bytes")).unwrap_or_default();
                println!("  {kind} {}{size}", m.path);
            }
            println!("  total={} timed_out={}", out.total_matches, out.timed_out);
        }
        Err(e) => println!("  ERROR: {e}"),
    }
    println!();
}

fn show_grep(label: &str, result: Result<GrepResult, String>) {
    println!("== {label} ==");
    match result {
        Ok(out) => {
            for m in &out.matches {
                if let Some(count) = m.match_count {
                    println!("  {}: {count} match(es)", m.path);
                } else {
                    println!("  {}:{}\t{}", m.path, m.line_number, m.line.trim_end());
                }
            }
            println!(
                "  total={} files_with={} files_searched={} limit_reached={:?} timed_out={}",
                out.total_matches,
                out.files_with_matches,
                out.files_searched,
                out.limit_reached,
                out.timed_out
            );
        }
        Err(e) => println!("  ERROR: {e}"),
    }
    println!();
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn main() {
    // A scratch project. Both engines resolve relative paths against the
    // process working directory, so we chdir into the scratch root.
    let project = std::env::temp_dir().join("cosh-sdk-find-example");
    let _ = std::fs::remove_dir_all(&project);
    write(
        &project.join("src/lib.rs"),
        "pub fn double(x: i32) -> i32 { x * 2 }\n",
    );
    write(
        &project.join("src/main.rs"),
        "fn main() { println!(\"hi\"); }\n",
    );
    write(&project.join("src/util/helper.rs"), "fn helper() {}\n");
    write(&project.join("tests/it.rs"), "fn double_works() {}\n");
    write(&project.join("docs/guide.md"), "# Guide\n");
    write(&project.join("README.md"), "# scratch\n");
    write(&project.join(".hidden.rs"), "// hidden\n");
    write(&project.join("node_modules/pkg/index.js"), "const x = 1;\n");
    std::env::set_current_dir(&project).unwrap();

    // ── 1. glob: recursive by default, paths relative to the root ───────────
    show_glob(
        "glob `*.rs` (recursive)",
        glob(GlobOptions {
            pattern: "*.rs".into(),
            path: ".".into(),
            file_type: None,
            recursive: None,
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 2. glob: non-recursive vs. file_type filter ────────────────────────
    // `recursive: false` keeps `*.rs` shallow: src/lib.rs and src/main.rs,
    // but NOT src/util/helper.rs.
    show_glob(
        "glob `*.rs` under src, non-recursive",
        glob(GlobOptions {
            pattern: "*.rs".into(),
            path: "src".into(),
            file_type: None,
            recursive: Some(false),
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    // `file_type: Dir` keeps only directories. `**` is recursive but matches
    // every entry, so the filter is the only thing that narrows the result.
    show_glob(
        "glob `**` under src, dirs only",
        glob(GlobOptions {
            pattern: "**".into(),
            path: "src".into(),
            file_type: Some(FileType::Dir),
            recursive: None,
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 3. glob: hidden + node_modules policy ───────────────────────────────
    // Hidden files are excluded by default; node_modules is pruned unless the
    // pattern mentions it.
    show_glob(
        "glob `**/*` (hidden default false)",
        glob(GlobOptions {
            pattern: "**/*".into(),
            path: ".".into(),
            file_type: None,
            recursive: None,
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    show_glob(
        "glob `**/*` with hidden=true, include_node_modules=true",
        glob(GlobOptions {
            pattern: "**/*".into(),
            path: ".".into(),
            file_type: None,
            recursive: None,
            hidden: Some(true),
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: Some(true),
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 4. glob_util: pattern normalization ─────────────────────────────────
    println!("== 4. build_glob_pattern ==");
    for (pattern, recursive) in [("*.rs", true), ("src/*.rs", true), ("*.{ts,tsx", true)] {
        println!(
            "  {pattern:>12} recursive={recursive:<5} -> {}",
            build_glob_pattern(pattern, recursive)
        );
    }
    println!();

    // ── 5. grep: content search with a glob filter ──────────────────────────
    show_grep(
        "grep `fn ` in *.rs",
        grep(GrepOptions {
            pattern: "fn ".into(),
            path: ".".into(),
            glob: Some("*.rs".into()),
            r#type: None,
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: None,
            context: None,
            max_columns: None,
            mode: None,
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 6. grep: count mode + type filter ───────────────────────────────────
    // Count mode reports one row per matched file with a per-file count;
    // `r#type: "rs"` restricts to Rust files.
    show_grep(
        "grep count mode, type=rs",
        grep(GrepOptions {
            pattern: "fn".into(),
            path: ".".into(),
            glob: None,
            r#type: Some("rs".into()),
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: None,
            context: None,
            max_columns: None,
            mode: Some(GrepOutputMode::Count),
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 7. grep: context lines + line truncation ────────────────────────────
    write(
        &project.join("notes.txt"),
        "alpha\nneedle one\nbeta\ngamma\nneedle two\n",
    );
    show_grep(
        "grep `needle` with context_after=1, max_columns=6",
        grep(GrepOptions {
            pattern: "needle".into(),
            path: "notes.txt".into(),
            glob: None,
            r#type: None,
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: Some(1),
            context: None,
            max_columns: Some(6),
            mode: None,
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    // ── 8. The scan cache: reuse, then invalidate ───────────────────────────
    println!("== 8. scan cache ==");
    let opts = || GlobOptions {
        pattern: "*.rs".into(),
        path: ".".into(),
        file_type: None,
        recursive: None,
        hidden: None,
        max_results: None,
        gitignore: None,
        cache: Some(true),
        sort_by_mtime: None,
        include_node_modules: None,
        timeout_ms: None,
        on_match: None,
    };
    let first = glob(opts()).unwrap();
    write(&project.join("src/new.rs"), "fn new() {}\n");
    // Within the TTL (1s), the cached scan is served: new.rs is NOT visible.
    let second = glob(opts()).unwrap();
    println!(
        "  after creating src/new.rs, first scan found {} files",
        first.total_matches
    );
    println!(
        "  cached scan (same TTL window) found {} files",
        second.total_matches
    );
    // Invalidate the cache and the next scan sees the new file.
    cosh_sdk::find::fs_cache::invalidate_all();
    let third = glob(opts()).unwrap();
    println!(
        "  after invalidate_all, scan found {} files",
        third.total_matches
    );
    println!();

    // ── 9. Cancellation: an AbortToken stops a running search ───────────────
    let ct = CancelToken::new(None);
    let abort: AbortToken = ct.abort_token();
    abort.abort(AbortReason::User);
    // glob() builds its own token from timeout_ms, so we demonstrate the
    // token contract directly: a pre-aborted token fails the heartbeat.
    println!("== 9. cancellation ==");
    println!("  aborted() = {}", ct.aborted());
    match ct.heartbeat_reason() {
        Ok(()) => println!("  heartbeat: Ok"),
        Err(reason) => println!("  heartbeat after abort: Err({reason})"),
    }
    println!();

    // ── 10. Error paths ─────────────────────────────────────────────────────
    show_glob(
        "glob missing path",
        glob(GlobOptions {
            pattern: "*.rs".into(),
            path: "does-not-exist".into(),
            file_type: None,
            recursive: None,
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    show_glob(
        "glob on a file (not a dir)",
        glob(GlobOptions {
            pattern: "*".into(),
            path: "notes.txt".into(),
            file_type: None,
            recursive: None,
            hidden: None,
            max_results: None,
            gitignore: None,
            cache: None,
            sort_by_mtime: None,
            include_node_modules: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    // A non-quantifier brace is tolerated (escaped to a literal), so `a{2`
    // simply matches nothing here — the pattern never errors.
    show_grep(
        "grep `a{2` (brace tolerance)",
        grep(GrepOptions {
            pattern: "a{2".into(),
            path: "notes.txt".into(),
            glob: None,
            r#type: None,
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: None,
            context: None,
            max_columns: None,
            mode: None,
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    // An unclosed character class has no tolerance path — genuine error.
    show_grep(
        "grep `[abc` (genuine regex error)",
        grep(GrepOptions {
            pattern: "[abc".into(),
            path: "notes.txt".into(),
            glob: None,
            r#type: None,
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: None,
            context: None,
            max_columns: None,
            mode: None,
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );
    show_grep(
        "grep missing path",
        grep(GrepOptions {
            pattern: "needle".into(),
            path: "nope.txt".into(),
            glob: None,
            r#type: None,
            ignore_case: None,
            multiline: None,
            hidden: None,
            gitignore: None,
            cache: None,
            max_count: None,
            offset: None,
            context_before: None,
            context_after: None,
            context: None,
            max_columns: None,
            mode: None,
            max_count_per_file: None,
            timeout_ms: None,
            on_match: None,
        }),
    );

    let _ = &project;
    std::env::set_current_dir("/").ok();
}
