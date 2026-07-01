use super::fetch::WebFetch;
use super::search::WebSearch;

#[tokio::test]
async fn reject_empty_query() {
    let e = super::search(&WebSearch::default()).await.unwrap_err();
    assert!(e.contains("query"), "{e}");
}

#[tokio::test]
async fn reject_zero_results() {
    let e = super::search(&WebSearch {
        num_results: 0,
        query: "x".to_string(),
    })
    .await
    .unwrap_err();
    assert!(e.contains("num_results"), "{e}");
}

#[tokio::test]
#[ignore]
async fn fetch_url() {
    let fetch = WebFetch {
        url: "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
    };
    let r = super::fetch(&fetch).await.unwrap();
    assert!(r.contains("Rust"));
}

#[tokio::test]
#[ignore]
async fn search_single_result() {
    let r = super::search(&WebSearch {
        num_results: 1,
        query: "rust programming language".to_string(),
    })
    .await
    .unwrap();
    assert!(r.contains("URL:"));
}

#[tokio::test]
#[ignore]
async fn search_multiple_results() {
    let r = super::search(&WebSearch {
        num_results: 5,
        query: "rust async concurrent programming".to_string(),
    })
    .await
    .unwrap();
    let n = r.lines().filter(|l| l.starts_with("URL:")).count();
    assert!(n >= 2, "got {n} results:\n{r}");
}

#[tokio::test]
#[ignore]
async fn search_no_results() {
    let e = super::search(&WebSearch {
        num_results: 1,
        query: "xylophone_zzz_nonexistent_12345".to_string(),
    })
    .await
    .unwrap_err();
    assert!(e.contains("no results") || e.contains("not found"), "{e}");
}
