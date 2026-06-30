use crate::core::renderable::Renderable;
use crate::core::renderables::select::{SelectOption, SelectRenderable};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn make_options(n: usize) -> Vec<SelectOption> {
    (0..n)
        .map(|i| SelectOption {
            name: format!("Option {}", i),
            description: format!("Description for option {}", i),
        })
        .collect()
}

#[test]
fn test_new_empty() {
    let s = SelectRenderable::new();
    assert!(s.options().is_empty());
    assert_eq!(s.selected_index(), 0);
    assert!(s.selected_option().is_none());
}

#[test]
fn test_set_options() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(5));
    assert_eq!(s.options().len(), 5);
    assert_eq!(s.selected_index(), 0);
}

#[test]
fn test_set_options_empty() {
    let mut s = SelectRenderable::new();
    s.set_options(Vec::new());
    assert!(s.options().is_empty());
}

#[test]
fn test_selected_option() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(3));
    let opt = s.selected_option();
    assert!(opt.is_some());
    assert_eq!(opt.unwrap().name, "Option 0");
}

#[test]
fn test_set_selected_index() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(5));
    s.set_selected_index(2);
    assert_eq!(s.selected_index(), 2);
}

#[test]
fn test_set_selected_index_out_of_bounds() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(3));
    s.set_selected_index(10);
    // Should remain at current index since 10 is out of bounds
    assert_eq!(s.selected_index(), 0);
}

#[test]
fn test_move_down() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(5));
    assert_eq!(s.selected_index(), 0);
    s.move_down(1);
    assert_eq!(s.selected_index(), 1);
}

#[test]
fn test_move_down_past_end_no_wrap() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(3));
    s.set_selected_index(2);
    s.move_down(1);
    // Stays at last since no wrap
    assert_eq!(s.selected_index(), 2);
}

#[test]
fn test_move_up() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(5));
    s.set_selected_index(3);
    s.move_up(1);
    assert_eq!(s.selected_index(), 2);
}

#[test]
fn test_move_up_past_start_no_wrap() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(3));
    s.move_up(1);
    // Stays at first since no wrap
    assert_eq!(s.selected_index(), 0);
}

#[test]
fn test_move_down_fast() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(20));
    s.move_down(5);
    assert_eq!(s.selected_index(), 5);
}

#[test]
fn test_move_up_fast() {
    let mut s = SelectRenderable::new();
    s.set_options(make_options(20));
    s.set_selected_index(10);
    s.move_up(5);
    assert_eq!(s.selected_index(), 5);
}

#[test]
fn test_is_focusable() {
    let s = SelectRenderable::new();
    assert!(s.is_focusable());
}

#[test]
fn test_render_keeps_selected_item_visible_in_small_area() {
    let mut s = SelectRenderable::new();
    s.set_show_description(false);
    s.set_options(make_options(15));
    s.set_selected_index(14);

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    s.render_self(&mut buf, area);

    let rendered: String = (0..area.width)
        .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(rendered.contains("▶ Option 14"));
}
