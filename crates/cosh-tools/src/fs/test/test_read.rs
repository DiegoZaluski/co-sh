use super::super::read::read;
use super::super::types::{FsRead, Target};

#[tokio::test]
async fn test_function_search() {
    let results = read(
        &FsRead,
        vec![
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
    )
    .await;
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
    let results = read(
        &FsRead,
        vec![Target {
            path: "/home/inky/cosh/crates/cosh-sdk/src/hashline/tokenizer.rs",
            line: Some(5),
            symbol: None,
        }],
    )
    .await;
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
