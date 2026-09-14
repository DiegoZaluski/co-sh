use super::super::types::{FsMetadata, FsWrite, TargetFile};
use super::super::write::write;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root: each test gets its own directory, so tests never
/// depend on (or touch) a developer's real project tree.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cosh_fs_write_{label}_{id}_{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Path of a fixture file inside the root.
    fn file(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn meta_root(root: &Path) -> FsMetadata {
    FsMetadata {
        root: root.to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

#[tokio::test]
async fn write_creates_file_and_returns_hash_header() {
    let root = TempRoot::new("create");
    let path = root.file("ftest.txt");
    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "hello world".to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].header.contains(&path),
        "header must carry the written path: {}",
        results[0].header
    );
    assert!(results[0].warnings.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
}

#[tokio::test]
async fn write_creates_multiple_files_in_single_call() {
    let root = TempRoot::new("multi");
    let path_a = root.file("ftest.txt");
    let path_b = root.file("ftst2.txt");
    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![
                TargetFile {
                    path: path_a.clone(),
                    text: "hello world".to_string(),
                    file_hash: None,
                },
                TargetFile {
                    path: path_b.clone(),
                    text: "test".to_string(),
                    file_hash: None,
                },
            ],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].header.contains(&path_a));
    assert!(results[0].warnings.is_none());
    assert!(results[1].header.contains(&path_b));
    assert!(results[1].warnings.is_none());
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "hello world");
    assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "test");
}

#[tokio::test]
async fn write_denied_when_path_is_in_blocklist() {
    let root = TempRoot::new("blocked");
    let path = root.file("ftest.txt");
    let metadata = FsMetadata {
        root: root.path().to_path_buf(),
        allowlist: None,
        blocklist: Some(vec![PathBuf::from(&path)]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "should not be written".to_string(),
                file_hash: None,
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
    assert!(
        !Path::new(&path).exists(),
        "blocked file must not be written"
    );
}

#[tokio::test]
async fn write_reports_empty_text_inline_and_skips_file() {
    let root = TempRoot::new("empty");
    let path = root.file("cosh_test_empty.txt");

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "".to_string(),
                file_hash: None,
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
    assert!(!Path::new(&path).exists());
}

#[tokio::test]
async fn write_allowed_outside_root_when_path_in_allowlist() {
    let root = TempRoot::new("allow");
    // Outside the root: a sibling scratch file, not under the temp root.
    let outside = std::env::temp_dir().join(format!(
        "cosh_fs_write_allow_out_{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&outside);
    let outside_path = outside.to_string_lossy().into_owned();
    let metadata = FsMetadata {
        root: root.path().to_path_buf(),
        allowlist: Some(vec![outside.clone()]),
        blocklist: None,
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: outside_path.clone(),
                text: "outside root but explicitly allowed".to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].header.contains(outside_path.as_str()),
        "header must carry the allowed path: {}",
        results[0].header
    );
    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn write_errors_on_inconsistent_blocklist_and_allowlist() {
    let root = TempRoot::new("mismatch");
    let path = root.file("ftest.txt");
    let metadata = FsMetadata {
        root: root.path().to_path_buf(),
        allowlist: Some(vec![PathBuf::from(&path)]),
        blocklist: Some(vec![PathBuf::from(&path)]),
    };

    let result = write(
        metadata,
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "should never be written".to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("both blocklist and allowlist"));
}

#[tokio::test]
async fn write_strips_hashline_prefixes_and_reports_warning() {
    let root = TempRoot::new("striphl");
    let path = root.file("cosh_test_strip_hashline.txt");
    let content = "[main.rs#ABCD]\n42: fn main() {\n43:     println!(\"hello\");\n44: }";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
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

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("[main.rs#ABCD]"));
    assert!(!written.contains("42:"));
    assert!(written.contains("fn main()"));
    assert!(written.contains("println!"));
}

#[tokio::test]
async fn write_strips_hashline_prefixes_without_bracket_header() {
    let root = TempRoot::new("strippfx");
    let path = root.file("cosh_test_strip_line_prefixes.txt");
    let content = "42: fn main() {\n43:     println!(\"hello\");\n44: }";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
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

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("42:"));
    assert!(written.contains("fn main()"));
}

#[tokio::test]
async fn write_does_not_strip_normal_content() {
    let root = TempRoot::new("nostrip");
    let path = root.file("cosh_test_no_strip.txt");
    let content = "fn main() {\n    println!(\"hello\");\n}";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());

    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, content);
}

#[tokio::test]
#[cfg(unix)]
async fn write_chmods_executable_for_shebang() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempRoot::new("shebang");
    let path = root.file("cosh_test_shebang.sh");
    let content = "#!/usr/bin/env bash\necho hello";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
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

    let meta = std::fs::metadata(&path).unwrap();
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "file should have execute bits"
    );
}

#[tokio::test]
async fn write_refuses_to_overwrite_auto_generated_file() {
    let root = TempRoot::new("generated");
    let path = root.file("cosh_test_generated.txt");
    let original = "// Code generated by tool. DO NOT EDIT.\noriginal\n";
    std::fs::write(&path, original).unwrap();

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "replacement".to_string(),
                file_hash: None,
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
            .contains("auto-generated"),
        "expected an auto-generated refusal, got: {:?}",
        results[0].warnings
    );
    // The original content is left untouched.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[tokio::test]
async fn write_allows_creating_file_with_generated_marker() {
    // Creating a brand-new file is always allowed — the guard protects
    // overwriting existing generated files, not generating new ones.
    let root = TempRoot::new("gennew");
    let path = root.file("cosh_test_create_generated.txt");
    let content = "// Code generated by tool. DO NOT EDIT.\nhello\n";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
}

#[tokio::test]
async fn write_normalizes_crlf_to_lf() {
    // Parity with read: CRLF content is canonicalized to LF so the returned
    // hash/header matches what a follow-up read would report.
    let root = TempRoot::new("crlf");
    let path = root.file("cosh_test_crlf.txt");

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: "line1\r\nline2\r\n".to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "line1\nline2\n");
}

#[tokio::test]
#[cfg(unix)]
async fn write_does_not_chmod_without_shebang() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempRoot::new("noshebang");
    let path = root.file("cosh_test_no_shebang.txt");
    let content = "plain text file";

    let result = write(
        meta_root(root.path()),
        FsWrite {
            targets: vec![TargetFile {
                path: path.clone(),
                text: content.to_string(),
                file_hash: None,
            }],
        },
    )
    .await;
    assert!(result.is_ok());
    let results = result.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].warnings.is_none());

    let meta = std::fs::metadata(&path).unwrap();
    let mode = meta.permissions().mode() & 0o111;
    assert_eq!(mode, 0, "file should NOT have execute bits");
}
