use pulldown_cmark::{CodeBlockKind, CowStr, HeadingLevel, Tag, TagEnd};

use super::super::MarkdownContext;
use super::super::context::MarkdownElement;

#[test]
fn test_empty_context() {
    let ctx = MarkdownContext::new();
    assert_eq!(ctx.current_element(), None);
    assert_eq!(ctx.heading_level(), None);
    assert!(!ctx.in_code_block());
    assert!(!ctx.in_blockquote());
}

#[test]
fn test_heading_tracking() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::Heading {
        level: HeadingLevel::H2,
        id: None,
        classes: Vec::new(),
        attrs: Vec::new(),
    });
    assert_eq!(ctx.heading_level(), Some(2));
    assert_eq!(ctx.current_element(), None); // heading is block-level, not returned by current_element

    ctx.handle_end(&TagEnd::Heading(HeadingLevel::H2));
    assert_eq!(ctx.heading_level(), None);
}

#[test]
fn test_emphasis_nesting() {
    let mut ctx = MarkdownContext::new();
    assert_eq!(ctx.current_element(), None);

    ctx.handle_start(&Tag::Emphasis);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Emphasis));

    ctx.handle_start(&Tag::Strong);
    // Strong is deeper, should be preferred
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Strong));

    ctx.handle_end(&TagEnd::Strong);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Emphasis));

    ctx.handle_end(&TagEnd::Emphasis);
    assert_eq!(ctx.current_element(), None);
}

#[test]
fn test_code_block() {
    let mut ctx = MarkdownContext::new();
    assert!(!ctx.in_code_block());

    ctx.handle_start(&Tag::CodeBlock(CodeBlockKind::Fenced("rust".into())));
    assert!(ctx.in_code_block());
    assert_eq!(ctx.code_block_lang(), "rust");
    assert_eq!(ctx.current_element(), Some(MarkdownElement::CodeBlock));

    ctx.handle_end(&TagEnd::CodeBlock);
    assert!(!ctx.in_code_block());
    assert_eq!(ctx.code_block_lang(), "");
}

#[test]
fn test_list_tracking_ordered() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::List(Some(1)));
    assert_eq!(ctx.list_ordered(), Some(true));

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 1)));

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 2)));

    ctx.handle_end(&TagEnd::List(false));
    assert_eq!(ctx.list_ordered(), None);
}

#[test]
fn test_list_tracking_unordered() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::List(None));
    assert_eq!(ctx.list_ordered(), Some(false));

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((false, 1))); // counter is incremented but unused

    ctx.handle_end(&TagEnd::Item);
    ctx.handle_end(&TagEnd::List(false));
    assert_eq!(ctx.list_ordered(), None);
}

#[test]
fn test_blockquote() {
    let mut ctx = MarkdownContext::new();
    assert!(!ctx.in_blockquote());

    ctx.handle_start(&Tag::BlockQuote(None));
    assert!(ctx.in_blockquote());

    ctx.handle_end(&TagEnd::BlockQuote(None));
    assert!(!ctx.in_blockquote());
}

#[test]
fn test_link() {
    let mut ctx = MarkdownContext::new();
    assert_eq!(ctx.current_element(), None);

    ctx.handle_start(&Tag::Link {
        link_type: pulldown_cmark::LinkType::Inline,
        dest_url: CowStr::Borrowed("https://example.com"),
        title: CowStr::Borrowed(""),
        id: CowStr::Borrowed(""),
    });
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Link));

    ctx.handle_end(&TagEnd::Link);
    assert_eq!(ctx.current_element(), None);
}

#[test]
fn test_strikethrough() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::Strikethrough);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Strikethrough));

    ctx.handle_end(&TagEnd::Strikethrough);
    assert_eq!(ctx.current_element(), None);
}

#[test]
fn test_emphasis_precedence_over_link() {
    // Inside [*text*](url), emphasis is innermost
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::Link {
        link_type: pulldown_cmark::LinkType::Inline,
        dest_url: CowStr::Borrowed("https://example.com"),
        title: CowStr::Borrowed(""),
        id: CowStr::Borrowed(""),
    });
    ctx.handle_start(&Tag::Emphasis);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Emphasis));

    ctx.handle_end(&TagEnd::Emphasis);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Link));

    ctx.handle_end(&TagEnd::Link);
    assert_eq!(ctx.current_element(), None);
}

#[test]
fn test_nested_list_tracking() {
    let mut ctx = MarkdownContext::new();

    // Outer ordered list
    ctx.handle_start(&Tag::List(Some(1)));
    assert_eq!(ctx.list_ordered(), Some(true));

    // First item
    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 1)));

    // Inner unordered list
    ctx.handle_start(&Tag::List(None));
    assert_eq!(ctx.list_ordered(), Some(false));

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((false, 1)));

    ctx.handle_end(&TagEnd::Item);

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((false, 2)));

    ctx.handle_end(&TagEnd::Item);

    // Exit inner list
    ctx.handle_end(&TagEnd::List(false));
    assert_eq!(ctx.list_ordered(), Some(true));
    assert_eq!(ctx.list_marker(), Some((true, 1))); // back to outer item

    ctx.handle_end(&TagEnd::Item);

    // Second outer item
    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 2)));

    ctx.handle_end(&TagEnd::Item);

    ctx.handle_end(&TagEnd::List(false));
    assert_eq!(ctx.list_ordered(), None);
}

#[test]
fn test_deep_inline_nesting() {
    let mut ctx = MarkdownContext::new();

    ctx.handle_start(&Tag::Emphasis);
    ctx.handle_start(&Tag::Strong);
    ctx.handle_start(&Tag::Strikethrough);
    // Strikethrough is innermost
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Strikethrough));

    ctx.handle_end(&TagEnd::Strikethrough);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Strong));

    ctx.handle_end(&TagEnd::Strong);
    assert_eq!(ctx.current_element(), Some(MarkdownElement::Emphasis));

    ctx.handle_end(&TagEnd::Emphasis);
    assert_eq!(ctx.current_element(), None);
}

#[test]
fn test_heading_level_resets_after_end() {
    let mut ctx = MarkdownContext::new();

    ctx.handle_start(&Tag::Heading {
        level: HeadingLevel::H3,
        id: None,
        classes: Vec::new(),
        attrs: Vec::new(),
    });
    assert_eq!(ctx.heading_level(), Some(3));

    ctx.handle_end(&TagEnd::Heading(HeadingLevel::H3));
    assert_eq!(ctx.heading_level(), None);

    // New heading should work after reset
    ctx.handle_start(&Tag::Heading {
        level: HeadingLevel::H1,
        id: None,
        classes: Vec::new(),
        attrs: Vec::new(),
    });
    assert_eq!(ctx.heading_level(), Some(1));
}

#[test]
fn test_indented_code_block_lang_is_empty() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::CodeBlock(CodeBlockKind::Indented));
    assert!(ctx.in_code_block());
    assert_eq!(ctx.code_block_lang(), "");
    ctx.handle_end(&TagEnd::CodeBlock);
}

#[test]
fn test_link_does_not_set_code_block_or_blockquote() {
    let mut ctx = MarkdownContext::new();
    ctx.handle_start(&Tag::Link {
        link_type: pulldown_cmark::LinkType::Inline,
        dest_url: CowStr::Borrowed("https://example.com"),
        title: CowStr::Borrowed(""),
        id: CowStr::Borrowed(""),
    });
    assert!(!ctx.in_code_block());
    assert!(!ctx.in_blockquote());
    ctx.handle_end(&TagEnd::Link);
}

#[test]
fn test_list_counter_with_custom_start() {
    let mut ctx = MarkdownContext::new();

    // Ordered list starting at 5
    ctx.handle_start(&Tag::List(Some(5)));
    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 5)));

    ctx.handle_start(&Tag::Item);
    assert_eq!(ctx.list_marker(), Some((true, 6)));

    ctx.handle_end(&TagEnd::List(false));
}

#[test]
fn test_empty_after_all_ends() {
    let mut ctx = MarkdownContext::new();
    // Simulate full document
    ctx.handle_start(&Tag::Heading {
        level: HeadingLevel::H1,
        id: None,
        classes: Vec::new(),
        attrs: Vec::new(),
    });
    ctx.handle_start(&Tag::Emphasis);
    ctx.handle_start(&Tag::Strong);
    ctx.handle_end(&TagEnd::Strong);
    ctx.handle_end(&TagEnd::Emphasis);
    ctx.handle_end(&TagEnd::Heading(HeadingLevel::H1));
    ctx.handle_start(&Tag::CodeBlock(CodeBlockKind::Fenced("rust".into())));
    ctx.handle_end(&TagEnd::CodeBlock);
    ctx.handle_start(&Tag::BlockQuote(None));
    ctx.handle_end(&TagEnd::BlockQuote(None));
    ctx.handle_start(&Tag::List(Some(1)));
    ctx.handle_start(&Tag::Item);
    ctx.handle_end(&TagEnd::Item);
    ctx.handle_end(&TagEnd::List(false));

    // All state should be reset
    assert_eq!(ctx.heading_level(), None);
    assert!(!ctx.in_code_block());
    assert!(!ctx.in_blockquote());
    assert_eq!(ctx.current_element(), None);
    assert_eq!(ctx.list_ordered(), None);
}
