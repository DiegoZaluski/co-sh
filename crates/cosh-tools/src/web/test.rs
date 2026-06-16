use super::search::SearchArgs;

#[tokio::test]
async fn reject_empty_args() {
    let e = super::search(SearchArgs { query: None, url: None, num_results: 1 })
        .await
        .unwrap_err();
    assert!(e.contains("query") || e.contains("url"), "{e}");
}

#[tokio::test]
async fn reject_zero_results() {
    let e = super::search(SearchArgs { query: Some("x".into()), url: None, num_results: 0 })
        .await
        .unwrap_err();
    assert!(e.contains("num_results"), "{e}");
}

#[tokio::test]
#[ignore]
async fn fetch_url() {
    let r = super::search(SearchArgs {
        num_results: 1,
        query: None,
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
        query: Some("rust programming language".into()),
        url: None,
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
        query: Some("rust async concurrent programming".into()),
        url: None,
    })
    .await
    .unwrap();
    let n = r.lines().filter(|l| l.starts_with("URL:")).count();
    assert!(n >= 2, "got {n} results:\n{r}");
}

#[tokio::test]
#[ignore]
async fn search_url_beats_query() {
    let r = super::search(SearchArgs {
        num_results: 3,
        query: Some("ignored".into()),
        url: Some("https://en.wikipedia.org/wiki/Rust_(programming_language)".into()),
    })
    .await
    .unwrap();
    assert!(r.contains("Rust"));
    assert!(!r.contains("\nURL:"), "should not contain search URLs:\n{r}");
}

#[tokio::test]
#[ignore]
async fn search_no_results() {
    let e = super::search(SearchArgs {
        num_results: 1,
        query: Some("xylophone_zzz_nonexistent_12345".into()),
        url: None,
    })
    .await
    .unwrap_err();
    assert!(e.contains("no results") || e.contains("not found"), "{e}");
}
