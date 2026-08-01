//! Tests for basic struct functionality (Role)

use crate::harness::context_manager::Role;

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
