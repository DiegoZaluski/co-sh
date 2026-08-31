//! Cost-tracking tests: the provider-reported REAL cost must always win over
//! the models.dev estimate, and requests with neither must stay unpriced
//! (surfaced as a warning, never guessed).

use super::App;
use cosh_sdk::connector::{Pricing, TokenUsage};

/// $3/M in, $15/M out — Claude-Sonnet-like rates for easy mental math.
fn pricing() -> Pricing {
    Pricing {
        input: 3.0,
        output: 15.0,
        cache_read: 0.0,
        cache_write: 0.0,
    }
}

#[test]
fn reported_cost_is_the_source_of_truth() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    // The estimate for this usage would be $18; the provider reported $0.42 —
    // the reported figure MUST win untouched.
    let (cost, reported) = App::resolve_recorded_cost(Some(0.42), &usage, Some(pricing()));
    assert_eq!((cost, reported), (Some(0.42), Some(0.42)));
}

#[test]
fn estimate_is_used_only_without_reported_cost() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    let (cost, reported) = App::resolve_recorded_cost(None, &usage, Some(pricing()));
    assert_eq!((cost, reported), (Some(3.0), None));
}

#[test]
fn no_reported_cost_and_no_price_stays_unpriced() {
    let usage = TokenUsage::default();
    let (cost, reported) = App::resolve_recorded_cost(None, &usage, None);
    assert_eq!((cost, reported), (None, None));
}

#[test]
fn zero_reported_cost_is_real_and_not_re_priced() {
    // Free-tier models genuinely report $0 — never replaced by an estimate.
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    let (cost, reported) = App::resolve_recorded_cost(Some(0.0), &usage, Some(pricing()));
    assert_eq!((cost, reported), (Some(0.0), Some(0.0)));
}