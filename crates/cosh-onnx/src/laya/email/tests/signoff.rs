use super::*;

// ------------------------------------------- a sign-off word inside the body is not a sign-off
#[test]
fn signoff_word_short_mail_keeps_the_request() {
    let short = "Hi,\n\nThanks for the quick reply.\nCould you refund invoice 4411 as well?";
    assert_eq!(clean(short), short, "signoff word/short mail keeps the request");
    assert_eq!(
        clean(
            "Hello,\n\nWe were billed twice in March.\nThanks for looking into it.\n\
             The duplicate is 49 EUR on invoice 4411."
        ),
        "Hello,\n\nWe were billed twice in March.\nThanks for looking into it.\n\
         The duplicate is 49 EUR on invoice 4411.",
        "signoff word/thanks mid-body keeps what follows"
    );
    assert_eq!(
        clean(
            "Hi team,\n\nOur account is locked.\nBest practice would be a manual unlock.\n\
             Please unlock account 88213 today."
        ),
        "Hi team,\n\nOur account is locked.\nBest practice would be a manual unlock.\n\
         Please unlock account 88213 today.",
        "signoff word/best mid-body keeps what follows"
    );
    assert_eq!(
        email_state("Duplicate charge", SHORT_BODY, None, true, &[])["body"],
        json!(SHORT_BODY),
        "signoff word/email_state keeps the request"
    );
}

const SHORT_BODY: &str = "Hi,\n\nThanks for the quick reply.\nCould you refund invoice 4411 as well?";

// ------------------------------------------- real sign-offs are still cut (positive controls)
#[test]
fn signoff_cut_positive_controls() {
    let body = "Hi,\n\nPlease refund invoice 4411.";
    for (label, tail) in [
        ("thanks comma", "Thanks,\nAnna"),
        ("thanks bang", "Thanks!"),
        ("best regards", "Best regards,\nAnna"),
        ("kind regards", "Kind regards"),
        ("cheers name", "Cheers, Anna"),
        ("many thanks", "Many thanks,\nAnna Meier"),
        ("thank you", "Thank you,"),
        ("thanks in advance", "Thanks in advance,"),
        ("sincerely", "Sincerely,\nA. Meier"),
        ("sent from phone", "Sent from my iPhone"),
        ("dash delimiter", "--\nAnna Meier\nSupport"),
    ] {
        assert_eq!(
            clean(&format!("{body}\n\n{tail}")),
            body,
            "signoff cut/{label} — got {:?}",
            clean(&format!("{body}\n\n{tail}"))
        );
    }
}

// ------------------------------------------- closings the case rule did not reach (#132 follow-up)
#[test]
fn signoff_cut_wider_closings_and_non_ascii_names() {
    let body = "Hi,\n\nPlease refund invoice 4411.";
    for (label, tail) in [
        ("thanks and regards", "Thanks and regards,\nAnna"),
        ("thanks & regards", "Thanks & Regards,\nAnna"),
        ("warmest regards", "Warmest regards,\nAnna"),
        ("warmest wishes", "Warmest wishes,"),
        ("non-ascii name", "Regards, Łukasz"),
        ("non-ascii name, accented", "Thanks, José"),
        ("cyrillic name", "Regards, Дмитрий"),
    ] {
        assert_eq!(
            clean(&format!("{body}\n\n{tail}")),
            body,
            "signoff cut/{label} — got {:?}",
            clean(&format!("{body}\n\n{tail}"))
        );
    }
}

#[test]
fn signoff_kept_wider_closings_inside_a_sentence() {
    // ...and the wider closing must not swallow a sentence that merely
    // starts the same way
    for (label, body) in [
        ("and + sentence", "Hi,\n\nPlease refund 4411.\nThanks and the team will confirm it today."),
        ("warmest + sentence", "Hi,\n\nThe room is cold.\nWarmest setting still reads 18 degrees."),
    ] {
        assert_eq!(clean(body), body, "signoff kept/{label}");
    }
}

// ------------------------------------------------- the word, without the disclaimer
#[test]
fn word_only_requests_are_kept() {
    // `confidential` was a bare substring of the disclaimer pattern, so any
    // sentence that merely mentioned it was dropped. The Portuguese branches
    // beside it were already tied to disclaimer phrasing; English now is too.
    for (label, body) in [
        ("a question about the word", "Is this confidential?"),
        ("a policy question", "What is your confidentiality policy?"),
        ("a request containing the word", "Please keep this confidential but process my refund."),
        ("a request about handling", "Please treat this as confidential."),
        ("a question with a dash", "This is confidential - can you help?"),
        ("a question about an attachment", "Is the attached document confidential?"),
        ("a label prefix", "Confidential: I need a refund."),
        ("a question about information", "What is the information policy for contractors?"),
    ] {
        assert_eq!(clean(body), body, "word only/kept: {label}");
    }
    assert!(
        !clean("Is this confidential?").trim().is_empty(),
        "word only/body is never emptied"
    );
    assert_eq!(
        email_state("Question", "Is this confidential?", None, true, &[])["body"],
        json!("Is this confidential?"),
        "word only/email_state keeps the request"
    );
}

#[test]
fn word_only_real_footers_still_dropped() {
    // ...while the real footers those branches exist for are still dropped
    for (label, body) in [
        ("named addressee", "This email is confidential and intended solely for the named addressee."),
        ("the individual addressed",
         "This message is confidential and intended solely for the use of the individual to whom it is addressed."),
        ("may be privileged", "The information in this email is confidential and may be privileged."),
        ("wrapped across lines",
         "This email and any files transmitted with it are\n\
          confidential and intended solely for the named addressee."),
    ] {
        assert!(
            clean(body).trim().is_empty(),
            "word only/still dropped: {label} — got {:?}",
            clean(body)
        );
    }
}

#[test]
fn word_only_request_before_a_footer_survives() {
    assert_eq!(
        clean(
            "My account is locked.\n\
             This email is confidential and intended solely for the named addressee.\n\
             Please unlock it."
        ),
        "My account is locked. Please unlock it.",
        "word only/request before a footer survives"
    );
}

#[test]
fn word_only_request_inside_one_sentence_survives() {
    assert_eq!(
        clean(
            "Please unlock it. This email is confidential and intended solely \
             for the named addressee."
        ),
        "Please unlock it.",
        "word only/request inside one sentence survives"
    );
}

