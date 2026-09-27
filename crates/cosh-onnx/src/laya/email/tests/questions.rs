use super::*;

// ------------------------------------------- email_questions has exactly one definition
#[test]
fn email_questions_one_definition_behind_both_paths() {
    // `laya.email.email_questions is laya.presets.email_questions`: the Rust
    // re-export is the same function, so both paths answer identically.
    assert_eq!(
        super::email_questions(None),
        email_questions(None),
        "email_questions/one definition behind both module paths"
    );
    assert_eq!(
        crate::laya::email::email_questions(None),
        crate::laya::presets::email_questions(None),
        "email_questions/the package export is that same object"
    );
    assert_eq!(
        super::email_questions(None),
        crate::laya::email::email_questions(None),
        "email_questions/both paths answer the same"
    );
    // A caller override reaches both paths.
    let override_categories: serde_json::Map<String, Value> =
        serde_json::from_value(json!({"legal": "contracts"})).expect("override categories");
    assert_eq!(
        email_questions(Some(override_categories.clone()))["category"]["criteria"],
        crate::laya::email::email_questions(Some(override_categories))["category"]["criteria"],
        "email_questions/a caller override reaches both paths"
    );
}

