#[allow(unused_imports)]
use super::App;

/// The full create-db field drag selection flow through the app's real
/// mouse dispatch: Down anchors, Drag extends, the render paints the
/// selection, and Up auto-copies then clears it.
#[tokio::test]
#[cfg(feature = "embed")]
async fn rag_field_drag_selection_through_app_mouse_events() {
    use crate::routes::rag::models::CreateDbFocus;
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let mut app = App::new("/tmp".to_string());
    app.show_rag = true;
    app.rag_view.toggle_create_db();
    app.rag_view.create_db_focus = CreateDbFocus::Name;
    app.rag_view.db_name_input = "hello world".into();

    // Geometry mirrors the app mouse handler (tools_area = Rect(0, 1, 80, 20))
    // and the rag render: gap + input, then gap + model line + gap, and the
    // value starts after the label.
    let input_h = app.rag_view.url_input.height(72);
    let name_y = 1 + 2 + input_h + 1 + 2;
    let value_x = 0 + 4 + 2 + 7; // pad (cx + 2) + label width

    let mouse = |kind: CKind, x: u16, y: u16| CMouse {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };

    // Press on the Name field → anchor the selection at byte 0.
    assert!(
        app.handle_mouse_event(mouse(CKind::Down(CBtn::Left), value_x, name_y))
            .unwrap()
    );
    assert!(app.rag_view.field_selection.is_some());

    // Drag to the end of "hello world" → selection 0..11.
    assert!(
        app.handle_mouse_event(mouse(CKind::Drag(CBtn::Left), value_x + 11, name_y))
            .unwrap()
    );
    assert!(matches!(
        app.rag_view.field_selection,
        Some(sel) if sel.field() == CreateDbFocus::Name
            && sel.range_for(CreateDbFocus::Name, 11) == Some((0, 11))
    ));
    assert_eq!(app.rag_view.selected_field_text(), "hello world");

    // The render paints the selection: each selected cell gets the field
    // text color as background (light bar) — visible over the markdown.
    let theme = app.theme.clone();
    let text_color = {
        let (r, g, b, _) = theme.text.to_ints();
        ratatui::style::Color::Rgb(r, g, b)
    };
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    app.rag_view
        .render(&mut buf, Rect::new(0, 1, 80, 20), &theme);
    for x in value_x..value_x + 11 {
        assert_eq!(
            buf[(x, name_y)].bg,
            text_color,
            "selected cell at ({x},{name_y}) must show the highlight bar"
        );
    }
    // The cell right after the selection is the caret (byte 11), which is
    // drawn with the same light background; two cells past it the field
    // background is untouched.
    assert_ne!(buf[(value_x + 12, name_y)].bg, text_color);

    // Release → auto-copy clears the selection.
    assert!(
        app.handle_mouse_event(mouse(CKind::Up(CBtn::Left), value_x + 11, name_y))
            .unwrap()
    );
    assert!(app.rag_view.field_selection.is_none());
}
