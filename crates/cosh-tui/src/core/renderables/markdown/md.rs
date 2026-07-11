use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use pulldown_cmark::{Event, Options, Tag, TagEnd};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_sdk::tree_sitter::highlight::{HighlightCategory, highlight};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, parse_color};
use crate::core::rgba::RGBA;

use super::context::{MarkdownContext, MarkdownElement};
use super::styles::MarkdownPalette;

static NEXT_MARKDOWN_NUM: AtomicU64 = AtomicU64::new(1);

/// Renders markdown content into a fixed-area `Buffer`.
///
/// Mirrors OpenTUI's `MarkdownRenderable` in spirit:
/// - Processes a `pulldown_cmark` event stream
/// - Uses a `MarkdownContext` to track nesting (headings, emphasis, lists, etc.)
/// - Applies theme-derived styles through `MarkdownPalette`
/// - Delegates syntax highlighting to tree-sitter for fenced code blocks
pub struct MarkdownRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    /// Raw markdown source.
    content: String,
    /// Foreground colour override (falls back to a light grey).
    fg: Option<RGBA>,
    /// Background colour override (falls back to transparent black).
    bg: Option<RGBA>,
    /// Optional table border colour. Falls back to the palette's muted colour.
    table_border_color: Option<RGBA>,
}

impl MarkdownRenderable {
    pub fn new(content: Option<String>) -> Self {
        let num = NEXT_MARKDOWN_NUM.fetch_add(1, Ordering::Relaxed);
        Self {
            id: format!("md-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            content: content.unwrap_or_default(),
            fg: None,
            bg: None,
            table_border_color: None,
        }
    }

    // ── Builder-style setters ──────────────────────────────────

    pub fn set_content(&mut self, value: String) {
        self.content = value;
    }

    pub fn set_fg(&mut self, value: Option<ColorInput>) {
        self.fg = value.map(parse_color);
    }

    pub fn set_bg(&mut self, value: Option<ColorInput>) {
        self.bg = value.map(parse_color);
    }

    /// Override the colour used for table borders and separators.
    /// When `None` (the default), the palette's muted colour is used.
    pub fn set_table_border_color(&mut self, value: Option<ColorInput>) {
        self.table_border_color = value.map(parse_color);
    }

    // ── Accessors ──────────────────────────────────────────────

    pub fn content(&self) -> &str {
        &self.content
    }

    fn default_fg(&self) -> RGBA {
        self.fg.unwrap_or(RGBA::from_ints(220, 220, 220, 255))
    }

    fn default_bg(&self) -> RGBA {
        self.bg.unwrap_or(RGBA::from_ints(0, 0, 0, 0))
    }

    /// Build a palette from the configured fg/bg.
    fn palette(&self) -> MarkdownPalette {
        MarkdownPalette::new(self.default_fg(), self.default_bg())
    }

    // ── Rendering helpers ──────────────────────────────────────

    /// Write `text` into the buffer one character at a time, wrapping at
    /// `max_x`.  Leading spaces after a wrap are skipped to avoid visual
    /// indentation on continuation lines.
    #[allow(clippy::too_many_arguments)]
    fn render_text(
        text: &str,
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        style: Style,
    ) {
        for ch in text.chars() {
            if *x >= max_x {
                *y += 1;
                *x = area_x;
                if *y >= max_y {
                    break;
                }
                if ch == ' ' {
                    continue;
                }
            }
            if let Some(cell) = buf.cell_mut((*x, *y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            *x += 1;
        }
    }

    /// Fill a whole row with a solid background style.
    fn fill_row(buf: &mut Buffer, x: u16, y: u16, max_x: u16, style: Style) {
        for cx in x..max_x {
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_style(style);
                cell.set_char(' ');
            }
        }
    }

    /// Derive a syntax-highlighted style from a category.
    fn highlight_style(cat: Option<HighlightCategory>, default_fg: Color, bg: Color) -> Style {
        let fg = match cat {
            Some(HighlightCategory::Keyword) => Color::Rgb(255, 180, 100),
            Some(HighlightCategory::String) => Color::Rgb(150, 200, 150),
            Some(HighlightCategory::Comment) => Color::Rgb(130, 130, 140),
            Some(HighlightCategory::Type) => Color::Rgb(100, 180, 255),
            Some(HighlightCategory::Function) => Color::Rgb(200, 180, 255),
            Some(HighlightCategory::Number) => Color::Rgb(255, 200, 100),
            Some(HighlightCategory::Builtin) => Color::Rgb(100, 200, 255),
            None => default_fg,
        };
        Style::default().fg(fg).bg(bg)
    }
}

// ── Renderable trait ─────────────────────────────────────────────

impl Renderable for MarkdownRenderable {
    fn id(&self) -> &str {
        &self.id
    }

    fn add_child(&mut self, child: Box<dyn Renderable>) -> usize {
        let idx = self.children.len();
        self.children.push(child);
        idx
    }

    fn remove_child(&mut self, id: &str) {
        self.children.retain(|c| c.id() != id);
    }

    fn insert_child_before(
        &mut self,
        child: Box<dyn Renderable>,
        anchor_id: &str,
    ) -> Option<usize> {
        if let Some(pos) = self.children.iter().position(|c| c.id() == anchor_id) {
            self.children.insert(pos, child);
            Some(pos)
        } else {
            None
        }
    }

    fn num(&self) -> u64 {
        self.num
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focusable(&self) -> bool {
        self.focusable
    }

    fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    fn parent_num(&self) -> Option<u64> {
        self.parent_num
    }

    fn set_parent_num(&mut self, parent_num: Option<u64>) {
        self.parent_num = parent_num;
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn children(&self) -> &[Box<dyn Renderable>] {
        &self.children
    }

    #[allow(clippy::too_many_lines)]
    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        if self.content.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        let palette = self.palette();
        let bg_color = palette.bg_color();

        // ── Fill entire area ────────────────────────────────────
        let fill_style = Style::default().bg(bg_color);
        for row in area.y..max_y {
            for col in area.x..max_x {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(fill_style);
                    cell.set_char(' ');
                }
            }
        }

        // ── State ───────────────────────────────────────────────
        let mut ctx = MarkdownContext::new();
        let mut y = area.y;
        let mut x = area.x;

        // Table-buffering state
        let mut in_table = false;
        let mut tbl_headers: Vec<String> = Vec::new();
        let mut tbl_rows: Vec<Vec<String>> = Vec::new();
        let mut tbl_cur_row: Vec<String> = Vec::new();
        let mut tbl_cur_cell = String::new();
        let mut tbl_in_header = false;

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        let parser = pulldown_cmark::Parser::new_ext(&self.content, options);

        for event in parser {
            if !in_table && y >= max_y {
                break;
            }

            // ── Table buffering mode ────────────────────────────
            if in_table {
                match event {
                    Event::Start(tag) => {
                        ctx.handle_start(&tag);
                        match &tag {
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
                        match &tag_end {
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
                                // pulldown_cmark 0.13: header cells are direct TableHead children
                                // (no TableRow wrapper), so we flush here instead of at TableRow.
                                if tbl_in_header && !tbl_cur_row.is_empty() {
                                    tbl_headers = std::mem::take(&mut tbl_cur_row);
                                }
                                tbl_in_header = false;
                            }
                            TagEnd::Table => {
                                // Render the complete table
                                Self::render_table(
                                    buf, &mut x, &mut y,
                                    area.x, max_x, max_y,
                                    &tbl_headers, &tbl_rows,
                                    &palette,
                                    self.table_border_color.as_ref(),
                                );
                                in_table = false;
                            }
                            _ => {}
                        }
                        ctx.handle_end(&tag_end);
                    }
                    Event::Text(text) | Event::InlineHtml(text) | Event::Code(text) => {
                        tbl_cur_cell.push_str(&text);
                    }
                    Event::SoftBreak | Event::HardBreak => {
                        tbl_cur_cell.push('\n');
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                // ── Block / inline start ────────────────────────
                Event::Start(tag) => {
                    ctx.handle_start(&tag);
                    match &tag {
                        Tag::Table(_) => {
                            // Start table buffering
                            in_table = true;
                            tbl_headers.clear();
                            tbl_rows.clear();
                            tbl_cur_row.clear();
                            tbl_cur_cell.clear();
                            tbl_in_header = false;

                            // Ensure we're on a fresh line
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                        }
                        Tag::Paragraph | Tag::BlockQuote(_)
                        | Tag::Heading { .. } => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                        }
                        Tag::CodeBlock(_) => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                            if y < max_y {
                                let cb_bg = palette.code_bg_color();
                                Self::fill_row(buf, area.x, y, max_x, Style::default().bg(cb_bg));
                            }
                        }
                        Tag::List(_start) => {}
                        Tag::Item => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                            let marker_style = Style::default().fg(rgba_to_color(palette.list_marker_color()));
                            let marker = ctx
                                .list_marker()
                                .map_or_else(|| "• ".to_string(), |(ordered, num)| {
                                    if ordered {
                                        format!("{num}. ")
                                    } else {
                                        "• ".to_string()
                                    }
                                });
                            Self::render_text(
                                &marker, buf, &mut x, &mut y,
                                area.x, max_x, max_y, marker_style,
                            );
                        }
                        Tag::TableHead
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

                // ── Block / inline end ──────────────────────────
                Event::End(tag_end) => {
                    match &tag_end {
                        TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::BlockQuote(_)
                        | TagEnd::Item => {
                            y += 1;
                            x = area.x;
                        }
                        TagEnd::CodeBlock => {
                            y += 1;
                            x = area.x;
                        }
                        TagEnd::TableRow
                        | TagEnd::Table => {
                            y += 1;
                            x = area.x;
                        }
                        TagEnd::List(_) => {}
                        TagEnd::TableCell => {
                            x = x.saturating_add(2);
                        }
                        TagEnd::TableHead
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
                    }
                    ctx.handle_end(&tag_end);
                }

                // ── Text content ────────────────────────────────
                Event::Text(text)
                | Event::FootnoteReference(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::InlineHtml(text) => {
                    if ctx.in_code_block() {
                        self.render_code_block(
                            &text, buf, &mut x, &mut y,
                            area.x, max_x, max_y, ctx.code_block_lang(),
                        );
                    } else {
                        let element = ctx.current_element();
                        let heading_level = ctx.heading_level();
                        let mut style = palette.style_for(element, heading_level);
                        if ctx.in_blockquote() {
                            style = style.fg(rgba_to_color(palette.muted_color()));
                        }
                        Self::render_text(
                            &text, buf, &mut x, &mut y,
                            area.x, max_x, max_y, style,
                        );
                    }
                }

                // ── Inline code ─────────────────────────────────
                Event::Code(text) => {
                    let style = palette.style_for(Some(MarkdownElement::InlineCode), None);
                    Self::render_text(
                        &text, buf, &mut x, &mut y,
                        area.x, max_x, max_y, style,
                    );
                }

                // ── Raw HTML ────────────────────────────────────
                Event::Html(html) => {
                    let style = Style::default().fg(rgba_to_color(palette.muted_color()));
                    Self::render_text(
                        &html, buf, &mut x, &mut y,
                        area.x, max_x, max_y, style,
                    );
                }

                // ── Line breaks ─────────────────────────────────
                Event::SoftBreak | Event::HardBreak => {
                    x = area.x;
                    y += 1;
                }

                // ── Horizontal rule ─────────────────────────────
                Event::Rule => {
                    if y < max_y {
                        let rule_style = Style::default().fg(rgba_to_color(palette.muted_color()));
                        for cx in area.x..max_x {
                            if let Some(cell) = buf.cell_mut((cx, y)) {
                                cell.set_char('─');
                                cell.set_style(rule_style);
                            }
                        }
                        y += 1;
                        x = area.x;
                    }
                }

                // ── Task list markers ───────────────────────────
                Event::TaskListMarker(checked) => {
                    let marker = if checked { "[x] " } else { "[ ] " };
                    let style = Style::default().fg(rgba_to_color(palette.list_marker_color()));
                    Self::render_text(
                        marker, buf, &mut x, &mut y,
                        area.x, max_x, max_y, style,
                    );
                }
            }
        }
    }
}

// ── Code-block rendering ─────────────────────────────────────────

impl MarkdownRenderable {
    /// Render a code block segment with syntax highlighting (via tree-sitter)
    /// when a language is declared.
    #[allow(clippy::too_many_arguments)]
    fn render_code_block(
        &self,
        text: &str,
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        lang: &str,
    ) {
        let palette = self.palette();
        let code_bg = palette.code_bg_color();
        let default_fg = rgba_to_color(palette.text_color());

        // When no language is specified (e.g. LLM output without ```lang),
        // default to JavaScript — the most popular language, with syntax
        // similar to many others (C, Java, TypeScript, etc.).
        let effective_lang = if lang.is_empty() { "javascript" } else { lang };

        // Build byte-to-category map for syntax highlighting
        let spans = highlight(text, effective_lang);
        let mut cat_map: Vec<Option<HighlightCategory>> = vec![None; text.len()];
        if let Some(ref spans) = spans {
            for span in spans {
                for item in &mut cat_map[span.start..span.end.min(text.len())] {
                    *item = Some(span.category);
                }
            }
        }

        let mut byte_offset = 0;

        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                *y += 1;
                *x = area_x;
                if *y >= max_y {
                    break;
                }
            }

            // Fill the entire line with code-block background
            Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));

            // Render each character with its highlight style
            for (ci, ch) in line.char_indices() {
                if *x >= max_x {
                    break;
                }
                let byte_pos = byte_offset + ci;
                let cat = cat_map.get(byte_pos).copied().flatten();
                let style = Self::highlight_style(cat, default_fg, code_bg);
                if let Some(cell) = buf.cell_mut((*x, *y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                *x += 1;
            }
            byte_offset += line.len() + 1;
        }
    }
}

// ── Table rendering ─────────────────────────────────────────────

impl MarkdownRenderable {
    /// Render a markdown table as a grid with borders.
    #[allow(clippy::too_many_arguments)]
    fn render_table(
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        headers: &[String],
        rows: &[Vec<String>],
        palette: &MarkdownPalette,
        table_border_color: Option<&RGBA>,
    ) {
        if headers.is_empty() || max_x <= area_x {
            return;
        }

        let col_count = headers.len();

        // ── Calculate column widths ─────────────────────────────
        let mut col_widths: Vec<u16> = headers.iter().map(|h| h.chars().count() as u16).collect();
        for row in rows {
            for (ci, cell) in row.iter().enumerate() {
                if ci < col_count {
                    let cw = cell.chars().count() as u16;
                    col_widths[ci] = col_widths[ci].max(cw);
                }
            }
        }

        // Clamp total width to available space
        let padding: u16 = 1; // 1 char padding on each side
        let border_gaps = if col_count > 1 { col_count as u16 - 1 } else { 0 };
        let total_w: u16 = col_widths.iter().map(|w| w + 2 * padding).sum::<u16>() + border_gaps;
        let available = max_x.saturating_sub(area_x);
        if total_w > available {
            // Scale columns proportionally
            let scale = available as f64 / total_w as f64;
            for w in &mut col_widths {
                *w = (*w as f64 * scale).max(3.0) as u16; // min 3 chars per column
            }
        }

        let border_color = table_border_color
            .map(|c| rgba_to_color(*c))
            .unwrap_or_else(|| rgba_to_color(palette.muted_color()));
        let border_style = Style::default().fg(border_color);
        let text_style = Style::default().fg(rgba_to_color(palette.text_color()));
        let header_style = Style::default()
            .fg(rgba_to_color(palette.text_color()))
            .add_modifier(ratatui::style::Modifier::BOLD);

        // Compute column start positions
        let mut col_starts: Vec<u16> = Vec::with_capacity(col_count);
        let mut cx = area_x;
        for &cw in &col_widths {
            col_starts.push(cx);
            cx += cw + 2 * padding;
            cx += 1; // border between columns
        }

        // Helper to render a border line
        let render_border = |buf: &mut Buffer, y: u16, left: char, _mid: char, right: char, sep: char| {
            if y >= max_y {
                return;
            }
            for ci in 0..col_count {
                let sx = col_starts[ci];
                let cw = col_widths[ci] + 2 * padding;
                let start_char = if ci == 0 { left } else { sep };
                // Left edge: draw corner/sep at sx-1 (saturates to 0 for ci=0)
                let corner_pos = sx.saturating_sub(1);
                if let Some(cell) = buf.cell_mut((corner_pos, y)) {
                    cell.set_char(start_char);
                    cell.set_style(border_style);
                }
                // Horizontal line: skip the corner position (already drawn above)
                for dx in 0..cw {
                    let px = sx + dx;
                    if px == corner_pos && ci == 0 {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((px, y)) {
                        cell.set_char('─');
                        cell.set_style(border_style);
                    }
                }
                let ex = sx + cw;
                let corner = if ci + 1 < col_count { sep } else { right };
                if let Some(cell) = buf.cell_mut((ex, y)) {
                    cell.set_char(corner);
                    cell.set_style(border_style);
                }
            }
        };

        // Helper to render a row of cells
        let render_row = |buf: &mut Buffer, y: u16, cells: &[String], is_header: bool| {
            if y >= max_y {
                return;
            }
            let cell_style = if is_header { header_style } else { text_style };
            for ci in 0..col_count {
                let sx = col_starts[ci];
                let content = cells.get(ci).map(|s| s.as_str()).unwrap_or("");

                // Vertical border on the left of first cell
                if ci == 0 {
                    if let Some(cell) = buf.cell_mut((sx.saturating_sub(1), y)) {
                        cell.set_char('│');
                        cell.set_style(border_style);
                    }
                }

                // Render cell content with padding
                let mut cx = sx + padding;
                for ch in content.chars() {
                    if cx >= sx + col_widths[ci] + padding {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(ch);
                        cell.set_style(cell_style);
                    }
                    cx += 1;
                }

                // Vertical border on the right of each cell
                let ex = sx + col_widths[ci] + 2 * padding;
                if let Some(cell) = buf.cell_mut((ex, y)) {
                    cell.set_char('│');
                    cell.set_style(border_style);
                }
            }
        };

        // ── Top border ──────────────────────────────────────────
        render_border(buf, *y, '┌', '─', '┐', '┬');
        *y += 1;

        // ── Header row ──────────────────────────────────────────
        if *y < max_y {
            render_row(buf, *y, headers, true);
            *y += 1;
        }

        // ── Header/body separator ───────────────────────────────
        if *y < max_y {
            render_border(buf, *y, '├', '─', '┤', '┼');
            *y += 1;
        }

        // ── Body rows ───────────────────────────────────────────
        for row in rows {
            if *y >= max_y {
                break;
            }
            render_row(buf, *y, row, false);
            *y += 1;
        }

        // ── Bottom border ───────────────────────────────────────
        if *y < max_y {
            render_border(buf, *y, '└', '─', '┘', '┴');
            *y += 1;
        }

        *x = area_x;
    }
}

// ── Colour helpers ───────────────────────────────────────────────

fn rgba_to_color(c: RGBA) -> Color {
    let (r, g, b, _) = c.to_ints();
    Color::Rgb(r, g, b)
}
