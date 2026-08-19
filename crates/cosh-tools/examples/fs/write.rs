//! Demonstrate `fs::write`: creating files inside the project root, how paths
//! outside the root are refused, and how the allowlist grants access.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example fs-write
//! ```

use cosh_tools::fs::{Fs, TargetFile};

#[tokio::main]
async fn main() {
    let project = std::env::temp_dir().join("cosh-fs-write-example");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let outside = std::env::temp_dir().join("cosh-fs-write-outside");
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();

    // 1. A fresh `Fs` scoped to the project root: everything under the root is
    //    writable by default. Note that `write` does NOT create parent
    //    directories — create them first.
    let fs = Fs::new().cwd(&project);
    std::fs::create_dir_all(project.join("scripts")).unwrap();

    let results = fs
        .write(vec![
            TargetFile {
                path: "notes.txt".to_string(),
                text: "first line\nsecond line\n".to_string(),
             file_hash: None, },
            TargetFile {
                path: "scripts/hello.sh".to_string(),
                text: "#!/bin/sh\necho hello\n".to_string(),
             file_hash: None, },
        ])
        .await
        .unwrap();

    for r in &results {
        println!("== wrote {} ==", r.path);
        println!("header:   {}", r.header);
        println!("warnings: {:?}", r.warnings);
    }

    // 2. A path outside the root is denied per-file (batch continues). The
    //    result carries a warning instead of the write going through.
    let results = fs
        .write(vec![TargetFile {
            path: outside.join("secret.txt").to_string_lossy().to_string(),
            text: "do not touch\n".to_string(),
         file_hash: None, }])
        .await
        .unwrap();

    println!("\n== write outside the root ==");
    println!("warnings: {:?}", results[0].warnings);

    // 3. The same path becomes writable once it is on the allowlist. The
    //    allowlist grants access to specific paths OUTSIDE the root; entries
    //    are matched exactly, so the file path itself must be listed.
    let fs = Fs::new()
        .cwd(&project)
        .allowlist([outside.join("secret.txt")]);

    let results = fs
        .write(vec![TargetFile {
            path: outside.join("secret.txt").to_string_lossy().to_string(),
            text: "allowed now\n".to_string(),
         file_hash: None, }])
        .await
        .unwrap();

    println!("\n== write outside the root, allowlisted ==");
    println!("header:   {}", results[0].header);
    println!("warnings: {:?}", results[0].warnings);

    // 4. Auto-generated files are refused: a file whose header declares itself
    //    generated is never overwritten. Per-file warning, batch continues.
    let generated = project.join("generated.rs");
    std::fs::write(
        &generated,
        "// DO NOT EDIT. This file is auto-generated.\n\npub fn generated() {}\n",
    )
    .unwrap();

    let results = fs
        .write(vec![TargetFile {
            path: "generated.rs".to_string(),
            text: "// my replacement\n".to_string(),
         file_hash: None, }])
        .await
        .unwrap();

    println!("\n== overwriting a generated file ==");
    println!("warnings: {:?}", results[0].warnings);

    let final_text = std::fs::read_to_string(project.join("generated.rs")).unwrap();
    println!(
        "file still contains: {}",
        final_text.lines().next().unwrap()
    );
}
