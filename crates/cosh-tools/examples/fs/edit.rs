//! Demonstrate `fs::edit` with the hashline replace engine: hash-anchored,
//! order-carries-intention edits with replace, delete, and insert operations.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example fs-edit
//! ```

use cosh_tools::fs::{Fs, Target};

#[tokio::main]
async fn main() {
    let project = std::env::temp_dir().join("cosh-fs-edit-example");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();

    let source = "\
mod math {
    pub fn double(x: i32) -> i32 {
        x * 2
    }
}

// this comment is going away
// so is this one

fn main() {
    let doubled = math::double(4);
    println!(\"{doubled}\");
}
";
    std::fs::write(project.join("main.rs"), source).unwrap();

    let fs = Fs::new().cwd(&project);

    // Read the file first: every edit is anchored on the hashline tag in the
    // response header (`¶path#TAG`). Copy the path and tag verbatim.
    let results = fs
        .read(vec![Target {
            path: "main.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;
    let header = &results[0].header;
    println!("read header: {header}");

    let (edit_path, file_hash) = split_header(header);
    println!("edit path:   {edit_path}");
    println!("file hash:   {file_hash}");

    // Build a multi-operation edit:
    //   - rename `double` to `triple` (replace the whole fn body block),
    //   - delete the two comment lines,
    //   - append a new function at the end of the file.
    let ops = "replace 2..4:\n\
         +    pub fn triple(x: i32) -> i32 {\n\
         +        x * 3\n\
         +    }\n\
         delete 7..8\n\
         insert tail:\n\
         +fn triple_main() {\n\
         +    let t = math::triple(2);\n\
         +    println!(\"{t}\");\n\
         +}\n";

    let args = serde_json::json!({
        "targets": [{
            "path": edit_path,
            "file_hash": file_hash,
            "ops": ops,
        }]
    });

    let results = fs.edit(args).await.expect("edit should succeed");
    let r = &results[0];
    println!("\n== edit result ==");
    println!("new header:  {}", r.header);
    println!("first_changed_line: {:?}", r.first_changed_line);
    println!("warnings:    {:?}", r.warnings);
    if let Some(diff) = &r.diff {
        println!("diff:\n{diff}");
    }

    // Failure semantics: targets are applied in order, and the batch aborts at
    // the first failure. Here the first target applies cleanly, the second
    // carries an anchor edit with a hash that is stale AND unknown to the
    // session history (recovery has no base to 3-way-merge against, so it must
    // reject), and the third is therefore skipped as a consequence — never
    // reported as an independent failure. Earlier results stay committed in
    // `EditBatchError::applied`.
    let source_b = "mod b { pub fn x() {}\n}\n";
    let source_c = "mod c { pub fn y() {}\n}\n";
    std::fs::write(project.join("b.rs"), source_b).unwrap();
    std::fs::write(project.join("c.rs"), source_c).unwrap();

    let read_b = fs
        .read(vec![Target {
            path: "b.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;
    let read_c = fs
        .read(vec![Target {
            path: "c.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
        }])
        .await;

    let (path_b, hash_b) = split_header(&read_b[0].header);
    let (path_c, _hash_c) = split_header(&read_c[0].header);

    let args = serde_json::json!({
        "targets": [
            {
                "path": path_b,
                "file_hash": hash_b,
                "ops": "insert tail:\n+// trailing comment\n",
            },
            {
                "path": path_c,
                "file_hash": "0000", // stale AND unknown: recovery cannot merge
                "ops": "replace 1..1:\n+// never lands\n",
            },
            {
                "path": path_c,
                "file_hash": "0000",
                "ops": "insert tail:\n+// never reached\n",
            },
        ]
    });

    match fs.edit(args).await {
        Ok(_) => println!("\n== unexpected success =="),
        Err(e) => println!("\n== batch failure ==\n{e}"),
    }
}

/// Split a hashline `¶path#TAG` header into its `(path, tag)` components.
fn split_header(header: &str) -> (&str, &str) {
    let body = header.strip_prefix('¶').expect("hashline prefix");
    body.rsplit_once('#').expect("path#hash")
}
