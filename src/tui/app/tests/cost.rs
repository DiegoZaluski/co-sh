//! Cost-tracking tests: the provider-reported REAL cost is the ONLY source
//! of truth. Requests whose provider does not report a cost stay unpriced —
//! no models.dev estimate is applied anymore (providers without cost
//! reporting simply show no price; see `cosh_sdk::connector::supports_cost_reporting`).

use super::App;

#[test]
fn reported_cost_is_the_source_of_truth() {
    // The provider reported $0.42 — recorded verbatim, never re-priced.
    let cost = App::resolve_recorded_cost(Some(0.42));
    assert_eq!(cost, Some(0.42));
}

#[test]
fn no_reported_cost_stays_unpriced() {
    // No estimate fallback: the record stays unpriced and is excluded
    // from dollar totals instead of showing a guessed figure.
    let cost = App::resolve_recorded_cost(None);
    assert_eq!(cost, None);
}

#[test]
fn zero_reported_cost_is_real_and_not_re_priced() {
    // Free-tier models genuinely report $0 — kept as a real (zero) cost.
    let cost = App::resolve_recorded_cost(Some(0.0));
    assert_eq!(cost, Some(0.0));
}
