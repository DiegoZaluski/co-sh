use super::super::types::{FsMetadata, FsWrite, TargetFile};
use super::super::write::write;
use std::path::{Path, PathBuf};

fn meta() -> FsMetadata {
    FsMetadata {
        root: PathBuf::from("/home/inky/cosh"),
        allowlist: None,
        blocklist: None,
    }
}

#[tokio::test]
async fn write_creates_file_and_returns_hash_header() {
    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: "/home/inky/cosh/ftest.txt".to_string(),
                text: "hello world".to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].header.contains("¶/home/inky/cosh/ftest.txt#"));
    assert!(results[0].warnings.is_none());
    let _ = std::fs::remove_file("/home/inky/cosh/ftest.txt");
}

#[tokio::test]
async fn write_creates_multiple_files_in_single_call() {
    let result = write(
        meta(),
        FsWrite {
            targets: vec![
                TargetFile {
                    path: "/home/inky/cosh/ftest.txt".to_string(),
                    text: "hello world".to_string(),
                },
                TargetFile {
                    path: "/home/inky/cosh/ftst2.txt".to_string(),
                    text: "test".to_string(),
                },
            ],
        },
    )
    .await;
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
    let metadata = FsMetadata {
        root: Path::new("/home/inky/cosh").to_path_buf(),
        allowlist: None,
        blocklist: Some(vec![PathBuf::from("/home/inky/cosh/ftest.txt")]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: "/home/inky/cosh/ftest.txt".to_string(),
                text: "should not be written".to_string(),
            }],
        },
    )
    .await;
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
    let _ = std::fs::remove_file(path);

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: "".to_string(),
            }],
        },
    )
    .await;
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
    let metadata = FsMetadata {
        root: PathBuf::from("/home/inky/cosh"),
        allowlist: Some(vec![PathBuf::from(path)]),
        blocklist: None,
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: "outside root but explicitly allowed".to_string(),
            }],
        },
    )
    .await;
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
    let metadata = FsMetadata {
        root: PathBuf::from("/home/inky/cosh"),
        allowlist: Some(vec![PathBuf::from("/home/inky/cosh/ftest.txt")]),
        blocklist: Some(vec![PathBuf::from("/home/inky/cosh/ftest.txt")]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: "/home/inky/cosh/ftest.txt".to_string(),
                text: "should never be written".to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("both blocklist and allowlist"));
}

#[tokio::test]
async fn write_strips_hashline_prefixes_and_reports_warning() {
    let path = "/home/inky/cosh/cosh_test_strip_hashline.txt";
    let content = "[main.rs#ABCD]\n42: fn main() {\n43:     println!(\"hello\");\n44: }";

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: content.to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .as_deref()
            .unwrap()
            .contains("auto-stripped hashline")
    );

    let written = std::fs::read_to_string(path).unwrap();
    assert!(!written.contains("[main.rs#ABCD]"));
    assert!(!written.contains("42:"));
    assert!(written.contains("fn main()"));
    assert!(written.contains("println!"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn write_strips_hashline_prefixes_without_bracket_header() {
    let path = "/home/inky/cosh/cosh_test_strip_line_prefixes.txt";
    let content = "42: fn main() {\n43:     println!(\"hello\");\n44: }";

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: content.to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .as_deref()
            .unwrap()
            .contains("auto-stripped hashline")
    );

    let written = std::fs::read_to_string(path).unwrap();
    assert!(!written.contains("42:"));
    assert!(written.contains("fn main()"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn write_does_not_strip_normal_content() {
    let path = "/home/inky/cosh/cosh_test_no_strip.txt";
    let content = "fn main() {\n    println!(\"hello\");\n}";

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: content.to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());

    let written = std::fs::read_to_string(path).unwrap();
    assert_eq!(written, content);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
#[cfg(unix)]
async fn write_chmods_executable_for_shebang() {
    use std::os::unix::fs::PermissionsExt;

    let path = "/home/inky/cosh/cosh_test_shebang.sh";
    let content = "#!/usr/bin/env bash\necho hello";

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: content.to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .warnings
            .as_deref()
            .unwrap()
            .contains("made executable")
    );

    let meta = std::fs::metadata(path).unwrap();
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "file should have execute bits"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
#[cfg(unix)]
async fn write_does_not_chmod_without_shebang() {
    use std::os::unix::fs::PermissionsExt;

    let path = "/home/inky/cosh/cosh_test_no_shebang.txt";
    let content = "plain text file";

    let result = write(
        meta(),
        FsWrite {
            targets: vec![TargetFile {
                path: path.to_string(),
                text: content.to_string(),
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());

    let meta = std::fs::metadata(path).unwrap();
    let mode = meta.permissions().mode() & 0o111;
    assert_eq!(mode, 0, "file should NOT have execute bits");
    let _ = std::fs::remove_file(path);
}
