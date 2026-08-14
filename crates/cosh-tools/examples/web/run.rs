//! Demonstrate `web`: the `Web` wrapper, the tool schemas, input
//! validation, and (best-effort) live fetching and searching. The live
//! calls hit real endpoints (Exa MCP / the local extractor), so each one
//! prints its result on success or the error string on failure — the
//! example never panics when the network is unavailable.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example web-run
//! ```

use cosh_tools::web::{Web, WebFetch, WebSearch};

#[tokio::main]
async fn main() {
    // ── 1. The tool schemas -------------------------------------------------
    let web = Web::new();
    println!("== 1. tool schemas ==");
    println!(
        "  web_fetch  requires: {:?}",
        web.description_fetch["inputSchema"]["required"]
    );
    println!(
        "  web_search requires: {:?}\n",
        web.description_search["inputSchema"]["required"]
    );

    // ── 2. Validation errors (no network needed) ----------------------------
    println!("== 2. validation ==");
    let err = cosh_tools::web::search(&WebSearch {
        num_results: 5,
        query: String::new(),
    })
    .await
    .unwrap_err();
    println!("  empty query -> {err}");
    let err = cosh_tools::web::search(&WebSearch {
        num_results: 0,
        query: "x".into(),
    })
    .await
    .unwrap_err();
    println!("  num_results 0 -> {err}\n");

    // ── 3. Live fetch (best-effort) ------------------------------------------
    println!("== 3. web.fetch example.com (best effort) ==");
    match web.fetch(WebFetch {
        url: "https://example.com".into(),
    })
    .await
    {
        Ok(text) => {
            println!("  fetched {} bytes, first 200:", text.len());
            for line in text.lines().take(6) {
                println!("    {line}");
            }
        }
        Err(e) => println!("  fetch failed: {e}"),
    }
    println!();

    // ── 4. Live search (best-effort) -----------------------------------------
    let searcher = Web::new().num_results(3);
    println!("== 4. web.search (best effort, num_results=3) ==");
    match searcher.search("rust programming language").await {
        Ok(results) => {
            println!("  result blocks: {}", results.matches("\n---\n").count() + 1);
            for line in results.lines().take(9) {
                println!("    {line}");
            }
        }
        Err(e) => println!("  search failed: {e}"),
    }
    println!();
}
