use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use cosh::harness::events::ModelEntry;
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;

use crate::theme::Theme;

/// Visual item in the model list - either a provider header or a model
#[derive(Debug, Clone)]
enum VisualItem {
    Header(String),
    Model(ModelEntry),
}

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

#[derive(Debug, Clone)]
pub enum DialogType {
    Alert { message: String },
    Confirm { message: String },
    ThemeList {
        themes: Vec<String>,
        current: String,
        filter: String,
    },
    ModelList {
        models: Vec<ModelEntry>,
        current: String,
        filter: String,
    },
}

#[derive(Debug, Clone)]
pub struct DialogInstance {
    pub dialog_type: DialogType,
    pub selected: usize,
    pub last_filter_at: SystemTime,
    pub blink_start: SystemTime,
}

pub struct DialogState {
    pub stack: Vec<DialogInstance>,
}

impl DialogState {
    pub fn new() -> Self {
        DialogState { stack: Vec::new() }
    }

    pub fn show(&mut self, dialog_type: DialogType) {
        self.stack.push(DialogInstance {
            dialog_type,
            selected: 0,
            last_filter_at: SystemTime::now(),
            blink_start: SystemTime::now(),
        });
    }

    pub fn replace(&mut self, dialog_type: DialogType) {
        self.clear();
        self.show(dialog_type);
    }

    pub fn pop(&mut self) {
        self.stack.pop();
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn visible(&self) -> bool {
        !self.stack.is_empty()
    }

    pub fn current(&self) -> Option<&DialogInstance> {
        self.stack.last()
    }

    pub fn current_mut(&mut self) -> Option<&mut DialogInstance> {
        self.stack.last_mut()
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: SystemTime) {
        let Some(instance) = self.stack.last() else {
            return;
        };

        let dialog_w = 50.min(area.width.saturating_sub(4));
        let dialog_x = area.x + (area.width - dialog_w) / 2;

        match &instance.dialog_type {
            DialogType::Alert { message } => {
                let dialog_h = 5;
                let dialog_y = area.y + (area.height - dialog_h) / 2;
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let mut bg = BoxRenderable::new();
                bg.set_background_color(Some(theme.background_element.into()));
                bg.set_border_color(Some(theme.border_active.into()));
                bg.render_self(buf, dialog_area);

                draw_text_line(
                    buf,
                    message,
                    dialog_x + 2,
                    dialog_y + 1,
                    dialog_w.saturating_sub(4),
                    Style::default().fg(rgba_color(theme.text)),
                );

                let ok_text = "[ OK ]";
                let ok_x = dialog_x + (dialog_w - ok_text.len() as u16) / 2;
                let ok_style = Style::default().fg(rgba_color(theme.primary));
                draw_text_line(
                    buf,
                    ok_text,
                    ok_x,
                    dialog_y + 3,
                    dialog_w.saturating_sub(2),
                    ok_style,
                );
            }
            DialogType::Confirm { message } => {
                let dialog_h = 6;
                let dialog_y = area.y + (area.height - dialog_h) / 2;
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                let mut bg = BoxRenderable::new();
                bg.set_background_color(Some(theme.background_element.into()));
                bg.set_border_color(Some(theme.border_active.into()));
                bg.render_self(buf, dialog_area);

                draw_text_line(
                    buf,
                    message,
                    dialog_x + 2,
                    dialog_y + 1,
                    dialog_w.saturating_sub(4),
                    Style::default().fg(rgba_color(theme.text)),
                );

                let options = ["Yes", "No"];
                for (i, opt) in options.iter().enumerate() {
                    let oy = dialog_y + 3 + i as u16;
                    let prefix = if i == instance.selected {
                        "\u{25b8} "
                    } else {
                        "  "
                    };
                    let text = format!("{prefix}{opt}");
                    let style = if i == instance.selected {
                        Style::default().fg(rgba_color(theme.primary))
                    } else {
                        Style::default().fg(rgba_color(theme.text_muted))
                    };
                    draw_text_line(
                        buf,
                        &text,
                        dialog_x + 3,
                        oy,
                        dialog_w.saturating_sub(6),
                        style,
                    );
                }
            }
            DialogType::ThemeList { themes, current, filter } => {
                // Compute filtered list (like fuzzysort in original)
                let filtered: Vec<&str> = if filter.is_empty() {
                    themes.iter().map(|s| s.as_str()).collect()
                } else {
                    let lower = filter.to_lowercase();
                    themes.iter().filter(|t| t.to_lowercase().contains(&lower)).map(|s| s.as_str()).collect()
                };

                let selection = if instance.selected >= filtered.len() {
                    filtered.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                // Responsive sizing: shrink with terminal, minimum 24 cols
                let max_w = 40u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(24).min(area.width.saturating_sub(2));
                let dialog_x = area.x + (area.width - dialog_w) / 2;

                // Fit list to available height
                // Layout: 1 title + 1 filter + 1 gap + max_visible items + 1 paddingBottom = max_visible + 4
                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(filtered.len().max(1)).max(1).min(10);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area.y.saturating_add(
                    (area.height.saturating_sub(dialog_h)) / 2
                );
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background (NO border - original DialogSelect has no border)
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Header area (paddingLeft=4, paddingRight=4 like original)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: Title (bold, like original TextAttributes.BOLD) + "esc" label (right-aligned, muted)
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Themes", header_x, dialog_y, header_w, title_style);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 1: Filter input (background_element bg to match dialog, textMuted fg)
                let bg_element = rgba_color(theme.background_element);
                for cx in header_x..header_x + header_w {
                    if let Some(cell) = buf.cell_mut((cx, dialog_y + 1)) {
                        cell.set_style(Style::default().bg(bg_element));
                    }
                }

                // Blink cursor logic: steady for 500ms after typing, then blink 500ms on/off
                let idle_ms = now
                    .duration_since(instance.last_filter_at)
                    .map_or(0, |d| d.as_millis());
                let cursor_visible = if idle_ms < 500 {
                    true // steady after typing
                } else {
                    let elapsed_ms = now
                        .duration_since(instance.blink_start)
                        .map_or(0, |d| d.as_millis() % 1000);
                    elapsed_ms < 500
                };

                // Show "Search" when empty, otherwise show filter text + cursor
                let has_filter = !filter.is_empty();
                if has_filter {
                    draw_text_line(buf, filter.as_str(), header_x, dialog_y + 1, header_w,
                        Style::default().fg(rgba_color(theme.text)).bg(bg_element));
                    // Blinking cursor at end of filter text
                    let cursor_x = header_x + filter.len() as u16;
                    if cursor_x < header_x + header_w {
                        if let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1)) {
                            if cursor_visible {
                                cell.set_char('\u{2588}');
                                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg_element));
                            } else {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                } else {
                    // Show "Search" label when filter is empty
                    let search_label = "Search";
                    draw_text_line(buf, search_label, header_x, dialog_y + 1, header_w,
                        Style::default().fg(rgba_color(theme.text_muted)).bg(bg_element));
                    // Cursor AFTER "Search" (at position 6)
                    let cursor_x = header_x + search_label.len() as u16;
                    if cursor_x < header_x + header_w {
                        if let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1)) {
                            if cursor_visible {
                                cell.set_char('\u{2588}');
                                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg_element));
                            } else {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                }

                // Line 2: Gap (empty, matches original gap={1})

                // Lines 3+: Theme list (paddingLeft=1, paddingRight=1 like original scrollbox)
                // After list: paddingBottom=1 (line dialog_y + 3 + max_visible)
                let list_top = dialog_y + 3;
                let list_pad = 1; // original scrollbox paddingLeft=1
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);

                if filtered.is_empty() {
                    draw_text_line(
                        buf,
                        "No matching themes",
                        list_x,
                        list_top,
                        list_w,
                        Style::default().fg(rgba_color(theme.text_muted)),
                    );
                } else {
                    let bg_element = rgba_color(theme.background_element);
                    // Scroll offset: keep selection visible
                    let scroll_offset = if selection >= max_visible {
                        selection - max_visible + 1
                    } else {
                        0
                    };
                    let scroll_offset = scroll_offset.min(filtered.len().saturating_sub(max_visible));
                    for (vis_idx, &theme_name) in filtered.iter().enumerate().skip(scroll_offset).take(max_visible) {
                        let ry = list_top + (vis_idx - scroll_offset) as u16;
                        let is_current = theme_name == current.as_str();
                        let is_selected = vis_idx == selection;

                        // Draw full row background first
                        if is_selected {
                            for cx in list_x..list_x + list_w {
                                if let Some(cell) = buf.cell_mut((cx, ry)) {
                                    cell.set_style(Style::default().bg(rgba_color(theme.primary)));
                                }
                            }
                        } else {
                            // Match dialog background — NOT Color::Reset (which is terminal black)
                            for cx in list_x..list_x + list_w {
                                if let Some(cell) = buf.cell_mut((cx, ry)) {
                                    cell.set_style(Style::default().bg(bg_element));
                                }
                            }
                        }

                        // Indicator: ● (U+25cf) with accent color for current theme (changes with preview)
                        // For selected: use contrast color; for non-selected current: use accent
                        let (indicator_fg, indicator_ch) = if is_current {
                            if is_selected {
                                // Selected + current: ● uses contrast foreground
                                let (pr, pg, pb, _) = theme.primary.to_ints();
                                let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
                                (if lum > 0.5 { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) }, "\u{25cf}")
                            } else {
                                // Current but not selected: ● uses accent color (changes with theme preview)
                                (rgba_color(theme.accent), "\u{25cf}")
                            }
                        } else {
                            (Color::Reset, " ")
                        };

                        // Draw ● indicator with its color
                        if let Some(cell) = buf.cell_mut((list_x, ry)) {
                            cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                            cell.set_style(Style::default().fg(indicator_fg).bg(
                                if is_selected { rgba_color(theme.primary) } else { bg_element }
                            ));
                        }
                        // Space after indicator
                        if let Some(cell) = buf.cell_mut((list_x + 1, ry)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(
                                if is_selected { rgba_color(theme.primary) } else { bg_element }
                            ));
                        }

                        // Theme name
                        let (name_fg, name_bg) = if is_selected {
                            let (pr, pg, pb, _) = theme.primary.to_ints();
                            let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
                            (if lum > 0.5 { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) }, rgba_color(theme.primary))
                        } else {
                            (rgba_color(theme.text), bg_element)
                        };
                        draw_text_line(buf, theme_name, list_x + 2, ry, list_w.saturating_sub(2), Style::default().fg(name_fg).bg(name_bg));
                    }
                }
                // Lines after list: paddingBottom=1 (already filled with background)
                // NO footer, NO separator - matching original DialogSelect
            }
            DialogType::ModelList { models, current, filter } => {
                // Group models by provider and filter
                use std::collections::BTreeMap;

                let mut grouped: BTreeMap<String, Vec<&ModelEntry>> = BTreeMap::new();
                for entry in models.iter() {
                    if filter.is_empty() || entry.model.to_lowercase().contains(&filter.to_lowercase()) {
                        grouped.entry(entry.provider.clone()).or_default().push(entry);
                    }
                }

                // Flatten grouped models into a single list for selection
                let flat_entries: Vec<&ModelEntry> = grouped.values().flatten().cloned().collect();

                let selection = if instance.selected >= flat_entries.len() {
                    flat_entries.len().saturating_sub(1)
                } else {
                    instance.selected
                };

                // Responsive sizing: shrink with terminal, minimum 24 cols
                let max_w = 50u16.min(area.width.saturating_sub(4));
                let dialog_w = max_w.max(30).min(area.width.saturating_sub(2));
                let dialog_x = area.x + (area.width - dialog_w) / 2;

                // Calculate total items (models + provider headers)
                let total_items = flat_entries.len() + grouped.len();
                
                // Fit list to available height
                let max_visible_height = (area.height.saturating_sub(4)) as usize;
                let max_visible = max_visible_height.min(total_items.max(1)).max(1);
                let dialog_h = (max_visible + 4) as u16;
                let dialog_y = area.y.saturating_add(
                    (area.height.saturating_sub(dialog_h)) / 2
                );
                let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);

                // Fill background (NO border - original DialogSelect has no border)
                let bg_color = rgba_color(theme.background_element);
                for y in dialog_area.y..dialog_area.bottom() {
                    for x in dialog_area.x..dialog_area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_style(Style::default().bg(bg_color));
                        }
                    }
                }

                // Header area (paddingLeft=4, paddingRight=4 like original)
                let header_pad = 4;
                let header_x = dialog_x + header_pad;
                let header_w = dialog_w.saturating_sub(header_pad * 2);

                // Line 0: Title (bold, like original TextAttributes.BOLD) + "esc" label (right-aligned, muted)
                let title_style = Style::default()
                    .fg(rgba_color(theme.text))
                    .add_modifier(Modifier::BOLD);
                draw_text_line(buf, "Models", header_x, dialog_y, header_w, title_style);
                let esc_label = "esc";
                let esc_x = header_x + header_w.saturating_sub(esc_label.len() as u16);
                draw_text_line(
                    buf,
                    esc_label,
                    esc_x,
                    dialog_y,
                    header_w,
                    Style::default().fg(rgba_color(theme.text_muted)),
                );

                // Line 1: Filter input (background_element bg to match dialog, textMuted fg)
                let bg_element = rgba_color(theme.background_element);
                for cx in header_x..header_x + header_w {
                    if let Some(cell) = buf.cell_mut((cx, dialog_y + 1)) {
                        cell.set_style(Style::default().bg(bg_element));
                    }
                }

                // Blink cursor logic: steady for 500ms after typing, then blink 500ms on/off
                let idle_ms = now
                    .duration_since(instance.last_filter_at)
                    .map_or(0, |d| d.as_millis());
                let cursor_visible = if idle_ms < 500 {
                    true // steady after typing
                } else {
                    let elapsed_ms = now
                        .duration_since(instance.blink_start)
                        .map_or(0, |d| d.as_millis() % 1000);
                    elapsed_ms < 500
                };

                // Show "Search" when empty, otherwise show filter text + cursor
                let has_filter = !filter.is_empty();
                if has_filter {
                    draw_text_line(buf, filter.as_str(), header_x, dialog_y + 1, header_w,
                        Style::default().fg(rgba_color(theme.text)).bg(bg_element));
                    // Blinking cursor at end of filter text
                    let cursor_x = header_x + filter.len() as u16;
                    if cursor_x < header_x + header_w {
                        if let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1)) {
                            if cursor_visible {
                                cell.set_char('\u{2588}');
                                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg_element));
                            } else {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                } else {
                    // Show "Search" label when filter is empty
                    let search_label = "Search";
                    draw_text_line(buf, search_label, header_x, dialog_y + 1, header_w,
                        Style::default().fg(rgba_color(theme.text_muted)).bg(bg_element));
                    // Cursor AFTER "Search" (at position 6)
                    let cursor_x = header_x + search_label.len() as u16;
                    if cursor_x < header_x + header_w {
                        if let Some(cell) = buf.cell_mut((cursor_x, dialog_y + 1)) {
                            if cursor_visible {
                                cell.set_char('\u{2588}');
                                cell.set_style(Style::default().fg(rgba_color(theme.primary)).bg(bg_element));
                            } else {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(bg_element));
                            }
                        }
                    }
                }

                // Line 2: Gap (empty, matches original gap={1})

                // Lines 3+: Model list grouped by provider
                let list_top = dialog_y + 3;
                let list_pad = 1;
                let list_x = dialog_x + list_pad;
                let list_w = dialog_w.saturating_sub(list_pad * 2);

                if flat_entries.is_empty() {
                    draw_text_line(
                        buf,
                        "No matching models",
                        list_x,
                        list_top,
                        list_w,
                        Style::default().fg(rgba_color(theme.text_muted)),
                    );
                } else {
                    let bg_element = rgba_color(theme.background_element);
                    let mut current_y = list_top;

                    // Build visual items: headers + models
                    // Each visual item is either a header or a model
                    // We need to map selection (model index) to visual index
                    let mut visual_items: Vec<VisualItem> = Vec::new();
                    for (provider, entries) in grouped.iter() {
                        visual_items.push(VisualItem::Header(provider.clone()));
                        for entry in entries {
                            visual_items.push(VisualItem::Model((*entry).clone()));
                        }
                    }
                    
                    // Map selection to visual index
                    // Selection is a model index (0..flat_entries.len())
                    // Visual index includes headers
                    let mut model_count = 0;
                    let mut visual_selection = 0;
                    for item in &visual_items {
                        match item {
                            VisualItem::Header(_) => {
                                if model_count <= selection {
                                    visual_selection += 1;
                                }
                            }
                            VisualItem::Model(_) => {
                                if model_count == selection {
                                    visual_selection += 1;
                                    break;
                                }
                                visual_selection += 1;
                                model_count += 1;
                            }
                        }
                    }
                    
                    // Calculate scroll offset
                    let mut scroll_offset = 0;
                    if visual_selection >= max_visible && max_visible < visual_items.len() {
                        scroll_offset = visual_selection.saturating_sub(max_visible - 1);
                    }
                    scroll_offset = scroll_offset.min(visual_items.len().saturating_sub(max_visible));

                    // Draw visible items
                    let mut visual_index = 0;
                    let mut model_index = 0;
                    for item in &visual_items {
                        match item {
                            VisualItem::Header(provider) => {
                                if visual_index >= scroll_offset && (visual_index - scroll_offset) < max_visible {
                                    let header_style = Style::default()
                                        .fg(rgba_color(theme.text_muted))
                                        .add_modifier(Modifier::BOLD);
                                    draw_text_line(buf, provider, list_x, current_y, list_w, header_style);
                                    current_y += 1;
                                }
                            }
                            VisualItem::Model(entry) => {
                                let is_current = entry.model == current.as_str();
                                let is_selected = model_index == selection;
                                
                                if visual_index >= scroll_offset && (visual_index - scroll_offset) < max_visible {
                                    // Draw full row background first
                                    if is_selected {
                                        for cx in list_x..list_x + list_w {
                                            if let Some(cell) = buf.cell_mut((cx, current_y)) {
                                                cell.set_style(Style::default().bg(rgba_color(theme.primary)));
                                            }
                                        }
                                    } else {
                                        for cx in list_x..list_x + list_w {
                                            if let Some(cell) = buf.cell_mut((cx, current_y)) {
                                                cell.set_style(Style::default().bg(bg_element));
                                            }
                                        }
                                    }

                                    // Indicator: ● (U+25cf) with accent color for current model
                                    let (indicator_fg, indicator_ch) = if is_current {
                                        if is_selected {
                                            let (pr, pg, pb, _) = theme.primary.to_ints();
                                            let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
                                            (if lum > 0.5 { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) }, "\u{25cf}")
                                        } else {
                                            (rgba_color(theme.accent), "\u{25cf}")
                                        }
                                    } else {
                                        (Color::Reset, " ")
                                    };

                                    // Draw ● indicator with its color
                                    if let Some(cell) = buf.cell_mut((list_x, current_y)) {
                                        cell.set_char(indicator_ch.chars().next().unwrap_or(' '));
                                        cell.set_style(Style::default().fg(indicator_fg).bg(
                                            if is_selected { rgba_color(theme.primary) } else { bg_element }
                                        ));
                                    }
                                    // Space after indicator
                                    if let Some(cell) = buf.cell_mut((list_x + 1, current_y)) {
                                        cell.set_char(' ');
                                        cell.set_style(Style::default().bg(
                                            if is_selected { rgba_color(theme.primary) } else { bg_element }
                                        ));
                                    }

                                    // Model name
                                    let (name_fg, name_bg) = if is_selected {
                                        let (pr, pg, pb, _) = theme.primary.to_ints();
                                        let lum = (0.299 * f32::from(pr) + 0.587 * f32::from(pg) + 0.114 * f32::from(pb)) / 255.0;
                                        (if lum > 0.5 { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) }, rgba_color(theme.primary))
                                    } else {
                                        (rgba_color(theme.text), bg_element)
                                    };
                                    draw_text_line(buf, &entry.model, list_x + 2, current_y, list_w.saturating_sub(2), Style::default().fg(name_fg).bg(name_bg));

                                    current_y += 1;
                                }
                                model_index += 1;
                            }
                        }
                        visual_index += 1;
                    }
                }
                // Lines after list: paddingBottom=1 (already filled with background)
                // NO footer, NO separator - matching original DialogSelect
            }
        }
    }
}
