use super::*;

// --------------------------------------------------------------- unrelated cleaning is unchanged
#[test]
fn quoted_history_is_still_removed() {
    assert_eq!(
        clean("Thanks for the update.\nOn Mon, Sep 20, Bob wrote:\n> original text"),
        "Thanks for the update.",
        "quoted history is still removed"
    );
}

#[test]
fn signature_block_is_still_removed() {
    assert_eq!(
        clean("Hi team,\nCan you confirm the refund?\nRegards,\nAlice"),
        "Hi team,\nCan you confirm the refund?",
        "signature block is still removed"
    );
}

#[test]
fn empty_body_stays_empty() {
    assert_eq!(clean(""), "", "empty body stays empty");
}
