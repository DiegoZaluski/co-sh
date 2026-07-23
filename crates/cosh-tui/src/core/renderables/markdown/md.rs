use std::any::Any;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;

use pulldown_cmark::{Event, Options, Tag, TagEnd};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh_sdk::tree_sitter::highlight::{HighlightCategory, HighlightSpan, highlight};

use crate::core::renderable::Renderable;
use crate::core::rgba::RGBA;
use crate::core::rgba::{ColorInput, parse_color};

use super::context::{MarkdownContext, MarkdownElement};
use super::styles::{MarkdownPalette, rgba_to_ratatui as rgba_to_color};

// ── Constants ────────────────────────────────────────────────────
/// Unicode bullet character for unordered list items.
const LIST_BULLET: &str = "• ";
/// Horizontal padding (left/right) inside code blocks.
const CODE_PAD_H: u16 = 2;
/// Vertical padding rows inside code blocks (top/bottom).
const CODE_PAD_V: u16 = 1;

static NEXT_MARKDOWN_NUM: AtomicU64 = AtomicU64::new(1);

#[allow(clippy::unwrap_used)]
static HIGHLIGHT_CACHE: std::sync::LazyLock<Mutex<LruCache<u64, Vec<HighlightSpan>>>> =
    std::sync::LazyLock::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));

fn highlight_cache_key(text: &str, lang: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    lang.hash(&mut hasher);
    hasher.finish()
}

/// Renders markdown content into a fixed-area \`Buffer\`.
///
/// Mirrors \``OpenTUI`\`'s \``MarkdownRenderable`\` in spirit:
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
    /// When true, skip tree-sitter syntax highlighting and render code blocks
    /// as plain text. Set during LLM streaming to avoid ~10ms `highlight()` calls
    /// on every frame while code block text is still growing.
    streaming: bool,
}

impl MarkdownRenderable {
    #[must_use]
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
            streaming: false,
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

    /// When streaming, skip tree-sitter syntax highlighting to avoid
    /// re-highlighting every frame as code block text grows.
    pub const fn set_streaming(&mut self, streaming: bool) {
        self.streaming = streaming;
    }

    // ── Accessors ──────────────────────────────────────────────

    #[must_use]
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

    /// Write `text` into the buffer one grapheme at a time, wrapping at
    /// `max_x`.  Accounts for each grapheme's display width (e.g. CJK,
    /// emoji, flag pairs, ZWJ sequences are 2 columns wide) to prevent
    /// visual corruption and line-wrapping errors.
    ///
    /// Leading spaces after a wrap are skipped to avoid visual indentation
    /// on continuation lines.
    fn render_text(
        text: &str,
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        style: Style,
        bq_indent: u16,
    ) {
        let mut word = String::new();
        let mut word_w = 0u16;

        for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(text) {
            if grapheme == "\n" {
                Self::flush_render_word(
                    &mut word,
                    &mut word_w,
                    buf,
                    x,
                    y,
                    area_x,
                    max_x,
                    max_y,
                    style,
                    bq_indent,
                );
                if *y >= max_y {
                    return;
                }
                *y += 1;
                *x = area_x.saturating_add(bq_indent);
                continue;
            }

            if grapheme == " " {
                Self::flush_render_word(
                    &mut word,
                    &mut word_w,
                    buf,
                    x,
                    y,
                    area_x,
                    max_x,
                    max_y,
                    style,
                    bq_indent,
                );
                if *y >= max_y {
                    return;
                }
                if *x < max_x && *x > area_x.saturating_add(bq_indent) {
                    if let Some(cell) = buf.cell_mut((*x, *y)) {
                        cell.set_char(' ');
                        cell.set_style(style);
                    }
                    *x += 1;
                }
                continue;
            }

            word.push_str(grapheme);
            word_w += w;
        }

        Self::flush_render_word(
            &mut word,
            &mut word_w,
            buf,
            x,
            y,
            area_x,
            max_x,
            max_y,
            style,
            bq_indent,
        );
    }

    /// Flush the accumulated word to the buffer, wrapping to the next line if
    /// it doesn't fit on the current line.
    fn flush_render_word(
        word: &mut String,
        word_w: &mut u16,
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        style: Style,
        bq_indent: u16,
    ) {
        if *word_w == 0 {
            return;
        }
        if *x + *word_w > max_x && *x > area_x.saturating_add(bq_indent) {
            *y += 1;
            *x = area_x.saturating_add(bq_indent);
        }
        if *y >= max_y {
            word.clear();
            *word_w = 0;
            return;
        }
        for (g, gw) in crate::core::lib::unicode_util::graphemes_with_width(word) {
            if let Some(cell) = buf.cell_mut((*x, *y)) {
                if g.len() == 1 {
                    if let Some(c) = g.chars().next() {
                        cell.set_char(c);
                    }
                } else {
                    cell.set_symbol(g);
                }
                cell.set_style(style);
            }
            if gw > 1 {
                for dx in 1..gw {
                    if let Some(cell) = buf.cell_mut((*x + dx, *y)) {
                        cell.set_diff_option(CellDiffOption::Skip);
                    }
                }
            }
            *x += gw;
        }
        word.clear();
        *word_w = 0;
    }

    /// Flush the accumulated word parts for code-block rendering.
    /// If the word doesn't fit on the current line, wraps to the next line
    /// (including filling its background row).
    fn flush_code_word(
        word_parts: &mut Vec<(&str, u16, Style)>,
        word_w: &mut u16,
        buf: &mut Buffer,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        code_bg: Color,
        code_pad: u16,
    ) {
        if *word_w == 0 {
            return;
        }
        if *x + *word_w > max_x && *x > area_x + code_pad {
            *y += 1;
            *x = area_x + code_pad;
            if *y < max_y {
                Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
            }
        }
        if *y >= max_y {
            word_parts.clear();
            *word_w = 0;
            return;
        }
        for (grapheme, w, style) in word_parts.drain(..) {
            if let Some(cell) = buf.cell_mut((*x, *y)) {
                if grapheme.len() == 1 {
                    if let Some(c) = grapheme.chars().next() {
                        cell.set_char(c);
                    }
                } else {
                    cell.set_symbol(grapheme);
                }
                cell.set_style(style);
            }
            if w > 1 {
                for dx in 1..w {
                    if let Some(next_cell) = buf.cell_mut((*x + dx, *y)) {
                        next_cell.set_diff_option(CellDiffOption::Skip);
                    }
                }
            }
            *x += w;
        }
        *word_w = 0;
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

    fn render_self(&self, buf: &mut Buffer, area: Rect) {
        if self.content.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let start = std::time::Instant::now();

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
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_STRIKETHROUGH);
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
                                    buf,
                                    &mut x,
                                    &mut y,
                                    area.x,
                                    max_x,
                                    max_y,
                                    &tbl_headers,
                                    &tbl_rows,
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
                        Tag::Paragraph | Tag::Heading { .. } => {
                            if x != area.x {
                                if ctx.in_blockquote() {
                                    // Inside a blockquote: x was set to area.x + 2 by
                                    // the BlockQuote handler (bar + indent). Don't advance
                                    // y — content should stay on the same row as the bar.
                                } else {
                                    y += 1;
                                    x = area.x;
                                }
                            }
                            if ctx.in_blockquote() && x == area.x {
                                // Second+ paragraph inside blockquote: re-apply indent.
                                x = area.x.saturating_add(2);
                            }
                        }
                        Tag::BlockQuote(_) => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                            if y < max_y {
                                x = area.x.saturating_add(2);
                            }
                        }
                        Tag::CodeBlock(_) => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                            // Code block background fill row (1 row of margin via para TagEnd)
                            if y < max_y {
                                y += 1;
                                x = area.x;
                            }
                            if y < max_y {
                                let cb_bg = palette.code_bg_color();
                                Self::fill_row(buf, area.x, y, max_x, Style::default().bg(cb_bg));

                                // Draw language label on the top gap row
                                let lang = ctx.code_block_lang();
                                if !lang.is_empty() {
                                    let label_style = Style::default()
                                        .fg(rgba_to_color(palette.muted_color()))
                                        .bg(cb_bg)
                                        .add_modifier(Modifier::ITALIC);
                                    let mut lx = area.x.saturating_add(2);
                                    for ch in lang.chars() {
                                        if lx >= max_x {
                                            break;
                                        }
                                        if let Some(cell) = buf.cell_mut((lx, y)) {
                                            cell.set_char(ch);
                                            cell.set_style(label_style);
                                        }
                                        lx += 1;
                                    }
                                }
                            }
                        }
                        Tag::List(_start) => {}
                        Tag::Item => {
                            if x != area.x {
                                y += 1;
                                x = area.x;
                            }
                            let marker_style =
                                Style::default().fg(rgba_to_color(palette.list_marker_color()));
                            let marker = ctx.list_marker().map_or_else(
                                || LIST_BULLET.to_string(),
                                |(ordered, num)| {
                                    if ordered {
                                        format!("{num}. ")
                                    } else {
                                        LIST_BULLET.to_string()
                                    }
                                },
                            );
                            Self::render_text(
                                &marker,
                                buf,
                                &mut x,
                                &mut y,
                                area.x,
                                max_x,
                                max_y,
                                marker_style,
                                0,
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
                        | TagEnd::Item
                        | TagEnd::CodeBlock
                        | TagEnd::TableRow
                        | TagEnd::Table => {
                            y += 1;
                            x = area.x;
                        }
                        TagEnd::List(_)
                        | TagEnd::TableHead
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

                // ── Text content ────────────────────────────────
                Event::Text(text)
                | Event::FootnoteReference(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::InlineHtml(text) => {
                    if ctx.in_code_block() {
                        self.render_code_block(
                            &text,
                            buf,
                            &mut x,
                            &mut y,
                            area.x,
                            max_x,
                            max_y,
                            ctx.code_block_lang(),
                        );
                    } else {
                        let element = ctx.current_element();
                        let heading_level = ctx.heading_level();
                        let mut style = palette.style_for(element, heading_level);
                        let bq_indent = if ctx.in_blockquote() { 2u16 } else { 0u16 };
                        if ctx.in_blockquote() && text.starts_with('⚠') {
                            // Warning blockquote: apply yellow background + black text + bold
                            // (Only for the warning, not regular blockquotes)
                            let warning_bg = Color::Rgb(238, 241, 112);

                            // Calculate box width
                            let first_line = text.lines().next().unwrap_or("");
                            let text_w =
                                crate::core::lib::unicode_util::str_display_width(first_line)
                                    as u16;
                            let box_end = area
                                .x
                                .saturating_add(2) // indent
                                .saturating_add(text_w) // text width
                                .saturating_add(2) // padding after text
                                .min(max_x);

                            // Fill the background from left edge to past text
                            for cx in area.x..box_end {
                                if let Some(cell) = buf.cell_mut((cx, y)) {
                                    cell.set_style(Style::default().bg(warning_bg));
                                    cell.set_char(' ');
                                }
                            }

                            style = style
                                .fg(Color::Rgb(0, 0, 0))
                                .bg(warning_bg)
                                .add_modifier(Modifier::BOLD);
                        }
                        Self::render_text(
                            &text, buf, &mut x, &mut y, area.x, max_x, max_y, style, bq_indent,
                        );
                    }
                }

                // ── Inline code ─────────────────────────────────
                Event::Code(text) => {
                    let style = palette.style_for(Some(MarkdownElement::InlineCode), None);
                    Self::render_text(&text, buf, &mut x, &mut y, area.x, max_x, max_y, style, 0);
                }

                // ── Raw HTML ────────────────────────────────────
                Event::Html(html) => {
                    let style = Style::default().fg(rgba_to_color(palette.muted_color()));
                    Self::render_text(&html, buf, &mut x, &mut y, area.x, max_x, max_y, style, 0);
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
                    // Use Unicode checkbox symbols for a more polished look
                    let marker = if checked { "☑ " } else { "☐ " };
                    let style = Style::default().fg(rgba_to_color(palette.list_marker_color()));
                    Self::render_text(marker, buf, &mut x, &mut y, area.x, max_x, max_y, style, 0);
                }
            }
        }

        let elapsed = start.elapsed().as_micros();
        if elapsed > 500 {
            log::debug!(
                "[PERF] markdown_render_self: content_len={} area={}x{} elapsed={elapsed}us",
                self.content.len(),
                area.width,
                area.height
            );
        }
    }
}

// ── Code-block rendering ─────────────────────────────────────────

impl MarkdownRenderable {
    /// Render a code block segment with syntax highlighting (via tree-sitter)
    /// when a language is declared.
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
        let cb_start = std::time::Instant::now();
        let palette = self.palette();
        let code_bg = palette.code_bg_color();
        let default_fg = rgba_to_color(palette.text_color());

        // Syntax highlighting strategy:
        // - ``` (no language tag) → no highlighting, render as plain text
        // - ```lang (known/supported) → use tree-sitter highlighting
        // - ```lang (unknown/unsupported) → fall back to JavaScript (versatile default)
        //
        // Build byte-to-category map for syntax highlighting (cached)
        let spans: Option<Vec<HighlightSpan>> = if self.streaming || lang.is_empty() {
            None
        } else {
            let key = highlight_cache_key(text, lang);
            #[allow(clippy::unwrap_used)]
            let mut cache = HIGHLIGHT_CACHE.lock().unwrap();
            let result = cache.get(&key).cloned().or_else(|| {
                let computed = highlight(text, lang);
                if let Some(ref spans) = computed {
                    cache.push(key, spans.clone());
                }
                computed
            });

            // Fallback: if the specified language is not supported by tree-sitter,
            // retry with JavaScript as a versatile generic highlighter.
            if result.is_none() && lang != "javascript" {
                let js_key = highlight_cache_key(text, "javascript");
                cache.get(&js_key).cloned().or_else(|| {
                    let computed = highlight(text, "javascript");
                    if let Some(ref spans) = computed {
                        cache.push(js_key, spans.clone());
                    }
                    computed
                })
            } else {
                result
            }
        };
        let mut cat_map: Vec<Option<HighlightCategory>> = vec![None; text.len()];
        if let Some(ref spans) = spans {
            for span in spans {
                for item in &mut cat_map[span.start..span.end.min(text.len())] {
                    *item = Some(span.category);
                }
            }
        }

        let mut byte_offset = 0;

        // The CodeBlock start handler already drew the top gap row (filled
        // with the code background), so the first code line begins on the
        // next row via the loop below. Only a bottom gap is added here.
        *x = area_x.saturating_add(CODE_PAD_H);

        for line in text.lines() {
            // Every code line (including the first) gets its own fresh row,
            // so the top-gap row above is preserved.
            if *y >= max_y {
                break;
            }
            *y += 1;
            *x = area_x.saturating_add(CODE_PAD_H);
            if *y >= max_y {
                break;
            }

            // Fill the entire line with code-block background
            Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
            *x = area_x.saturating_add(CODE_PAD_H);

            // Word-aware rendering with syntax highlighting preservation.
            // Non-space graphemes are buffered into "words"; when a space or
            // overflow is encountered, the whole word is flushed to the buffer.
            let mut remaining_offset = 0;
            let mut word_parts: Vec<(&str, u16, Style)> = Vec::new();
            let mut word_w = 0u16;

            for (grapheme, w) in crate::core::lib::unicode_util::graphemes_with_width(line) {
                let cat = cat_map
                    .get(byte_offset + remaining_offset)
                    .copied()
                    .flatten();
                let style = Self::highlight_style(cat, default_fg, code_bg);
                remaining_offset += grapheme.len();

                if grapheme == "\n" {
                    Self::flush_code_word(
                        &mut word_parts,
                        &mut word_w,
                        buf,
                        x,
                        y,
                        area_x,
                        max_x,
                        max_y,
                        code_bg,
                        CODE_PAD_H,
                    );
                    if *y >= max_y {
                        break;
                    }
                    *y += 1;
                    *x = area_x.saturating_add(CODE_PAD_H);
                    if *y < max_y {
                        Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
                    }
                    continue;
                }

                if grapheme == " " {
                    Self::flush_code_word(
                        &mut word_parts,
                        &mut word_w,
                        buf,
                        x,
                        y,
                        area_x,
                        max_x,
                        max_y,
                        code_bg,
                        CODE_PAD_H,
                    );
                    if *y >= max_y {
                        break;
                    }
                    if *x < max_x {
                        if let Some(cell) = buf.cell_mut((*x, *y)) {
                            cell.set_char(' ');
                            cell.set_style(style);
                        }
                        *x += 1;
                    }
                    continue;
                }

                word_parts.push((grapheme, w, style));
                word_w += w;
            }

            Self::flush_code_word(
                &mut word_parts,
                &mut word_w,
                buf,
                x,
                y,
                area_x,
                max_x,
                max_y,
                code_bg,
                CODE_PAD_H,
            );
            byte_offset += line.len() + 1;
        }

        // Internal bottom padding (blank background rows)
        *x = area_x;
        for _ in 0..CODE_PAD_V {
            if *y >= max_y {
                break;
            }
            *y += 1;
            Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
        }

        // Blank separator row after code block bottom padding
        if *y < max_y {
            *y += 1;
            *x = area_x;
            // This row keeps the default background (not code_bg),
            // providing a true blank separator between the code block
            // background area and the following content.
        }

        let cb_us = cb_start.elapsed().as_micros();
        if cb_us > 500 {
            log::debug!(
                "[PERF] code_block_render: text_len={} lang={} elapsed={cb_us}us",
                text.len(),
                if lang.is_empty() { "none" } else { lang }
            );
        }
    }
}

/// Blend two RGBA colors: result = base * (1 - factor) + overlay * factor.
fn blend_color(base: RGBA, overlay: RGBA, factor: f64) -> RGBA {
    let (br, bg, bb, _) = base.to_ints();
    let (or_, og, ob, _) = overlay.to_ints();
    let t = factor.clamp(0.0, 1.0);
    #[allow(clippy::cast_sign_loss, clippy::suboptimal_flops)]
    RGBA::from_ints(
        (f64::from(br) * (1.0 - t) + f64::from(or_) * t) as u8,
        (f64::from(bg) * (1.0 - t) + f64::from(og) * t) as u8,
        (f64::from(bb) * (1.0 - t) + f64::from(ob) * t) as u8,
        255,
    )
}

// ── Table rendering ─────────────────────────────────────────────

impl MarkdownRenderable {
    /// Render a markdown table as a grid with borders.
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
        let border_gaps = if col_count > 1 {
            col_count as u16 - 1
        } else {
            0
        };
        let total_w: u16 = col_widths.iter().map(|w| w + 2 * padding).sum::<u16>() + border_gaps;
        let available = max_x.saturating_sub(area_x);
        if total_w > available {
            // Scale columns proportionally (integer floor division)
            for w in &mut col_widths {
                *w = ((*w as u32 * available as u32) / total_w as u32) as u16;
            }
            // Ensure minimum width of 1 for every column
            for w in &mut col_widths {
                *w = (*w).max(1);
            }
            // Greedy redistribution: iteratively reduce the largest column
            // until the total fits within the available width.
            // This corrects rounding errors from proportional scaling.
            while col_widths.iter().map(|w| w + 2 * padding).sum::<u16>() + border_gaps > available
            {
                if let Some(max_idx) = (0..col_widths.len())
                    .filter(|&i| col_widths[i] > 1)
                    .max_by_key(|&i| col_widths[i])
                {
                    col_widths[max_idx] -= 1;
                } else {
                    break; // All columns at minimum width, can't reduce further
                }
            }
        }

        let border_color = table_border_color.map_or_else(
            || rgba_to_color(palette.muted_color()),
            |c| rgba_to_color(*c),
        );
        let border_style = Style::default().fg(border_color);
        let text_style = Style::default().fg(rgba_to_color(palette.text_color()));
        let header_style = Style::default()
            .fg(rgba_to_color(palette.text_color()))
            .add_modifier(ratatui::style::Modifier::BOLD);

        // Alternating row background: slightly lighter/dimmer variant of the base bg
        let alt_bg_color = rgba_to_color(blend_color(
            palette.background_color(),
            palette.text_color(),
            0.06,
        ));

        // Compute column start positions
        let mut col_starts: Vec<u16> = Vec::with_capacity(col_count);
        let mut cx = area_x;
        for &cw in &col_widths {
            col_starts.push(cx);
            cx += cw + 2 * padding;
            cx += 1; // border between columns
        }

        // Helper to render a border line
        let render_border = |buf: &mut Buffer, y: u16, left: char, right: char, sep: char| {
            if y >= max_y {
                return;
            }
            for ci in 0..col_count {
                let sx = col_starts[ci];
                let cw = col_widths[ci] + 2 * padding;
                let start_char = if ci == 0 { left } else { sep };
                // Corner position: for the first column (ci=0), the left edge is at `sx`;
                // for subsequent columns, the separator sits between this column and the previous one.
                let corner_pos = if ci == 0 { sx } else { sx.saturating_sub(1) };
                if let Some(cell) = buf.cell_mut((corner_pos, y)) {
                    cell.set_char(start_char);
                    cell.set_style(border_style);
                }
                // Horizontal line: skip the corner position (already drawn above)
                for dx in 0..cw {
                    let px = sx + dx;
                    if px == corner_pos {
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

        // Helper to compute word-wrapped lines for a cell's content.
        // Falls back to character-level breaking when words exceed column width.
        let word_wrap_cell = |content: &str, col_w: u16| -> Vec<String> {
            if col_w < 1 || content.is_empty() {
                return vec![String::new()];
            }
            // First try word-level wrapping (splits on spaces)
            let lines = crate::core::lib::unicode_util::word_wrap(content, col_w);
            // Then break any resulting line that still exceeds col_w at character level
            let mut final_lines: Vec<String> = Vec::new();
            for line in &lines {
                let line_w = crate::core::lib::unicode_util::str_display_width(line) as u16;
                if line_w <= col_w {
                    final_lines.push(line.clone());
                } else {
                    // Character-level breaking for lines wider than col_w
                    let mut current = String::new();
                    let mut current_w = 0u16;
                    for (g, gw) in crate::core::lib::unicode_util::graphemes_with_width(line) {
                        if current_w + gw > col_w && !current.is_empty() {
                            final_lines.push(current);
                            current = String::new();
                            current_w = 0;
                        }
                        current.push_str(g);
                        current_w += gw;
                    }
                    if !current.is_empty() {
                        final_lines.push(current);
                    }
                }
            }
            final_lines
        };

        // Helper to render a single display line from pre-wrapped cell content
        let render_cell_line = |buf: &mut Buffer,
                                y: u16,
                                wrapped: &[Vec<String>],
                                is_header: bool,
                                row_idx: usize,
                                line_idx: usize| {
            if y >= max_y {
                return;
            }
            let cell_style = if is_header { header_style } else { text_style };

            let (actual_border_style, actual_cell_style) =
                if !is_header && row_idx.is_multiple_of(2) {
                    (border_style.bg(alt_bg_color), cell_style.bg(alt_bg_color))
                } else {
                    (border_style, cell_style)
                };

            for ci in 0..col_count {
                let sx = col_starts[ci];
                // Get the pre-wrapped line for this cell at the given line index
                let content = wrapped
                    .get(ci)
                    .and_then(|lines| lines.get(line_idx))
                    .map_or("", |s| s.as_str());

                // Vertical border on the left of each cell
                let vline_x = if ci == 0 { sx } else { sx.saturating_sub(1) };
                if let Some(cell) = buf.cell_mut((vline_x, y)) {
                    cell.set_char('│');
                    cell.set_style(actual_border_style);
                }

                // Render cell content line with padding
                let mut cx = sx + padding;
                for ch in content.chars() {
                    if cx >= sx + col_widths[ci] + padding {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(ch);
                        cell.set_style(actual_cell_style);
                    }
                    cx += 1;
                }

                // Vertical border on the right of each cell
                let ex = sx + col_widths[ci] + 2 * padding;
                if let Some(cell) = buf.cell_mut((ex, y)) {
                    cell.set_char('│');
                    cell.set_style(actual_border_style);
                }
            }
        };

        // Pre-compute word-wrapped lines for all cells in a logical row.
        // Returns (wrapped_cells, max_line_count).
        let precompute_wrapped = |cells: &[String]| -> (Vec<Vec<String>>, usize) {
            let mut wrapped: Vec<Vec<String>> = Vec::with_capacity(col_count);
            let mut max_lines = 1usize;
            for (ci, &cw) in col_widths.iter().enumerate() {
                let content = cells.get(ci).map_or("", |s| s.as_str());
                let lines = word_wrap_cell(content, cw);
                max_lines = max_lines.max(lines.len());
                wrapped.push(lines);
            }
            (wrapped, max_lines)
        };

        // Pre-fill alternating background for all display lines of a row.
        // Fills from the left edge to the rightmost border column (no +1 to avoid bleeding).
        let fill_alt_bg = |buf: &mut Buffer, start_y: u16, nlines: usize| {
            let right_bound = col_starts
                .last()
                .copied()
                .unwrap_or(area_x)
                .saturating_add(col_widths[col_count - 1] + 2 * padding)
                .min(max_x);
            for li in 0..nlines {
                let y_line = start_y + li as u16;
                if y_line >= max_y {
                    break;
                }
                for cx in area_x..right_bound {
                    if let Some(cell) = buf.cell_mut((cx, y_line)) {
                        cell.set_style(Style::default().bg(alt_bg_color));
                        cell.set_char(' ');
                    }
                }
            }
        };

        // ── Top border ──────────────────────────────────────────
        render_border(buf, *y, '┌', '┐', '┬');
        *y += 1;

        // ── Header row (with wrapping) ──────────────────────────
        if *y < max_y {
            let (header_wrapped, header_lines) = precompute_wrapped(headers);
            for li in 0..header_lines {
                if *y >= max_y {
                    break;
                }
                render_cell_line(buf, *y, &header_wrapped, true, 0, li);
                *y += 1;
            }
        }

        // ── Header/body separator ───────────────────────────────
        if *y < max_y {
            render_border(buf, *y, '├', '┤', '┼');
            *y += 1;
        }

        // ── Body rows (word-wrapped with alternating background) ─
        for (row_idx, row) in rows.iter().enumerate() {
            if *y >= max_y {
                break;
            }

            let (wrapped, nlines) = precompute_wrapped(row);

            // Pre-fill alternating background for ALL display lines of this row
            if row_idx % 2 == 0 {
                fill_alt_bg(buf, *y, nlines);
            }

            // Render each display line
            for li in 0..nlines {
                if *y >= max_y {
                    break;
                }
                render_cell_line(buf, *y, &wrapped, false, row_idx, li);
                *y += 1;
            }
        }

        // ── Bottom border ───────────────────────────────────────
        if *y < max_y {
            render_border(buf, *y, '└', '┘', '┴');
            *y += 1;
        }

        *x = area_x;
    }
}

/// Strip markdown formatting and return the visible text content,
/// matching what `MarkdownRenderable` actually displays on screen.
///
/// Uses the same `pulldown_cmark` parser and `MarkdownContext` that
/// the renderer uses, so the output word-wraps and line-breaks
/// identically to the rendered output.
#[must_use]
pub fn markdown_to_visible_text(markdown: &str) -> String {
    if markdown.is_empty() {
        return String::new();
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(markdown, options);
    let mut ctx = MarkdownContext::new();
    let mut result = String::new();

    for event in parser {
        match event {
            Event::Start(tag) => {
                ctx.handle_start(&tag);
                match &tag {
                    Tag::Item => {
                        let marker = ctx.list_marker().map_or_else(
                            || LIST_BULLET.to_string(),
                            |(ordered, num)| {
                                if ordered {
                                    format!("{num}. ")
                                } else {
                                    LIST_BULLET.to_string()
                                }
                            },
                        );
                        result.push_str(&marker);
                    }
                    Tag::Paragraph
                    | Tag::BlockQuote(_)
                    | Tag::Heading { .. }
                    | Tag::CodeBlock(_)
                        if !result.is_empty() && !result.ends_with('\n') =>
                    {
                        result.push('\n');
                    }
                    _ => {}
                }
            }
            Event::End(tag_end) => {
                match &tag_end {
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::BlockQuote(_)
                    | TagEnd::Item
                    | TagEnd::CodeBlock
                    | TagEnd::TableRow
                        if !result.ends_with('\n') =>
                    {
                        result.push('\n');
                    }
                    _ => {}
                }
                ctx.handle_end(&tag_end);
            }
            Event::Text(text) | Event::Code(text) | Event::InlineHtml(text) => {
                result.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak => {
                result.push('\n');
            }
            Event::TaskListMarker(checked) => {
                // Match the Unicode checkbox symbols used in rendered output
                if checked {
                    result.push_str("☑ ");
                } else {
                    result.push_str("☐ ");
                }
            }
            Event::Rule if !result.ends_with('\n') => {
                result.push('\n');
            }
            _ => {}
        }
    }

    result.trim_end().to_string()
}
