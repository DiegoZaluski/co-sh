use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{ColorInput, RGBA, parse_color};
use crate::core::syntax_style::SyntaxStyle;

static NEXT_MARKDOWN_NUM: AtomicU64 = AtomicU64::new(1);

pub struct MarkdownRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    content: String,
    syntax_style: Option<SyntaxStyle>,
    fg: Option<RGBA>,
    bg: Option<RGBA>,
    conceal: bool,
}

impl MarkdownRenderable {
    #[must_use]
    pub fn new(content: Option<String>) -> Self {
        let num = NEXT_MARKDOWN_NUM.fetch_add(1, Ordering::Relaxed);
        MarkdownRenderable {
            id: format!("md-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            content: content.unwrap_or_default(),
            syntax_style: None,
            fg: None,
            bg: None,
            conceal: true,
        }
    }

    pub fn set_content(&mut self, value: String) {
        self.content = value;
    }

    pub fn set_syntax_style(&mut self, value: Option<SyntaxStyle>) {
        self.syntax_style = value;
    }

    pub fn set_fg(&mut self, value: Option<ColorInput>) {
        self.fg = value.map(parse_color);
    }

    pub fn set_bg(&mut self, value: Option<ColorInput>) {
        self.bg = value.map(parse_color);
    }

    pub fn set_conceal(&mut self, value: bool) {
        self.conceal = value;
    }

    fn default_fg(&self) -> RGBA {
        self.fg.unwrap_or(RGBA::from_ints(220, 220, 220, 255))
    }

    fn default_bg(&self) -> RGBA {
        self.bg.unwrap_or(RGBA::from_ints(0, 0, 0, 0))
    }

    fn rgba_to_ratatui(rgba: RGBA) -> Color {
        let (r, g, b, a) = rgba.to_ints();
        if a == 0 {
            Color::Reset
        } else {
            Color::Rgb(r, g, b)
        }
    }

    fn render_text(text: &str, buf: &mut Buffer, x: &mut u16, y: u16, max_x: u16, style: Style) {
        for ch in text.chars() {
            if *x >= max_x {
                *x = max_x;
                break;
            }
            if let Some(cell) = buf.cell_mut((*x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            *x += 1;
        }
    }
}

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
        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);

        let fg = self.default_fg();
        let bg = self.default_bg();
        let fg_color = Self::rgba_to_ratatui(fg);
        let bg_color = Self::rgba_to_ratatui(bg);

        let default_style = Style::default().fg(fg_color).bg(bg_color);

        let mut y = area.y;
        let mut x = area.x;
        let mut list_depth: usize = 0;
        let mut numbered_list_counters: Vec<usize> = Vec::new();

        let parser = pulldown_cmark::Parser::new(&self.content);

        for event in parser {
            if y >= max_y {
                break;
            }

            match event {
                Event::Start(tag) => match tag {
                    Tag::Paragraph | Tag::BlockQuote(_) | Tag::Table(_) => {
                        if x != area.x {
                            y += 1;
                            x = area.x;
                        }
                    }
                    Tag::Heading {
                        level: _level,
                        id: _id,
                        classes: _classes,
                        attrs: _attrs,
                    } => {
                        if x != area.x {
                            y += 1;
                            x = area.x;
                        }
                    }
                    Tag::CodeBlock(kind) => {
                        if x != area.x {
                            y += 1;
                            x = area.x;
                        }
                        // read code lang if fenced
                        let _lang = match kind {
                            CodeBlockKind::Fenced(info) => info.to_string(),
                            CodeBlockKind::Indented => String::new(),
                        };
                        // code will be rendered as text, in a dim block
                        if y < max_y {
                            for cx in area.x..max_x {
                                if let Some(cell) = buf.cell_mut((cx, y)) {
                                    cell.set_style(
                                        default_style
                                            .bg(Color::Rgb(30, 30, 30))
                                            .add_modifier(Modifier::DIM),
                                    );
                                }
                            }
                        }
                    }
                    Tag::List(start) => {
                        list_depth += 1;
                        numbered_list_counters.push(start.unwrap_or(1) as usize);
                    }
                    Tag::Item => {
                        if x != area.x {
                            y += 1;
                            x = area.x;
                        }
                        // draw bullet
                        let bullet = if let Some(counter) = numbered_list_counters.last_mut() {
                            let marker = format!("{}. ", *counter);
                            *counter += 1;
                            marker
                        } else {
                            "• ".to_string()
                        };
                        Self::render_text(&bullet, buf, &mut x, y, max_x, default_style);
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
                    | Tag::Link {
                        link_type: _,
                        dest_url: _,
                        title: _,
                        id: _,
                    }
                    | Tag::Image {
                        link_type: _,
                        dest_url: _,
                        title: _,
                        id: _,
                    }
                    | Tag::MetadataBlock(_)
                    | Tag::HtmlBlock
                    | Tag::Superscript
                    | Tag::Subscript => {}
                },
                Event::End(tag_end) => match tag_end {
                    TagEnd::List(_) => {
                        list_depth = list_depth.saturating_sub(1);
                        numbered_list_counters.pop();
                    }
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::BlockQuote(_)
                    | TagEnd::CodeBlock
                    | TagEnd::Item
                    | TagEnd::Table
                    | TagEnd::TableRow => {
                        y += 1;
                        x = area.x;
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
                    TagEnd::TableCell => {
                        x += 2; // small gap between cells
                    }
                },
                Event::Text(text)
                | Event::FootnoteReference(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::InlineHtml(text) => {
                    Self::render_text(&text, buf, &mut x, y, max_x, default_style);
                }
                Event::Code(text) => {
                    let code_style = default_style
                        .bg(Color::Rgb(40, 40, 40))
                        .fg(Color::Rgb(200, 150, 100));
                    Self::render_text(&text, buf, &mut x, y, max_x, code_style);
                }
                Event::Html(html) => {
                    Self::render_text(&html, buf, &mut x, y, max_x, default_style);
                }
                Event::SoftBreak | Event::HardBreak => {
                    x = area.x;
                    y += 1;
                }
                Event::Rule => {
                    if y < max_y {
                        let rule_style = default_style.fg(Color::Rgb(80, 80, 80));
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
                Event::TaskListMarker(checked) => {
                    let marker = if checked { "[x] " } else { "[ ] " };
                    Self::render_text(marker, buf, &mut x, y, max_x, default_style);
                }
            }
        }
    }
}
