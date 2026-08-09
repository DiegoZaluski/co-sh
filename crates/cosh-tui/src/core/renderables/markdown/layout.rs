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

    // Table-buffering state (mirrors md.rs — full cell content buffering
    // for accurate multi-line height estimation)
    let mut in_table = false;
    let mut tbl_headers: Vec<String> = Vec::new();
    let mut tbl_rows: Vec<Vec<String>> = Vec::new();
    let mut tbl_cur_row: Vec<String> = Vec::new();
    let mut tbl_cur_cell = String::new();
    let mut tbl_in_header = false;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(text, options);

    for event in parser {
        // ── Table buffering mode ───────────────────────────────
        if in_table {
            match &event {
                Event::Start(tag) => {
                    ctx.handle_start(tag);
                    match tag {
                        Tag::TableHead => {
                            tbl_in_header = true;
                            tbl_cur_row.clear();
                        }
                        Tag::TableRow => {
                            tbl_cur_row.clear();
                        }
                        Tag::TableCell => {
                            tbl_cur_cell.clear();
                        }
                        _ => {}
                    }
                }
                Event::End(tag_end) => {
                    match tag_end {
                        TagEnd::TableCell => {
                            tbl_cur_row.push(std::mem::take(&mut tbl_cur_cell));
                        }
                        TagEnd::TableRow => {
                            let row = std::mem::take(&mut tbl_cur_row);
                            if tbl_in_header {
                                tbl_headers = row;
                            } else {
                                tbl_rows.push(row);
                            }
                        }
                        TagEnd::TableHead => {
                            if tbl_in_header && !tbl_cur_row.is_empty() {
                                tbl_headers = std::mem::take(&mut tbl_cur_row);
                            }
                            tbl_in_header = false;
                        }
                        TagEnd::Table => {
                            // ── Calculate accurate height with wrapping ──
                            let col_count = tbl_headers.len();
                            if col_count > 0 && max_w > 0 {
                                let padding: u16 = 1;
                                let border_gaps = if col_count > 1 {
                                    col_count as u16 - 1
                                } else {
                                    0
                                };

                                // Calculate natural column widths
                                let mut col_widths: Vec<u16> = tbl_headers
                                    .iter()
                                    .map(|h| h.chars().count() as u16)
                                    .collect();
                                for row in &tbl_rows {
                                    for (ci, cell) in row.iter().enumerate() {
                                        if ci < col_count {
                                            col_widths[ci] =
                                                col_widths[ci].max(cell.chars().count() as u16);
                                        }
                                    }
                                }

                                // Scale and redistribute (same algorithm as render_table in md.rs)
                                let total_w: u16 =
                                    col_widths.iter().map(|w| w + 2 * padding).sum::<u16>()
                                        + border_gaps;
                                let available = max_w;
                                if total_w > available {
                                    for w in &mut col_widths {
                                        *w = ((u32::from(*w) * u32::from(available))
                                            / u32::from(total_w))
                                            as u16;
                                    }
                                    for w in &mut col_widths {
                                        *w = (*w).max(1);
                                    }
                                    while col_widths.iter().map(|w| w + 2 * padding).sum::<u16>()
                                        + border_gaps
                                        > available
                                    {
                                        if let Some(max_idx) = (0..col_widths.len())
                                            .filter(|&i| col_widths[i] > 1)
                                            .max_by_key(|&i| col_widths[i])
                                        {
                                            col_widths[max_idx] -= 1;
                                        } else {
                                            break;
                                        }
                                    }
                                }

                                // Estimate wrapped line count per cell
                                let estimate_cell_lines = |content: &str, col_w: u16| -> usize {
                                    if col_w < 1 || content.is_empty() {
                                        return 1;
                                    }
                                    let lines =
                                        crate::core::lib::unicode_util::word_wrap(content, col_w);
                                    let mut total = 0usize;
                                    for line in &lines {
                                        let lw =
                                            crate::core::lib::unicode_util::str_display_width(line)
                                                as u16;
                                        if lw <= col_w {
                                            total += 1;
                                        } else {
                                            // Character-level: ceil(lw / col_w)
                                            total += lw.div_ceil(col_w) as usize;
                                        }
                                    }
                                    total.max(1)
                                };

                                // Header wrapped lines
                                let header_lines = tbl_headers
                                    .iter()
                                    .enumerate()
                                    .map(|(ci, h)| estimate_cell_lines(h, col_widths[ci]))
                                    .max()
                                    .unwrap_or(1);

                                // Body wrapped lines
                                let mut body_lines = 0usize;
                                for row in &tbl_rows {
                                    let row_lines = row
                                        .iter()
                                        .enumerate()
                                        .map(|(ci, cell)| estimate_cell_lines(cell, col_widths[ci]))
                                        .max()
                                        .unwrap_or(1);
                                    body_lines += row_lines;
                                }

                                // Total: top(1) + header + sep(1) + body + bottom(1)
                                let table_h = 1 + header_lines + 1 + body_lines + 1;
                                if x != area_x {
                                    y += 1;
                                }
                                y += table_h as u16;
                            } else {
                                if x != area_x {
                                    y += 1;
                                }
                                y += 1;
                            }
                            x = area_x;
                            in_table = false;
                        }
                        _ => {}
                    }
                    ctx.handle_end(tag_end);
                }
                Event::Text(text) | Event::InlineHtml(text) | Event::Code(text) => {
                    tbl_cur_cell.push_str(text.as_ref());
                }
                Event::SoftBreak | Event::HardBreak => {
                    tbl_cur_cell.push('\n');
                }
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => {
                ctx.handle_start(&tag);
                match &tag {
                    Tag::Table(_) => {
                        in_table = true;
                        tbl_headers.clear();
                        tbl_rows.clear();
                        tbl_cur_row.clear();
                        tbl_cur_cell.clear();
                        tbl_in_header = false;
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
                    let code_pad_v = 1u16;
                    // The renderer (md.rs) advances the cursor through a code
                    // block as follows: ONE top gap row (CodeBlock start),
                    // one fresh row per code line, ONE bottom-padding row,
                    // ONE blank separator row (both inside render_code_block),
                    // and ONE more blank row at TagEnd::CodeBlock = N+4 rows
                    // total. When content FOLLOWS the block, all of those rows
                    // are real layout rows — the trailing content lands after
                    // them, so the estimate must count them all or that
                    // content is clipped off the bottom of the message.
                    //
                    // The estimator mirrors that: `code_pad_v * 2 + 2` = top
                    // gap row + first code line's row + bottom-padding row +
                    // blank separator row; the remaining code rows are added
                    // below; and the TagEnd::CodeBlock branch adds the final
                    // blank row. (A code block at the very end of the text
                    // leaves the last three rows blank, which the glyph-based
                    // content scan ignores — that small over-estimate just
                    // shows blank rows under the block. For the Summarizing
                    // box this also shifts its collapsed tail window up by
                    // those rows for summaries ending in a code block; that
                    // cosmetic trade-off is accepted so that summaries with
                    // text AFTER a code block are never clipped.)
                    y = y.saturating_add(code_pad_v * 2 + 2);
                    // Every code row beyond the first is a fresh row (the pad
                    // already accounted for the first row). Rows are counted
                    // per line with the renderer's exact wrap behavior.
                    let mut rows = 0u16;
                    for line in text.lines() {
                        rows = rows.saturating_add(code_line_rows(line, max_w));
                    }
                    if rows > 0 {
                        y = y.saturating_add(rows.saturating_sub(1));
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
/// current line; otherwise wraps to the next line first. Words longer than
/// the full line width are broken at character level across multiple lines,
/// mirroring `MarkdownRenderable::render_text`.
fn layout_word_wrap(text: &str, max_w: u16, x: &mut u16, y: &mut u16, area_x: u16) {
    let mut word_w = 0u16;

    for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(text) {
        if grapheme == "\n" {
            flush_layout_word(word_w, x, y, max_w);
            word_w = 0;
            *y += 1;
            *x = area_x;
            continue;
        }

        if grapheme == " " {
            flush_layout_word(word_w, x, y, max_w);
            word_w = 0;
            if *x < max_w {
                *x += 1;
            }
            continue;
        }

        word_w += w;
    }

    // Flush last word
    flush_layout_word(word_w, x, y, max_w);
}

/// Count the rows a single code line occupies at box width `max_w`, mirroring
/// the renderer's `flush_code_word` (md.rs) EXACTLY — this keeps
/// `estimate_height` in lockstep with the real code-block layout:
///
/// - a word that doesn't fit the remaining space but fits a full line wraps
///   whole to the next line;
/// - a word wider than the whole line is broken at character level, wrapping
///   in place only when the cursor would pass the right edge (so a word that
///   exactly fills a row does NOT add an extra row);
/// - the renderer reserves `CODE_PAD_H = 2` columns on the left of every code
///   row, mirrored here by `min_x`.
///
/// The generic `word_wrap` utility is NOT used here: it starts a word wider
/// than the remaining space on a fresh line, while the renderer packs it onto
/// the current line — a 1-row drift for long unbroken words.
///
/// NOTE: this is a hand-synced mirror of `flush_code_word` in md.rs — any
/// change to the renderer's code-line wrap must be reflected here. The TUI
/// test `markdown_estimate_matches_render_height` pins that `estimate_height`
/// never under-counts the renderer's rows (so chat messages are never
/// clipped).
fn code_line_rows(line: &str, max_w: u16) -> u16 {
    // A code block inside a 1-2 column box cannot render meaningfully anyway
    // (CODE_PAD_H alone overflows it) — treat it as a single row.
    if max_w < 3 {
        return 1;
    }
    let min_x = 2u16; // mirrors CODE_PAD_H in md.rs
    let avail = max_w.saturating_sub(min_x);
    let mut x = min_x;
    let mut rows = 1u16;
    let mut word_w = 0u16;

    let flush = |word_w: u16, x: &mut u16, rows: &mut u16| {
        if word_w == 0 {
            return;
        }
        if *x + word_w <= max_w {
            *x += word_w;
            return;
        }
        if *x > min_x && word_w <= avail {
            // Whole-word wrap: fits intact on a fresh line.
            *rows += 1;
            *x = min_x + word_w;
            return;
        }
        // Word wider than the whole line: break at character level, wrapping
        // only when the cursor would exceed the right edge (mirrors
        // flush_code_word's per-grapheme wrap).
        let mut remaining = word_w;
        while remaining > 0 {
            if *x >= max_w {
                // Cursor already at the right edge: the next grapheme must
                // wrap onto a fresh line.
                *rows += 1;
                *x = min_x;
                continue;
            }
            let fit = max_w - *x;
            if remaining <= fit {
                *x += remaining;
                break;
            }
            remaining -= fit;
            *rows += 1;
            *x = min_x;
        }
    };

    for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(line) {
        if grapheme == "\n" {
            flush(word_w, &mut x, &mut rows);
            word_w = 0;
            rows += 1;
            x = min_x;
            continue;
        }
        if grapheme == " " {
            flush(word_w, &mut x, &mut rows);
            word_w = 0;
            if x < max_w {
                x += 1;
            }
            continue;
        }
        word_w += w;
    }
    flush(word_w, &mut x, &mut rows);
    rows
}

/// Flush an accumulated word of `word_w` columns starting at `*x`, advancing
/// `y` by the number of extra lines the word spans.
///
/// A word that doesn't fit on the current line wraps whole to the next line
/// when it is shorter than the full line width; a word longer than the whole
/// line is broken at character level (mirrors `flush_render_word`).
fn flush_layout_word(word_w: u16, x: &mut u16, y: &mut u16, max_w: u16) {
    if word_w == 0 {
        return;
    }
    if *x + word_w <= max_w {
        *x += word_w;
        return;
    }
    if word_w <= max_w {
        // Whole word fits on a fresh line: wrap it there intact.
        *y += 1;
        *x = word_w;
        return;
    }
    // The word alone is wider than the whole line: break at character level.
    let end = u32::from(*x) + u32::from(word_w);
    let lines = end.div_ceil(u32::from(max_w)) as u16;
    *y = y.saturating_add(lines.saturating_sub(1));
    // If the last chunk exactly fills the line, mirror the renderer by
    // leaving the cursor at the right edge (so the next word wraps).
    let rem = end % u32::from(max_w);
    *x = if rem == 0 { max_w } else { rem as u16 };
}
