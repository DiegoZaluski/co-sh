use text_splitter::TextSplitter;

#[test]
fn text_splitter_produces_at_least_one_chunk() {
    let splitter = TextSplitter::new(200);
    let chunks: Vec<&str> = splitter.chunks("Hello world. How are you?").collect();
    assert!(!chunks.is_empty(), "text-splitter should produce at least one chunk");
    assert!(
        chunks.iter().any(|c| c.contains("Hello")),
        "chunks should contain original text"
    );
}

#[test]
fn text_splitter_splits_long_text_into_multiple_chunks() {
    let splitter = TextSplitter::new(50);
    let long = "This is a long text. With many sentences. That should be split. Into multiple chunks. By the text splitter.";
    let chunks: Vec<&str> = splitter.chunks(long).collect();
    assert!(chunks.len() >= 2, "~130 chars with capacity 50 should produce at least 2 chunks");
}

#[test]
fn text_splitter_handles_empty_string() {
    let splitter = TextSplitter::new(200);
    let chunks: Vec<&str> = splitter.chunks("").collect();
    assert!(chunks.is_empty() || chunks.iter().all(|c| c.trim().is_empty()),
        "empty input should produce empty or whitespace-only chunks");
}
