use crate::core::renderable::Renderable;
use crate::core::renderables::tab_select::{TabSelectOption, TabSelectRenderable};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn make_options(n: usize) -> Vec<TabSelectOption> {
    (0..n)
        .map(|i| TabSelectOption {
            name: format!("Tab {}", i),
            description: format!("Description for tab {}", i),
        })
        .collect()
}

#[test]
fn test_new_empty() {
    let t = TabSelectRenderable::new();
    assert!(t.options().is_empty());
    assert_eq!(t.selected_index(), 0);
}

#[test]
fn test_set_options() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(5));
    assert_eq!(t.options().len(), 5);
    assert_eq!(t.selected_index(), 0);
}

#[test]
fn test_selected_option() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(3));
    let opt = t.selected_option();
    assert!(opt.is_some());
    assert_eq!(opt.unwrap().name, "Tab 0");
}

#[test]
fn test_set_selected_index() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(5));
    t.set_selected_index(2);
    assert_eq!(t.selected_index(), 2);
}

#[test]
fn test_move_left() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(5));
    t.set_selected_index(3);
    t.move_left();
    assert_eq!(t.selected_index(), 2);
}

#[test]
fn test_move_left_at_start() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(3));
    t.move_left();
    assert_eq!(t.selected_index(), 0);
}

#[test]
fn test_move_right() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(5));
    t.move_right();
    assert_eq!(t.selected_index(), 1);
}

#[test]
fn test_move_right_at_end() {
    let mut t = TabSelectRenderable::new();
    t.set_options(make_options(3));
    t.set_selected_index(2);
    t.move_right();
    assert_eq!(t.selected_index(), 2);
}

#[test]
fn test_set_tab_width() {
    let mut t = TabSelectRenderable::new();
    assert_eq!(t.tab_width(), 20);
    t.set_tab_width(30);
    assert_eq!(t.tab_width(), 30);
}

#[test]
fn test_is_focusable() {
    let t = TabSelectRenderable::new();
    assert!(t.is_focusable());
}

#[test]
fn test_render_keeps_selected_tab_visible_in_small_area() {
    let mut t = TabSelectRenderable::new();
    t.set_show_description(false);
    t.set_show_underline(false);
    t.set_options(make_options(15));
    t.set_selected_index(14);

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    t.render_self(&mut buf, area);

    let rendered: String = (0..area.width)
        .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(rendered.contains("Tab 14"));
}
