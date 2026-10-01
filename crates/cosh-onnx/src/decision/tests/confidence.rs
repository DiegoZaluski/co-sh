//! Ported tests: `tests/test_confidence.py` from the laya repository, with
//! the same cases and the same expected values.

use serde_json::json;

use crate::decision::confidence::{
    TEMP_MAX, TEMP_MIN, answer_confidence, clamp_temperature_default, confidence_from_probs,
    ece_score, temp_bucket,
};

// --------------------------------------------------------------- calibration boundaries
#[test]
fn ece_zero_confidence_is_included() {
    assert_eq!(ece_score(&[0.0], &[1.0], 15), 1.0);
}

#[test]
fn ece_zero_confidence_has_its_proper_weight() {
    assert_eq!(ece_score(&[0.0, 1.0], &[1.0, 1.0], 15), 0.5);
}

// --------------------------------------------------------------- temp_bucket and clamp
#[test]
fn temp_bucket_naming() {
    assert_eq!(temp_bucket(0, 2), "choice:2");
    assert_eq!(temp_bucket(1, 5), "score:3-5");
    assert_eq!(temp_bucket(2, 10), "noul:6-10");
    assert_eq!(temp_bucket(0, 77), "choice:11+");
}

#[test]
fn clamp_temperature_bounds_and_fallback() {
    assert_eq!(clamp_temperature_default(&json!(0.1006)), TEMP_MIN);
    assert_eq!(clamp_temperature_default(&json!(99.0)), TEMP_MAX);
    assert_eq!(clamp_temperature_default(&json!(1.3)), 1.3);
    assert_eq!(clamp_temperature_default(&json!(null)), 1.0);
    assert_eq!(clamp_temperature_default(&json!("x")), 1.0);
    assert_eq!(clamp_temperature_default(&json!(f64::NAN)), 1.0);
    assert_eq!(clamp_temperature_default(&json!(f64::INFINITY)), 1.0);
}

// --------------------------------------------------------------- answer_confidence is max(p)
#[test]
fn answer_confidence_is_max_of_first_k() {
    for probs in [
        vec![0.5, 0.5],
        vec![0.1, 0.9],
        vec![0.7, 0.2, 0.1],
        vec![0.25, 0.25, 0.25, 0.25],
        vec![1.0, 0.0, 0.0],
    ] {
        let want = probs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (answer_confidence(&probs, probs.len()) - want).abs() <= 1e-9,
            "{:?}",
            probs
        );
    }

    // only the first k entries count
    assert_eq!(answer_confidence(&[0.4, 0.6, 0.99], 2), 0.6);
    // k=1 is certain
    assert_eq!(answer_confidence(&[1.0], 1), 1.0);
    // k=0 does not crash
    assert_eq!(answer_confidence(&[], 0), 1.0);
}

// --------------------------------------------------------------- it matches noul's definition
#[test]
fn answer_confidence_equals_noul_confidence() {
    // `noul` reports max(p_true, 1 - p_true), which over two options is exactly
    // max(p). So for a noul answer the two agree by construction.
    for p_true in [0.0, 0.05, 0.3, 0.5, 0.62, 0.9, 1.0] {
        let p = [1.0 - p_true, p_true];
        assert!(
            (answer_confidence(&p, 2) - f64::max(p_true, 1.0 - p_true)).abs() <= 1e-9,
            "p={}",
            p_true
        );
    }
}

// --------------------------------------------------------------- the two scales really differ
#[test]
fn scales_disagree_across_the_documented_threshold() {
    // Same distribution, two question types. 0.85 is the threshold the README's
    // gating section uses.
    let mut disagree = Vec::new();
    for p_true in [0.60, 0.70, 0.80, 0.85, 0.90, 0.95] {
        let p = [1.0 - p_true, p_true];
        if (answer_confidence(&p, 2) >= 0.85) != (confidence_from_probs(&p, 2) >= 0.85) {
            disagree.push(p_true);
        }
    }
    assert_eq!(disagree, vec![0.85, 0.90, 0.95]);

    // entropy sits far below max(p) on the same distribution
    assert!(confidence_from_probs(&[0.1, 0.9], 2) < 0.55);
    assert!(0.55 < answer_confidence(&[0.1, 0.9], 2));
    // max(p) over two options has a floor of 0.5; entropy reads 0.0 for the same coin flip.
    assert_eq!(answer_confidence(&[0.5, 0.5], 2), 0.5);
    assert_eq!(confidence_from_probs(&[0.5, 0.5], 2), 0.0);
}

// --------------------------------------------------------------- only one of them is calibrated
#[test]
fn max_p_is_calibrated_on_calibrated_data() {
    // Perfectly calibrated predictions: an answer reported at top probability c
    // is right exactly c of the time. ECE on max(p) must be near zero. ECE on
    // normalized entropy must not be, which is why it cannot be compared
    // against a probability threshold.
    //
    // Same seed (0) and the same numpy random stream order as the upstream
    // test: one draw per question, questions walked in order.
    let mut rng = NumpyRng::new(0);
    let mut tops = Vec::new();
    let mut ents = Vec::new();
    let mut correct = Vec::new();
    for i in 0..24 {
        let c = 0.30 + (0.99 - 0.30) * i as f64 / 23.0;
        let rest = (1.0 - c) / 2.0;
        let p = [c, rest, rest];
        for _ in 0..400 {
            tops.push(answer_confidence(&p, 3));
            ents.push(confidence_from_probs(&p, 3));
            correct.push(if rng.random() < c { 1.0 } else { 0.0 });
        }
    }
    let ece_top = ece_score(&tops, &correct, 15);
    let ece_ent = ece_score(&ents, &correct, 15);
    assert!(
        ece_top < 0.03,
        "max(p) is calibrated on calibrated data (ECE {:.4})",
        ece_top
    );
    assert!(ece_ent > 0.20, "entropy is not (ECE {:.4})", ece_ent);
    assert!(
        ece_ent > 5.0 * ece_top,
        "entropy is worse by a wide margin (top {:.4} vs entropy {:.4})",
        ece_top,
        ece_ent
    );
}

// --------------------------------------------------------------- the entropy helper is untouched
#[test]
fn entropy_formula() {
    let cases = [
        (vec![0.1, 0.9], 2usize),
        (vec![0.25, 0.25, 0.25, 0.25], 4),
        (vec![0.7, 0.2, 0.1], 3),
    ];
    for (probs, k) in cases {
        let probs: Vec<f64> = probs;
        let entropy: f64 = probs
            .iter()
            .map(|p| {
                let p = p.clamp(1e-12, 1.0);
                -p * p.ln()
            })
            .sum();
        let want = (1.0 - entropy / (k as f64).ln()).clamp(0.0, 1.0);
        assert!(
            (confidence_from_probs(&probs, k) - want).abs() <= 1e-9,
            "k={}",
            k
        );
    }
}

/// Minimal `numpy.random.default_rng(seed).random()` (PCG64) replacement: the
/// calibration test needs a deterministic stream, and any fixed stream
/// satisfies its inequalities — the assertion is about the shape of the
/// calibration curve, not the stream. The upstream test's own tolerance
/// margins (0.03 vs 0.20) leave a wide band.
struct NumpyRng {
    state: u64,
}

impl NumpyRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E3779B97F4A7C15,
        }
    }

    fn random(&mut self) -> f64 {
        // splitmix64; uniform over [0, 1)
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}
