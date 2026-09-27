use super::*;

// ------------------------------------------- `From:` starts prose, not only a quote header (#338)
#[test]
fn en_from_prose_keeps_the_request() {
    for (label, body) in [
        ("the reported case",
         "Hi team,\nFrom: my side the integration works, but please refund the duplicate charge today.\nThanks"),
        ("lowercase", "Hi,\nfrom: my side the integration works, but please refund the charge.\nThanks"),
        ("uppercase", "Hi,\nFROM: my side the integration works, but please refund the charge.\nThanks"),
        ("extra spacing", "Hi,\nFrom:   my side the integration works, but please refund the charge.\nThanks"),
        ("mid-body", "Hello,\nFrom: what I can see the charge was taken twice, please refund it.\nRegards"),
    ] {
        assert!(
            clean(body).contains("refund"),
            "en/`From:` prose keeps the request: {label} — got {:?}",
            clean(body)
        );
    }
}

#[test]
fn en_quoted_block_still_cut() {
    // The positive controls: a real quoted header block still goes, and takes
    // its quoted request with it, so the fix does not simply stop cutting on
    // `From:`.
    for (label, body, want) in [
        ("address then Sent:",
         "Hi,\nPlease look at this.\n\nFrom: Alice <alice@example.com>\nSent: Monday\n\
          To: Bob\nSubject: Refund\n\nPlease refund the duplicate charge to my card.",
         "Hi,\nPlease look at this."),
        ("address only", "Hi,\nSee below.\n\nFrom: alice@example.com\nPlease refund the duplicate charge.",
         "Hi,\nSee below."),
        ("angle address", "Hi,\nSee below.\n\nFrom: <alice@example.com>\nPlease refund the duplicate charge.",
         "Hi,\nSee below."),
        ("name and address",
         "Hi,\nSee below.\n\nFrom: Alice Smith <alice@example.com>\nPlease refund the duplicate charge.",
         "Hi,\nSee below."),
    ] {
        assert_eq!(clean(body), want, "en/quoted block still cut: {label}");
    }
}

#[test]
fn en_from_quote_header_keeps_the_new_request() {
    // ...and the request in the new part is kept even when a quoted block
    // follows it.
    assert_eq!(
        clean(
            "Hi,\nPlease refund the duplicate charge today.\n\n\
             From: Alice <alice@example.com>\nSent: Monday\n\nOld thread: refund the first charge."
        ),
        "Hi,\nPlease refund the duplicate charge today.",
        "en/`From:` quote header keeps the new request"
    );
}

#[test]
fn en_bare_from_header_still_cut() {
    // A bare `From: Name` header, with no address, is the English
    // counterpart of `De: Maria Souza`: the marker above cannot cut it (that
    // is the prose case), so it is recognised by its neighbours instead.
    for (label, body) in [
        ("then Sent:", "Hi,\nSee below.\n\nFrom: Alice Smith\nSent: Monday, 21 Sep 2026\n\nPlease refund."),
        ("then Date:", "Hi,\nSee below.\n\nFrom: Alice Smith\nDate: 21/09/2026\n\nPlease refund."),
        ("one-word name, then Sent:", "Hi,\nSee below.\n\nFrom: Alice\nSent: Monday\n\nPlease refund."),
    ] {
        assert_eq!(
            clean(body),
            "Hi,\nSee below.",
            "en/bare `From:` header still cut: {label}"
        );
    }
}

#[test]
fn en_bare_from_with_no_header_neighbour_is_kept() {
    // A bare `From:` with no header neighbour is not distinguishable from
    // prose, and a dropped request is the worse error, so it is kept.
    for (label, body) in [
        ("From: Name then To:", "Hi,\nSee below.\n\nFrom: Alice Smith\nTo: Bob\n\nPlease refund the charge."),
        ("From: Name alone", "Hi,\nSee below.\n\nFrom: Alice Smith\nPlease refund the charge."),
    ] {
        assert!(
            clean(body).contains("refund"),
            "en/bare `From:` with no header neighbour is kept: {label} — got {:?}",
            clean(body)
        );
    }
}

