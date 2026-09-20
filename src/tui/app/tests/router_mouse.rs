use super::super::AppMode;
use super::{App, HOME_LOCK, isolate_home};
use crate::routes::router::FocusTarget;
use crossterm::event::{
    KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
};
use ratatui::layout::Rect;

fn left_click(x: u16, y: u16) -> [CMouse; 2] {
    [
        CMouse {
            kind: CKind::Down(CBtn::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
        CMouse {
            kind: CKind::Up(CBtn::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    ]
}

/// Regression: clicks on the router body must reach the automatic boxes
/// through `App::handle_mouse_event`. The tab dispatch runs
/// `handle_prompt_corrector_mouse` first and returns early on any action;
/// when the auto tab was shown, that handler consumed every body click
/// (its collapsed section rects made the fallback-column check match any
/// x), so the right box never took focus.
#[tokio::test]
async fn auto_tab_body_click_focuses_boxes_through_app_dispatch() {
    let _guard = HOME_LOCK.lock();
    isolate_home();

    let mut app = App::new("/tmp".to_string());
    app.set_test_size(100, 40);
    app.show_router = true;
    assert!(matches!(app.mode(), AppMode::Router));

    // Mirror the dispatch geometry exactly: the router starts ON the header
    // row (its tab buttons share the line with the "← esc" hint) and
    // reserves footer + spacer rows.
    let area = app.terminal_size();
    let main = app.session_main_area(area).main;
    let router_area = Rect::new(main.x, main.y, main.width, main.height.saturating_sub(3));

    // Right half of the screen, one row below the tab bar and its section
    // gap: the fallbacks box. Focus moves on any row of the column, before
    // the per-row hit-test.
    let (right_x, body_y) = (router_area.x + router_area.width / 2 + 2, router_area.y + 3);
    for evt in left_click(right_x, body_y) {
        app.handle_mouse_event(evt).expect("mouse handled");
    }
    assert!(
        matches!(app.router_view.focus, FocusTarget::Fallbacks),
        "a click on the right box must move focus to the fallback chain"
    );

    // Left half: the models box takes focus the same way.
    for evt in left_click(router_area.x + 2, body_y) {
        app.handle_mouse_event(evt).expect("mouse handled");
    }
    assert!(
        matches!(app.router_view.focus, FocusTarget::Models),
        "a click on the left box must move focus to the model list"
    );
}
