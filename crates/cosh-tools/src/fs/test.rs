use super::read::read;
use super::types::{EditFile, EditTarget, FsMetadata, ReadFile, Target, TargetFile, WriteAllFile};
// READ
#[tokio::test]
async fn test_function_search() {
    let config = ReadFile {
        read: vec![
            Target {
                path: "/home/inky/cosh/crates/cosh-sdk/src/hashline/tokenizer.rs",
                line: None,
                symbol: Some("tokenize"),
            },
            Target {
                path: "/home/inky/cosh/crates/cosh-sdk/src/hashline/types.rs",
                line: None,
                symbol: None,
            },
        ],
    };

    let results = read(config).await;
    assert!(!results.is_empty(), "expected at least one result");
    for r in &results {
        assert!(r.warnings.is_none(), "unexpected warning: {:?}", r.warnings);
        assert!(!r.file_hash.is_empty(), "file_hash should not be empty");
        assert!(!r.header.is_empty(), "header should not be empty");
        assert!(!r.content.is_empty(), "content should not be empty");
    }
}

#[tokio::test]
async fn test_line_block() {
    let config = ReadFile {
        read: vec![Target {
            path: "/home/inky/cosh/crates/cosh-sdk/src/hashline/tokenizer.rs",
            line: Some(5),
            symbol: None,
        }],
    };

    let results = read(config).await;
    assert!(!results.is_empty(), "expected one result");
    let result = &results[0];
    assert!(
        result.warnings.is_none(),
        "unexpected warning: {:?}",
        result.warnings
    );
    assert!(!result.file_hash.is_empty());
    assert!(!result.header.is_empty());
    assert!(!result.content.is_empty());
}

// ---
use super::write::write;
use std::path::Path;

// WRITE
#[tokio::test]
async fn write_creates_file_and_returns_hash_header() {
    let target = WriteAllFile {
        write: vec![TargetFile {
            path: "/home/inky/cosh/ftest.txt",
            text: "hello world",
        }],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: None,
        write_path_blocklist: None,
    };

    let result = write(target, metadata).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].header.contains("¶/home/inky/cosh/ftest.txt#"));
    assert!(results[0].warnings.is_none());
    let _ = std::fs::remove_file("/home/inky/cosh/ftest.txt");
}

#[tokio::test]
async fn write_creates_multiple_files_in_single_call() {
    let target = WriteAllFile {
        write: vec![
            TargetFile {
                path: "/home/inky/cosh/ftest.txt",
                text: "hello world",
            },
            TargetFile {
                path: "/home/inky/cosh/ftst2.txt",
                text: "test",
            },
        ],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: None,
        write_path_blocklist: None,
    };

    let result = write(target, metadata).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].header.contains("¶/home/inky/cosh/ftest.txt#"));
    assert!(results[0].warnings.is_none());
    assert!(results[1].header.contains("¶/home/inky/cosh/ftst2.txt#"));
    assert!(results[1].warnings.is_none());
    let _ = std::fs::remove_file("/home/inky/cosh/ftest.txt");
    let _ = std::fs::remove_file("/home/inky/cosh/ftst2.txt");
}

#[tokio::test]
async fn write_denied_when_path_is_in_blocklist() {
    let target = WriteAllFile {
        write: vec![TargetFile {
            path: "/home/inky/cosh/ftest.txt",
            text: "should not be written",
        }],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: None,
        write_path_blocklist: Some(vec![Path::new("/home/inky/cosh/ftest.txt")]),
    };

    let result = write(target, metadata).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .as_deref()
            .unwrap()
            .contains("write permission denied")
    );
}

#[tokio::test]
async fn write_reports_empty_text_inline_and_skips_file() {
    let path = "/home/inky/cosh/cosh_test_empty.txt";
    // guard: file must not exist before the test
    let _ = std::fs::remove_file(path);

    let target = WriteAllFile {
        write: vec![TargetFile { path, text: "" }],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: None,
        write_path_blocklist: None,
    };

    let result = write(target, metadata).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .as_deref()
            .unwrap()
            .contains("text is empty")
    );
    assert!(!Path::new(path).exists());
}

#[tokio::test]
async fn write_allowed_outside_root_when_path_in_allowlist() {
    let path = "/tmp/cosh_test_allowlist_write.txt";
    let target = WriteAllFile {
        write: vec![TargetFile {
            path,
            text: "outside root but explicitly allowed",
        }],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: Some(vec![Path::new(path)]),
        write_path_blocklist: None,
    };

    let result = write(target, metadata).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .header
            .contains("¶/tmp/cosh_test_allowlist_write.txt#")
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn write_errors_on_inconsistent_blocklist_and_allowlist() {
    let target = WriteAllFile {
        write: vec![TargetFile {
            path: "/home/inky/cosh/ftest.txt",
            text: "should never be written",
        }],
    };

    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: Some(vec![Path::new("/home/inky/cosh/ftest.txt")]),
        write_path_blocklist: Some(vec![Path::new("/home/inky/cosh/ftest.txt")]),
    };

    let result = write(target, metadata).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("both blocklist and allowlist"));
}

// ---

// EDIT
use super::edit::edit;
use cosh_sdk::hashline::format::compute_file_hash;

fn make_metadata() -> FsMetadata<'static> {
    FsMetadata {
        root: Path::new("/home/inky/cosh"),
        write_path_allowlist: None,
        write_path_blocklist: None,
    }
}

fn hash_file(path: &str) -> String {
    let content = std::fs::read_to_string(path).unwrap();
    compute_file_hash(&content)
}

#[tokio::test]
async fn edit_replaces_single_line_in_file() {
    let path = "/home/inky/cosh/cosh_test_edit_replace.txt";
    std::fs::write(path, "line1\nline2\nline3\n").unwrap();
    let file_hash = hash_file(path);

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops: "replace 2..2:\n+REPLACED",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].first_changed_line, Some(2));

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "line1\nREPLACED\nline3\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_replaces_multi_line_range() {
    let path = "/home/inky/cosh/cosh_test_edit_range.txt";
    std::fs::write(path, "a\nb\nc\nd\ne\n").unwrap();
    let file_hash = hash_file(path);

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops: "replace 2..4:\n+X\n+Y",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "a\nX\nY\ne\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_inserts_before_and_after_anchor() {
    let path = "/home/inky/cosh/cosh_test_edit_insert.txt";
    std::fs::write(path, "keep\nanchor\nkeep\n").unwrap();
    let file_hash = hash_file(path);

    let ops = "insert before 2:\n+BEFORE\ninsert after 2:\n+AFTER";

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops,
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "keep\nBEFORE\nanchor\nAFTER\nkeep\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_inserts_at_head_and_tail() {
    let path = "/home/inky/cosh/cosh_test_edit_headtail.txt";
    std::fs::write(path, "middle\n").unwrap();
    let file_hash = hash_file(path);

    let ops = "insert head:\n+HEAD\ninsert tail:\n+TAIL";

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops,
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "HEAD\nmiddle\nTAIL\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_deletes_range_of_lines() {
    let path = "/home/inky/cosh/cosh_test_edit_delete.txt";
    std::fs::write(path, "keep1\ndelete1\ndelete2\nkeep2\n").unwrap();
    let file_hash = hash_file(path);

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops: "delete 2..3",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "keep1\nkeep2\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_replaces_syntactic_block_in_rust_file() {
    let path = "/home/inky/cosh/cosh_test_edit_block.rs";
    std::fs::write(path, "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
    let file_hash = hash_file(path);

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: &file_hash,
            ops: "replace block 1:\n+fn main() {\n+    println!(\"hello\");\n+}",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());

    let content = std::fs::read_to_string(path).unwrap();
    assert_eq!(content, "fn main() {\n    println!(\"hello\");\n}\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_processes_multiple_files_in_single_call() {
    let path_a = "/home/inky/cosh/cosh_test_edit_multi_a.txt";
    let path_b = "/home/inky/cosh/cosh_test_edit_multi_b.txt";
    std::fs::write(path_a, "alpha\n").unwrap();
    std::fs::write(path_b, "beta\n").unwrap();
    let hash_a = hash_file(path_a);
    let hash_b = hash_file(path_b);

    let config = EditFile {
        edit: vec![
            EditTarget {
                path: path_a,
                file_hash: &hash_a,
                ops: "replace 1..1:\n+ALPHA",
            },
            EditTarget {
                path: path_b,
                file_hash: &hash_b,
                ops: "replace 1..1:\n+BETA",
            },
        ],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 2);

    assert_eq!(std::fs::read_to_string(path_a).unwrap(), "ALPHA\n");
    assert_eq!(std::fs::read_to_string(path_b).unwrap(), "BETA\n");
    let _ = std::fs::remove_file(path_a);
    let _ = std::fs::remove_file(path_b);
}

#[tokio::test]
async fn edit_returns_error_on_hash_mismatch() {
    let path = "/home/inky/cosh/cosh_test_edit_hashfail.txt";
    std::fs::write(path, "original\n").unwrap();

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: "BEEF",
            ops: "replace 1..1:\n+changed",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("hash mismatch"));
    // file content must be unchanged
    assert_eq!(std::fs::read_to_string(path).unwrap(), "original\n");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn edit_returns_error_when_file_not_found() {
    let path = "/home/inky/cosh/cosh_test_edit_nonexistent.txt";
    let _ = std::fs::remove_file(path); // ensure it doesn't exist

    let config = EditFile {
        edit: vec![EditTarget {
            path,
            file_hash: "BEEF",
            ops: "replace 1..1:\n+anything",
        }],
    };

    let result = edit(config, make_metadata()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}
