//! Demonstrate the `hashline` edit engine end to end: content hashing,
//! parsing the patch language, applying edits in memory, snapshot-backed
//! stale-tag recovery, mismatch rejection, and a real disk apply through
//! the `Patcher`.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example hashline-run
//! ```

use cosh_sdk::hashline::format::{
    compute_file_hash, format_hashline_header, format_numbered_lines, format_replace_header,
};
use cosh_sdk::hashline::fs::{DiskFilesystem, Filesystem, InMemoryFilesystem};
use cosh_sdk::hashline::input::{Patch, PatchSection};
use cosh_sdk::hashline::types::SplitOptions;
use cosh_sdk::hashline::patcher::Patcher;
use cosh_sdk::hashline::snapshots::{
    InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore,
};

const LIB_RS: &str = "\
fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    let n = double(4);
    println!(\"doubled: {n}\");
}
";

#[tokio::main]
async fn main() {
    // ── 1. Content hashing: the tag binds a patch to a file version ────────
    println!("== 1. compute_file_hash ==");
    let tag = compute_file_hash(LIB_RS);
    println!("  content tag: {tag}");
    println!(
        "  byte-identical text mints the same tag: {}",
        compute_file_hash(LIB_RS) == tag
    );
    println!(
        "  CRLF endings do not invalidate it: {}",
        compute_file_hash(&LIB_RS.replace('\n', "\r\n")) == tag
    );
    println!("  header: {}", format_hashline_header("src/lib.rs", &tag));
    println!();

    // ── 2. Parsing the patch language ───────────────────────────────────────
    println!("== 2. Patch::parse ==");
    let patch_input = format!(
        "\
¶src/lib.rs#{tag}
{replace}:
+fn double(x: i32) -> i32 {{
+    x * 2
+}}

¶src/main.rs#9F3E
insert tail:
+    println!(\"doubled: {{n}}\");
",
        replace = format_replace_header(1, 1)
    );
    let patch = Patch::parse(&patch_input, &SplitOptions::default()).unwrap();
    for section in &patch.sections {
        println!(
            "  section {} (hash {:?}): {} edits, {} warnings",
            section.path,
            section.file_hash,
            section.edits().len(),
            section.warnings().len()
        );
    }
    println!();

    // ── 3. In-memory apply: pure, no filesystem ─────────────────────────────
    println!("== 3. apply_edits on a text body ==");
    let single: PatchSection = Patch::parse_single(
        &format!(
            "¶src/lib.rs#{tag}\nreplace 2..3:\n+    x * 2 + 1\n+}}"
        ),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = single.apply_to(LIB_RS, None);
    println!(
        "  first changed line: {:?} (warnings: {})",
        result.first_changed_line,
        result.warnings.len()
    );
    for line in result.text.lines() {
        println!("    {line}");
    }
    println!();

    // ── 4. The snapshot store: recording versions, fusing identical reads ───
    println!("== 4. InMemorySnapshotStore ==");
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag1 = store.record("src/lib.rs", LIB_RS);
    let tag2 = store.record("src/lib.rs", LIB_RS); // same bytes → same tag
    println!("  record #1 -> {tag1}, record #2 -> {tag2} (fused: {})", tag1 == tag2);
    let head = store.head("src/lib.rs").unwrap();
    println!("  head text has {} lines, recorded at {}", head.text.lines().count(), head.recorded_at);
    let by_tag = store.by_hash("src/lib.rs", &tag1).unwrap();
    println!("  by_hash({tag1}) resolves to {} lines", by_tag.text.lines().count());
    println!();

    // ── 5. Tag validation + drift recovery ─────────────────────────────────
    println!("== 5. stale tag -> 3-way-merge recovery ==");
    // Read the file, then someone edits it externally before we apply.
    let fs = InMemoryFilesystem::new([("src/lib.rs".to_string(), LIB_RS.to_string())]);
    // Drift: an external tool appends a helper at the tail of the file — far
    // outside the hunk's context window, so the 3-way merge applies cleanly.
    let drifted = format!("{LIB_RS}fn helper() -> i32 {{ 7 }}\n");
    fs.set("src/lib.rs", drifted.clone());

    // Record the *read* version (mints the tag the patch will carry)…
    let read_tag = store.record("src/lib.rs", LIB_RS);
    // …then author a patch against that read, but apply it to the drifted file.
    let stale_patch = Patch::parse(
        &format!(
            "¶src/lib.rs#{read_tag}\ninsert after 5:\n+    // todo: handle negative inputs"
        ),
        &SplitOptions::default(),
    )
    .unwrap();
    let mut patcher = Patcher::new(fs, store, None);
    let outcome = patcher.apply(&stale_patch).await.unwrap();
    let section = &outcome.sections[0];
    println!("  op: {:?}, new tag: {}", section.op, section.file_hash);
    for w in &section.warnings {
        println!("  warning: {w}");
    }
    println!("  merged content (external helper survives):");
    for line in section.after.lines() {
        println!("    {line}");
    }
    println!();

    // ── 6. Unrecognized tag -> hard mismatch ────────────────────────────────
    println!("== 6. unknown tag -> MismatchError ==");
    let fs2 = InMemoryFilesystem::new([("src/lib.rs".to_string(), LIB_RS.to_string())]);
    let mut patcher2 = Patcher::new(fs2, InMemorySnapshotStore::new(&Default::default()), None);
    // Tag was never recorded in this store — like a hash from a prior session.
    let bad_patch = Patch::parse(
        "¶src/lib.rs#BEEF\nreplace 3..4:\n+    let n = double(4); // updated",
        &SplitOptions::default(),
    )
    .unwrap();
    match patcher2.apply(&bad_patch).await {
        Err(err) => println!("  rejected: {err}"),
        Ok(_) => println!("  (unexpected success)"),
    }
    println!();

    // ── 7. End to end on disk ───────────────────────────────────────────────
    println!("== 7. Patcher + DiskFilesystem ==");
    let dir = std::env::temp_dir().join("cosh-hashline-example");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lib.rs");
    std::fs::write(&path, LIB_RS).unwrap();
    let path_str = path.to_string_lossy();

    // DiskFilesystem canonicalizes to absolute paths, so snapshot keys and
    // the patch path must agree: author the patch with the absolute path.
    let disk_fs = DiskFilesystem::new();
    let mut disk_store = InMemorySnapshotStore::new(&Default::default());
    let abs = disk_fs.canonical_path(&path_str).await;
    let disk_tag = disk_store.record(&abs, LIB_RS);

    let mut disk_patcher = Patcher::new(disk_fs, disk_store, None);
    let disk_patch = Patch::parse(
        &format!(
            "¶{path_str}#{disk_tag}\ninsert after 4:\n+// edited via hashline on disk"
        ),
        &SplitOptions::default(),
    )
    .unwrap();
    let disk_result = disk_patcher.apply(&disk_patch).await.unwrap();
    let section = &disk_result.sections[0];
    println!("  op: {:?}, header: {}", section.op, section.header);
    let on_disk = std::fs::read_to_string(&path).unwrap();
    println!(
        "  on-disk content matches the reported `after`: {}",
        on_disk == section.after
    );
    println!("  numbered display of the result:");
    print!("{}", format_numbered_lines(&section.after, 1));
    let _ = std::fs::remove_dir_all(&dir);
}
