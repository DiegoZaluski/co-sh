use super::super::init;

#[test]
fn hierarchical_vs_flat_both_produce_output() {
    let text = "# Section 1\n\nThis is the first paragraph. It has some content.\n\n# Section 2\n\nThis is the second paragraph. More information here.";
    let flat = init(text, false, 0.8, 0.7, 0.4);
    let hierarchical = init(text, true, 0.8, 0.7, 0.4);
    assert!(!flat.is_empty(), "flat mode should produce output");
    assert!(
        !hierarchical.is_empty(),
        "hierarchical mode should produce output"
    );
}

#[test]
fn hierarchical_preserves_headings() {
    let text = "# Title\n\nSome content here. More details. Additional info.\n\n# Another Section\n\nDifferent content. Final words.";
    let result = init(text, true, 0.8, 0.7, 0.3);
    assert!(!result.is_empty(), "hierarchical should produce output");
    assert!(
        result.contains("Title") || result.contains("# Title"),
        "hierarchical compression should preserve heading content"
    );
}

#[test]
fn flat_mode_also_works_on_markdown() {
    let text = "Just plain text. No headings at all. Nothing special.";
    let result = init(text, false, 0.8, 0.7, 0.5);
    assert!(!result.is_empty(), "flat mode works on plain text");
}

#[test]
fn hierarchical_on_plain_text_without_headings() {
    let text = "Just plain text. No headings at all. Nothing special.";
    let result = init(text, true, 0.8, 0.7, 0.5);
    assert!(
        !result.is_empty(),
        "hierarchical mode works on plain text without headings"
    );
}
