use crate::util::token_counter::estimate_tokens;
use super::super::init;

pub fn compression_report(text: &str, use_hierarchical: bool) -> (usize, usize) {
    let tokens_before = estimate_tokens(text);

    if tokens_before == 0 {
        println!("\n>>> Compression Report <<<\n  Input empty\n");
        return (0, 0);
    }

    let result = init(text, use_hierarchical, 0.8, 0.7, 0.4);
    let tokens_after = estimate_tokens(&result);
    let reduction = (tokens_before.saturating_sub(tokens_after) as f64 / tokens_before as f64) * 100.0;
    let mode = if use_hierarchical { "hierarchical" } else { "flat" };

    println!("\n>>> Compression Report ({mode}) <<<");
    println!("  Input tokens:  {tokens_before}");
    println!("  Output tokens: {tokens_after}");
    println!("  Reduction:     {reduction:.1}%");
    println!("  {}", "-".repeat(40));
    println!("  Compressed output:");
    for line in result.lines() {
        println!("  {line}");
    }
    println!("  {}", "-".repeat(40));

    (tokens_before, tokens_after)
}

#[test]
fn report_on_plain_paragraphs() {
    let text = concat!(
        "The quick brown fox jumps over the lazy dog. ",
        "This classic pangram has been used for decades to test typewriters. ",
        "It originally appeared in a late 19th-century newspaper article. ",
        "Over time, the sentence became famous beyond its original context. ",
        "Teachers used it to teach touch typing. ",
        "Typists competed to see who could type it the fastest. ",
        "Eventually it became embedded in popular culture. ",
        "Font designers use it to showcase their typefaces. ",
        "Programmers use it as sample text. ",
        "The story of this sentence teaches us about language and culture. ",
        "Sometimes the most mundane creations can have the most lasting impact. ",
        "A simple sentence written as a joke over a hundred years ago ",
        "continues to appear in classrooms and software interfaces around the world."
    );

    let (before, after) = compression_report(text, false);
    assert!(
        after <= before,
        "flat: must not increase (before={before}, after={after})"
    );
}

#[test]
fn report_on_markdown_with_headings() {
    let text = concat!(
        "# Introduction\n\n",
        "The quick brown fox jumps over the lazy dog. ",
        "This classic pangram has a fascinating history. ",
        "Many people do not know about its origins.\n\n",
        "# History\n\n",
        "The phrase first appeared in a late 19th-century newspaper article. ",
        "It was written as a practical joke by a typing instructor.\n\n",
        "# Cultural Impact\n\n",
        "The sentence became a standard tool for teaching touch typing. ",
        "It is used by font designers and programmers alike.\n\n",
        "# Conclusion\n\n",
        "A simple sentence written over a hundred years ago ",
        "continues to influence modern technology and culture."
    );

    let (before, after) = compression_report(text, true);
    assert!(
        after <= before,
        "hierarchical: must not increase (before={before}, after={after})"
    );
}

#[test]
fn report_on_code_snippet() {
    let text = concat!(
        "Here is a Rust function that computes the Fibonacci sequence. ",
        "It uses an iterative approach for efficiency.\n\n",
        "```rust\n",
        "fn fibonacci(n: u64) -> u64 {\n",
        "    match n {\n",
        "        0 => 0,\n",
        "        1 => 1,\n",
        "        _ => {\n",
        "            let mut a = 0;\n",
        "            let mut b = 1;\n",
        "            for _ in 2..=n {\n",
        "                let temp = a + b;\n",
        "                a = b;\n",
        "                b = temp;\n",
        "            }\n",
        "            b\n",
        "        }\n",
        "    }\n",
        "}\n",
        "```\n\n",
        "This runs in O(n) time and O(1) space. ",
        "It handles edge cases like n=0 and n=1 correctly. ",
        "For larger values it simply iterates, updating two variables. ",
        "This is a classic example of dynamic programming."
    );

    let (before, after) = compression_report(text, false);
    assert!(
        after <= before,
        "code: must not increase (before={before}, after={after})"
    );
}
