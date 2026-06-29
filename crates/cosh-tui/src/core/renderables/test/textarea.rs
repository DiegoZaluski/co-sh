use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::core::renderable::Renderable;
use crate::core::renderables::textarea::{TextareaOptions, TextareaRenderable};

#[test]
fn test_new_creates_empty() {
    let ta = TextareaRenderable::new(TextareaOptions::default());
    assert_eq!(ta.value(), "");
    assert!(ta.is_focusable());
    assert!(!ta.is_focused());
}

#[test]
fn test_new_with_initial_value() {
    let opts = TextareaOptions {
        initial_value: Some("hello\nworld".into()),
        ..Default::default()
    };
    let ta = TextareaRenderable::new(opts);
    assert_eq!(ta.value(), "hello\nworld");
}

#[test]
fn test_focus_blur() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    assert!(!ta.is_focused());
    ta.focus();
    assert!(ta.is_focused());
    ta.blur();
    assert!(!ta.is_focused());
}

#[test]
fn test_set_value() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("abc\ndef");
    assert_eq!(ta.value(), "abc\ndef");
}

#[test]
fn test_insert_text() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.insert_text("hello");
    assert_eq!(ta.value(), "hello");
}

#[test]
fn test_insert_text_with_newline() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.insert_text("a\nb");
    assert_eq!(ta.value(), "a\nb");
}

#[test]
fn test_delete_char_backward() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("abc");
    ta.move_cursor_right();
    ta.move_cursor_right();
    ta.move_cursor_right();
    assert!(ta.delete_char_backward());
    assert_eq!(ta.value(), "ab");
}

#[test]
fn test_delete_char_backward_at_start() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("a");
    assert!(!ta.delete_char_backward());
}

#[test]
fn test_delete_char() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("abc");
    assert!(ta.delete_char());
    assert_eq!(ta.value(), "bc");
}

#[test]
fn test_cursor_movement() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("a\nb\nc");
    ta.move_cursor_down();
    assert!(ta.value().contains('b'));
    ta.move_cursor_down();
    ta.move_cursor_up();
}

#[test]
fn test_undo_redo() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.insert_text("hello");
    assert_eq!(ta.value(), "hello");
    ta.undo();
    assert_eq!(ta.value(), "");
    ta.redo();
    assert_eq!(ta.value(), "hello");
}

#[test]
fn test_select_all() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("hello\nworld");
    ta.select_all();
    ta.delete_char();
    assert_eq!(ta.value(), "");
}

#[test]
fn test_goto_buffer_home_end() {
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_value("line1\nline2\nline3");
    ta.move_cursor_down();
    ta.move_cursor_down();
    ta.goto_buffer_home();
    ta.delete_char();
    assert_eq!(ta.value(), "ine1\nline2\nline3");
    ta.goto_buffer_end();
    ta.insert_text("!");
    assert_eq!(ta.value(), "ine1\nline2\nline3!");
}

#[test]
fn test_submit_called() {
    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();
    let mut ta = TextareaRenderable::new(TextareaOptions::default());
    ta.set_on_submit(Some(Box::new(move || {
        called_clone.store(true, Ordering::Relaxed);
    })));
    ta.submit();
    assert!(called.load(Ordering::Relaxed));
}

#[test]
fn test_placeholder() {
    let mut ta = TextareaRenderable::new(TextareaOptions {
        placeholder: Some("type here...".into()),
        ..Default::default()
    });
    assert_eq!(ta.placeholder(), Some("type here..."));
    ta.set_placeholder(Some("new".into()));
    assert_eq!(ta.placeholder(), Some("new"));
}
