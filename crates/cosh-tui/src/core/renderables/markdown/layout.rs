use pulldown_cmark::{Event, Options, Tag, TagEnd};

use super::context::MarkdownContext;

/// Estimate the number of terminal rows required to render `text` as markdown
/// within a viewport `max_w` columns wide.
///
/// This function follows **exactly** the same wrapping and line-break logic as
/// `MarkdownRenderable::render_self` so that callers (e.g. `SessionView`) can
/// pre-allocate the correct area height before rendering.
///
/// Uses `MarkdownContext` under the hood so that list markers, code blocks,
/// and other constructs are counted identically to the renderer.
#[must_use]
pub fn estimate_height(text: &str, max_w: u16) -> u16 {
    if text.is_empty() || max_w == 0 {
        return 1;
    }

    let area_x = 0u16;
    let mut y: u16 = 0;
    let mut x: u16 = 0;

    let mut ctx = MarkdownContext::new();

    // Table-buffering state (mirrors md.rs)
    let mut in_table = false;
    let mut tbl_rows_count: u16 = 0;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(text, options);

    for event in parser {
        // ── Table buffering mode ───────────────────────────────
        if in_table {
            if let Event::End(tag_end) = &event {
                match &tag_end {
                    TagEnd::TableRow => {
                        tbl_rows_count += 1;
                    }
                    TagEnd::Table => {
                        // Height: top(1) + header(1) + sep(1) + body rows + bottom(1)
                        let table_h = 4 + tbl_rows_count;
                        if x != area_x {
                            y += 1;
                        }
                        y += table_h;
                        x = area_x;
                        in_table = false;
                    }
                    _ => {}
                }
            }
            // Forward End events to context for state tracking
            if let Event::End(end) = &event {
                ctx.handle_end(end);
            } else {
                continue;
            }
            continue;
        }

        match event {
            Event::Start(tag) => {
                ctx.handle_start(&tag);
                match &tag {
                    Tag::Table(_) => {
                        in_table = true;
                        tbl_rows_count = 0;
                        if x != area_x {
                            y += 1;
                            x = area_x;
                        }
                    }
                    Tag::Paragraph
                    | Tag::Heading { .. }
                    | Tag::BlockQuote(_)
                    | Tag::CodeBlock(_) => {
                        if x != area_x {
                            y += 1;
                            x = area_x;
                        }
                    }
                    Tag::Item => {
                        if x != area_x {
                            y += 1;
                            x = area_x;
                        }
                        let marker = ctx.list_marker().map_or_else(
                            || "• ".to_string(),
                            |(ordered, num)| {
                                if ordered {
                                    format!("{num}. ")
                                } else {
                                    "• ".to_string()
                                }
                            },
                        );
                        for (_grapheme, w) in
                            crate::core::lib::unicode_util::graphemes_with_width(&marker)
                        {
                            if x >= max_w {
                                y += 1;
                                x = area_x;
                            }
                            x += w;
                        }
                    }
                    Tag::List(_)
                    | Tag::TableHead
                    | Tag::TableRow
                    | Tag::TableCell
                    | Tag::FootnoteDefinition(_)
                    | Tag::DefinitionList
                    | Tag::DefinitionListTitle
                    | Tag::DefinitionListDefinition
                    | Tag::Strikethrough
                    | Tag::Emphasis
                    | Tag::Strong
                    | Tag::Link { .. }
                    | Tag::Image { .. }
                    | Tag::MetadataBlock(_)
                    | Tag::HtmlBlock
                    | Tag::Superscript
                    | Tag::Subscript => {}
                }
            }
            Event::End(tag_end) => {
                match &tag_end {
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::BlockQuote(_)
                    | TagEnd::Item
                    | TagEnd::CodeBlock
                    | TagEnd::TableRow => {
                        y += 1;
                        x = area_x;
                    }
                    TagEnd::List(_)
                    | TagEnd::TableHead
                    | TagEnd::Table
                    | TagEnd::FootnoteDefinition
                    | TagEnd::DefinitionList
                    | TagEnd::DefinitionListTitle
                    | TagEnd::DefinitionListDefinition
                    | TagEnd::Strikethrough
                    | TagEnd::Emphasis
                    | TagEnd::Strong
                    | TagEnd::Link
                    | TagEnd::Image
                    | TagEnd::MetadataBlock(_)
                    | TagEnd::HtmlBlock
                    | TagEnd::Superscript
                    | TagEnd::Subscript => {}
                    TagEnd::TableCell => {
                        x = x.saturating_add(2);
                    }
                }
                ctx.handle_end(&tag_end);
            }
            Event::Text(text)
            | Event::FootnoteReference(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::InlineHtml(text) => {
                let text: &str = text.as_ref();
                if ctx.in_code_block() {
                    let code_max_w = max_w.saturating_sub(2).max(1);
                    let code_pad_v = 1u16;
                    // Internal top + bottom padding rows (2 rows each for symmetry)
                    // plus 2 extra rows for the blank separators (1 top, 1 bottom)
                    y = y.saturating_add(code_pad_v * 2 + 2);
                    let mut first = true;
                    for line in text.lines() {
                        let wrapped = crate::core::lib::unicode_util::word_wrap(line, code_max_w);
                        for wl in &wrapped {
                            if !first {
                                y += 1;
                                x = area_x;
                            }
                            first = false;
                            let w = crate::core::lib::unicode_util::str_display_width(wl) as u16;
                            x = x.saturating_add(w.min(max_w.saturating_sub(x)));
                        }
                    }
                } else {
                    layout_word_wrap(text, max_w, &mut x, &mut y, area_x);
                }
            }
            Event::Code(text) | Event::Html(text) => {
                layout_word_wrap(text.as_ref(), max_w, &mut x, &mut y, area_x);
            }
            Event::SoftBreak | Event::HardBreak | Event::Rule => {
                x = area_x;
                y += 1;
            }
            Event::TaskListMarker(_checked) => {
                let marker = "[ ] ";
                for (_grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(marker) {
                    if x >= max_w {
                        y += 1;
                        x = area_x;
                    }
                    x += w;
                }
            }
        }
    }

    y.max(1)
}

/// Word-aware width tracking for layout estimation. Accumulates non-space
/// graphemes as a "word" and only advances past the word if it fits on the
/// current line; otherwise wraps to the next line first.
fn layout_word_wrap(text: &str, max_w: u16, x: &mut u16, y: &mut u16, area_x: u16) {
    let mut word_w = 0u16;

    for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(text) {
        if grapheme == "\n" {
            *x += word_w;
            word_w = 0;
            *y += 1;
            *x = area_x;
            continue;
        }

        if grapheme == " " {
            if *x + word_w > max_w && *x > area_x {
                *y += 1;
                *x = area_x;
            }
            *x += word_w;
            word_w = 0;
            if *x < max_w {
                *x += 1;
            }
            continue;
        }

        word_w += w;
    }

    // Flush last word
    if word_w > 0 {
        if *x + word_w > max_w && *x > area_x {
            *y += 1;
            *x = area_x;
        }
        *x += word_w;
    }
}
