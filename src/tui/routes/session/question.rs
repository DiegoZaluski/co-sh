use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use crate::theme::Theme;

fn rgba_color(rgba: RGBA) -> Color {
    let (r, g, b, _) = rgba.to_ints();
    Color::Rgb(r, g, b)
}

fn draw_text_line(buf: &mut Buffer, text: &str, x: u16, y: u16, max_w: u16, style: Style) {
    let right = x + max_w;
    for (i, ch) in text.chars().enumerate() {
        let cx = x + i as u16;
        if cx >= right {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn left_border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: ' ',
        horizontal: ' ',
        vertical: '┃',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '┃',
        right_t: ' ',
        cross: ' ',
    }
}

/// Mock questions for visual testing.
#[derive(Debug, Clone)]
struct MockTab {
    header: &'static str,
    question: &'static str,
    multi: bool,
    options: &'static [&'static str],
    answered: bool,
}

const MOCK_TABS: &[MockTab] = &[
    MockTab {
        header: "Language",
        question: "What programming language do you want to use?",
        multi: false,
        options: &["Rust", "Python", "TypeScript", "Go", "Zig"],
        answered: false,
    },
    MockTab {
        header: "Features",
        question: "Which features should we include? (select all that apply)",
        multi: true,
        options: &["CLI", "Web Server", "Database", "Auth", "Tests"],
        answered: false,
    },
    MockTab {
        header: "Name",
        question: "What should we name the project?",
        multi: false,
        options: &[],
        answered: false,
    },
    MockTab {
        header: "Confirm",
        question: "Do you confirm the project setup?",
        multi: false,
        options: &["Yes", "No"],
        answered: false,
    },
];

/// Count of options for a given tab (including the custom answer option).
fn option_count(tab: usize) -> usize {
    if let Some(m) = MOCK_TABS.get(tab) {
        if m.options.is_empty() {
            return 0;
        }
        m.options.len() + 1 // +1 for "Type your own answer"
    } else {
        0
    }
}

/// Inline question dialog rendered inside the session chat (like OpenCode).
/// Mocked: always visible when rendered, showing sample questions.
pub struct QuestionDialog {
    pub visible: bool,
    pub current_tab: usize,
    pub selected_option: usize,
}

impl QuestionDialog {
    pub fn new() -> Self {
        QuestionDialog {
            visible: true,
            current_tab: 0,
            selected_option: 0,
        }
    }

    pub fn set_visible(&mut self, v: bool) {
        self.visible = v;
    }

    /// Handle key events. Returns true if the key was consumed.
    pub fn handle_key(&mut self, key: crossterm::event::KeyCode) -> bool {
        if !self.visible {
            return false;
        }

        let is_confirm = self.current_tab >= MOCK_TABS.len();
        let tab_count = MOCK_TABS.len() + 1; // questions + confirm tab

        match key {
            // Navigation between tabs
            KeyCode::Left | KeyCode::Char('h') => {
                if tab_count > 1 {
                    self.current_tab = if self.current_tab == 0 {
                        tab_count - 1
                    } else {
                        self.current_tab - 1
                    };
                    self.selected_option = 0;
                }
                true
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if tab_count > 1 {
                    self.current_tab = (self.current_tab + 1) % tab_count;
                    self.selected_option = 0;
                }
                true
            }
            KeyCode::Tab => {
                if tab_count > 1 {
                    self.current_tab = (self.current_tab + 1) % tab_count;
                    self.selected_option = 0;
                }
                true
            }

            // Navigation between options (only when not on confirm tab)
            _ if !is_confirm => match key {
                KeyCode::Up | KeyCode::Char('k') => {
                    let count = option_count(self.current_tab);
                    if count > 0 {
                        self.selected_option = if self.selected_option == 0 {
                            count - 1
                        } else {
                            self.selected_option - 1
                        };
                    }
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let count = option_count(self.current_tab);
                    if count > 0 {
                        self.selected_option = (self.selected_option + 1) % count;
                    }
                    true
                }
                KeyCode::Enter => {
                    // Simulate selecting an option
                    let mock = MOCK_TABS.get(self.current_tab);
                    if let Some(m) = mock {
                        let is_custom = self.selected_option >= m.options.len() && !m.options.is_empty();
                        if is_custom && m.options.is_empty() {
                            // No custom option for empty options
                            return true;
                        }
                        // For mock: toggle answered state and move to next tab
                        if tab_count > 1 {
                            // Move to next tab (or confirm)
                            self.current_tab = (self.current_tab + 1) % tab_count;
                            self.selected_option = 0;
                        }
                    }
                    true
                }
                _ => false,
            },

            // Confirm tab
            _ => match key {
                KeyCode::Enter => {
                    // Simulate submit
                    self.visible = false;
                    true
                }
                _ => false,
            },
        }
    }

    /// Render the question prompt inline inside the given area.
    #[allow(clippy::too_many_lines)]
    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        if !self.visible {
            return;
        }

        let is_confirm = self.current_tab >= MOCK_TABS.len();
        let mock = MOCK_TABS.get(self.current_tab);

        // Calculate required height
        let mut h: u16 = 2; // padding top/bottom
        if !is_confirm {
            h += 1; // question text
            if let Some(m) = mock {
                let n_opts = m.options.len() + 1; // options + custom
                h += n_opts as u16;
            }
            if MOCK_TABS.len() > 1 {
                h += 1; // tab bar
            }
        } else {
            h += 1 + MOCK_TABS.len() as u16; // Review header + each question
        }
        h += 1; // footer
        h = h.min(area.height);

        let inner_area = Rect::new(area.x, area.y, area.width, h);

        // Left border + background (matches OpenCode QuestionPrompt style)
        let mut border_box = BoxRenderable::new();
        border_box.set_background_color(Some(theme.background_panel.into()));
        border_box.set_border_color(Some(theme.accent.into()));
        border_box.set_border_sides(BorderSidesConfig {
            left: true,
            top: false,
            right: false,
            bottom: false,
        });
        border_box.set_custom_border_chars(left_border_chars());
        border_box.render_self(buf, inner_area);

        let pad = 3u16;
        let inner_x = area.x + pad;
        let inner_w = area.width.saturating_sub(pad + 2);
        let mut y = area.y + 1;

        // --- Tab bar (for multi questions) ---
        if MOCK_TABS.len() > 1 {
            let mut tab_x = inner_x;
            for (i, t) in MOCK_TABS.iter().enumerate() {
                let is_active = i == self.current_tab;
                let tab_bg = if is_active {
                    rgba_color(theme.accent)
                } else {
                    rgba_color(theme.background_panel)
                };
                let tab_fg = if is_active {
                    let (r, g, b, _) = theme.accent.to_ints();
                    let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
                    if lum > 128.0 {
                        RGBA::from_ints(0, 0, 0, 255)
                    } else {
                        RGBA::from_ints(255, 255, 255, 255)
                    }
                } else if t.answered {
                    theme.text
                } else {
                    theme.text_muted
                };
                let label = format!(" {} ", t.header);
                let tab_style = Style::default().fg(rgba_color(tab_fg)).bg(tab_bg);
                for (ci, ch) in label.chars().enumerate() {
                    let cx = tab_x + ci as u16;
                    if cx >= inner_x + inner_w {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(ch);
                        cell.set_style(tab_style);
                    }
                }
                tab_x += label.len() as u16 + 1;
            }
            // Confirm tab
            let confirm_bg = if is_confirm {
                rgba_color(theme.accent)
            } else {
                rgba_color(theme.background_panel)
            };
            let confirm_fg = if is_confirm {
                let (r, g, b, _) = theme.accent.to_ints();
                let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
                if lum > 128.0 {
                    RGBA::from_ints(0, 0, 0, 255)
                } else {
                    RGBA::from_ints(255, 255, 255, 255)
                }
            } else {
                theme.text_muted
            };
            let confirm_label = " Confirm ";
            let confirm_style = Style::default().fg(rgba_color(confirm_fg)).bg(confirm_bg);
            for (ci, ch) in confirm_label.chars().enumerate() {
                let cx = tab_x + ci as u16;
                if cx >= inner_x + inner_w {
                    break;
                }
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_char(ch);
                    cell.set_style(confirm_style);
                }
            }
            y += 1;
        }

        // --- Separator ---
        y += 1;

        if is_confirm {
            // --- Review screen ---
            let review_style = Style::default().fg(rgba_color(theme.text));
            draw_text_line(buf, "Review", inner_x, y, inner_w, review_style);
            y += 1;
            for t in MOCK_TABS {
                let value = if t.answered { t.options[0] } else { "(not answered)" };
                let label = format!("{}: ", t.header);
                let label_style = Style::default().fg(rgba_color(theme.text_muted));
                draw_text_line(buf, &label, inner_x, y, inner_w, label_style);
                let val_x = inner_x + label.len() as u16;
                let val_style = if t.answered {
                    Style::default().fg(rgba_color(theme.text))
                } else {
                    Style::default().fg(rgba_color(theme.error))
                };
                draw_text_line(buf, value, val_x, y, inner_w.saturating_sub(val_x - inner_x), val_style);
                y += 1;
            }
        } else if let Some(m) = mock {
            // --- Question text ---
            let question_text = if m.multi {
                format!("{} (select all that apply)", m.question)
            } else {
                m.question.to_string()
            };
            let qstyle = Style::default().fg(rgba_color(theme.text));
            draw_text_line(buf, &question_text, inner_x, y, inner_w, qstyle);
            y += 1;

            // --- Options ---
            if !m.options.is_empty() {
                for (i, opt) in m.options.iter().enumerate() {
                    let is_active = i == self.selected_option;

                    // Active row background
                    if is_active {
                        let bg_color = rgba_color(theme.background_element);
                        for cx in inner_x..inner_x + inner_w {
                            if let Some(cell) = buf.cell_mut((cx, y)) {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_color));
                            }
                        }
                    }

                    // Number
                    let num_label = format!("{}.", i + 1);
                    let num_fg = if is_active {
                        theme.secondary
                    } else {
                        theme.text_muted
                    };
                    draw_text_line(
                        buf,
                        &num_label,
                        inner_x,
                        y,
                        inner_w,
                        Style::default().fg(rgba_color(num_fg)),
                    );

                    // Option label
                    let opt_x = inner_x + 3;
                    let opt_label = if m.multi {
                        format!("[ ] {opt}")
                    } else {
                        opt.to_string()
                    };
                    let opt_fg = if is_active {
                        theme.secondary
                    } else {
                        theme.text
                    };
                    draw_text_line(
                        buf,
                        &opt_label,
                        opt_x,
                        y,
                        inner_w.saturating_sub(3),
                        Style::default().fg(rgba_color(opt_fg)),
                    );

                    // Description
                    if is_active {
                        let desc = format!("Option {} - choose below", i + 1);
                        draw_text_line(
                            buf,
                            &desc,
                            inner_x + 4,
                            y + 1,
                            inner_w.saturating_sub(4),
                            Style::default().fg(rgba_color(theme.text_muted)),
                        );
                        y += 1;
                    }

                    y += 1;
                }
            }

            // --- Custom answer option ---
            let custom_active = self.selected_option >= m.options.len() && !m.options.is_empty();
            let custom_fg = if custom_active {
                theme.secondary
            } else {
                theme.text_muted
            };
            if custom_active {
                let bg_color = rgba_color(theme.background_element);
                for cx in inner_x..inner_x + inner_w {
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(bg_color));
                    }
                }
            }
            let num_label = format!("{}.", m.options.len() + 1);
            draw_text_line(
                buf,
                &num_label,
                inner_x,
                y,
                inner_w,
                Style::default().fg(rgba_color(if custom_active { theme.secondary } else { theme.text_muted })),
            );
            let custom_text = if m.multi {
                "[ ] Type your own answer"
            } else {
                "Type your own answer"
            };
            draw_text_line(
                buf,
                custom_text,
                inner_x + 3,
                y,
                inner_w.saturating_sub(3),
                Style::default().fg(rgba_color(custom_fg)),
            );
            y += 1;
        }

        // --- Footer ---
        let footer_y = inner_area.bottom().saturating_sub(1);
        let mut fx = inner_x;
        if MOCK_TABS.len() > 1 {
            let hint = "⇆";
            draw_text_line(buf, hint, fx, footer_y, inner_w.saturating_sub(fx - inner_x),
                Style::default().fg(rgba_color(theme.text)));
            fx += hint.len() as u16 + 1;
            draw_text_line(buf, "tab", fx, footer_y, inner_w.saturating_sub(fx - inner_x),
                Style::default().fg(rgba_color(theme.text_muted)));
            fx += 4;
        }
        if !is_confirm {
            let hint = "↑↓";
            draw_text_line(buf, hint, fx, footer_y, inner_w.saturating_sub(fx - inner_x),
                Style::default().fg(rgba_color(theme.text)));
            fx += hint.len() as u16 + 1;
            draw_text_line(buf, "select", fx, footer_y, inner_w.saturating_sub(fx - inner_x),
                Style::default().fg(rgba_color(theme.text_muted)));
            fx += 7;
        }
        let enter_action = if is_confirm { "submit" } else { "select" };
        draw_text_line(buf, "enter", fx, footer_y, inner_w.saturating_sub(fx - inner_x),
            Style::default().fg(rgba_color(theme.text)));
        fx += 6;
        draw_text_line(buf, enter_action, fx, footer_y, inner_w.saturating_sub(fx - inner_x),
            Style::default().fg(rgba_color(theme.text_muted)));
        fx += enter_action.len() as u16 + 2;
        draw_text_line(buf, "esc", fx, footer_y, inner_w.saturating_sub(fx - inner_x),
            Style::default().fg(rgba_color(theme.text)));
        fx += 4;
        draw_text_line(buf, "dismiss", fx, footer_y, inner_w.saturating_sub(fx - inner_x),
            Style::default().fg(rgba_color(theme.text_muted)));

        // [MOCK] badge
        let mock_label = "[MOCK]";
        let badge_x = inner_x + inner_w.saturating_sub(mock_label.len() as u16);
        draw_text_line(buf, mock_label, badge_x, footer_y, mock_label.len() as u16,
            Style::default().fg(rgba_color(theme.warning)));
    }
}

// Import needed for handle_key
use crossterm::event::KeyCode;
