use super::*;

impl SessionView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_reasoning(
        buf: &mut Buffer,
        x: u16,
        y: u16,
        line_h: &mut u16,
        max_w: u16,
        part: &ReasoningPart,
        expanded: bool,
        theme: &Theme,
    ) {
        let header = if expanded { "- " } else { "+ " };
        let title = format!("{header}Thought");
        let header_style = Style::default().fg(rgba_color(theme.warning));
        draw_text_line(buf, &title, x, y, max_w, header_style);
        *line_h = 1;

        if expanded && !part.text.is_empty() {
            // The body is markdown (word-wrapped) at width `max_w - 2`; its
            // row count must come from the renderer's own layout algorithm
            // (`estimate_height`), not from the source line count: a long
            // paragraph wraps into several visual rows, and underestimating
            // the height clipped the body on screen and desynced the copy
            // regions from what is displayed.
            let body_w = max_w.saturating_sub(2).max(1);
            // Sanitize before the markdown renderer so control chars can't
            // reach buffer cells (ratatui cell_width panic). `\n` is kept —
            // the markdown renderer handles line breaks itself. The height
            // is measured on the sanitized text so it matches the render.
            let content = sanitize_text(&part.text);
            let md_h = estimate_height(&content, body_w).max(1);
            let md_area = Rect::new(x + 2, y + 1, body_w, md_h);
            let mut md =
                cosh_tui::core::renderables::markdown::MarkdownRenderable::new(Some(content));
            md.set_fg(Some(ColorInput::RGBA(theme.text_muted)));
            md.set_bg(Some(ColorInput::RGBA(theme.background)));
            crate::util::markdown::apply_theme(&mut md, theme);
            md.render_self(buf, md_area);
            // Dim the whole reasoning body toward the background so it reads as
            // opaque "thinking" text — white becomes gray and syntax-highlight
            // colors get grayed out. Uses the theme's `thinking_opacity`. Cells
            // whose foreground is unset (Reset — how the markdown renderer
            // paints plain paragraphs) are colored from the muted base first,
            // so the plain text also ends up dimmed/gray instead of matching
            // the normal reply's color.
            Self::dim_reasoning_region(
                buf,
                md_area,
                theme.thinking_opacity,
                theme.text_muted,
                theme.background,
            );
            *line_h = 1 + md_h;
        }
    }
}

impl SessionView {
    /// Resolve a ratatui [`Color`] to its (r, g, b) channels when possible.
    pub(super) fn color_to_rgb(c: Color) -> Option<(u8, u8, u8)> {
        match c {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Indexed(i) => Some(ansi256_index_to_rgb(i)),
            _ => None,
        }
    }
}

impl SessionView {
    /// Blend a foreground channel toward a background channel by `keep`
    /// (1.0 = keep the fg fully, 0.0 = fully the bg colour).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(super) fn blend_channel(fg: u8, bg: u8, keep: f64) -> u8 {
        (f64::from(fg) * keep + f64::from(bg) * (1.0 - keep)).round() as u8
    }
}

impl SessionView {
    /// Dim a rendered region toward its background so it looks "pushed back" /
    /// opaque. Used for the reasoning/thinking body so the model's internal
    /// text — including syntax highlighting — appears grayed-out (less bright)
    /// compared to the normal reply. `opacity` comes from the theme's
    /// `thinking_opacity` (1.0 = no dimming). `base_fg` is the fallback colour
    /// used for cells with no explicit foreground (Reset — how plain markdown
    /// paragraphs are painted), which is then also dimmed so the plain text is
    /// visibly gray rather than matching the normal reply.
    pub(super) fn dim_reasoning_region(
        buf: &mut Buffer,
        area: Rect,
        opacity: f64,
        base_fg: RGBA,
        fallback_bg: RGBA,
    ) {
        let keep = opacity.clamp(0.0, 1.0);
        if keep >= 1.0 {
            return;
        }
        let base = Self::color_to_rgb(rgba_color(base_fg)).unwrap_or((220, 220, 220));
        let fallback = Self::color_to_rgb(rgba_color(fallback_bg)).unwrap_or((0, 0, 0));
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let Some(cell) = buf.cell_mut((x, y)) else {
                    continue;
                };
                let (fr, fg_, fb) = Self::color_to_rgb(cell.fg).unwrap_or(base);
                let (br, bg_, bb) = Self::color_to_rgb(cell.bg).unwrap_or(fallback);
                cell.fg = Color::Rgb(
                    Self::blend_channel(fr, br, keep),
                    Self::blend_channel(fg_, bg_, keep),
                    Self::blend_channel(fb, bb, keep),
                );
            }
        }
    }
}
