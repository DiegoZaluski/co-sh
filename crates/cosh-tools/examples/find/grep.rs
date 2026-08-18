//! Demonstrate `find::grep`: searching file contents, the file-window
//! pagination bound (`skip`), single-file scopes, and — the payoff — editing a
//! matched file directly via the hashline anchor carried in the result.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example find-grep
//! ```

use cosh_tools::find::{Find, Grep};
use cosh_tools::fs::{Fs, Target};

fn show(label: &str, out: &cosh_tools::find::GrepOutput) {
    println!("== {label} ==");
    for m in &out.matches {
        println!("  {}:{}\t{}", m.path, m.line_number, m.line.trim_end());
    }
    println!(
        "  total={} files_with={} files_searched={} useless={:?} timed_out={:?}",
        out.total_matches, out.files_with_matches, out.files_searched, out.useless, out.timed_out
    );
    if let Some(note) = &out.note {
        println!("  note: {note}");
    }
    println!();
}

#[tokio::main]
async fn main() {
    // A small scratch project, searched with `Find` scoped to its root.
    let project = std::env::temp_dir().join("cosh-find-grep-example");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(project.join("src/util")).unwrap();

    let src = "\
fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    let n = double(4);
    println!(\"doubled: {n}\");
}
";
    std::fs::write(project.join("src/lib.rs"), src).unwrap();
    std::fs::write(project.join("src/main.rs"), src).unwrap();
    std::fs::write(
        project.join("src/util/helper.rs"),
        "fn helper() -> i32 { 1 }\n",
    )
    .unwrap();

    let find = Find::new().cwd(&project);

    // 1. Directory search: paths are relative to the searched root, matches
    //    carry whole-file hashline anchors in `files`.
    let out = find
        .grep("fn double", "src")
        .unwrap_or_else(|_| panic!("grep should not fail"));
    show("directory search `fn double` under project root", &out);

    // 2. The file window: more than 20 matching files surface the first 20
    //    with `file_limit_reached` and a note telling you to `skip` ahead.
    let paging = project.join("paging");
    std::fs::create_dir_all(&paging).unwrap();
    for i in 0..22 {
        std::fs::write(paging.join(format!("p{i:02}.rs")), "fn page_test() {}\n").unwrap();
    }
    let page1 = find.grep("fn page_test", "paging").unwrap();
    println!("== file window: first page ==");
    println!(
        "  matches_shown={} files_with={} file_limit_reached={} note={:?}",
        page1.matches.len(),
        page1.files_with_matches,
        page1.file_limit_reached,
        page1.note,
    );
    let page2 = find
        .grep_skipping("fn page_test", "paging", Some(20))
        .unwrap();
    println!("== file window: skip=20 ==");
    println!(
        "  matches_shown={} file_limit_reached={} first_match={}:{}",
        page2.matches.len(),
        page2.file_limit_reached,
        page2
            .matches
            .first()
            .map(|m| m.path.as_str())
            .unwrap_or("-"),
        page2.matches.first().map(|m| m.line_number).unwrap_or(0),
    );
    println!();

    // 3. No matches is `useless: true`, not an error.
    show(
        "no match",
        &find
            .grep("zzz_nothing", &project.join("src").to_string_lossy())
            .unwrap(),
    );

    // 3. Single-file scope: absolute paths, and `line_range` is allowed.
    show(
        "single file + line_range 1-2",
        &find
            .grep_with(
                "fn ",
                Some(project.join("src/lib.rs").to_string_lossy().into()),
                None,
                None,
                Some("1-2".into()),
            )
            .unwrap(),
    );

    // 4. Context lines via the free function + `Grep` config struct.
    show(
        "context_after=1",
        &cosh_tools::find::grep(
            &Grep {
                context_after: Some(1),
                ..Default::default()
            },
            "let n",
            &project.join("src").to_string_lossy(),
        )
        .unwrap(),
    );

    // 5. The payoff: edit a matched file directly from the grep anchor, no
    //    re-read. `files[0]` is the first matched file with a hashline tag.
    let out = find
        .grep("fn double", &project.join("src").to_string_lossy())
        .unwrap();
    let anchor = &out.files[0];
    let abs_path = anchor
        .header
        .trim_start_matches('¶') // strip the `¶` file marker (multi-byte!)
        .split('#')
        .next()
        .unwrap();

    let fs = Fs::new().cwd(&project);
    let results = fs
        .edit(serde_json::json!({
            "targets": [{
                "path": abs_path,
                "file_hash": anchor.file_hash,
                "ops": "replace 1..3:\n+fn triple(x: i32) -> i32 {\n+    x * 3\n+}",
            }]
        }))
        .await
        .expect("edit via grep anchor should succeed");

    println!("== edit from grep anchor ==");
    println!("  matched: {} ({})", anchor.path, anchor.header);
    println!("  new header: {}", results[0].header);
    if let Some(diff) = &results[0].diff {
        println!("  diff:\n{diff}");
    }

    // Confirm the edit landed, read back through `Fs`.
    let back = fs
        .read(vec![Target {
            path: abs_path.to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;
    println!("  after edit, disk content:");
    for line in back[0].content.lines().skip(1) {
        println!("    {line}");
    }
}
