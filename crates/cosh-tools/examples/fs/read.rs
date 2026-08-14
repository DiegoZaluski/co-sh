//! Demonstrate the four `fs::read` modes: whole file, syntactic block by line,
//! symbol lookup, and exact line ranges.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example fs-read
//! ```

use cosh_tools::fs::{Fs, Target};

#[tokio::main]
async fn main() {
    // Build a small scratch project in the OS temp directory. `Fs::cwd` sets
    // the project root that all path guards validate against. The directory is
    // wiped first so re-runs are deterministic.
    let project = std::env::temp_dir().join("cosh-fs-read-example");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();

    let source = "\
// A tiny greeting program used to demonstrate fs::read.
fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

fn main() {
    let message = greet(\"world\");
    println!(\"{message}\");
}
";
    let file = project.join("greeter.rs");
    std::fs::write(&file, source).unwrap();

    let fs = Fs::new().cwd(&project);

    // 1. Whole-file read: just a path, nothing else.
    let results = fs
        .read(vec![Target {
            path: "greeter.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;

    let whole = &results[0];
    println!("== whole-file read ==");
    println!("header:   {}", whole.header);
    println!("file_hash: {}", whole.file_hash);
    println!("warnings: {:?}", whole.warnings);
    println!("content:\n{}", whole.content);

    // 2. Read by line: the syntactic block containing line 6 (`fn main`).
    let results = fs
        .read(vec![Target {
            path: "greeter.rs".to_string(),
            line: Some(6),
            symbol: None,
            line_range: None,
        }])
        .await;

    println!("\n== block containing line 6 ==");
    println!("{}", results[0].content);

    // 3. Read by symbol: the definition of `greet`.
    let results = fs
        .read(vec![Target {
            path: "greeter.rs".to_string(),
            line: None,
            symbol: Some("greet".to_string()),
            line_range: None,
        }])
        .await;

    println!("\n== symbol `greet` ==");
    println!("{}", results[0].content);

    // 4. Read by line_range: exact lines, no AST resolution.
    let results = fs
        .read(vec![Target {
            path: "greeter.rs".to_string(),
            line: None,
            symbol: None,
            line_range: Some("5-7".to_string()),
        }])
        .await;

    println!("\n== exact line range 5-7 ==");
    println!("{}", results[0].content);

    // 5. Errors are per-target warnings, never batch aborts: reading a file
    //    that does not exist yields a result with a warning.
    let results = fs
        .read(vec![Target {
            path: "missing.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;

    println!("\n== missing file ==");
    println!("warnings: {:?}", results[0].warnings);
}