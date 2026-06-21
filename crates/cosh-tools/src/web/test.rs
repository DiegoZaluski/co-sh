use super::fetch::WebFetch;
use super::search::WebSearch;

#[tokio::test]
async fn reject_empty_query() {
    let e = super::search(&WebSearch::default(), "").await.unwrap_err();
    assert!(e.contains("query"), "{e}");
}

#[tokio::test]
async fn reject_zero_results() {
    let e = super::search(&WebSearch { num_results: 0 }, "x")
        .await
        .unwrap_err();
    assert!(e.contains("num_results"), "{e}");
}

#[tokio::test]
#[ignore]
async fn fetch_url() {
    let r = super::fetch(
        &WebFetch,
        "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    )
    .await
    .unwrap();
    assert!(r.contains("Rust"));
}

#[tokio::test]
#[ignore]
async fn search_single_result() {
    let r = super::search(&WebSearch { num_results: 1 }, "rust programming language")
        .await
        .unwrap();
    assert!(r.contains("URL:"));
}

#[tokio::test]
#[ignore]
async fn search_multiple_results() {
    let r = super::search(
        &WebSearch { num_results: 5 },
        "rust async concurrent programming",
    )
    .await
    .unwrap();
    let n = r.lines().filter(|l| l.starts_with("URL:")).count();
    assert!(n >= 2, "got {n} results:\n{r}");
}

#[tokio::test]
#[ignore]
async fn search_no_results() {
    let e = super::search(
        &WebSearch { num_results: 1 },
        "xylophone_zzz_nonexistent_12345",
    )
    .await
    .unwrap_err();
    assert!(e.contains("no results") || e.contains("not found"), "{e}");
}
