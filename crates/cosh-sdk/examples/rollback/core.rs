//! Demonstrate the `rollback` engine: recording versions, restoring by
//! explicit hash, stepping back one version at a time, undoing a restore,
//! and the error paths.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example rollback
//! ```

use cosh_sdk::rollback::{RestoreInput, record, restore};

#[tokio::main]
async fn main() {
    let path = std::env::temp_dir()
        .join("cosh-rollback-example.txt")
        .to_string_lossy()
        .to_string();

    // ── 1. Record a version chain ───────────────────────────────────────────
    println!("== 1. record ==");
    let versions = ["v1: original\n", "v2: edited\n", "v3: final\n"];
    let mut hashes = Vec::new();
    for (i, v) in versions.iter().enumerate() {
        std::fs::write(&path, v).unwrap();
        let h = record(&path, v).unwrap();
        println!("  v{} -> {h}", i + 1);
        hashes.push(h);
    }
    // Recording the same content again fuses: same tag, no new entry.
    let again = record(&path, versions[2]).unwrap();
    println!(
        "  re-record v3 -> {again} (fused: {})",
        again == hashes[2]
    );
    println!();

    // ── 2. Restore by explicit hash ─────────────────────────────────────────
    println!("== 2. restore by hash (back to v1) ==");
    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(hashes[0].clone()),
    })
    .await
    .unwrap();
    println!(
        "  disk: {:?}  header: {}  replaced: {}  warning: {:?}",
        std::fs::read_to_string(&path).unwrap().trim(),
        out.header,
        out.replaced_hash,
        out.warning
    );
    println!();

    // ── 3. Undo the restore: roll forward again via replaced_hash ───────────
    println!("== 3. undo the restore (replaced_hash) ==");
    let out2 = restore(RestoreInput {
        path: path.clone(),
        hash: Some(out.replaced_hash.clone()),
    })
    .await
    .unwrap();
    println!(
        "  disk: {:?}  replaced: {}",
        std::fs::read_to_string(&path).unwrap().trim(),
        out2.replaced_hash
    );
    println!();

    // ── 4. Step back one version at a time (hash: None) ─────────────────────
    println!("== 4. hash: None walks back through history ==");
    let mut steps = Vec::new();
    for _ in 0..2 {
        restore(RestoreInput {
            path: path.clone(),
            hash: None,
        })
        .await
        .unwrap();
        steps.push(std::fs::read_to_string(&path).unwrap().trim().to_string());
    }
    println!("  stepped: {steps:?}");
    // The oldest retained version has no predecessor.
    let err = err_of(restore(RestoreInput {
        path: path.clone(),
        hash: None,
    })
    .await);
    println!("  at oldest: {err}");
    println!();

    // ── 5. Error paths ──────────────────────────────────────────────────────
    println!("== 5. errors ==");
    let unknown = err_of(restore(RestoreInput {
        path: path.clone(),
        hash: Some("DEAD".to_string()),
    })
    .await);
    println!("  unknown hash: {unknown}\n");

    let fresh = std::env::temp_dir()
        .join("cosh-rollback-never-read.txt")
        .to_string_lossy()
        .to_string();
    std::fs::write(&fresh, "content\n").unwrap();
    let err = err_of(restore(RestoreInput {
        path: fresh.clone(),
        hash: None,
    })
    .await);
    println!("  no history: {err}\n");
    let _ = std::fs::remove_file(&fresh);

    // ── 6. Recreate a deleted file ──────────────────────────────────────────
    println!("== 6. recreate a deleted file ==");
    std::fs::remove_file(&path).unwrap();
    let out = restore(RestoreInput {
        path: path.clone(),
        hash: Some(hashes[2].clone()),
    })
    .await
    .unwrap();
    println!(
        "  file exists: {}, disk: {:?}",
        std::path::Path::new(&path).exists(),
        std::fs::read_to_string(&path).unwrap().trim()
    );
    println!("  warning: {}", out.warning.unwrap_or_default());
    let _ = std::fs::remove_file(&path);
}

/// `RestoreOutput` has no `Debug`, so unwrap errors via a match.
fn err_of(result: Result<cosh_sdk::rollback::RestoreOutput, String>) -> String {
    match result {
        Err(e) => e,
        Ok(_) => "(unexpected success)".to_string(),
    }
}
