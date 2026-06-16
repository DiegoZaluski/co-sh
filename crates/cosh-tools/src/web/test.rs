use tokio;

#[tokio::test]
#[ignore]
async fn test_search() {
    let has_exa_key = std::env::var("EXA_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());

    let args = if has_exa_key {
        super::search::SearchArgs {
            num_results: 1,
            query: Some("Rust language".to_string()),
            url: None,
        }
    } else {
        super::search::SearchArgs {
            num_results: 1,
            query: None,
            url: Some("https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string()),
        }
    };

    let res = super::search::search(args).await.unwrap();
    assert!(!res.trim().is_empty());
}
