//! Demonstrate `fs::edit` with the AST structural engine: matching tree-sitter
//! patterns (`pat`) and rewriting each match to a template (`out`), including
//! directory-wide rewrites and metavariable capture.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example fs-ast-edit
//! ```

use cosh_tools::fs::{Fs, Target};

#[tokio::main]
async fn main() {
    let project = std::env::temp_dir().join("cosh-fs-ast-edit-example");
    let _ = std::fs::remove_dir_all(&project);
    let src = project.join("src");
    std::fs::create_dir_all(src.join("util")).unwrap();

    // Two files under `src`, one in a nested directory (all sharing the same
    // `double` helper and the same logging idiom), and one file that matches
    // none of the patterns — it will come back with a "no AST matches found"
    // warning rather than aborting the batch.
    let file_a = "\
fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    let n = double(4);
    println!(\"doubled: {n}\");
}
";
    std::fs::write(src.join("lib.rs"), file_a).unwrap();
    std::fs::write(src.join("main.rs"), file_a).unwrap();
    std::fs::write(src.join("util/helper.rs"), file_a).unwrap();
    std::fs::write(src.join("util/unrelated.rs"), "pub fn keep(x: u32) -> u32 {\n    x\n}\n").unwrap();

    let fs = Fs::new().cwd(&project);

    // A single edit call rewrites every matched file under `src`. The `ast`
    // argument carries ops (applied in order per file) and the paths to search
    // (files, directories, or globs — here a directory, walked recursively).
    //
    // op 1: structural rename with metavariables. `$A` captures exactly one
    //       node (the parameter), `$$$BODY` captures zero or more nodes (the
    //       function body). `out` re-inserts the captured nodes.
    // op 2: a whole-literal rewrite. A metavariable captures a whole node, so
    //       the pattern must include the quotes around the string literal.
    // op 3: rename the *call sites* of `double` too.
    let args = serde_json::json!({
        "ast": {
            "ops": [
                {
                    "pat": "fn double($A) -> i32 { $$$BODY }",
                    "out": "fn triple($A) -> i32 { $$$BODY }",
                },
                {
                    "pat": "println!(\"$M\")",
                    "out": "println!(\"[$M]\")",
                },
                {
                    "pat": "double($A)",
                    "out": "triple($A)",
                },
            ],
            "paths": ["src"],
        }
    });

    let results = fs.edit(args).await.expect("AST edit should succeed");

    for r in &results {
        println!("== {} ==", r.path);
        println!("new header:      {}", r.header);
        println!("first_changed_line: {:?}", r.first_changed_line);
        println!("warnings:        {:?}", r.warnings);
        if let Some(diff) = &r.diff {
            println!("diff:\n{diff}");
        }
        println!();
    }

    // The result list only contains files that changed or produced warnings:
    // the three rewritten files and the unrelated file with its no-match
    // warning (diff: None). Verify the rewrite on disk:
    let rewritten = std::fs::read_to_string(project.join("src/lib.rs")).unwrap();
    println!("== rewritten src/lib.rs ==\n{rewritten}");

    // Read the file back through `fs.read` to show the fresh hashline anchor.
    let results = fs
        .read(vec![Target {
            path: "src/lib.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;
    println!("== read back anchor ==");
    println!("{}", results[0].header);
}