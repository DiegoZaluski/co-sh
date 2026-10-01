//! Confidence estimation and temperature handling — the calibrated surface
//! of the decision contract.
//!
//! `answer_confidence` (max of the first k probabilities) is the quantity
//! temperature scaling fits and the one every calibration figure is computed
//! on; `confidence_from_probs` reports a different quantity on a different
//! scale and must not be compared against the same threshold.

use serde_json::Value;

use crate::decision::question::qtype_name;
use crate::pycompat::py_float;

/// A fitted temperature below 1 sharpens the logits instead of softening them.
/// The shipped `choice:11+` bucket is 0.1006, which multiplies them ~10x: a
/// 0.24 top probability is published as 0.99, so a caller gating on confidence
/// is told a coin flip is a certainty. No honest calibration needs to sharpen
/// this hard, so refuse to apply one that does.
pub const TEMP_MIN: f64 = 0.5;
pub const TEMP_MAX: f64 = 5.0;

/// `clamp_temperature`: `t` confined to `[lo, hi]`, falling back to 1.0 if it
/// is not a number.
///
/// Upstream calls Python `float(t)`, so a numeric string such as `"1.5"`
/// parses (a silently-dropped fitted temperature would change calibration),
/// and the bool spellings follow Python's `float(True) == 1.0`. Only values
/// `float()` rejects — `None` and non-numeric strings — fall back to 1.0.
pub fn clamp_temperature(t: &Value, lo: f64, hi: f64) -> f64 {
    let Some(f) = py_float(t) else {
        return 1.0;
    };
    if f.is_nan() || f.is_infinite() {
        return 1.0;
    }
    hi.min(lo.max(f))
}

/// `clamp_temperature(t)` with the shipped bounds.
pub fn clamp_temperature_default(t: &Value) -> f64 {
    clamp_temperature(t, TEMP_MIN, TEMP_MAX)
}

/// `temp_bucket(qtype, k)`: the temperature-by-options key a question of
/// `qtype` with `k` options reads.
pub fn temp_bucket(qtype: u8, k: usize) -> String {
    let size = if k <= 2 {
        "2"
    } else if k <= 5 {
        "3-5"
    } else if k <= 10 {
        "6-10"
    } else {
        "11+"
    };
    format!("{}:{}", qtype_name(qtype), size)
}

/// `answer_confidence`: probability mass on the answer being reported, i.e.
/// `max(p)` over the first `k` entries.
///
/// This is the quantity temperature scaling fits, and the quantity every
/// calibration figure in the upstream repository is computed on. It is the one
/// confidence with the property the README's gating section relies on: of the
/// answers returned at confidence c, about c of them are right.
/// `confidence_from_probs` reports a different quantity on a different scale
/// and carries no such guarantee, so the two must not be compared against the
/// same threshold.
pub fn answer_confidence(p: &[f64], k: usize) -> f64 {
    if k < 1 {
        return 1.0;
    }
    let n = k.min(p.len());
    let max = p[..n].iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    max.clamp(0.0, 1.0)
}

/// `confidence_from_probs`: normalized Shannon entropy confidence,
/// `1 - H(p) / log(k)`.
///
/// How concentrated the whole distribution is. Useful, but not calibrated: it
/// is not what temperature scaling fits and not what the reported ECE
/// measures. See `answer_confidence`.
pub fn confidence_from_probs(p: &[f64], k: usize) -> f64 {
    if k < 2 {
        return 1.0;
    }
    let n = k.min(p.len());
    let entropy: f64 = p[..n]
        .iter()
        .map(|pi| {
            let pi = pi.clamp(1e-12, 1.0);
            -pi * pi.ln()
        })
        .sum();
    (1.0 - entropy / (k as f64).ln()).clamp(0.0, 1.0)
}

/// `ece_score`: Expected Calibration Error across confidence bins.
///
/// `conf` and `correct` must be equally long; returns NaN on an empty input,
/// matching the upstream `float("nan")`.
pub fn ece_score(conf: &[f64], correct: &[f64], bins: usize) -> f64 {
    if conf.is_empty() {
        return f64::NAN;
    }
    let mut e = 0.0;
    for i in 0..bins {
        let lo = i as f64 / bins as f64;
        let hi = (i + 1) as f64 / bins as f64;
        let selected: Vec<usize> = (0..conf.len())
            .filter(|&j| {
                if i == 0 {
                    conf[j] >= lo && conf[j] <= hi
                } else {
                    conf[j] > lo && conf[j] <= hi
                }
            })
            .collect();
        if !selected.is_empty() {
            let mean_conf = selected.iter().map(|&j| conf[j]).sum::<f64>() / selected.len() as f64;
            let mean_correct =
                selected.iter().map(|&j| correct[j]).sum::<f64>() / selected.len() as f64;
            e += (selected.len() as f64 / conf.len() as f64) * (mean_conf - mean_correct).abs();
        }
    }
    e
}
