use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::core::lib::unicode_util;
use crate::core::renderable::Renderable;
use crate::core::rgba::RGBA;
use cosh_sdk::tree_sitter::highlight::{HighlightCategory, highlight};

use super::markdown::SyntaxColors;

static NEXT_DIFF_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffLineType {
    Context,
    Add,
    Remove,
    HunkHeader,
    FileHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffViewMode {
    Unified,
    Split,
}

#[derive(Clone)]
struct LineInfo {
    content: String,
    line_type: DiffLineType,
    old_ln: Option<u32>,
    new_ln: Option<u32>,
}

/// A paired line for split view: left side (old) and right side (new).
struct SplitLine {
    left: Option<LineInfo>,
    right: Option<LineInfo>,
}

/// Geometry of one panel in the split view.
struct PanelLayout {
    /// Left edge of the panel.
    x: u16,
    /// Panel width in columns.
    w: u16,
    /// Line-number column width.
    ln_w: u16,
    /// Rightmost column of the whole diff area.
    max_x: u16,
}

pub struct DiffRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    diff: String,
    view_mode: DiffViewMode,
    added_bg: RGBA,
    removed_bg: RGBA,
    context_bg: RGBA,
    added_sign_color: RGBA,
    removed_sign_color: RGBA,
    hunk_header_fg: RGBA,
    line_number_fg: RGBA,
    added_line_number_bg: RGBA,
    removed_line_number_bg: RGBA,
    show_line_numbers: bool,
    /// Tree-sitter language for syntax-highlighting the code body of
    /// added/removed/context lines (e.g. `"rust"`). `None` disables it.
    code_lang: Option<String>,
    /// Theme syntax colors — the same palette `MarkdownRenderable` uses for
    /// fenced code blocks, so diffs and codeblocks share one coloration.
    syntax_colors: SyntaxColors,
    /// Fallback foreground for code without a highlight category.
    code_default_fg: RGBA,
}

const fn rgba_color(c: RGBA) -> Color {
    let (r, g, b, a) = c.to_ints();
    if a == 0 {
        Color::Reset
    } else {
        Color::Rgb(r, g, b)
    }
}

impl DiffRenderable {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(diff: Option<String>) -> Self {
        let num = NEXT_DIFF_NUM.fetch_add(1, Ordering::Relaxed);
        Self {
            id: format!("diff-{num}"),
            num,
            visible: true,
            focusable: false,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),
            diff: diff.unwrap_or_default(),
            view_mode: DiffViewMode::Unified,
            added_bg: RGBA::from_hex("#1a4d1a"),
            removed_bg: RGBA::from_hex("#4d1a1a"),
            context_bg: RGBA::from_ints(0, 0, 0, 0),
            added_sign_color: RGBA::from_hex("#22c55e"),
            removed_sign_color: RGBA::from_hex("#ef4444"),
            hunk_header_fg: RGBA::from_hex("#828bb8"),
            line_number_fg: RGBA::from_hex("#888888"),
            added_line_number_bg: RGBA::from_ints(0, 0, 0, 0),
            removed_line_number_bg: RGBA::from_ints(0, 0, 0, 0),
            show_line_numbers: true,
            code_lang: None,
            syntax_colors: SyntaxColors::default(),
            code_default_fg: RGBA::from_ints(255, 255, 255, 255),
        }
    }

    pub fn set_diff(&mut self, value: String) {
        self.diff = value;
    }
    pub const fn set_view_mode(&mut self, value: DiffViewMode) {
        self.view_mode = value;
    }
    pub const fn set_added_bg(&mut self, value: RGBA) {
        self.added_bg = value;
    }
    pub const fn set_removed_bg(&mut self, value: RGBA) {
        self.removed_bg = value;
    }
    pub const fn set_context_bg(&mut self, value: RGBA) {
        self.context_bg = value;
    }
    pub const fn set_added_sign_color(&mut self, value: RGBA) {
        self.added_sign_color = value;
    }
    pub const fn set_removed_sign_color(&mut self, value: RGBA) {
        self.removed_sign_color = value;
    }
    pub const fn set_hunk_header_fg(&mut self, value: RGBA) {
        self.hunk_header_fg = value;
    }
    pub const fn set_line_number_fg(&mut self, value: RGBA) {
        self.line_number_fg = value;
    }
    pub const fn set_added_line_number_bg(&mut self, value: RGBA) {
        self.added_line_number_bg = value;
    }
    pub const fn set_removed_line_number_bg(&mut self, value: RGBA) {
        self.removed_line_number_bg = value;
    }
    pub const fn set_show_line_numbers(&mut self, value: bool) {
        self.show_line_numbers = value;
    }
    pub fn set_code_lang(&mut self, value: Option<String>) {
        self.code_lang = value;
    }
    pub fn set_syntax_colors(&mut self, value: SyntaxColors) {
        self.syntax_colors = value;
    }
    pub const fn set_code_default_fg(&mut self, value: RGBA) {
        self.code_default_fg = value;
    }

    /// Resolve a tree-sitter category through the codeblock syntax palette,
    /// falling back to the default code foreground.
    fn syntax_fg(&self, cat: Option<HighlightCategory>) -> RGBA {
        let s = &self.syntax_colors;
        let opt = match cat {
            Some(HighlightCategory::Comment) => s.comment,
            Some(HighlightCategory::Keyword) => s.keyword,
            Some(HighlightCategory::Function) => s.function,
            Some(HighlightCategory::String) => s.string,
            Some(HighlightCategory::Number) => s.number,
            Some(HighlightCategory::Type) => s.r#type,
            Some(HighlightCategory::Builtin) => s.builtin,
            None => None,
        };
        opt.unwrap_or(self.code_default_fg)
    }

    /// Byte-to-category map for one code fragment, or `None` when no
    /// language is set / the language is unsupported. Mirrors how
    /// `MarkdownRenderable` highlights fenced code blocks.
    fn highlight_cats(&self, text: &str) -> Option<Vec<Option<HighlightCategory>>> {
        let lang = self.code_lang.as_deref()?;
        if lang.is_empty() || text.is_empty() {
            return None;
        }
        let spans = highlight(text, lang)?;
        let mut cats: Vec<Option<HighlightCategory>> = vec![None; text.len()];
        for span in &spans {
            let end = span.end.min(text.len());
            if span.start >= end {
                continue;
            }
            for item in &mut cats[span.start..end] {
                *item = Some(span.category);
            }
        }
        Some(cats)
    }

    /// Per-grapheme style for a code fragment: syntax foreground on the
    /// diff background. `cats` is the [`Self::highlight_cats`] map for
    /// `text`; `byte_pos` is the byte offset of the grapheme in `text`.
    fn code_style(
        &self,
        cats: Option<&[Option<HighlightCategory>]>,
        byte_pos: usize,
        bg: Color,
    ) -> Style {
        let cat = cats.and_then(|c| c.get(byte_pos).copied().flatten());
        Style::default().bg(bg).fg(rgba_color(self.syntax_fg(cat)))
    }

    fn classify_line(line: &str) -> DiffLineType {
        if line.starts_with("+++") || line.starts_with("---") {
            DiffLineType::FileHeader
        } else if line.starts_with("@@") {
            DiffLineType::HunkHeader
        } else if line.starts_with('+') {
            DiffLineType::Add
        } else if line.starts_with('-') {
            DiffLineType::Remove
        } else {
            DiffLineType::Context
        }
    }

    fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
        let t = line.trim_start_matches("@@").trim_end_matches("@@").trim();
        let parts: Vec<&str> = t.split(' ').collect();
        if parts.len() < 2 {
            return None;
        }
        let old = parts[0]
            .trim_start_matches('-')
            .split(',')
            .next()?
            .parse::<u32>()
            .ok()?;
        let new = parts[1]
            .trim_start_matches('+')
            .split(',')
            .next()?
            .parse::<u32>()
            .ok()?;
        Some((old, new))
    }

    fn parse_lines(&self) -> Vec<LineInfo> {
        let mut out: Vec<LineInfo> = Vec::new();
        let mut old_ln = 0u32;
        let mut new_ln = 0u32;

        for raw in self.diff.lines() {
            let lt = Self::classify_line(raw);
            match lt {
                DiffLineType::HunkHeader => {
                    if let Some((ol, nl)) = Self::parse_hunk_header(raw) {
                        old_ln = ol;
                        new_ln = nl;
                    }
                    out.push(LineInfo {
                        content: raw.to_string(),
                        line_type: lt,
                        old_ln: None,
                        new_ln: None,
                    });
                }
                DiffLineType::FileHeader => {
                    out.push(LineInfo {
                        content: raw.to_string(),
                        line_type: lt,
                        old_ln: None,
                        new_ln: None,
                    });
                }
                DiffLineType::Add => {
                    out.push(LineInfo {
                        content: raw.to_string(),
                        line_type: lt,
                        old_ln: None,
                        new_ln: Some(new_ln),
                    });
                    new_ln = new_ln.saturating_add(1);
                }
                DiffLineType::Remove => {
                    out.push(LineInfo {
                        content: raw.to_string(),
                        line_type: lt,
                        old_ln: Some(old_ln),
                        new_ln: None,
                    });
                    old_ln = old_ln.saturating_add(1);
                }
                DiffLineType::Context => {
                    out.push(LineInfo {
                        content: raw.to_string(),
                        line_type: lt,
                        old_ln: Some(old_ln),
                        new_ln: Some(new_ln),
                    });
                    old_ln = old_ln.saturating_add(1);
                    new_ln = new_ln.saturating_add(1);
                }
            }
        }
        out
    }

    /// Build paired left/right lines for split view.
    /// Groups consecutive removes with following adds into aligned change blocks.
    fn build_split_lines(&self) -> Vec<SplitLine> {
        let lines = self.parse_lines();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            match lines[i].line_type {
                DiffLineType::FileHeader | DiffLineType::HunkHeader | DiffLineType::Context => {
                    out.push(SplitLine {
                        left: Some(lines[i].clone()),
                        right: Some(lines[i].clone()),
                    });
                    i += 1;
                }
                DiffLineType::Remove => {
                    let mut removes = Vec::new();
                    while i < lines.len() && lines[i].line_type == DiffLineType::Remove {
                        removes.push(lines[i].clone());
                        i += 1;
                    }
                    let mut adds = Vec::new();
                    while i < lines.len() && lines[i].line_type == DiffLineType::Add {
                        adds.push(lines[i].clone());
                        i += 1;
                    }
                    let max_len = removes.len().max(adds.len());
                    for j in 0..max_len {
                        out.push(SplitLine {
                            left: removes.get(j).cloned(),
                            right: adds.get(j).cloned(),
                        });
                    }
                }
                DiffLineType::Add => {
                    out.push(SplitLine {
                        left: None,
                        right: Some(lines[i].clone()),
                    });
                    i += 1;
                }
            }
        }
        out
    }

    fn render_split_view(&self, buf: &mut Buffer, area: Rect) {
        let split_lines = self.build_split_lines();
        if split_lines.is_empty() {
            return;
        }

        let max_y = area.y.saturating_add(area.height);
        let max_x = area.x.saturating_add(area.width);

        // Max line numbers per side
        let left_max_n = split_lines
            .iter()
            .filter_map(|sl| sl.left.as_ref().and_then(|l| l.old_ln.or(l.new_ln)))
            .max()
            .unwrap_or(0);
        let right_max_n = split_lines
            .iter()
            .filter_map(|sl| sl.right.as_ref().and_then(|l| l.old_ln.or(l.new_ln)))
            .max()
            .unwrap_or(0);

        let left_ln_w = if self.show_line_numbers && left_max_n > 0 {
            left_max_n.ilog10() as u16 + 1
        } else {
            1
        };
        let right_ln_w = if self.show_line_numbers && right_max_n > 0 {
            right_max_n.ilog10() as u16 + 1
        } else {
            1
        };

        // Panel widths: left gets floor, right gets ceil, 1 separator column
        let avail_w = area.width;
        if avail_w < 3 {
            return;
        }
        let left_panel_w = (avail_w.saturating_sub(1)) / 2;
        let right_panel_w = avail_w.saturating_sub(1) - left_panel_w;

        let left_x = area.x;
        let sep_x = left_x + left_panel_w;
        let right_x = sep_x + 1;

        let mut y = area.y;
        for sl in &split_lines {
            if y >= max_y {
                break;
            }

            // Left side
            self.render_split_line(
                buf,
                y,
                &PanelLayout {
                    x: left_x,
                    w: left_panel_w,
                    ln_w: left_ln_w,
                    max_x,
                },
                true,
                sl.left.as_ref(),
            );

            // Separator column
            if sep_x < max_x
                && let Some(cell) = buf.cell_mut((sep_x, y))
            {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }

            // Right side
            self.render_split_line(
                buf,
                y,
                &PanelLayout {
                    x: right_x,
                    w: right_panel_w,
                    ln_w: right_ln_w,
                    max_x,
                },
                false,
                sl.right.as_ref(),
            );

            y += 1;
        }
    }

    /// Leaf padding: columns of leaf-colored blank space between the area's
    /// left edge and the line-number gutter. Shifts the gutter (and the
    /// content after it) off the leaf edge; the trailing background fill
    /// mirrors it on the right edge. Shared by BOTH views — split panels and
    /// the unified leaf — so their line numbers land on the same column.
    const LEAF_PAD: u16 = 1;

    /// Render one line within a split panel.
    fn render_split_line(
        &self,
        buf: &mut Buffer,
        y: u16,
        layout: &PanelLayout,
        is_left: bool,
        line: Option<&LineInfo>,
    ) {
        let max_x_panel = (layout.x + layout.w).min(layout.max_x);

        let (default_bg, sign_ch, line_num) = line.map_or_else(
            || {
                let bg = if is_left {
                    rgba_color(self.removed_bg)
                } else {
                    rgba_color(self.added_bg)
                };
                (bg, ' ', None)
            },
            |li| {
                let bg = if is_left {
                    match li.line_type {
                        DiffLineType::Remove => rgba_color(self.removed_bg),
                        _ => rgba_color(self.context_bg),
                    }
                } else {
                    match li.line_type {
                        DiffLineType::Add => rgba_color(self.added_bg),
                        _ => rgba_color(self.context_bg),
                    }
                };
                let sign = match (li.line_type, is_left) {
                    (DiffLineType::Remove, true) => '-',
                    (DiffLineType::Add, false) => '+',
                    _ => ' ',
                };
                let ln = if is_left { li.old_ln } else { li.new_ln };
                (bg, sign, ln)
            },
        );

        let content_style = Style::default().bg(default_bg);
        // File headers (`--- a/` / `+++ b/`) read in the same red/green the
        // sign characters use — painted on the FONT, background untouched —
        // so they match the unified view's header coloring.
        let content_style = match line {
            Some(li) if matches!(li.line_type, DiffLineType::FileHeader) => content_style
                .fg(rgba_color(self.file_header_fg(&li.content))),
            _ => content_style,
        };
        let ln_style = Style::default()
            .bg(default_bg)
            .fg(rgba_color(self.line_number_fg));

        let mut x = layout.x;

        // Leaf padding: LEAF_PAD columns of gutter background between the
        // panel's left edge and the line-number gutter. Everything after it
        // (numbers, sign, content) shifts right by the same amount, so both
        // leaves indent their gutter identically; the trailing background
        // fill mirrors this on the panel's right edge.
        for _ in 0..Self::LEAF_PAD {
            if x >= max_x_panel {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(ln_style);
            }
            x += 1;
        }

        // Line number
        if self.show_line_numbers {
            let ln_str = line_num.map_or_else(
                || " ".repeat(layout.ln_w as usize),
                |n| format!("{:>w$}", n, w = layout.ln_w as usize),
            );
            for ch in ln_str.chars() {
                if x >= max_x_panel {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(ln_style);
                }
                x += 1;
            }
        }

        // Space after line number
        if self.show_line_numbers && x < max_x_panel {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(ln_style);
            }
            x += 1;
        }

        // Sign character
        if x < max_x_panel {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(sign_ch);
                cell.set_style(content_style);
            }
            x += 1;
        }

        // Space after sign
        if x < max_x_panel {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(content_style);
            }
            x += 1;
        }

        // Content (truncated to panel width, no wrapping), syntax-highlighted
        // with the codeblock palette when the line carries code.
        if let Some(li) = line {
            let content = Self::content_part(&li.content, li.line_type);
            let is_code = matches!(
                li.line_type,
                DiffLineType::Add | DiffLineType::Remove | DiffLineType::Context
            );
            let cats = if is_code {
                self.highlight_cats(content)
            } else {
                None
            };
            let mut byte_pos = 0usize;
            // Skip control characters (e.g. \t in diffs): writing them into
            // buffer cells makes ratatui's buffer diff panic
            // ("control character passed to cell_width without filtering").
            for (grapheme, w) in unicode_util::graphemes_with_width(content) {
                let len = grapheme.len();
                let is_control = grapheme.chars().any(char::is_control);
                let cat_pos = byte_pos;
                byte_pos += len;
                if is_control {
                    continue;
                }
                if x + w > max_x_panel {
                    break;
                }
                let style = if is_code {
                    self.code_style(cats.as_deref(), cat_pos, default_bg)
                } else {
                    content_style
                };
                if let Some(cell) = buf.cell_mut((x, y)) {
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
                        if let Some(cell) = buf.cell_mut((x + dx, y)) {
                            cell.set_diff_option(CellDiffOption::Skip);
                        }
                    }
                }
                x += w;
            }
        }

        // Fill remaining with background
        while x < max_x_panel {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(content_style);
            }
            x += 1;
        }
    }

    fn max_ln(lines: &[LineInfo]) -> u16 {
        let mx = lines
            .iter()
            .filter_map(|l| l.old_ln.or(l.new_ln))
            .max()
            .unwrap_or(0);
        if mx == 0 {
            return 1;
        }
        mx.ilog10() as u16 + 1
    }

    fn content_part(content: &str, lt: DiffLineType) -> &str {
        match lt {
            DiffLineType::Add | DiffLineType::Remove if !content.is_empty() => &content[1..],
            _ => content,
        }
    }

    /// Foreground for the `--- a/` / `+++ b/` file headers: the removed /
    /// added sign colors, so the headers read in the same red/green the diff
    /// lines use — painted on the FONT, never as a background. Anything else
    /// (none today) keeps the hunk-header color.
    fn file_header_fg(&self, content: &str) -> RGBA {
        if content.starts_with("+++") {
            self.added_sign_color
        } else if content.starts_with("---") {
            self.removed_sign_color
        } else {
            self.hunk_header_fg
        }
    }

    fn line_styles(&self, lt: DiffLineType, content: &str) -> (Style, Style, Style) {
        // returns (sign_style, content_style, ln_style)
        match lt {
            DiffLineType::Add => {
                let bg = rgba_color(self.added_bg);
                let sf = rgba_color(self.added_sign_color);
                let lb = rgba_color(self.added_line_number_bg);
                (
                    Style::default().bg(bg).fg(sf),
                    Style::default().bg(bg),
                    Style::default().bg(lb).fg(sf),
                )
            }
            DiffLineType::Remove => {
                let bg = rgba_color(self.removed_bg);
                let sf = rgba_color(self.removed_sign_color);
                let lb = rgba_color(self.removed_line_number_bg);
                (
                    Style::default().bg(bg).fg(sf),
                    Style::default().bg(bg),
                    Style::default().bg(lb).fg(sf),
                )
            }
            DiffLineType::HunkHeader => {
                let bg = rgba_color(self.context_bg);
                let fg = rgba_color(self.hunk_header_fg);
                let bold = Style::default().fg(fg).add_modifier(Modifier::BOLD);
                (
                    bold.bg(bg),
                    Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
                    Style::default().bg(bg),
                )
            }
            DiffLineType::FileHeader => {
                let bg = rgba_color(self.context_bg);
                let fg = rgba_color(self.file_header_fg(content));
                (
                    Style::default().bg(bg).fg(fg),
                    Style::default().bg(bg).fg(fg),
                    Style::default().bg(bg),
                )
            }
            DiffLineType::Context => {
                let bg = rgba_color(self.context_bg);
                let lf = rgba_color(self.line_number_fg);
                (
                    Style::default().bg(bg),
                    Style::default().bg(bg),
                    Style::default().bg(bg).fg(lf),
                )
            }
        }
    }
}

impl Renderable for DiffRenderable {
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
        if self.view_mode == DiffViewMode::Split && area.width >= 50 {
            self.render_split_view(buf, area);
        } else {
            self.render_unified_view(buf, area);
        }
    }
}

impl DiffRenderable {
    /// Number of buffer rows `render_self` paints for an area `area_width`
    /// columns wide (the view mode is chosen exactly as `render_self` does).
    /// The split view pairs consecutive removes with adds on shared rows, so
    /// it paints FEWER rows than the diff has source lines; the unified view
    /// may paint more (long lines wrap). Box callers size themselves from
    /// this so their bottom padding stays symmetric.
    pub fn painted_rows(&self, area_width: u16) -> usize {
        if self.view_mode == DiffViewMode::Split && area_width >= 50 {
            self.build_split_lines().len()
        } else {
            self.parse_lines().len()
        }
    }

    fn render_unified_view(&self, buf: &mut Buffer, area: Rect) {
        let lines = self.parse_lines();
        if lines.is_empty() {
            return;
        }

        let max_x = area.x.saturating_add(area.width);
        let max_y = area.y.saturating_add(area.height);
        let max_ln_width = if self.show_line_numbers {
            Self::max_ln(&lines)
        } else {
            0
        };
        let gutter_w = if self.show_line_numbers {
            max_ln_width + 2
        } else {
            2
        };
        let mut y = area.y;

        for li in &lines {
            if y >= max_y {
                break;
            }

            let (sign_style, content_style, ln_style) =
                self.line_styles(li.line_type, &li.content);
            let content_w = Self::content_part(&li.content, li.line_type);
            let line_num = match li.line_type {
                DiffLineType::Add => li.new_ln,
                DiffLineType::Remove | DiffLineType::Context => li.old_ln,
                _ => None,
            };

            // Build prefix (gutter) for the first visual line
            let prefix = if self.show_line_numbers {
                line_num.map_or_else(
                    || format!("{:>w$} ", "", w = max_ln_width as usize),
                    |n| format!("{:>w$} ", n, w = max_ln_width as usize),
                )
            } else {
                String::from(" ")
            };

            // The sign character
            let sign_ch = match li.line_type {
                DiffLineType::Add => '+',
                DiffLineType::Remove => '-',
                _ => ' ',
            };

            // The leaf padding column is carved out of the content width so
            // wrapped lines never bleed into the right edge.
            let content_max_w = area
                .width
                .saturating_sub(gutter_w)
                .saturating_sub(Self::LEAF_PAD);
            if content_max_w == 0 {
                continue;
            }

            let wrapped = unicode_util::word_wrap(content_w, content_max_w);
            let is_code = matches!(
                li.line_type,
                DiffLineType::Add | DiffLineType::Remove | DiffLineType::Context
            );
            let content_bg = content_style.bg.unwrap_or(Color::Reset);

            for (wl_idx, wl) in wrapped.iter().enumerate() {
                if y >= max_y {
                    break;
                }
                let mut x = area.x;

                // Leaf padding: one leaf-colored column before the gutter on
                // every visual row (continuation rows too), mirroring the
                // split panels so both views indent identically.
                for _ in 0..Self::LEAF_PAD {
                    if x >= max_x {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(' ');
                        cell.set_style(ln_style);
                    }
                    x += 1;
                }

                if wl_idx == 0 {
                    // Render line number area (gutter)
                    if self.show_line_numbers {
                        for ch in prefix.chars() {
                            if x >= max_x {
                                break;
                            }
                            if let Some(cell) = buf.cell_mut((x, y)) {
                                cell.set_char(ch);
                                cell.set_style(ln_style);
                            }
                            x += 1;
                        }
                    }

                    // Render sign character
                    if x < max_x {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(sign_ch);
                            cell.set_style(sign_style);
                        }
                        x += 1;
                    }

                    // Space after sign
                    if x < max_x {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(sign_style);
                        }
                        x += 1;
                    }
                } else {
                    // Continuation line: fill gutter with content background
                    for _ in 0..gutter_w {
                        if x >= max_x {
                            break;
                        }
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                            cell.set_style(content_style);
                        }
                        x += 1;
                    }
                }

                // Render content, syntax-highlighted with the codeblock
                // palette when the line carries code. Each wrapped chunk is
                // highlighted on its own so grapheme byte offsets line up
                // with the highlight map.
                let cats = if is_code {
                    self.highlight_cats(wl)
                } else {
                    None
                };
                let mut byte_pos = 0usize;
                for (grapheme, w) in unicode_util::graphemes_with_width(wl) {
                    let len = grapheme.len();
                    // Skip control characters (e.g. \t in diffs) — see above.
                    let is_control = grapheme.chars().any(char::is_control);
                    let cat_pos = byte_pos;
                    byte_pos += len;
                    if is_control {
                        continue;
                    }
                    if x + w > max_x {
                        break;
                    }
                    let style = if is_code {
                        self.code_style(cats.as_deref(), cat_pos, content_bg)
                    } else {
                        content_style
                    };
                    if let Some(cell) = buf.cell_mut((x, y)) {
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
                            if let Some(cell) = buf.cell_mut((x + dx, y)) {
                                cell.set_diff_option(CellDiffOption::Skip);
                            }
                        }
                    }
                    x += w;
                }

                // Fill remaining width with content bg
                while x < max_x {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(content_style);
                    }
                    x += 1;
                }

                y += 1;
            }
        }
    }
}
