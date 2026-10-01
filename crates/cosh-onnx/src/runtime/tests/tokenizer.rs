//! Ported tests: `tests/test_tokenizer_cache.py` from the laya repository.
//! `_fix_tokenizer_config` must not write through a HuggingFace snapshot
//! symlink: snapshots link into a shared `blobs/` store, so writing through
//! the link truncates the shared blob. The tests build that exact layout and
//! assert the blob survives, the snapshot is swapped in atomically, and no
//! temporary file is left behind.

use serde_json::{Value, json};

use crate::runtime::tokenizer::{fix_tokenizer_config, fix_tokenizer_config_inner};

const TOKENIZER_CACHE_ORIGINAL: &str =
    r#"{"tokenizer_class": "TokenizersBackend", "backend": "x", "is_local": true}"#;

/// Build `<root>/models--x/blobs/<blob>` plus a symlink at
/// `<root>/models--x/snapshots/<rev>/tokenizer/tokenizer_config.json`.
#[cfg(unix)]
fn build_snapshot_layout(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let blob_dir = root.join("models--x").join("blobs");
    let snap_dir = root
        .join("models--x")
        .join("snapshots")
        .join("rev1")
        .join("tokenizer");
    std::fs::create_dir_all(&blob_dir).unwrap();
    std::fs::create_dir_all(&snap_dir).unwrap();
    let blob = blob_dir.join("deadbeef");
    std::fs::write(&blob, TOKENIZER_CACHE_ORIGINAL).unwrap();
    let link = snap_dir.join("tokenizer_config.json");
    std::os::unix::fs::symlink(&blob, &link).unwrap();
    (blob, link)
}

/// The snapshot dir is the parent of the `tokenizer` directory the upstream
/// test passes: `os.path.dirname(snap_dir)`.
#[cfg(unix)]
fn snap_parent(link: &std::path::Path) -> std::path::PathBuf {
    link.parent().unwrap().parent().unwrap().to_path_buf()
}

#[cfg(unix)]
#[test]
fn tokenizer_config_patch_keeps_the_shared_blob_intact() {
    let root = std::env::temp_dir().join(format!("cosh-onnx-tokcache-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let (blob, link) = build_snapshot_layout(&root);
    // setup/snapshot file is a symlink
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

    fix_tokenizer_config(&snap_parent(&link));

    // blob/is untouched
    assert_eq!(
        std::fs::read_to_string(&blob).unwrap(),
        TOKENIZER_CACHE_ORIGINAL
    );
    // snapshot/is now a regular file
    assert!(!link.symlink_metadata().unwrap().file_type().is_symlink());
    // snapshot/carries the patch
    let patched = std::fs::read_to_string(&link).unwrap();
    assert!(
        patched.contains("\"PreTrainedTokenizerFast\""),
        "{}",
        patched
    );
    // snapshot/drops backend+is_local as intended
    assert!(!patched.contains("\"backend\": \"x\""), "{}", patched);
    // snapshot/no temporary file left behind
    let leftovers: Vec<_> = std::fs::read_dir(link.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".tokenizer_config.")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "{:?}",
        leftovers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
    // snapshot/preserves the file mode
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let blob_mode = std::fs::metadata(&blob).unwrap().permissions().mode() & 0o777;
        let link_mode = std::fs::metadata(&link).unwrap().permissions().mode() & 0o777;
        assert_eq!(link_mode, blob_mode);
    }

    // a second call is a no-op and must not disturb anything either
    fix_tokenizer_config(&snap_parent(&link));
    // idempotent/blob still untouched
    assert_eq!(
        std::fs::read_to_string(&blob).unwrap(),
        TOKENIZER_CACHE_ORIGINAL
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn tokenizer_config_bad_json_warns_instead_of_raising() {
    let root = std::env::temp_dir().join(format!("cosh-onnx-tokbad-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let bad_dir = root.join("bad").join("tokenizer");
    std::fs::create_dir_all(&bad_dir).unwrap();
    let bad_file = bad_dir.join("tokenizer_config.json");
    std::fs::write(&bad_file, "{ not json").unwrap();

    fix_tokenizer_config(bad_dir.parent().unwrap());

    // bad-json/file left as written
    assert_eq!(std::fs::read_to_string(&bad_file).unwrap(), "{ not json");

    std::fs::remove_dir_all(&root).ok();
}

/// The replace-failure case from the upstream test, as far as it is reachable
/// here: the temp-file creation failure must clean up and leave the original
/// intact. The upstream test patches `os.replace` to raise *after* a
/// successful write; that exact branch (persist error -> temp unlink) needs
/// the write to succeed and the rename to fail, which cannot be forced
/// portably for a plain file target (an EISDIR target fails at
/// `read_to_string`, before the temp file exists). The persist-error cleanup
/// itself is guaranteed by `tempfile`'s `PersistError: Drop` unlink, and the
/// success path is exercised by the other tokenizer-config tests. Running as
/// root would also pass these assertions vacuously (mode 0500 does not block
/// root writes).
#[cfg(unix)]
#[test]
fn tokenizer_config_replace_failure_cleans_up() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("cosh-onnx-tokfail-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let (blob, _) = build_snapshot_layout(&root);
    let fail_dir = root.join("fail").join("tokenizer");
    std::fs::create_dir_all(&fail_dir).unwrap();
    let fail_link = fail_dir.join("tokenizer_config.json");
    std::os::unix::fs::symlink(&blob, &fail_link).unwrap();

    // Make the tokenizer directory read-only so creating the temp file fails;
    // the temp file must never be left behind and the original symlink must
    // stay intact.
    let dir_meta = std::fs::metadata(&fail_dir).unwrap();
    let original_mode = dir_meta.permissions().mode();
    std::fs::set_permissions(&fail_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    // The warning path is exercised by running the patch through the inner
    // result so the assertion can check the failure directly.
    let outcome = fix_tokenizer_config_inner(&fail_link);

    // restore permissions first so the cleanup assertions can see everything
    std::fs::set_permissions(&fail_dir, std::fs::Permissions::from_mode(original_mode)).unwrap();

    assert!(outcome.is_err(), "replace failure must surface as an error");
    // replace-failure/no temp file left behind
    let leftovers: Vec<_> = std::fs::read_dir(&fail_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".tokenizer_config.")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "{:?}",
        leftovers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
    // replace-failure/original symlink intact
    assert!(
        fail_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // replace-failure/blob untouched
    assert_eq!(
        std::fs::read_to_string(&blob).unwrap(),
        TOKENIZER_CACHE_ORIGINAL
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The direct-patch shape from the upstream flow (a plain directory, not a
/// snapshot): the fields are patched in place and the class is set.
#[test]
fn tokenizer_config_plain_directory_is_patched() {
    let root = std::env::temp_dir().join(format!("cosh-onnx-tokplain-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let dir = root.join("tokenizer");
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("tokenizer_config.json");
    std::fs::write(
        &cfg,
        r#"{"tokenizer_class": "TokenizersBackend", "extra_special_tokens": ["a", "b"]}"#,
    )
    .unwrap();

    fix_tokenizer_config(&root);

    let patched: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(patched["tokenizer_class"], json!("PreTrainedTokenizerFast"));
    assert!(patched.get("backend").is_none());
    assert!(patched.get("is_local").is_none());
    // the list-valued extra_special_tokens becomes a mapping
    let extra = patched["extra_special_tokens"].as_object().unwrap();
    assert_eq!(extra.len(), 2);
    assert_eq!(extra["extra_0"], json!("a"));
    assert_eq!(extra["extra_1"], json!("b"));

    std::fs::remove_dir_all(&root).ok();
}
