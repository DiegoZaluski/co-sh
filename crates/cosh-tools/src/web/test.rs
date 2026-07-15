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

/// Debug test: see what the LLM actually receives when calling web_fetch
/// on a JSON API (Pokémon API). Run with:
///   cargo test debug_fetch_local_pokemon -- --nocapture --ignored
#[tokio::test]
#[ignore]
async fn debug_fetch_local_pokemon() {
    let url = "https://pokeapi.co/api/v2/pokemon/1";
    println!("=== DEBUG: web_fetch on Pokémon API ===");
    println!("URL: {url}");
    println!("EXA_FIRST=false (local first)");

    let fetch = WebFetch {
        url: url.to_string(),
    };
    match super::fetch(&fetch).await {
        Ok(text) => {
            println!("=== RESULT ({} bytes) ===", text.len());
            println!("{}", &text[..text.len().min(2000)]);
            if text.len() > 2000 {
                println!("... (truncated, total {} bytes)", text.len());
            }
            println!("=== END RESULT ===");
        }
        Err(e) => {
            println!("=== ERROR ===");
            println!("{e}");
            println!("=== END ERROR ===");
        }
    }
}

/// Debug test: fetch the raw URL with reqwest and feed it to
/// rs_trafilatura directly, to see what it extracts from a JSON API.
/// Run with:
///   cargo test debug_rs_trafilatura_pokemon -- --nocapture --ignored
#[tokio::test]
#[ignore]
async fn debug_rs_trafilatura_pokemon() {
    let url = "https://pokeapi.co/api/v2/pokemon/1";
    println!("=== DEBUG: rs_trafilatura on Pokémon API ===");
    println!("URL: {url}");

    // Download raw content
    let client = reqwest::Client::builder()
        .user_agent("Cosh/0.1")
        .build()
        .unwrap();
    let raw = client.get(url).send().await.unwrap();
    let content_type = raw.headers().get("content-type").cloned();
    let html = raw.text().await.unwrap();
    println!("Content-Type: {:?}", content_type);
    println!("Raw size: {} bytes", html.len());
    println!("First 500 chars of raw: {}", &html[..html.len().min(500)]);

    // Try rs_trafilatura
    match rs_trafilatura::extract(&html) {
        Ok(result) => {
            println!();
            println!("=== rs_trafilatura result ===");
            println!("content_text: {} bytes", result.content_text.len());
            println!(
                "content_html: {:?}",
                result
                    .content_html
                    .as_ref()
                    .map(|s| format!("{} bytes", s.len()))
            );
            println!(
                "content_markdown: {:?}",
                result
                    .content_markdown
                    .as_ref()
                    .map(|s| format!("{} bytes", s.len()))
            );
            println!("extraction_quality: {}", result.extraction_quality);
            println!(
                "classification_confidence: {:?}",
                result.classification_confidence
            );
            println!("metadata title: {:?}", result.metadata.title);
            println!();
            println!("First 500 chars of content_text:");
            println!(
                "{:?}",
                &result.content_text[..result.content_text.len().min(500)]
            );
            println!("=== END ===");
        }
        Err(e) => {
            println!("rs_trafilatura ERROR: {e}");
        }
    }
}

/// Debug test: compare extraction_quality for a real HTML article
/// Run with:
///   cargo test debug_rs_trafilatura_html -- --nocapture --ignored
#[tokio::test]
#[ignore]
async fn debug_rs_trafilatura_html() {
    let url = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
    println!("=== DEBUG: rs_trafilatura on HTML article ===");
    println!("URL: {url}");

    let client = reqwest::Client::builder()
        .user_agent("Cosh/0.1")
        .build()
        .unwrap();
    let raw = client.get(url).send().await.unwrap();
    let content_type = raw.headers().get("content-type").cloned();
    let html = raw.text().await.unwrap();
    println!("Content-Type: {:?}", content_type);
    println!("Raw size: {} bytes", html.len());

    match rs_trafilatura::extract(&html) {
        Ok(result) => {
            println!();
            println!("=== rs_trafilatura result ===");
            println!("content_text: {} bytes", result.content_text.len());
            println!(
                "content_html: {:?}",
                result
                    .content_html
                    .as_ref()
                    .map(|s| format!("{} bytes", s.len()))
            );
            println!(
                "content_markdown: {:?}",
                result
                    .content_markdown
                    .as_ref()
                    .map(|s| format!("{} bytes", s.len()))
            );
            println!("extraction_quality: {}", result.extraction_quality);
            println!(
                "classification_confidence: {:?}",
                result.classification_confidence
            );
            println!("metadata title: {:?}", result.metadata.title);
            println!();
            println!("First 500 chars of content_text:");
            println!(
                "{:?}",
                &result.content_text[..result.content_text.len().min(500)]
            );
            println!("=== END ===");
        }
        Err(e) => {
            println!("rs_trafilatura ERROR: {e}");
        }
    }
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
