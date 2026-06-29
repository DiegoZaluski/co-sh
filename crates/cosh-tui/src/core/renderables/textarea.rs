use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::core::renderable::Renderable;
use crate::core::rgba::{parse_color, ColorInput, RGBA};

static NEXT_TEXTAREA_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextareaAction {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    LineHome,
    LineEnd,
    SelectLineHome,
    SelectLineEnd,
    VisualLineHome,
    VisualLineEnd,
    SelectVisualLineHome,
    SelectVisualLineEnd,
    BufferHome,
    BufferEnd,
    SelectBufferHome,
    SelectBufferEnd,
    DeleteLine,
    DeleteToLineEnd,
    DeleteToLineStart,
    Backspace,
    Delete,
    Newline,
    Undo,
    Redo,
    WordForward,
    WordBackward,
    SelectWordForward,
    SelectWordBackward,
    DeleteWordForward,
    DeleteWordBackward,
    SelectAll,
    Submit,
}

#[derive(Debug, Clone)]
pub struct TextareaKeyBinding {
    pub name: String,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub super_key: bool,
    pub action: TextareaAction,
}

#[derive(Debug, Clone)]
pub struct TextareaOptions {
    pub initial_value: Option<String>,
    pub background_color: Option<ColorInput>,
    pub text_color: Option<ColorInput>,
    pub focused_background_color: Option<ColorInput>,
    pub focused_text_color: Option<ColorInput>,
    pub placeholder: Option<String>,
    pub placeholder_color: Option<ColorInput>,
    pub key_bindings: Option<Vec<TextareaKeyBinding>>,
    pub wrap: bool,
    pub min_height: usize,
    pub max_height: usize,
}

impl Default for TextareaOptions {
    fn default() -> Self {
        TextareaOptions {
            initial_value: None,
            background_color: Some(ColorInput::String("transparent".into())),
            text_color: Some(ColorInput::String("#FFFFFF".into())),
            focused_background_color: Some(ColorInput::String("transparent".into())),
            focused_text_color: Some(ColorInput::String("#FFFFFF".into())),
            placeholder: None,
            placeholder_color: Some(ColorInput::String("#666666".into())),
            key_bindings: None,
            wrap: true,
            min_height: 1,
            max_height: usize::MAX,
        }
    }
}

pub type SubmitEvent = ();

pub type OnSubmitCallback = Box<dyn FnMut()>;

pub struct TextareaRenderable {
    id: String,
    num: u64,
    visible: bool,
    focusable: bool,
    destroyed: bool,
    parent_num: Option<u64>,
    children: Vec<Box<dyn Renderable>>,

    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    sel_start: Option<(usize, usize)>,
    sel_end: Option<(usize, usize)>,
    is_focused: bool,

    unfocused_bg: RGBA,
    unfocused_fg: RGBA,
    focused_bg: RGBA,
    focused_fg: RGBA,
    current_bg: RGBA,
    current_fg: RGBA,
    placeholder: Option<String>,
    placeholder_color: RGBA,

    undo_stack: Vec<String>,
    redo_stack: Vec<String>,
    wrap: bool,

    submit_listener: Option<OnSubmitCallback>,
}

impl TextareaRenderable {
    #[must_use]
    pub fn new(options: TextareaOptions) -> Self {
        let num = NEXT_TEXTAREA_NUM.fetch_add(1, Ordering::Relaxed);
        let opts = options;

        let unfocused_bg = parse_color(opts.background_color.clone().unwrap_or(ColorInput::String("transparent".into())));
        let unfocused_fg = parse_color(opts.text_color.clone().unwrap_or(ColorInput::String("#FFFFFF".into())));
        let focused_bg = parse_color(
            opts.focused_background_color
                .or_else(|| opts.background_color.clone())
                .unwrap_or(ColorInput::String("transparent".into())),
        );
        let focused_fg = parse_color(
            opts.focused_text_color
                .or_else(|| opts.text_color.clone())
                .unwrap_or(ColorInput::String("#FFFFFF".into())),
        );
        let placeholder_color = parse_color(opts.placeholder_color.unwrap_or(ColorInput::String("#666666".into())));

        let lines = if let Some(ref val) = opts.initial_value {
            val.split('\n').map(String::from).collect()
        } else {
            vec![String::new()]
        };

        TextareaRenderable {
            id: format!("textarea-{num}"),
            num,
            visible: true,
            focusable: true,
            destroyed: false,
            parent_num: None,
            children: Vec::new(),

            lines,
            cursor_row: 0,
            cursor_col: 0,
            sel_start: None,
            sel_end: None,
            is_focused: false,

            unfocused_bg,
            unfocused_fg,
            focused_bg,
            focused_fg,
            current_bg: unfocused_bg,
            current_fg: unfocused_fg,
            placeholder: opts.placeholder,
            placeholder_color,

            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            wrap: opts.wrap,

            submit_listener: None,
        }
    }

    pub fn set_on_submit(&mut self, handler: Option<OnSubmitCallback>) {
        self.submit_listener = handler;
    }

    fn update_colors(&mut self) {
        self.current_bg = if self.is_focused { self.focused_bg } else { self.unfocused_bg };
        self.current_fg = if self.is_focused { self.focused_fg } else { self.unfocused_fg };
    }

    pub fn focus(&mut self) {
        self.is_focused = true;
        self.update_colors();
    }

    pub fn blur(&mut self) {
        self.is_focused = false;
        self.update_colors();
    }

    #[must_use]
    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    #[must_use]
    pub fn value(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_value(&mut self, value: &str) {
        self.push_undo();
        self.lines = value.split('\n').map(String::from).collect();
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.sel_start = None;
        self.sel_end = None;
    }

    #[must_use]
    pub fn placeholder(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }

    pub fn set_placeholder(&mut self, value: Option<String>) {
        self.placeholder = value;
    }

    pub fn insert_text(&mut self, text: &str) {
        self.push_undo();
        if let Some((start, end)) = self.sel_start.zip(self.sel_end) {
            self.delete_selection_range(start, end);
            self.sel_start = None;
            self.sel_end = None;
        }
        let sanitized = text.replace('\r', "");
        let mut first = true;
        for ch in sanitized.chars() {
            if ch == '\n' {
                let rest: String = self.lines[self.cursor_row][self.cursor_col..].to_string();
                self.lines[self.cursor_row].truncate(self.cursor_col);
                self.cursor_row += 1;
                self.cursor_col = 0;
                self.lines.insert(self.cursor_row, rest);
            } else {
                self.lines[self.cursor_row].insert(self.cursor_col, ch);
                self.cursor_col += 1;
            }
            if first {
                self.redo_stack.clear();
                first = false;
            }
        }
    }

    pub fn delete_char_backward(&mut self) -> bool {
        self.push_undo();
        if let Some((start, end)) = self.sel_start.zip(self.sel_end) {
            self.delete_selection_range(start, end);
            self.sel_start = None;
            self.sel_end = None;
            return true;
        }
        if self.cursor_col > 0 {
            let byte_idx = char_boundary(&self.lines[self.cursor_row], self.cursor_col - 1);
            self.lines[self.cursor_row].drain(byte_idx..self.cursor_col);
            self.cursor_col -= 1;
            true
        } else if self.cursor_row > 0 {
            let prev_len = self.lines[self.cursor_row - 1].len();
            let rest = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = prev_len;
            self.lines[self.cursor_row].push_str(&rest);
            true
        } else {
            false
        }
    }

    pub fn delete_char(&mut self) -> bool {
        self.push_undo();
        if let Some((start, end)) = self.sel_start.zip(self.sel_end) {
            self.delete_selection_range(start, end);
            self.sel_start = None;
            self.sel_end = None;
            return true;
        }
        if self.cursor_col < self.lines[self.cursor_row].len() {
            let end = char_boundary(&self.lines[self.cursor_row], self.cursor_col + 1);
            self.lines[self.cursor_row].drain(self.cursor_col..end);
            true
        } else if self.cursor_row + 1 < self.lines.len() {
            let rest = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&rest);
            true
        } else {
            false
        }
    }

    pub fn new_line(&mut self) -> bool {
        self.insert_text("\n");
        true
    }

    pub fn submit(&mut self) -> bool {
        if let Some(ref mut handler) = self.submit_listener {
            handler();
        }
        true
    }

    pub fn move_cursor_left(&mut self) -> bool {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
        true
    }

    pub fn move_cursor_right(&mut self) -> bool {
        if self.cursor_col < self.lines[self.cursor_row].len() {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        true
    }

    pub fn move_cursor_up(&mut self) -> bool {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
        true
    }

    pub fn move_cursor_down(&mut self) -> bool {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
        true
    }

    pub fn goto_buffer_home(&mut self) -> bool {
        self.cursor_row = 0;
        self.cursor_col = 0;
        true
    }

    pub fn goto_buffer_end(&mut self) -> bool {
        self.cursor_row = self.lines.len().saturating_sub(1);
        self.cursor_col = self.lines[self.cursor_row].len();
        true
    }

    pub fn goto_line_home(&mut self) -> bool {
        if self.cursor_col == 0 && self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        } else {
            self.cursor_col = 0;
        }
        true
    }

    pub fn goto_line_end(&mut self) -> bool {
        if self.cursor_col == self.lines[self.cursor_row].len() && self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col = self.lines[self.cursor_row].len();
        }
        true
    }

    pub fn delete_line(&mut self) -> bool {
        self.push_undo();
        if self.lines.len() > 1 {
            self.lines.remove(self.cursor_row);
            if self.cursor_row >= self.lines.len() {
                self.cursor_row = self.lines.len().saturating_sub(1);
            }
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        } else {
            self.lines[0].clear();
            self.cursor_col = 0;
        }
        true
    }

    pub fn delete_to_line_end(&mut self) -> bool {
        self.push_undo();
        self.lines[self.cursor_row].truncate(self.cursor_col);
        true
    }

    pub fn delete_to_line_start(&mut self) -> bool {
        self.push_undo();
        if self.cursor_col > 0 {
            self.lines[self.cursor_row].drain(..self.cursor_col);
            self.cursor_col = 0;
        } else if self.cursor_row > 0 {
            let prev_len = self.lines[self.cursor_row - 1].len();
            let rest = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = prev_len;
            self.lines[self.cursor_row].push_str(&rest);
        }
        true
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.value());
            self.lines = prev.split('\n').map(String::from).collect();
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
        true
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.push_undo();
            self.lines = next.split('\n').map(String::from).collect();
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
        true
    }

    pub fn select_all(&mut self) -> bool {
        let last_row = self.lines.len().saturating_sub(1);
        let last_col = self.lines[last_row].len();
        self.sel_start = Some((0, 0));
        self.sel_end = Some((last_row, last_col));
        true
    }

    pub fn set_focused_background_color(&mut self, color: Option<ColorInput>) {
        self.focused_bg = parse_color(color.unwrap_or(ColorInput::String("transparent".into())));
        self.update_colors();
    }

    pub fn set_focused_text_color(&mut self, color: Option<ColorInput>) {
        self.focused_fg = parse_color(color.unwrap_or(ColorInput::String("#FFFFFF".into())));
        self.update_colors();
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.value());
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
    }

    fn delete_selection_range(&mut self, start: (usize, usize), end: (usize, usize)) {
        let (sr, sc) = start;
        let (er, ec) = end;
        if sr == er {
            let from = sc.min(ec);
            let to = sc.max(ec);
            self.lines[sr].drain(from..to);
            self.cursor_row = sr;
            self.cursor_col = from;
        } else {
            self.lines[sr].truncate(sc.min(ec));
            let rest = self.lines[er][ec.min(self.lines[er].len())..].to_string();
            for _ in (sr + 1)..=er {
                self.lines.remove(sr + 1);
            }
            self.lines[sr].push_str(&rest);
            self.cursor_row = sr;
            self.cursor_col = sc.min(ec);
        }
    }

    fn visible_lines(&self, area: Rect) -> Vec<String> {
        let height = area.height as usize;
        let width = area.width as usize;
        let mut result = Vec::new();

        if self.lines.iter().all(std::string::String::is_empty) && self.placeholder.is_some()
            && !self.is_focused
        {
            let text = self.placeholder.as_deref().unwrap_or("");
            for line in text.lines().take(height) {
                let truncated: String = line.chars().take(width).collect();
                result.push(truncated);
            }
            while result.len() < height {
                result.push(String::new());
            }
            return result;
        }

        for line in &self.lines {
            if result.len() >= height {
                break;
            }
            if self.wrap && line.len() > width {
                let mut remaining = line.as_str();
                while !remaining.is_empty() && result.len() < height {
                    let truncated: String = remaining.chars().take(width).collect();
                    let bytes = truncated.len();
                    result.push(truncated);
                    remaining = &remaining[bytes..];
                }
            } else {
                let truncated: String = line.chars().take(width).collect();
                result.push(truncated);
            }
        }

        while result.len() < height {
            result.push(String::new());
        }

        result
    }
}

impl Renderable for TextareaRenderable {
    fn id(&self) -> &str {
        &self.id
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
        let (br, bg, bb, ba) = self.current_bg.to_ints();
        let (fr, fg, fb, fa) = self.current_fg.to_ints();

        let bg_style = if ba == 0 {
            Style::default()
        } else {
            Style::default().bg(Color::Rgb(br, bg, bb))
        };
        let fg_style = if fa == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Rgb(fr, fg, fb))
        };
        let cell_style = bg_style.patch(fg_style);

        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(cell_style);
                }
            }
        }

        let display = self.visible_lines(area);
        for (i, line) in display.iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.bottom() {
                break;
            }
            for (j, ch) in line.chars().enumerate() {
                let x = area.x + j as u16;
                if x >= area.right() {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                    cell.set_style(cell_style);
                }
            }
        }
    }
}

fn char_boundary(s: &str, char_idx: usize) -> usize {
    let mut count = 0;
    for (i, _) in s.char_indices() {
        if count == char_idx {
            return i;
        }
        count += 1;
    }
    s.len()
}
