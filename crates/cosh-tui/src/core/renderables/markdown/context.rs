use pulldown_cmark::{CodeBlockKind, HeadingLevel, Tag, TagEnd};

/// Semantic element types that influence text styling.
///
/// Mirrors OpenTUI's style-groups (e.g. `markup.heading`, `markup.strong`)
/// so that the palette can assign distinct visual styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownElement {
    /// Plain paragraph text.
    Paragraph,
    /// Atx or Setext heading (1 = h1, highest, 6 = h6).
    Heading(u8),
    /// `*italic*` or `_italic_`.
    Emphasis,
    /// `**bold**` or `__bold__`.
    Strong,
    /// `~~strikethrough~~`.
    Strikethrough,
    /// `[label](url)` — the label part.
    Link,
    /// Inline `` `code` ``.
    InlineCode,
    /// Fenced or indented code block.
    CodeBlock,
    /// Blockquote region.
    Blockquote,
    /// An item inside either an ordered or unordered list.
    ListItem { ordered: bool },
}

/// Context-aware state machine that tracks the current nesting of markdown
/// elements while iterating through a `pulldown_cmark` event stream.
///
/// Usage:
/// ```ignore
/// let mut ctx = MarkdownContext::new();
/// for event in parser {
///     match &event {
///         Event::Start(tag) => ctx.handle_start(tag),
///         Event::End(end)   => ctx.handle_end(end),
///         Event::Text(_)    => {
///             let el = ctx.current_element();
///             let level = ctx.heading_level();
///             // … render with appropriate style …
///         }
///         _ => {}
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct MarkdownContext {
    // ── Block-level state ──────────────────────────────────────
    heading_level: u8,

    // ── Inline nesting (counters because emphasis can nest) ────
    emphasis_depth: u8,
    strong_depth: u8,
    strikethrough_depth: u8,
    link_depth: u8,

    // ── Block flags ────────────────────────────────────────────
    in_code_block: bool,
    code_block_lang: String,
    in_blockquote: bool,

    // ── List tracking ──────────────────────────────────────────
    /// Stack — one entry per nesting level. `true` = ordered.
    list_ordered: Vec<bool>,
    /// Stack — running counter for each list level.
    list_counters: Vec<usize>,
}

impl MarkdownContext {
    pub fn new() -> Self {
        Self {
            heading_level: 0,
            emphasis_depth: 0,
            strong_depth: 0,
            strikethrough_depth: 0,
            link_depth: 0,
            in_code_block: false,
            code_block_lang: String::new(),
            in_blockquote: false,
            list_ordered: Vec::new(),
            list_counters: Vec::new(),
        }
    }

    // ── Event handlers ─────────────────────────────────────────

    pub fn handle_start(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Heading {
                level, ..
            } => {
                self.heading_level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
            }
            Tag::Emphasis => {
                self.emphasis_depth += 1;
            }
            Tag::Strong => {
                self.strong_depth += 1;
            }
            Tag::Strikethrough => {
                self.strikethrough_depth += 1;
            }
            Tag::Link { .. } => {
                self.link_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(info) => info.trim().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Tag::BlockQuote(_) => {
                self.in_blockquote = true;
            }
            Tag::List(start) => {
                let ordered = start.is_some();
                self.list_ordered.push(ordered);
                self.list_counters.push(start.unwrap_or(1) as usize);
            }
            Tag::Item => {
                // Counter is incremented on Item so each item gets the next number.
                // For unordered lists the counter is still bumped but we use "•" in the palette.
                if let Some(c) = self.list_counters.last_mut() {
                    *c += 1;
                }
            }
            // Elements we don't need to track for styling:
            Tag::Paragraph
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Image { .. }
            | Tag::MetadataBlock(_)
            | Tag::HtmlBlock
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    pub fn handle_end(&mut self, end: &TagEnd) {
        match end {
            TagEnd::Heading(_) => {
                self.heading_level = 0;
            }
            TagEnd::Emphasis => {
                self.emphasis_depth = self.emphasis_depth.saturating_sub(1);
            }
            TagEnd::Strong => {
                self.strong_depth = self.strong_depth.saturating_sub(1);
            }
            TagEnd::Strikethrough => {
                self.strikethrough_depth = self.strikethrough_depth.saturating_sub(1);
            }
            TagEnd::Link => {
                self.link_depth = self.link_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.code_block_lang.clear();
            }
            TagEnd::BlockQuote(_) => {
                self.in_blockquote = false;
            }
            TagEnd::List(_) => {
                self.list_ordered.pop();
                self.list_counters.pop();
            }
            TagEnd::Paragraph
            | TagEnd::Item
            | TagEnd::Table
            | TagEnd::TableRow
            | TagEnd::TableHead
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Image
            | TagEnd::MetadataBlock(_)
            | TagEnd::HtmlBlock
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    // ── Queries ────────────────────────────────────────────────

    /// The innermost inline-level element that applies to the current text.
    /// Returns `None` when the text is just plain paragraph content
    /// (block-level context like heading / blockquote can be checked separately).
    ///
    /// Priority: innermost (most nested) wins.
    /// In pulldown_cmark `***text***` = Start(Emphasis), Start(Strong), ...
    /// so Strong is innermost and should be preferred.
    pub fn current_element(&self) -> Option<MarkdownElement> {
        if self.in_code_block {
            return Some(MarkdownElement::CodeBlock);
        }
        // Innermost first (last opened)
        if self.strikethrough_depth > 0 {
            return Some(MarkdownElement::Strikethrough);
        }
        if self.strong_depth > 0 {
            return Some(MarkdownElement::Strong);
        }
        if self.emphasis_depth > 0 {
            return Some(MarkdownElement::Emphasis);
        }
        if self.link_depth > 0 {
            return Some(MarkdownElement::Link);
        }
        None
    }

    /// Current heading level (1-6), or `None` if outside any heading.
    pub fn heading_level(&self) -> Option<u8> {
        if self.heading_level > 0 {
            Some(self.heading_level)
        } else {
            None
        }
    }

    pub fn in_code_block(&self) -> bool {
        self.in_code_block
    }

    pub fn code_block_lang(&self) -> &str {
        &self.code_block_lang
    }

    pub fn in_blockquote(&self) -> bool {
        self.in_blockquote
    }

    /// Whether the innermost list is ordered (`true`) or unordered (`false`).
    pub fn list_ordered(&self) -> Option<bool> {
        self.list_ordered.last().copied()
    }

    /// The current list-item counter value (always `Some` inside a list item).
    /// For ordered lists this is the number to display; for unordered it is
    /// meaningless — the palette should emit `•` regardless.
    pub fn list_marker(&self) -> Option<(bool, usize)> {
        let ordered = self.list_ordered.last().copied()?;
        // We incremented at Tag::Item, so the current value is (start + item_index + 1).
        // The counter was start. At first Item, it becomes start+1. We want to display
        // start for the first item. So we show counter-1.
        let display = self.list_counters.last().map(|c| c.saturating_sub(1)).unwrap_or(1);
        Some((ordered, display))
    }
}

impl Default for MarkdownContext {
    fn default() -> Self {
        Self::new()
    }
}
