use super::fetch::FetchArgs;
use super::search::SearchArgs;

#[tokio::test]
async fn reject_empty_query() {
    let e = super::search(SearchArgs {
        query: String::new(),
        num_results: 1,
    })
    .await
    .unwrap_err();
    assert!(e.contains("query"), "{e}");
}

#[tokio::test]
async fn reject_zero_results() {
    let e = super::search(SearchArgs {
        query: "x".into(),
        num_results: 0,
    })
    .await
    .unwrap_err();
    assert!(e.contains("num_results"), "{e}");
}

#[tokio::test]
#[ignore]
async fn fetch_url() {
    let r = super::fetch(FetchArgs {
        path: None,
        url: Some("https://en.wikipedia.org/wiki/Rust_(programming_language)".into()),
    })
    .await
    .unwrap();
    assert!(r.contains("Rust"));
}

#[tokio::test]
#[ignore]
async fn search_single_result() {
    let r = super::search(SearchArgs {
        num_results: 1,
        query: "rust programming language".into(),
    })
    .await
    .unwrap();
    assert!(r.contains("URL:"));
}

#[tokio::test]
#[ignore]
async fn search_multiple_results() {
    let r = super::search(SearchArgs {
        num_results: 5,
        query: "rust async concurrent programming".into(),
    })
    .await
    .unwrap();
    let n = r.lines().filter(|l| l.starts_with("URL:")).count();
    assert!(n >= 2, "got {n} results:\n{r}");
}

#[tokio::test]
#[ignore]
async fn fetch_prefers_url_over_path() {
    let r = super::fetch(FetchArgs {
        path: Some("ignored".into()),
        url: Some("https://en.wikipedia.org/wiki/Rust_(programming_language)".into()),
    })
    .await
    .unwrap();
    assert!(r.contains("Rust"));
}

#[tokio::test]
#[ignore]
async fn search_no_results() {
    let e = super::search(SearchArgs {
        num_results: 1,
        query: "xylophone_zzz_nonexistent_12345".into(),
    })
    .await
    .unwrap_err();
    assert!(e.contains("no results") || e.contains("not found"), "{e}");
}
