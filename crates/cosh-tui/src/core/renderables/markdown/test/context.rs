use pulldown_cmark::{CodeBlockKind, CowStr, HeadingLevel, Tag, TagEnd};

use super::super::context::MarkdownElement;
use super::super::MarkdownContext;

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
