use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh_sdk::tree_sitter::highlight::{HighlightCategory, highlight};

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
        let (r, g, b, _a) = rgba.to_ints();
        Color::Rgb(r, g, b)
    }

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
                // Skip leading space when wrapping to avoid indent
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

        // Pre-fill entire area with background color so there's no gap between characters
        for row in area.y..max_y {
            for col in area.x..max_x {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_style(Style::default().bg(bg_color));
                    cell.set_char(' ');
                }
            }
        }

        let mut y = area.y;
        let mut x = area.x;
        let mut list_depth: usize = 0;
        let mut numbered_list_counters: Vec<usize> = Vec::new();
        let mut in_code_block = false;
        let mut code_block_lang = String::new();
        let code_bg = bg_color;

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
                        in_code_block = true;
                        code_block_lang = match kind {
                            CodeBlockKind::Fenced(info) => info.to_string(),
                            CodeBlockKind::Indented => String::new(),
                        };
                        if y < max_y {
                            for cx in area.x..max_x {
                                if let Some(cell) = buf.cell_mut((cx, y)) {
                                    cell.set_style(
                                        default_style.bg(code_bg).add_modifier(Modifier::DIM),
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
                        let bullet = if let Some(counter) = numbered_list_counters.last_mut() {
                            let marker = format!("{}. ", *counter);
                            *counter += 1;
                            marker
                        } else {
                            "• ".to_string()
                        };
                        Self::render_text(&bullet, buf, &mut x, &mut y, area.x, max_x, max_y, default_style);
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
                    | TagEnd::Item
                    | TagEnd::Table
                    | TagEnd::TableRow => {
                        y += 1;
                        x = area.x;
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        code_block_lang.clear();
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
                        x += 2;
                    }
                },
                Event::Text(text)
                | Event::FootnoteReference(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::InlineHtml(text) => {
                    if in_code_block {
                        let spans = highlight(&text, &code_block_lang);
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
                                y += 1;
                                x = area.x;
                            }
                            if y >= max_y {
                                break;
                            }
                            for cx in area.x..max_x {
                                if let Some(cell) = buf.cell_mut((cx, y)) {
                                    cell.set_style(
                                        default_style.bg(code_bg).add_modifier(Modifier::DIM),
                                    );
                                }
                            }
                            for (ci, ch) in line.char_indices() {
                                if x >= max_x {
                                    break;
                                }
                                let byte_pos = byte_offset + ci;
                                let cat = cat_map.get(byte_pos).copied().flatten();
                                let style = highlight_style(cat, fg_color, code_bg);
                                if let Some(cell) = buf.cell_mut((x, y)) {
                                    cell.set_char(ch);
                                    cell.set_style(style);
                                }
                                x += 1;
                            }
                            byte_offset += line.len() + 1;
                        }
                    } else {
                        Self::render_text(&text, buf, &mut x, &mut y, area.x, max_x, max_y, default_style);
                    }
                }
                Event::Code(text) => {
                    let code_style = default_style
                        .bg(Color::Rgb(40, 40, 40))
                        .fg(Color::Rgb(200, 150, 100));
                    Self::render_text(&text, buf, &mut x, &mut y, area.x, max_x, max_y, code_style);
                }
                Event::Html(html) => {
                    Self::render_text(&html, buf, &mut x, &mut y, area.x, max_x, max_y, default_style);
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
                    Self::render_text(marker, buf, &mut x, &mut y, area.x, max_x, max_y, default_style);
                }
            }
        }
    }
}

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
    Style::default().fg(fg).bg(bg).add_modifier(Modifier::DIM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_sdk::tree_sitter::highlight::highlight;

    #[test]
    fn test_pulldown_code_block_info_string() {
        let md = "```rust\nfn main() {}\n```\n";
        let parser = pulldown_cmark::Parser::new(md);
        let mut info_str = String::new();
        for event in parser {
            if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(kind)) = event {
                match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info_str = info.to_string();
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(info_str, "rust");
    }

    #[test]
    fn test_pulldown_code_block_with_space_before_lang() {
        let md = "``` rust\nfn main() {}\n```\n";
        let parser = pulldown_cmark::Parser::new(md);
        let mut info_str = String::new();
        for event in parser {
            if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(kind)) = event {
                match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info_str = info.to_string();
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(info_str, "rust", "pulldown trims leading whitespace");
    }

    #[test]
    fn test_pulldown_code_block_text_content() {
        let md = "```rust\nfn main() {}\n```\n";
        let parser = pulldown_cmark::Parser::new(md);
        let mut in_code = false;
        let mut code_text = String::new();
        let mut code_lang = String::new();

        for event in parser {
            match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(kind)) => {
                    in_code = true;
                    if let pulldown_cmark::CodeBlockKind::Fenced(info) = kind {
                        code_lang = info.to_string();
                    }
                }
                pulldown_cmark::Event::Text(text) if in_code => {
                    code_text = text.to_string();
                }
                pulldown_cmark::Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                    in_code = false;
                }
                _ => {}
            }
        }

        let result = highlight(&code_text, &code_lang);
        assert!(
            result.is_some(),
            "highlight should return Some for code block content"
        );
        let spans = result.unwrap();
        assert!(!spans.is_empty(), "should have at least one highlight span");
        let keywords: Vec<_> = spans
            .iter()
            .filter(|s| s.category == HighlightCategory::Keyword)
            .collect();
        assert!(!keywords.is_empty(), "should find keywords (fn)");
    }

    #[test]
    fn test_pulldown_multi_line_code_block() {
        let md = "```rust\nfn main() {\n    let x = 1;\n    println!(\"hello\");\n}\n```\n";
        let parser = pulldown_cmark::Parser::new(md);
        let mut in_code = false;
        let mut code_text = String::new();
        let mut code_lang = String::new();

        for event in parser {
            match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(kind)) => {
                    in_code = true;
                    if let pulldown_cmark::CodeBlockKind::Fenced(info) = kind {
                        code_lang = info.to_string();
                    }
                }
                pulldown_cmark::Event::Text(text) if in_code => {
                    code_text = text.to_string();
                }
                pulldown_cmark::Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                    in_code = false;
                }
                _ => {}
            }
        }

        let result = highlight(&code_text, &code_lang);
        assert!(result.is_some(), "highlight should return Some");
        let spans = result.unwrap();
        assert!(!spans.is_empty(), "should have spans");
    }

    #[test]
    fn test_render_rust_code_block_colors() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let md_content = "```rust\nfn main() {\n    let x = 1;\n}\n```\n";
        let md = super::MarkdownRenderable::new(Some(md_content.to_string()));

        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        let area = Rect::new(0, 0, 60, 20);

        md.render_self(&mut buf, area);

        // Verify the rendered characters at correct positions
        // Line 0 should be "fn main() {" starting at x=0
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "f");
        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "n");

        // Line 1 should be "    let x = 1;" starting at x=0, y=1
        let line1: String = (0..14u16)
            .map(|cx| {
                buf.cell((cx, 1))
                    .unwrap()
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            line1.contains("let x = 1"),
            "line 1 should contain the code"
        );

        // Check 'f' at (0,0) has Keyword color (orange)
        let keyword_color = Some(Color::Rgb(255, 180, 100));
        assert_eq!(buf.cell((0, 0)).unwrap().style().fg, keyword_color);
        assert_eq!(buf.cell((1, 0)).unwrap().style().fg, keyword_color);

        // Check 'm' at (3,0) has Function color (purple)
        let function_color = Some(Color::Rgb(200, 180, 255));
        assert_eq!(buf.cell((3, 0)).unwrap().style().fg, function_color);

        // Check 'l' at (4,1) has Keyword color
        assert_eq!(buf.cell((4, 1)).unwrap().style().fg, keyword_color);

        // Check '1' at (12,1) has Number color (gold)
        let number_color = Some(Color::Rgb(255, 200, 100));
        assert_eq!(buf.cell((12, 1)).unwrap().style().fg, number_color);

        // Check non-keyword chars have default foreground
        let default_fg = Some(Color::Rgb(220, 220, 220));
        assert_eq!(buf.cell((2, 0)).unwrap().style().fg, default_fg);

        // Check code_bg matches default background (no bg set)
        let code_bg = Some(Color::Rgb(0, 0, 0));
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, code_bg);

        // Check DIM modifier is applied
        assert!(
            buf.cell((0, 0))
                .unwrap()
                .style()
                .add_modifier
                .contains(Modifier::DIM)
        );
    }

    #[test]
    fn test_code_block_without_language() {
        let md = "```\nplain code\n```\n";
        let parser = pulldown_cmark::Parser::new(md);
        let mut in_code = false;
        let mut code_text = String::new();
        let mut code_lang = String::new();

        for event in parser {
            match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(kind)) => {
                    in_code = true;
                    if let pulldown_cmark::CodeBlockKind::Fenced(info) = kind {
                        code_lang = info.to_string();
                    }
                }
                pulldown_cmark::Event::Text(text) if in_code => {
                    code_text = text.to_string();
                }
                pulldown_cmark::Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                    in_code = false;
                }
                _ => {}
            }
        }

        let result = highlight(&code_text, &code_lang);
        assert!(
            result.is_none(),
            "highlight should return None for empty lang"
        );
    }
}
