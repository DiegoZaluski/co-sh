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
use super::styles::MarkdownPalette;

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

    #[allow(clippy::too_many_lines)]
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
                                || "• ".to_string(),
                                |(ordered, num)| {
                                    if ordered {
                                        format!("{num}. ")
                                    } else {
                                        "• ".to_string()
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
                        if ctx.in_blockquote() {
                            let bq_bg = palette.quote_bg_color();

                            // Calculate box width: starts at left edge (area.x),
                            // ends just past the text content.
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
                                    cell.set_style(Style::default().bg(bq_bg));
                                    cell.set_char(' ');
                                }
                            }

                            style = style
                                .fg(Color::Rgb(0, 0, 0))
                                .bg(bq_bg)
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
                    let marker = if checked { "[x] " } else { "[ ] " };
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
        let cb_start = std::time::Instant::now();
        let palette = self.palette();
        let code_bg = palette.code_bg_color();
        let default_fg = rgba_to_color(palette.text_color());

        // When no language is specified (e.g. LLM output without ```lang),
        // default to JavaScript — the most popular language, with syntax
        // matching many others (C, Java, TypeScript, etc.).
        let effective_lang = if lang.is_empty() { "javascript" } else { lang };

        // Build byte-to-category map for syntax highlighting (cached)
        let spans: Option<Vec<HighlightSpan>> = if self.streaming {
            None
        } else {
            let key = highlight_cache_key(text, effective_lang);
            #[allow(clippy::unwrap_used)]
            let mut cache = HIGHLIGHT_CACHE.lock().unwrap();
            cache.get(&key).cloned().or_else(|| {
                let computed = highlight(text, effective_lang);
                if let Some(ref spans) = computed {
                    cache.push(key, spans.clone());
                }
                computed
            })
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
        let code_pad = 2u16;
        let code_pad_v = 1u16;

        // The CodeBlock start handler already drew the top gap row (filled
        // with the code background), so the first code line begins on the
        // next row via the loop below. Only a bottom gap is added here.
        *x = area_x.saturating_add(code_pad);

        for line in text.lines() {
            // Every code line (including the first) gets its own fresh row,
            // so the top-gap row above is preserved.
            if *y >= max_y {
                break;
            }
            *y += 1;
            *x = area_x.saturating_add(code_pad);
            if *y >= max_y {
                break;
            }

            // Fill the entire line with code-block background
            Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
            *x = area_x.saturating_add(code_pad);

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
                        code_pad,
                    );
                    if *y >= max_y {
                        break;
                    }
                    *y += 1;
                    *x = area_x.saturating_add(code_pad);
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
                        code_pad,
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
                code_pad,
            );
            byte_offset += line.len() + 1;
        }

        // Internal bottom padding (blank background rows)
        *x = area_x;
        for _ in 0..code_pad_v {
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
        let border_gaps = if col_count > 1 {
            col_count as u16 - 1
        } else {
            0
        };
        let total_w: u16 = col_widths.iter().map(|w| w + 2 * padding).sum::<u16>() + border_gaps;
        let available = max_x.saturating_sub(area_x);
        if total_w > available {
            // Scale columns proportionally
            let scale = f64::from(available) / f64::from(total_w);
            for w in &mut col_widths {
                *w = (f64::from(*w) * scale).max(3.0) as u16; // min 3 chars per column
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

        // Compute column start positions
        let mut col_starts: Vec<u16> = Vec::with_capacity(col_count);
        let mut cx = area_x;
        for &cw in &col_widths {
            col_starts.push(cx);
            cx += cw + 2 * padding;
            cx += 1; // border between columns
        }

        // Helper to render a border line
        let render_border =
            |buf: &mut Buffer, y: u16, left: char, _mid: char, right: char, sep: char| {
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
                let content = cells.get(ci).map_or("", |s| s.as_str());

                // Vertical border on the left of first cell
                if let Some(cell) = buf.cell_mut((sx.saturating_sub(1), y))
                    && ci == 0
                {
                    cell.set_char('│');
                    cell.set_style(border_style);
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

const fn rgba_to_color(c: RGBA) -> Color {
    let (r, g, b, _) = c.to_ints();
    Color::Rgb(r, g, b)
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
                            || "• ".to_string(),
                            |(ordered, num)| {
                                if ordered {
                                    format!("{num}. ")
                                } else {
                                    "• ".to_string()
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
                if checked {
                    result.push_str("[x] ");
                } else {
                    result.push_str("[ ] ");
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
