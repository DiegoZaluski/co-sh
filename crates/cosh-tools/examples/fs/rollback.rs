//! Demonstrate `fs::rollback`: stepping a file back through the session's
//! rollback history, and rolling forward again using the recorded hash.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example fs-rollback
//! ```

use cosh_tools::fs::{Fs, TargetFile};

#[tokio::main]
async fn main() {
    let project = std::env::temp_dir().join("cosh-fs-rollback-example");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();

    let fs = Fs::new().cwd(&project);
    let path = "config.txt";

    let read_disk = || std::fs::read_to_string(project.join(path)).unwrap();

    // Every `write` records the file in the session rollback history: the
    // previous content first, then the new content. After these two writes the
    // history for `config.txt` is [v1, v2] and the disk holds v2.
    let v1 = "api_key = \"old\"\nretries = 3\n";
    let v2 = "api_key = \"new\"\nretries = 5\n";

    fs.write(vec![TargetFile {
        path: path.to_string(),
        text: v1.to_string(),
    }])
    .await
    .unwrap();

    let write2 = fs
        .write(vec![TargetFile {
            path: path.to_string(),
            text: v2.to_string(),
        }])
        .await
        .unwrap();

    println!("after write 2, disk hash: {}", write2[0].file_hash);
    assert_eq!(read_disk(), v2);

    // Roll back with an EMPTY hash: restore the version immediately preceding
    // the current disk content — v1. The result's `replaced_hash` names what
    // was on disk before the restore (v2).
    let rollback1 = fs.rollback(path, "").await.expect("rollback succeeds");
    println!("\n== rollback to previous version ==");
    println!("restored hash: {}", rollback1.file_hash);
    println!("replaced hash: {}", rollback1.replaced_hash);
    println!("warning:       {:?}", rollback1.warning);
    assert_eq!(read_disk(), v1);

    // Undo the restore: pass `replaced_hash` back to return to v2. This is the
    // "undo of undo" navigation.
    let rollback2 = fs
        .rollback(path, &rollback1.replaced_hash)
        .await
        .expect("roll forward succeeds");
    println!("\n== roll forward again ==");
    println!("restored hash: {}", rollback2.file_hash);
    println!("replaced hash: {}", rollback2.replaced_hash);
    assert_eq!(read_disk(), v2);

    // The returned header is a fresh hashline anchor for the restored version.
    println!("\nanchor after roll-forward: {}", rollback2.header);
}