use std::any::Any;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lru::LruCache;

use pulldown_cmark::{Event, Options, Tag, TagEnd};
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh_sdk::tree_sitter::highlight::{HighlightCategory, HighlightSpan, highlight};

use crate::core::renderable::Renderable;
use crate::core::rgba::RGBA;
use crate::core::rgba::{ColorInput, parse_color};
use crate::core::types::MouseEvent;

use super::canvas::GrowBuf;
use super::context::{MarkdownContext, MarkdownElement};
use super::parser::{MdBlocks, parse_blocks_incremental};
use super::styles::{MarkdownPalette, rgba_to_ratatui as rgba_to_color};

/// Rate-limiter for the markdown renderer's PERF debug logs: at most one
/// `[PERF] markdown_render_self` / `code_block_render` line per second across
/// the whole renderer. Without this, every re-render of a huge message (a
/// 7k-char markdown block takes 100ms+ to render) writes a log line through a
/// mutex-guarded file — tens of thousands of lines during a single heavy
/// frame, and a multi-hundred-MB debug log over a long session.
static LAST_PERF_LOG_MS: AtomicU64 = AtomicU64::new(0);

fn perf_log_allowed() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_PERF_LOG_MS.load(Ordering::Relaxed);
    now.saturating_sub(last) >= 1000
        && LAST_PERF_LOG_MS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

/// Unicode bullet character for unordered list items.
const LIST_BULLET: &str = "• ";
/// Horizontal padding (left/right) inside code blocks.
const CODE_PAD_H: u16 = 2;
/// Vertical padding rows inside code blocks (top/bottom).
const CODE_PAD_V: u16 = 1;

static NEXT_MARKDOWN_NUM: AtomicU64 = AtomicU64::new(1);

/// One open list level while rendering (phase 4). Mirrors OpenTUI's list
/// layout: markers are drawn at `item_indent`, content starts right after
/// the aligned marker column, and nested lists indent under the parent's
/// content column.
struct ListFrame {
    ordered: bool,
    /// Number the next item gets.
    next_num: usize,
    /// Total marker column width including the trailing space
    /// ("• " → 2, "10. " → 4).
    marker_width: u16,
    /// Column where this level's markers are drawn.
    item_indent: u16,
    /// Column where this level's item content starts/wraps.
    content_indent: u16,
}

/// Compute each list's aligned marker-column width by pre-scanning events.
///
/// OpenTUI pads markers to the widest one in the list (`padStart`), so "9."
/// and "10." align their numbers to the right. We don't know how many items a
/// list has until it ends, hence this cheap pass over already-parsed events;
/// results are returned in list-start order.
pub(crate) fn compute_list_marker_widths(events: &[Event<'_>]) -> Vec<Option<u16>> {
    let mut slots: Vec<Option<u16>> = Vec::new();
    // (slot index, ordered, first number, item count)
    let mut open: Vec<(usize, bool, usize, usize)> = Vec::new();

    for event in events {
        match event {
            Event::Start(Tag::List(start)) => {
                open.push((
                    slots.len(),
                    start.is_some(),
                    usize::try_from(start.unwrap_or(1)).unwrap_or(1),
                    0,
                ));
                slots.push(None);
            }
            Event::Start(Tag::Item) => {
                if let Some(frame) = open.last_mut() {
                    frame.3 += 1;
                }
            }
            Event::End(TagEnd::List(_)) => {
                if let Some((idx, ordered, first, items)) = open.pop() {
                    let width = if items == 0 {
                        2
                    } else if ordered {
                        format!("{}", first.saturating_add(items.saturating_sub(1))).len() + 2 // digits + ". "
                    } else {
                        2 // "• "
                    };
                    slots[idx] = Some(u16::try_from(width).unwrap_or(2).max(2));
                }
            }
            _ => {}
        }
    }

    slots
}

/// A styled run of text inside a table cell: inline formatting (bold, code,
/// emphasis...) survives into tables instead of being flattened to plain
/// strings.
#[derive(Clone)]
struct CellSpan {
    text: String,
    style: Style,
}

/// A table cell: styled spans concatenated left-to-right.
type TableCell = Vec<CellSpan>;

impl CellSpan {
    fn display_width(&self) -> usize {
        crate::core::lib::unicode_util::str_display_width(&self.text)
    }
}

/// LRU memo for [`wrap_cell_spans`] keyed by (cell content hash, column
/// width). Mirrors OpenTUI's per-cell cache (`getTableCellKey` +
/// `TableContentCache`): when a streaming update re-renders a table block,
/// unchanged cells reuse their wrapped styled lines instead of re-wrapping.
/// Memoized wrapped cell lines shared between cache and renderer.
type WrappedCell = std::sync::Arc<Vec<Vec<CellSpan>>>;
type CellWrapCacheKey = (u64, u16);

static CELL_WRAP_CACHE: std::sync::LazyLock<Mutex<LruCache<CellWrapCacheKey, WrappedCell>>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(LruCache::new(NonZeroUsize::new(2048).expect("non-zero")))
    });

/// Word-wrap a styled cell into display lines of styled spans, preserving
/// each grapheme's style. Word semantics mirror `unicode_util::word_wrap`
/// (whole-word moves, character-level breaking for oversized words).
fn wrap_cell_spans(cell: &[CellSpan], col_w: u16) -> WrappedCell {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for span in cell {
        span.text.hash(&mut hasher);
        span.style.hash(&mut hasher);
    }
    let key = (hasher.finish(), col_w);

    #[allow(clippy::unwrap_used)]
    let mut cache = CELL_WRAP_CACHE.lock().unwrap();
    if let Some(hit) = cache.get(&key) {
        return std::sync::Arc::clone(hit);
    }

    let mut b = SpanLineBuilder::new(col_w);
    for span in cell {
        b.push_span_text(&span.text, span.style);
    }
    let wrapped = std::sync::Arc::new(b.finish());
    cache.put(key, std::sync::Arc::clone(&wrapped));
    wrapped
}

/// Incremental span-aware line builder used by [`wrap_cell_spans`].
struct SpanLineBuilder {
    max_w: u16,
    lines: Vec<Vec<CellSpan>>,
    cur: Vec<CellSpan>,
    cur_w: u16,
    word: Vec<CellSpan>,
    word_w: u16,
}

impl SpanLineBuilder {
    fn new(max_w: u16) -> Self {
        Self {
            max_w,
            lines: Vec::new(),
            cur: Vec::new(),
            cur_w: 0,
            word: Vec::new(),
            word_w: 0,
        }
    }

    /// Append a grapheme to `vec`, merging runs that share a style.
    fn append(vec: &mut Vec<CellSpan>, grapheme: &str, style: Style) {
        if let Some(last) = vec.last_mut()
            && last.style == style
        {
            last.text.push_str(grapheme);
            return;
        }
        vec.push(CellSpan {
            text: grapheme.to_string(),
            style,
        });
    }

    fn push_span_text(&mut self, text: &str, style: Style) {
        for (grapheme, gw) in crate::core::lib::unicode_util::graphemes_with_width(text) {
            match grapheme {
                "\n" => {
                    self.flush_word();
                    self.end_line();
                }
                " " => {
                    self.flush_word();
                    if self.cur_w < self.max_w {
                        Self::append(&mut self.cur, " ", style);
                        self.cur_w += 1;
                    }
                }
                _ => {
                    Self::append(&mut self.word, grapheme, style);
                    self.word_w += gw;
                }
            }
        }
    }

    fn end_line(&mut self) {
        self.lines.push(std::mem::take(&mut self.cur));
        self.cur_w = 0;
    }

    fn flush_word(&mut self) {
        if self.word_w == 0 {
            return;
        }
        if self.cur_w + self.word_w > self.max_w && !self.cur.is_empty() {
            self.end_line();
        }
        if self.word_w <= self.max_w {
            let word = std::mem::take(&mut self.word);
            let word_width = self.word_w;
            self.word_w = 0;
            for span in &word {
                Self::append(&mut self.cur, &span.text, span.style);
            }
            self.cur_w += word_width;
            return;
        }
        // The word alone is wider than the whole column: break it at
        // grapheme level so no line exceeds the width.
        let word = std::mem::take(&mut self.word);
        self.word_w = 0;
        for span in &word {
            for (grapheme, gw) in crate::core::lib::unicode_util::graphemes_with_width(&span.text) {
                if self.cur_w + gw > self.max_w && !self.cur.is_empty() {
                    self.end_line();
                }
                Self::append(&mut self.cur, grapheme, span.style);
                self.cur_w += gw;
            }
        }
    }

    fn finish(mut self) -> Vec<Vec<CellSpan>> {
        self.flush_word();
        if !self.cur.is_empty() || self.lines.is_empty() {
            self.lines.push(self.cur);
        }
        self.lines
    }
}

#[allow(clippy::unwrap_used)]
static HIGHLIGHT_CACHE: std::sync::LazyLock<Mutex<LruCache<u64, Vec<HighlightSpan>>>> =
    std::sync::LazyLock::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(1000).unwrap())));

fn highlight_cache_key(text: &str, lang: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    lang.hash(&mut hasher);
    hasher.finish()
}

/// Maximum number of cached block renders per `MarkdownRenderable` instance.
/// Streaming updates reuse entries keyed by block-raw hash; stale entries are
/// evicted by the LRU.
const BLOCK_CACHE_CAPACITY: usize = 512;

/// A clickable link region recorded while rendering a block, in
/// block-relative coordinates (x from 0, y from block start).
#[derive(Debug, Clone)]
pub(crate) struct LinkRegion {
    pub y: u16,
    pub x0: u16,
    /// One past the last column.
    pub x1: u16,
    pub url: String,
}

/// A link region in absolute buffer coordinates, rebuilt on every render.
#[derive(Debug, Clone)]
pub struct ActiveLink {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub url: String,
}

/// A cached render of one top-level markdown block: styled rows ready to be
/// blitted, plus the geometry (height) and the inputs it was produced for.
///
/// `fingerprint` covers every style-affecting input (fg / bg / table border
/// colour) and `width` the layout-affecting input, so an entry is reusable
/// only when both match the current request.
#[derive(Clone)]
struct CachedBlock {
    width: u16,
    fingerprint: u64,
    height: u16,
    rows: Arc<Vec<Vec<Cell>>>,
    links: Vec<LinkRegion>,
}

/// Renders markdown content into a fixed-area \`Buffer\`.
///
/// Mirrors \``OpenTUI`\`'s \``MarkdownRenderable`\` in spirit:
/// - Splits content into top-level blocks (`parser::MdBlocks`) and re-parses
///   only the changed tail on content updates (incremental parsing)
/// - Caches each block's rendered rows and reuses them while the block's raw
///   source, width and palette stay unchanged (per-block reconciliation)
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
    /// Top-level blocks of `content`, updated incrementally in `set_content`.
    blocks: MdBlocks,
    /// LRU cache of rendered blocks, keyed by hash of the block's raw source.
    /// `render_self(&self)` fills this through interior mutability; entries
    /// are validated against width + style fingerprint before reuse.
    block_cache: Mutex<LruCache<u64, CachedBlock>>,
    /// Foreground colour override (falls back to a light grey).
    fg: Option<RGBA>,
    /// Background colour override (falls back to transparent black).
    bg: Option<RGBA>,
    /// Optional table border colour. Falls back to the palette's muted colour.
    table_border_color: Option<RGBA>,
    /// When true (default), link syntax is concealed: only the label is
    /// shown, styled as a link. When false, links render literally as
    /// `[label](url)` (OpenTUI's unconcealed mode).
    conceal: bool,
    /// Clickable link regions in absolute buffer coordinates, rebuilt on
    /// every `render_self`.
    active_links: Mutex<Vec<ActiveLink>>,
}

impl MarkdownRenderable {
    #[must_use]
    pub fn new(content: Option<String>) -> Self {
        let num = NEXT_MARKDOWN_NUM.fetch_add(1, Ordering::Relaxed);
        let mut md = Self {
            id: format!("md-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            content: String::new(),
            blocks: MdBlocks::default(),
            block_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(BLOCK_CACHE_CAPACITY).expect("non-zero"),
            )),
            fg: None,
            bg: None,
            table_border_color: None,
            conceal: true,
            active_links: Mutex::new(Vec::new()),
        };
        if let Some(content) = content {
            md.set_content(content);
        }
        md
    }

    // ── Builder-style setters ──────────────────────────────────

    pub fn set_content(&mut self, value: String) {
        if self.content == value {
            return;
        }
        // Incremental parse: reuse the stable block prefix, re-parse only the
        // changed tail (see `parser::parse_blocks_incremental`).
        let prev = std::mem::take(&mut self.blocks);
        self.blocks = parse_blocks_incremental(&value, Some(&prev));
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

    /// Toggle markdown-syntax concealment (phase 5). Default `true`: link
    /// labels only; `false` renders `[label](url)` literally.
    pub fn set_conceal(&mut self, value: bool) {
        self.conceal = value;
    }

    // ── Accessors ──────────────────────────────────────────────

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Clickable link regions in absolute buffer coordinates, as of the last
    /// `render_self`. Empty until the first render.
    #[must_use]
    pub fn active_links(&self) -> Vec<ActiveLink> {
        #[allow(clippy::unwrap_used)]
        self.active_links.lock().unwrap().clone()
    }

    /// Fingerprint of every style-affecting input. Cached block renders are
    /// keyed by this so a theme change invalidates all entries implicitly.
    fn style_fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for color in [&self.fg, &self.bg, &self.table_border_color] {
            match color {
                Some(c) => {
                    let (r, g, b, _) = c.to_ints();
                    (true, r, g, b).hash(&mut hasher);
                }
                None => false.hash(&mut hasher),
            }
        }
        self.conceal.hash(&mut hasher);
        hasher.finish()
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
        buf: &mut GrowBuf,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        style: Style,
        bq_indent: u16,
        link: Option<&str>,
        links: &mut Vec<LinkRegion>,
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
                    link,
                    links,
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
                    link,
                    links,
                );
                if *y >= max_y {
                    return;
                }
                if *x < max_x && *x > area_x.saturating_add(bq_indent) {
                    if let Some(cell) = buf.cell_mut((*x, *y)) {
                        cell.set_char(' ');
                        cell.set_style(style);
                    }
                    // Keep inter-word whitespace inside the link's hit map.
                    if let Some(url) = link {
                        match links.last_mut() {
                            Some(last) if last.y == *y && last.x1 == *x && last.url == url => {
                                last.x1 = last.x1.saturating_add(1);
                            }
                            _ => links.push(LinkRegion {
                                y: *y,
                                x0: *x,
                                x1: x.saturating_add(1),
                                url: url.to_string(),
                            }),
                        }
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
            link,
            links,
        );
    }

    /// Flush the accumulated word to the buffer, wrapping to the next line if
    /// it doesn't fit on the current line. Words wider than the whole line
    /// are broken at character level so they still wrap correctly.
    #[allow(clippy::too_many_arguments)]
    fn flush_render_word(
        word: &mut String,
        word_w: &mut u16,
        buf: &mut GrowBuf,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        style: Style,
        bq_indent: u16,
        link: Option<&str>,
        links: &mut Vec<LinkRegion>,
    ) {
        if *word_w == 0 {
            return;
        }
        let min_x = area_x.saturating_add(bq_indent);
        let avail = max_x.saturating_sub(min_x);
        // Whole-word wrap: if the word doesn't fit on the current line but
        // fits on a fresh line, move it there intact.
        if *x + *word_w > max_x && *x > min_x && *word_w <= avail {
            *y += 1;
            *x = min_x;
        }
        if *y >= max_y {
            word.clear();
            *word_w = 0;
            return;
        }
        // Write grapheme by grapheme. Words wider than the whole line are
        // broken at character level by wrapping whenever the cursor reaches
        // the right edge (mirrors `flush_layout_word` in layout.rs).
        // While inside a link, record one region per contiguous row segment
        // (phase 5 hit-testing).
        let mut seg_start: Option<u16> = None;
        let close_segment =
            |links: &mut Vec<LinkRegion>, seg: &mut Option<u16>, end_x: u16, cy: u16| {
                if let Some(x0) = seg.take()
                    && let Some(url) = link
                    && end_x > x0
                {
                    // Merge with an adjacent region of the same link so the
                    // space-extended prefix and wrapped words form one
                    // contiguous hit target.
                    match links.last_mut() {
                        Some(last) if last.y == cy && last.x1 == x0 && last.url == url => {
                            last.x1 = end_x;
                        }
                        _ => links.push(LinkRegion {
                            y: cy,
                            x0,
                            x1: end_x,
                            url: url.to_string(),
                        }),
                    }
                }
            };
        for (g, gw) in crate::core::lib::unicode_util::graphemes_with_width(word) {
            if *x + gw > max_x && *x > min_x {
                close_segment(links, &mut seg_start, *x, *y);
                *y += 1;
                *x = min_x;
            }
            if *y >= max_y {
                break;
            }
            if link.is_some() {
                seg_start.get_or_insert(*x);
            }
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
        close_segment(links, &mut seg_start, *x, *y);
        word.clear();
        *word_w = 0;
    }

    /// Flush the accumulated word parts for code-block rendering.
    /// If the word doesn't fit on the current line, wraps to the next line
    /// (including filling its background row).
    #[allow(clippy::too_many_arguments)]
    fn flush_code_word(
        word_parts: &mut Vec<(&str, u16, Style)>,
        word_w: &mut u16,
        buf: &mut GrowBuf,
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
        let min_x = area_x.saturating_add(code_pad);
        let avail = max_x.saturating_sub(min_x);
        // Whole-word wrap: if the word doesn't fit on the current line but
        // fits on a fresh line, move it there intact.
        if *x + *word_w > max_x && *x > min_x && *word_w <= avail {
            *y += 1;
            *x = min_x;
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
            // Words wider than the whole line: break at character level.
            if *x + w > max_x && *x > min_x {
                *y += 1;
                *x = min_x;
                if *y < max_y {
                    Self::fill_row(buf, area_x, *y, max_x, Style::default().bg(code_bg));
                }
            }
            if *y >= max_y {
                break;
            }
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
    fn fill_row(buf: &mut GrowBuf, x: u16, y: u16, max_x: u16, style: Style) {
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

    /// Left-click hit-test against the link map rebuilt by the last render;
    /// a hit opens the URL with the system handler.
    fn process_mouse_event(&mut self, event: &MouseEvent) -> bool {
        if !event.is_left_click() {
            return false;
        }
        #[allow(clippy::unwrap_used)]
        let hit = {
            let links = self.active_links.lock().unwrap();
            links
                .iter()
                .find(|l| event.y == l.y && event.x >= l.x0 && event.x < l.x1)
                .map(|l| l.url.clone())
        };
        match hit {
            Some(url) => {
                // Detached: launchers can block, and this runs on the UI
                // thread — a slow handler must not freeze rendering.
                if let Err(err) = open::that_detached(&url) {
                    log::warn!("failed to open link {url}: {err}");
                }
                true
            }
            None => false,
        }
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
        // Clear the click map FIRST: stale absolute-coordinate regions from a
        // previous frame must never survive content clears / area collapse.
        #[allow(clippy::unwrap_used)]
        {
            self.active_links.lock().unwrap().clear();
        }
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

        // ── Blit cached block renders (rendering misses on demand) ──
        // Blocks are laid out by their measured heights exactly as the
        // monolithic renderer advanced its cursor: each block's height already
        // includes the trailing blank row its End handler produced, so the
        // next block starts right after it — no extra separator is added.
        let fingerprint = self.style_fingerprint();
        let mut base_y = area.y;
        for block in &self.blocks.blocks {
            if base_y >= max_y {
                break;
            }

            let raw = self.blocks.source(block);
            let key = {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                raw.hash(&mut hasher);
                hasher.finish()
            };
            let cached = {
                #[allow(clippy::unwrap_used)]
                let mut cache = self.block_cache.lock().unwrap();
                match cache.get(&key) {
                    Some(entry)
                        if entry.width == area.width && entry.fingerprint == fingerprint =>
                    {
                        entry.clone()
                    }
                    _ => {
                        let mut canvas = GrowBuf::new(area.width, Style::default().bg(bg_color));
                        let (height, block_links) = Self::render_block_events(
                            raw,
                            &mut canvas,
                            area.width,
                            &palette,
                            self.table_border_color.as_ref(),
                            true,
                            self.conceal,
                        );
                        let entry = CachedBlock {
                            width: area.width,
                            fingerprint,
                            height,
                            rows: Arc::new(canvas.into_rows()),
                            links: block_links,
                        };
                        cache.put(key, entry.clone());
                        entry
                    }
                }
            };

            // Blit the cached rows, clipped to the viewport.
            for (row_idx, row) in cached.rows.iter().enumerate() {
                let target_y = base_y.saturating_add(row_idx as u16);
                if target_y >= max_y {
                    break;
                }
                for (col_idx, cell) in row.iter().enumerate() {
                    let target_x = area.x.saturating_add(col_idx as u16);
                    if target_x >= max_x {
                        break;
                    }
                    if let Some(dst) = buf.cell_mut((target_x, target_y)) {
                        *dst = cell.clone();
                    }
                }
            }

            #[allow(clippy::unwrap_used)]
            {
                let mut active = self.active_links.lock().unwrap();
                for region in &cached.links {
                    let abs_y = base_y.saturating_add(region.y);
                    // Rows blit-clipped by the viewport are invisible: never
                    // register clickable dead zones over other content.
                    if abs_y >= max_y {
                        continue;
                    }
                    active.push(ActiveLink {
                        y: abs_y,
                        x0: area.x.saturating_add(region.x0),
                        x1: area.x.saturating_add(region.x1),
                        url: region.url.clone(),
                    });
                }
            }

            base_y = base_y.saturating_add(cached.height);
        }

        let elapsed = start.elapsed().as_micros();
        if elapsed > 500 && perf_log_allowed() {
            log::debug!(
                "[PERF] markdown_render_self: content_len={} blocks={} reused={} area={}x{} elapsed={elapsed}us",
                self.content.len(),
                self.blocks.blocks.len(),
                self.blocks.stable_count,
                area.width,
                area.height
            );
        }
    }
}

// ── Per-block rendering (phase 1+2 core) ────────────────────────

impl MarkdownRenderable {
    /// Render ONE top-level markdown block into a growable canvas.
    ///
    /// This is the event loop previously inlined in `render_self`, operating
    /// on the block's raw source slice with `area_x = 0`, `max_x = width` and
    /// an unbounded `max_y` so the full block geometry can be measured and
    /// cached. The returned value is the block's layout height: the final
    /// cursor row, which includes the trailing blank row that the monolithic
    /// renderer's `End` handlers used to produce.
    ///
    /// When `highlight_code` is false, fenced code blocks skip tree-sitter
    /// highlighting entirely (styles never affect layout, so this keeps the
    /// height estimator cheap).
    fn render_block_events(
        content: &str,
        buf: &mut GrowBuf,
        width: u16,
        palette: &MarkdownPalette,
        table_border_color: Option<&RGBA>,
        highlight_code: bool,
        conceal: bool,
    ) -> (u16, Vec<LinkRegion>) {
        let area_x = 0u16;
        let max_x = width;
        let max_y = u16::MAX;

        // ── State ───────────────────────────────────────────────
        let mut ctx = MarkdownContext::new();
        let mut y = 0u16;
        let mut x = 0u16;

        // Table-buffering state (cells are styled span lists — see TableCell)
        let mut in_table = false;
        let mut tbl_headers: Vec<TableCell> = Vec::new();
        let mut tbl_rows: Vec<Vec<TableCell>> = Vec::new();
        let mut tbl_cur_row: Vec<TableCell> = Vec::new();
        let mut tbl_cur_cell: Vec<CellSpan> = Vec::new();
        let mut tbl_in_header = false;

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = pulldown_cmark::Parser::new_ext(content, options);

        // Collect events so a cheap pre-pass can compute per-list marker
        // widths (OpenTUI aligns ordered markers to the widest one, e.g.
        // " 9." / "10.").
        let events: Vec<Event<'_>> = parser.collect();
        let mut list_marker_widths = compute_list_marker_widths(&events);

        // List-nesting state: one frame per open list level. Mirrors
        // OpenTUI's list layout — markers are drawn at the parent content
        // indent and item content starts right after the (aligned) marker,
        // so nested lists indent naturally.
        let mut list_stack: Vec<ListFrame> = Vec::new();
        // Clickable regions recorded while rendering (phase 5).
        let mut links: Vec<LinkRegion> = Vec::new();

        // Effective indentation is DERIVED from open containers — never
        // stored-and-restored, which is what kept causing lost/doubled
        // indents in combinations like quote > list > quote:
        //     indent = 2 * open-quote-levels + top frame's content column.
        let mut bq_depth: u16 = 0;

        fn effective_indent(bq_depth: u16, stack: &[ListFrame]) -> u16 {
            bq_depth
                .saturating_mul(2)
                .saturating_add(stack.last().map_or(0, |f| f.content_indent))
        }

        for event in &events {
            // max_y is u16::MAX here (blocks render unbounded); this only
            // guards against cursor overflow for absurdly tall documents.
            if !in_table && y >= max_y.saturating_sub(1) {
                break;
            }

            // ── Table buffering mode ────────────────────────────
            if in_table {
                match event {
                    Event::Start(tag) => {
                        ctx.handle_start(tag);
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
                                    area_x,
                                    max_x,
                                    max_y,
                                    &tbl_headers,
                                    &tbl_rows,
                                    palette,
                                    table_border_color,
                                );
                                in_table = false;
                            }
                            _ => {}
                        }
                        ctx.handle_end(tag_end);
                    }
                    Event::Text(text) | Event::InlineHtml(text) | Event::Code(text) => {
                        // Keep inline styling inside table cells (bold, code,
                        // links...) instead of flattening to plain text.
                        tbl_cur_cell.push(CellSpan {
                            text: text.to_string(),
                            style: palette.style_for(ctx.current_element(), ctx.heading_level()),
                        });
                    }
                    Event::SoftBreak | Event::HardBreak => {
                        tbl_cur_cell.push(CellSpan {
                            text: " ".to_string(),
                            style: Style::default(),
                        });
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                // ── Block / inline start ────────────────────────
                Event::Start(tag) => {
                    ctx.handle_start(tag);
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
                            if x != area_x {
                                y += 1;
                                x = area_x;
                            }
                        }
                        Tag::Paragraph | Tag::Heading { .. } => {
                            if !list_stack.is_empty() {
                                // Inside a list item: the first paragraph
                                // continues on the marker row; later paragraphs
                                // get a blank separator and land back on the
                                // derived content column.
                                if x == area_x {
                                    y += 1;
                                    x = area_x
                                        .saturating_add(effective_indent(bq_depth, &list_stack));
                                }
                            } else if ctx.in_blockquote() {
                                if x != area_x {
                                    // The BlockQuote handler positioned x at
                                    // the quote indent — keep the row.
                                } else {
                                    // Second+ paragraph inside blockquote:
                                    // re-apply the quote indent.
                                    x = area_x
                                        .saturating_add(effective_indent(bq_depth, &list_stack));
                                }
                            } else if x != area_x {
                                y += 1;
                                x = area_x;
                            }
                        }
                        Tag::BlockQuote(_) => {
                            if x != area_x {
                                y += 1;
                            }
                            // Each open quote level contributes one indent
                            // step; nested quotes (and quotes inside list
                            // items) accumulate through `bq_depth`.
                            bq_depth = bq_depth.saturating_add(1);
                            if y < max_y {
                                x = area_x.saturating_add(effective_indent(bq_depth, &list_stack));
                            }
                        }
                        Tag::CodeBlock(_) => {
                            if x != area_x {
                                y += 1;
                                x = area_x;
                            }
                            // The blank row left by the previous block's TagEnd
                            // is the code block's top MARGIN (external spacing).
                            // Blocks WITHOUT a language tag get no internal
                            // top-gap row: the box starts directly at the first
                            // code line, so their only top spacing is that
                            // margin. Blocks WITH a language tag keep one
                            // internal top-gap row that carries the label.
                            // Cursor advance: N+3 for language-less blocks
                            // (N lines + bottom padding + separator + TagEnd
                            // blank), N+4 when the label row is present.
                            if !ctx.code_block_lang().is_empty() && y < max_y {
                                y += 1;
                                x = area_x;
                                if y < max_y {
                                    let cb_bg = palette.code_bg_color();
                                    Self::fill_row(
                                        buf,
                                        area_x,
                                        y,
                                        max_x,
                                        Style::default().bg(cb_bg),
                                    );

                                    // Draw language label on the top gap row
                                    let lang = ctx.code_block_lang();
                                    let label_style = Style::default()
                                        .fg(rgba_to_color(palette.muted_color()))
                                        .bg(cb_bg)
                                        .add_modifier(Modifier::ITALIC);
                                    let mut lx = area_x.saturating_add(2);
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
                        Tag::List(start) => {
                            // Marker column = enclosing item's content column,
                            // in PURE list coordinates: quote levels are added
                            // at use time so they can never be baked twice.
                            let parent_indent = list_stack.last().map_or(0, |f| f.content_indent);
                            // Consume THIS list's slot: find the first slot
                            // still filled, not always index 0 (nested lists
                            // consume earlier slots first).
                            let marker_width = list_marker_widths
                                .iter_mut()
                                .find(|o| o.is_some())
                                .and_then(|o| o.take())
                                .unwrap_or(2)
                                .max(2);
                            list_stack.push(ListFrame {
                                ordered: start.is_some(),
                                next_num: usize::try_from(start.unwrap_or(1)).unwrap_or(1),
                                marker_width,
                                item_indent: parent_indent,
                                content_indent: parent_indent.saturating_add(marker_width),
                            });
                        }
                        Tag::Item => {
                            // Advance to a fresh row only when genuinely
                            // mid-content: an enclosing quote's indentation
                            // leaves x exactly at our marker column, which is
                            // NOT mid-content.
                            let marker_col =
                                area_x.saturating_add(bq_depth.saturating_mul(2).saturating_add(
                                    list_stack.last().map_or(0, |f| f.item_indent),
                                ));
                            if x != area_x && x != marker_col {
                                y += 1;
                            }
                            let frame = list_stack.last_mut().expect("Item outside List");
                            let marker_style =
                                Style::default().fg(rgba_to_color(palette.list_marker_color()));
                            // Right-align ordered numbers to the widest one
                            // (" 9." / "10.") like OpenTUI's padStart.
                            let core = if frame.ordered {
                                let n = frame.next_num;
                                format!("{n}.")
                            } else {
                                "•".to_string()
                            };
                            frame.next_num = frame.next_num.saturating_add(1);
                            let marker = format!(
                                "{core:>width$} ",
                                width = usize::from(frame.marker_width - 1)
                            );

                            // Markers are short and never wrap: draw them
                            // grapheme-by-grapheme instead of going through
                            // `render_text`, which deliberately skips LEADING
                            // spaces at line start and would eat the
                            // right-alignment padding of " 1.".
                            x = marker_col;
                            for (grapheme, gw) in
                                crate::core::lib::unicode_util::graphemes_with_width(&marker)
                            {
                                if let Some(cell) = buf.cell_mut((x, y)) {
                                    cell.set_symbol(grapheme);
                                    cell.set_style(marker_style);
                                }
                                x += gw;
                            }
                            x = area_x.saturating_add(effective_indent(bq_depth, &list_stack));
                        }
                        // Unconcealed links render literally: "[" before the
                        // label (the label itself comes from inner events).
                        Tag::Link { .. } if !conceal => {
                            let style =
                                palette.style_for(Some(MarkdownElement::Link), ctx.heading_level());
                            let link_dest = ctx.link_dest();
                            let link = (!link_dest.is_empty()).then_some(link_dest);
                            Self::render_text(
                                "[",
                                buf,
                                &mut x,
                                &mut y,
                                area_x,
                                max_x,
                                max_y,
                                style,
                                effective_indent(bq_depth, &list_stack),
                                link,
                                &mut links,
                            );
                        }
                        // Concealed (default): the label alone represents the
                        // link — nothing to emit at the boundaries.
                        Tag::Link { .. } => {}
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
                        | TagEnd::Item
                        | TagEnd::CodeBlock
                        | TagEnd::TableRow
                        | TagEnd::Table => {
                            y += 1;
                            x = area_x;
                            // No indent bookkeeping: indentation derives from
                            // the still-open containers.
                        }
                        TagEnd::BlockQuote(_) => {
                            y += 1;
                            x = area_x;
                            bq_depth = bq_depth.saturating_sub(1);
                        }
                        TagEnd::List(_) => {
                            list_stack.pop();
                        }
                        // Unconcealed links render literally: "](url)" after
                        // the label (link context is still open here).
                        TagEnd::Link if !conceal => {
                            let style =
                                palette.style_for(Some(MarkdownElement::Link), ctx.heading_level());
                            let url = ctx.link_dest().to_string();
                            let literal = format!("]({url})");
                            let link_ref = (!url.is_empty()).then_some(url.as_str());
                            Self::render_text(
                                &literal,
                                buf,
                                &mut x,
                                &mut y,
                                area_x,
                                max_x,
                                max_y,
                                style,
                                effective_indent(bq_depth, &list_stack),
                                link_ref,
                                &mut links,
                            );
                        }
                        TagEnd::Link => {}
                        TagEnd::TableHead
                        | TagEnd::FootnoteDefinition
                        | TagEnd::DefinitionList
                        | TagEnd::DefinitionListTitle
                        | TagEnd::DefinitionListDefinition
                        | TagEnd::Strikethrough
                        | TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Image
                        | TagEnd::MetadataBlock(_)
                        | TagEnd::HtmlBlock
                        | TagEnd::Superscript
                        | TagEnd::Subscript => {}
                        TagEnd::TableCell => {
                            x = x.saturating_add(2);
                        }
                    }
                    ctx.handle_end(tag_end);
                }

                // ── Text content ────────────────────────────────
                Event::Text(text)
                | Event::FootnoteReference(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::InlineHtml(text) => {
                    if ctx.in_code_block() {
                        // Height estimation skips tree-sitter (styles never
                        // affect layout); the label row is drawn by the
                        // CodeBlock start handler either way.
                        let hl_lang = if highlight_code {
                            ctx.code_block_lang()
                        } else {
                            ""
                        };
                        Self::render_code_block(
                            text, buf, &mut x, &mut y, area_x, max_x, max_y, palette, hl_lang,
                        );
                    } else {
                        let element = ctx.current_element();
                        let heading_level = ctx.heading_level();
                        let mut style = palette.style_for(element, heading_level);
                        let bq_indent = effective_indent(bq_depth, &list_stack);
                        if ctx.in_blockquote() && text.starts_with('⚠') {
                            // Warning blockquote: apply yellow background + black text + bold
                            // (Only for the warning, not regular blockquotes)
                            let warning_bg = Color::Rgb(238, 241, 112);

                            // Calculate box width
                            let first_line = text.lines().next().unwrap_or("");
                            let text_w =
                                crate::core::lib::unicode_util::str_display_width(first_line)
                                    as u16;
                            let box_end = area_x
                                .saturating_add(2) // indent
                                .saturating_add(text_w) // text width
                                .saturating_add(2) // padding after text
                                .min(max_x);

                            // Fill the background from left edge to past text
                            for cx in area_x..box_end {
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
                        let link_dest = ctx.link_dest();
                        let link = (!link_dest.is_empty()).then_some(link_dest);
                        Self::render_text(
                            text, buf, &mut x, &mut y, area_x, max_x, max_y, style, bq_indent,
                            link, &mut links,
                        );
                    }
                }

                // ── Inline code ─────────────────────────────────
                Event::Code(text) => {
                    let style = palette.style_for(Some(MarkdownElement::InlineCode), None);
                    let link_dest = ctx.link_dest();
                    let link = (!link_dest.is_empty()).then_some(link_dest);
                    Self::render_text(
                        text,
                        buf,
                        &mut x,
                        &mut y,
                        area_x,
                        max_x,
                        max_y,
                        style,
                        effective_indent(bq_depth, &list_stack),
                        link,
                        &mut links,
                    );
                }

                // ── Raw HTML ────────────────────────────────────
                Event::Html(html) => {
                    let style = Style::default().fg(rgba_to_color(palette.muted_color()));
                    let link_dest = ctx.link_dest();
                    let link = (!link_dest.is_empty()).then_some(link_dest);
                    Self::render_text(
                        html,
                        buf,
                        &mut x,
                        &mut y,
                        area_x,
                        max_x,
                        max_y,
                        style,
                        effective_indent(bq_depth, &list_stack),
                        link,
                        &mut links,
                    );
                }

                // ── Line breaks ─────────────────────────────────
                Event::SoftBreak | Event::HardBreak => {
                    // Continuation lines keep the active indentation
                    // (blockquote + list content column).
                    x = area_x.saturating_add(effective_indent(bq_depth, &list_stack));
                    y += 1;
                }

                // ── Horizontal rule ─────────────────────────────
                Event::Rule => {
                    if y < max_y {
                        let rule_style = Style::default().fg(rgba_to_color(palette.muted_color()));
                        for cx in area_x..max_x {
                            if let Some(cell) = buf.cell_mut((cx, y)) {
                                cell.set_char('─');
                                cell.set_style(rule_style);
                            }
                        }
                        y += 1;
                        x = area_x;
                    }
                }

                // ── Task list markers ───────────────────────────
                Event::TaskListMarker(checked) => {
                    // Use Unicode checkbox symbols for a more polished look
                    let marker = if *checked { "☑ " } else { "☐ " };
                    let style = Style::default().fg(rgba_to_color(palette.list_marker_color()));
                    Self::render_text(
                        marker, buf, &mut x, &mut y, area_x, max_x, max_y, style, 0, None,
                        &mut links,
                    );
                }
            }
        }

        (y, links)
    }
}

// ── Code-block rendering ─────────────────────────────────────────

impl MarkdownRenderable {
    /// Render a code block segment with syntax highlighting (via tree-sitter)
    /// when a language is declared.
    ///
    /// `hl_lang` is the language used for highlighting; pass an empty string
    /// to render as plain text (also used by the height estimator, which must
    /// not pay for tree-sitter since styles do not affect layout).
    #[allow(clippy::too_many_arguments)]
    fn render_code_block(
        text: &str,
        buf: &mut GrowBuf,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        palette: &MarkdownPalette,
        hl_lang: &str,
    ) {
        let cb_start = std::time::Instant::now();
        let code_bg = palette.code_bg_color();
        let default_fg = rgba_to_color(palette.text_color());

        // Syntax highlighting strategy (phase 6):
        // - ``` (no language tag) → no highlighting, render as plain text
        // - ```lang (known/supported) → use tree-sitter highlighting
        // - ```lang (unknown/unsupported) → plain text. The old JavaScript
        //   fallback produced wrong colors for non-JS code and is gone.
        //
        // Build byte-to-category map for syntax highlighting (cached)
        let spans: Option<Vec<HighlightSpan>> = if hl_lang.is_empty() {
            None
        } else {
            let key = highlight_cache_key(text, hl_lang);
            #[allow(clippy::unwrap_used)]
            let mut cache = HIGHLIGHT_CACHE.lock().unwrap();
            cache.get(&key).cloned().or_else(|| {
                let computed = highlight(text, hl_lang);
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
        if cb_us > 500 && perf_log_allowed() {
            log::debug!(
                "[PERF] code_block_render: text_len={} lang={} elapsed={cb_us}us",
                text.len(),
                if hl_lang.is_empty() { "none" } else { hl_lang }
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
    #[allow(clippy::too_many_arguments)]
    fn render_table(
        buf: &mut GrowBuf,
        x: &mut u16,
        y: &mut u16,
        area_x: u16,
        max_x: u16,
        max_y: u16,
        headers: &[TableCell],
        rows: &[Vec<TableCell>],
        palette: &MarkdownPalette,
        table_border_color: Option<&RGBA>,
    ) {
        if headers.is_empty() || max_x <= area_x {
            return;
        }

        let col_count = headers.len();

        // ── Calculate column widths ─────────────────────────────
        // Sizing MUST use display width (grapheme-aware), not
        // `chars().count()`: a single emoji is 1 char but occupies 2
        // terminal columns, and a VS16 sequence like "⚙️" is 2 chars but
        // still 2 columns. Char counting under-/over-sizes columns and
        // desynchronizes the buffer grid from what the terminal renders,
        // which makes ratatui's diff leave stale glyphs behind when the
        // surrounding UI changes (the "leaking table row across views"
        // bug).
        let cell_display_width = |cell: &TableCell| -> u16 {
            cell.iter().map(CellSpan::display_width).sum::<usize>() as u16
        };
        let mut col_widths: Vec<u16> = headers.iter().map(cell_display_width).collect();
        for row in rows {
            for (ci, cell) in row.iter().enumerate() {
                if ci < col_count {
                    let cw = cell_display_width(cell);
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
                *w = ((u32::from(*w) * u32::from(available)) / u32::from(total_w)) as u16;
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
        let render_border = |buf: &mut GrowBuf, y: u16, left: char, right: char, sep: char| {
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

        // Helper to render a single display line from pre-wrapped cell content.
        // `wrapped` holds, per column, the styled display lines produced by
        // [`wrap_cell_spans`].
        let render_cell_line = |buf: &mut GrowBuf,
                                y: u16,
                                wrapped: &[std::sync::Arc<Vec<Vec<CellSpan>>>],
                                is_header: bool,
                                row_idx: usize,
                                line_idx: usize| {
            if y >= max_y {
                return;
            }
            let cell_style = if is_header { header_style } else { text_style };

            let (actual_border_style, base_cell_style) = if !is_header && row_idx.is_multiple_of(2)
            {
                (border_style.bg(alt_bg_color), cell_style.bg(alt_bg_color))
            } else {
                (border_style, cell_style)
            };

            for ci in 0..col_count {
                let sx = col_starts[ci];
                // Get the pre-wrapped styled line for this cell at the given
                // line index.
                let spans = wrapped
                    .get(ci)
                    .and_then(|lines| lines.get(line_idx))
                    .cloned()
                    .unwrap_or_default();

                // Vertical border on the left of each cell
                let vline_x = if ci == 0 { sx } else { sx.saturating_sub(1) };
                if let Some(cell) = buf.cell_mut((vline_x, y)) {
                    cell.set_char('│');
                    cell.set_style(actual_border_style);
                }

                // Render the cell content line with padding, grapheme-aware:
                // each grapheme cluster (emoji, "⚙️", CJK, flags) goes into
                // ONE cell with its trailing columns marked `Skip`, exactly
                // like `flush_render_word` does for prose. Writing raw chars
                // here used to split VS16 sequences across cells and leave
                // wide glyphs without shadow marks — both desynchronize the
                // buffer from the terminal grid and leak stale glyphs across
                // view switches. Inline span styles are patched over the
                // row's base style so bold/code/links survive inside cells;
                // headers keep their bold via the base style.
                let mut cx = sx + padding;
                for span in &spans {
                    for (grapheme, gw) in
                        crate::core::lib::unicode_util::graphemes_with_width(&span.text)
                    {
                        if cx + gw > sx + col_widths[ci] + padding {
                            break;
                        }
                        let style = base_cell_style.patch(span.style);
                        if let Some(cell) = buf.cell_mut((cx, y)) {
                            cell.set_symbol(grapheme);
                            cell.set_style(style);
                        }
                        if gw > 1 {
                            for dx in 1..gw {
                                if let Some(shadow) = buf.cell_mut((cx + dx, y)) {
                                    shadow.set_style(style);
                                    shadow.set_diff_option(CellDiffOption::Skip);
                                }
                            }
                        }
                        cx += gw;
                    }
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
        let precompute_wrapped =
            |cells: &[TableCell]| -> (Vec<std::sync::Arc<Vec<Vec<CellSpan>>>>, usize) {
                let mut wrapped: Vec<std::sync::Arc<Vec<Vec<CellSpan>>>> =
                    Vec::with_capacity(col_count);
                let mut max_lines = 1usize;
                for (ci, &cw) in col_widths.iter().enumerate() {
                    let cell = cells.get(ci).map_or(&[][..], |c| c.as_slice());
                    let lines = wrap_cell_spans(cell, cw);
                    max_lines = max_lines.max(lines.len());
                    wrapped.push(lines);
                }
                (wrapped, max_lines)
            };

        // Pre-fill alternating background for all display lines of a row.
        // Fills from the left edge to the rightmost border column (no +1 to avoid bleeding).
        let fill_alt_bg = |buf: &mut GrowBuf, start_y: u16, nlines: usize| {
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

// ── Height estimation ────────────────────────────────────────────

/// LRU cache for [`estimate_height_impl`]: key = (content hash, width).
/// Callers (chat layout, right panel) re-measure the same content across
/// frames while only widths or tails change.
static ESTIMATE_CACHE: std::sync::LazyLock<Mutex<LruCache<(u64, u16), u16>>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(LruCache::new(NonZeroUsize::new(1024).expect("non-zero")))
    });

/// Number of terminal rows `text` occupies when rendered at `max_w` columns.
///
/// Measures by actually rendering each top-level block into a growable canvas
/// (the same code path the renderer uses), so the estimate is exact by
/// construction and can never drift from the renderer's wrap logic — the
/// hand-synced mirror algorithms this replaces could (and did) diverge.
///
/// Styles are irrelevant to layout, so measurement skips tree-sitter
/// highlighting entirely.
pub(crate) fn estimate_height_impl(text: &str, max_w: u16) -> u16 {
    if text.is_empty() || max_w == 0 {
        return 1;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let key = (hasher.finish(), max_w);

    #[allow(clippy::unwrap_used)]
    {
        let mut cache = ESTIMATE_CACHE.lock().unwrap();
        if let Some(height) = cache.get(&key) {
            return *height;
        }
    }

    let blocks = parse_blocks_incremental(text, None);
    // Palette colors never affect layout; defaults are fine here.
    let palette = MarkdownPalette::new(
        RGBA::from_ints(220, 220, 220, 255),
        RGBA::from_ints(0, 0, 0, 0),
    );

    let mut total = 0u16;
    for block in &blocks.blocks {
        let source = &text[block.range.clone()];
        let mut canvas = GrowBuf::new(max_w, Style::default());
        // NOTE: estimation assumes concealment on (the default). Callers that
        // set_conceal(false) change layout in ways this public API cannot see.
        let (height, _links) = MarkdownRenderable::render_block_events(
            source,
            &mut canvas,
            max_w,
            &palette,
            None,
            false,
            true,
        );
        total = total.saturating_add(height);
    }

    let height = total.max(1);
    #[allow(clippy::unwrap_used)]
    ESTIMATE_CACHE.lock().unwrap().put(key, height);
    height
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
