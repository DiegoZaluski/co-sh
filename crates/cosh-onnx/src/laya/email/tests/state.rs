use super::*;

// --------------------------------------------------------------- email_state extras
#[test]
fn email_state_strips_the_subject_and_drops_none_extras() {
    let state = email_state(
        "  Locked out  ",
        "body text",
        Some("alice@example.com"),
        false,
        &[("ticket", json!(4411)), ("empty", Value::Null)],
    );
    assert_eq!(state["subject"], json!("Locked out"), "state/subject is stripped");
    assert_eq!(state["body"], json!("body text"), "state/unclean body passes through");
    assert_eq!(state["from"], json!("alice@example.com"), "state/from is set");
    assert_eq!(state["ticket"], json!(4411), "state/extra is inserted");
    assert!(state.get("empty").is_none(), "state/None extra is dropped");
}

#[test]
fn email_state_clean_false_keeps_the_raw_body() {
    let state = email_state(
        "s",
        "Hi,\n\nPlease refund invoice 4411.\n\nThanks,\nAnna",
        None,
        false,
        &[],
    );
    assert_eq!(
        state["body"],
        json!("Hi,\n\nPlease refund invoice 4411.\n\nThanks,\nAnna"),
        "state/clean=False keeps the raw body"
    );
    let cleaned = email_state(
        "s",
        "Hi,\n\nPlease refund invoice 4411.\n\nThanks,\nAnna",
        None,
        true,
        &[],
    );
    assert_eq!(
        cleaned["body"],
        json!("Hi,\n\nPlease refund invoice 4411."),
        "state/clean=True removes the signature"
    );
}
