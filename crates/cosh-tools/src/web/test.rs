use tokio;
#[tokio::test]
#[ignore]
async fn test_search() {
    let args = super::search::SearchArgs {
        num_results: 1,
        query: None,
        url: Some(
            "https://medium.com/@liranyoffe/reverse-engineering-claude-code-web-tools-1409249316c3"
                .to_string(),
        ),
    };
    let res = super::search::search(args).await.unwrap();

    println!("{}", res);
}
