use super::super::types::{FsMetadata, TargetFile, WriteAllFile};
use super::super::write::write;
use std::path::Path;

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
