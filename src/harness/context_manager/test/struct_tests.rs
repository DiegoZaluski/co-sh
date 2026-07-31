//! Tests for basic struct functionality (Context, Role)

use crate::harness::context_manager::{Context, Role};

// Verifies that Context struct fields are accessible and store values correctly.
#[test]
fn context_struct_fields() {
    let ctx = Context {
        hash_id: 42,
        lv: 1,
        user: String::new(),
        assistant: "hello".to_string(),
        tokens: 5,
        exhibition: 1,
    };
    assert_eq!(ctx.hash_id, 42);
    assert_eq!(ctx.lv, 1);
    assert_eq!(ctx.assistant, "hello");
    assert_eq!(ctx.tokens, 5);
}

// Verifies that Role struct works correctly.
#[test]
fn role_struct_works() {
    let r = Role::user("user text");
    assert_eq!(r.text(), "user text");
    assert_eq!(r.label(), "user");
    assert!(r.assistant.is_none());
    assert!(r.user.is_some());

    let r = Role::assistant("assistant text");
    assert_eq!(r.text(), "assistant text");
    assert_eq!(r.label(), "assistant");
    assert!(r.assistant.is_some());
    assert!(r.user.is_none());

    let r = Role {
        assistant: None,
        user: None,
    };
    assert_eq!(r.text(), "");
    assert_eq!(r.label(), "");
}