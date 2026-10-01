use super::*;

// --------------------------------------------------------------- the request survives the footer
#[test]
fn inline_footer_no_blank_line_keeps_the_request() {
    assert_eq!(
        clean(&format!(
            "My account is locked.\n{DISCLAIMER}\nPlease unlock it."
        )),
        "My account is locked. Please unlock it.",
        "inline footer/no blank line keeps the request"
    );
}

#[test]
fn inline_footer_unpunctuated_request_line_is_fully_recovered() {
    assert_eq!(
        clean(&format!(
            "My account is locked\n{DISCLAIMER}\nPlease unlock it."
        )),
        "My account is locked Please unlock it.",
        "inline footer/unpunctuated request line is fully recovered"
    );
}

#[test]
fn inline_footer_fused_request_without_a_trailing_sentence() {
    assert_eq!(
        clean(&format!("Please unlock my account\n{DISCLAIMER}")),
        "Please unlock my account",
        "inline footer/fused request without a trailing sentence"
    );
}

#[test]
fn inline_footer_fused_order_reference_is_fully_recovered() {
    assert_eq!(
        clean(&format!(
            "RMA 5521 is still pending\n{DISCLAIMER}\nPlease advise."
        )),
        "RMA 5521 is still pending Please advise.",
        "inline footer/fused order reference is fully recovered"
    );
}

#[test]
fn inline_footer_capitalised_continuation_stays() {
    // Keep-ward trade-off, documented on purpose: a capitalised continuation
    // of a boilerplate sentence can be a real fragment ("...in error,\nPlease
    // delete it."), so it stays. Leaving one boilerplate line behind is
    // harmless; dropping the request is not.
    assert_eq!(
        clean("If you have received this message in error,\nPlease delete it."),
        "Please delete it.",
        "inline footer/capitalised continuation stays"
    );
}

#[test]
fn inline_footer_lowercase_fusion_still_recovers_the_tail() {
    // Documented boundary: an all-lowercase fused request ("locked\nthis
    // email ...") is indistinguishable from a wrapped boilerplate footer, so
    // the first line is still lost. Only a newline followed by an uppercase
    // letter splits.
    assert_eq!(
        clean(
            "my account is locked\n\
             this email is confidential and intended solely for the named addressee.\n\
             please unlock it."
        ),
        "please unlock it.",
        "inline footer/lowercase fusion still recovers the tail"
    );
}

#[test]
fn inline_footer_body_is_never_emptied() {
    assert_eq!(
        clean(&format!("My account is locked. {DISCLAIMER}")),
        "My account is locked.",
        "inline footer/body is never emptied"
    );
    assert!(
        !clean(&format!("My account is locked. {DISCLAIMER}"))
            .trim()
            .is_empty(),
        "inline footer/is not empty"
    );
    assert_eq!(
        email_state(
            "Locked out",
            &format!("My account is locked. {DISCLAIMER}"),
            None,
            true,
            &[]
        )["body"],
        json!("My account is locked."),
        "email_state/body keeps the request"
    );
}

// --------------------------------------------------------------- a pure footer is still removed
#[test]
fn standalone_footer_paragraph_is_still_dropped() {
    assert_eq!(
        clean(&format!("My account is locked.\n\n{DISCLAIMER}")),
        "My account is locked.",
        "standalone footer paragraph is still dropped"
    );
}

#[test]
fn wrapped_standalone_footer_is_still_dropped() {
    assert_eq!(
        clean(
            "My account is locked.\n\nThis email and any files transmitted with it are\n\
             confidential and intended solely for the named addressee."
        ),
        "My account is locked.",
        "wrapped standalone footer is still dropped"
    );
}

#[test]
fn received_in_error_footer_is_still_dropped() {
    assert_eq!(
        clean(
            "Please reopen ticket 4411.\n\nIf you have received this message in error, delete it."
        ),
        "Please reopen ticket 4411.",
        "received-in-error footer is still dropped"
    );
}
