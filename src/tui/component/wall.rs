use cosh_tui::core::lib::border::{BorderCharacters, BorderSidesConfig};
use cosh_tui::core::lib::rgba::RGBA;
use cosh_tui::core::renderable::Renderable;
use cosh_tui::core::renderables::r#box::BoxRenderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Border glyphs for the shared side-rail wall used by inline dialogs and
/// the prompt. The rails sit on the terminal background; the wall fill is
/// painted separately one column inside each rail.
const fn border_chars() -> BorderCharacters {
    BorderCharacters {
        top_left: ' ',
        top_right: ' ',
        bottom_left: ' ',
        bottom_right: '\u{2579}',
        horizontal: ' ',
        vertical: '┃',
        top_t: ' ',
        bottom_t: ' ',
        left_t: '┃',
        right_t: ' ',
        cross: ' ',
    }
}

/// Render the shared wall: a `┃` rail on each side and a background area
/// that stops before both rails.
pub(crate) fn render(buf: &mut Buffer, area: Rect, border_color: RGBA, background_color: RGBA) {
    let mut border_box = BoxRenderable::new();
    border_box.set_border_color(Some(border_color.into()));
    border_box.set_border_sides(BorderSidesConfig {
        left: true,
        top: false,
        right: true,
        bottom: false,
    });
    border_box.set_custom_border_chars(border_chars());
    border_box.render_self(buf, area);

    let mut background_box = BoxRenderable::new();
    background_box.set_background_color(Some(background_color.into()));
    let background_area = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    background_box.render_self(buf, background_area);
}
