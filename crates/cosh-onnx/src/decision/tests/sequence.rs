//! Ported tests: the sequence-construction sections of `tests/test_criteria.py`,
//! `tests/test_truncation_direction.py` and `tests/test_training.py` from the
//! laya repository, with the same cases and the same expected values.

use serde_json::{Value, json};

use crate::decision::sequence::{BuildOptions, build_sequence};
use crate::runtime::tokenizer::Tokenizer;

// --------------------------------------------------------------- build_sequence left truncation
// From tests/test_criteria.py: with no room left for the state, `st[-0:]`
// kept all of it — the closing [SEP] was replaced by the *first* state token,
// i.e. the wrong end of the state and an unterminated sequence.

/// The `_SeqTok` tokenizer from the upstream test: ids are assigned per new
/// word, 100 + insertion order.
struct SeqTok {
    vocab: std::sync::Mutex<std::collections::HashMap<String, u32>>,
}

impl SeqTok {
    fn new() -> Self {
        Self {
            vocab: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Tokenizer for SeqTok {
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32> {
        let mut vocab = self.vocab.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids = Vec::new();
        for w in text.split_whitespace() {
            let next = 100 + vocab.len() as u32;
            ids.push(*vocab.entry(w.to_string()).or_insert(next));
        }
        if truncation && let Some(max_length) = max_length {
            ids.truncate(max_length);
        }
        ids
    }

    fn mask_token(&self) -> &str {
        "[MASK]"
    }
    fn mask_token_id(&self) -> u32 {
        1
    }
    fn cls_token_id(&self) -> u32 {
        2
    }
    fn sep_token_id(&self) -> u32 {
        3
    }
    fn pad_token_id(&self) -> u32 {
        0
    }
}

#[test]
fn truncate_left_keeps_the_tail() {
    let tok = SeqTok::new();
    let q = json!({"t": "noul", "ins": "Is it urgent?", "crit": null});
    // prompt + closing [SEP], no state
    let full = build_sequence(
        &tok,
        &json!(""),
        &q,
        &BuildOptions {
            max_len: 1_000_000,
            ..default_opts()
        },
    )
    .unwrap()
    .0
    .len();
    for (room, kept) in [
        (0usize, Vec::new()),
        (2, vec!["two", "three"]),
        (10, vec!["one", "two", "three"]),
    ] {
        let opts = BuildOptions {
            max_len: full + room,
            truncate_left: true,
            ..default_opts()
        };
        let ids = build_sequence(&tok, &json!("one two three"), &q, &opts)
            .unwrap()
            .0;
        let mut want: Vec<u32> = kept
            .iter()
            .map(|w| *tok.vocab.lock().unwrap().get(*w).unwrap())
            .collect();
        want.push(tok.sep_token_id());
        assert_eq!(&ids[full - 1..], want.as_slice(), "room={}", room);
    }
}

fn default_opts() -> BuildOptions<'static> {
    BuildOptions {
        max_len: 512,
        head_max_len: 192,
        option_order: None,
        truncate_left: false,
        state_ids: None,
    }
}

// --------------------------------------------------------------- truncation direction
// From tests/test_truncation_direction.py, the checks that run against
// build_sequence directly (the Agent-path halves need the inference runtime
// and arrive with Phase 3).

/// The `_FakeTok` from the upstream test: ids keyed by word length.
struct FakeTok;

impl Tokenizer for FakeTok {
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32> {
        // [10 + (len(w) % 90) for w in text.split() if w]
        let mut ids: Vec<u32> = text
            .split_whitespace()
            .map(|w| 10 + (w.chars().count() % 90) as u32)
            .collect();
        if truncation && let Some(max_length) = max_length {
            ids.truncate(max_length);
        }
        ids
    }

    fn mask_token(&self) -> &str {
        "[MASK]"
    }
    fn mask_token_id(&self) -> u32 {
        4
    }
    fn cls_token_id(&self) -> u32 {
        0
    }
    fn sep_token_id(&self) -> u32 {
        1
    }
    fn pad_token_id(&self) -> u32 {
        2
    }
}

fn truncation_question() -> serde_json::Value {
    json!({
        "t": "choice", "ins": "What action?",
        "crit": {"refund": "money back", "escalate": "manager", "hold": "wait"}
    })
}

#[test]
fn string_state_preserves_head() {
    // A string state must still preserve the head (backward compatible).
    let tok = FakeTok;
    let state = format!("HEADMARKERWORD {} TAILMARKER", "filler ".repeat(20));
    let state_ids = tok.encode(&state.replace("[MASK]", " "), false, None);
    let head_id = state_ids[0];
    let tail_id = state_ids[state_ids.len() - 1];
    assert!(head_id != tail_id, "head and tail must have different ids");

    let opts = BuildOptions {
        max_len: 30,
        head_max_len: 12,
        ..default_opts()
    };
    let (seq, _) = build_sequence(&tok, &json!(state), &truncation_question(), &opts).unwrap();
    // prefix = the prompt length (reference build with an empty state, minus
    // its closing [SEP])
    let reference = build_sequence(
        &tok,
        &json!(""),
        &truncation_question(),
        &BuildOptions {
            max_len: 30,
            head_max_len: 12,
            ..default_opts()
        },
    )
    .unwrap()
    .0
    .len();
    let prefix = reference - 1;
    let kept = &seq[prefix..seq.len() - 1];

    assert!(
        kept.contains(&head_id),
        "string state must preserve the head (default mode)"
    );
    assert!(
        !kept.contains(&tail_id),
        "string state must drop the tail (default mode)"
    );
}

#[test]
fn build_sequence_default_unchanged() {
    // build_sequence's default behavior is unchanged for non-list callers.
    let tok = FakeTok;
    let state = format!("OLDFRONT {} NEWBACK", "filler ".repeat(20));
    let default_build = BuildOptions {
        max_len: 30,
        head_max_len: 12,
        ..default_opts()
    };
    let explicit_opts = BuildOptions {
        max_len: 30,
        head_max_len: 12,
        truncate_left: false,
        ..default_opts()
    };
    let (seq_default, _) =
        build_sequence(&tok, &json!(state), &truncation_question(), &default_build).unwrap();
    let (seq_explicit, _) =
        build_sequence(&tok, &json!(state), &truncation_question(), &explicit_opts).unwrap();
    assert_eq!(
        seq_default, seq_explicit,
        "default must remain truncate_left=false"
    );
}

// --------------------------------------------------------------- build_sequence basics
// From tests/test_training.py, the build_sequence section. The `_Tok` stand-in
// is one id per character (`ids = [10 + (ord(c) % 50) for c in text]`), distinct
// from the word-based `_FakeTok` of test_truncation_direction.py above.
/// The `_Tok` from the upstream test: one id per character.
struct CharTok;

impl Tokenizer for CharTok {
    fn encode(&self, text: &str, truncation: bool, max_length: Option<usize>) -> Vec<u32> {
        // [10 + (ord(c) % 50) for c in text]
        let mut ids: Vec<u32> = text.chars().map(|c| 10 + (c as u32 % 50)).collect();
        if truncation && let Some(max_length) = max_length {
            ids.truncate(max_length);
        }
        ids
    }

    fn mask_token(&self) -> &str {
        "[M]"
    }
    fn mask_token_id(&self) -> u32 {
        3
    }
    fn cls_token_id(&self) -> u32 {
        1
    }
    fn sep_token_id(&self) -> u32 {
        2
    }
    fn pad_token_id(&self) -> u32 {
        0
    }
}

fn training_question() -> Value {
    json!({
        "t": "choice", "ins": "Which team?",
        "crit": {"a": "first", "b": "second", "c": "third"}
    })
}

#[test]
fn build_sequence_shape_checks() {
    let tok = CharTok;
    let (seq, markers) = build_sequence(
        &tok,
        &json!("some state text"),
        &training_question(),
        &BuildOptions {
            max_len: 128,
            head_max_len: 64,
            ..default_opts()
        },
    )
    .unwrap();
    assert_eq!(markers.len(), 3, "build_sequence/one marker per option");
    assert!(
        markers.iter().all(|m| *m < seq.len()),
        "build_sequence/markers point inside the sequence"
    );
    assert_eq!(seq[0], tok.cls_token_id(), "build_sequence/starts with CLS");
    assert_eq!(
        seq[seq.len() - 1],
        tok.sep_token_id(),
        "build_sequence/ends with SEP"
    );
    assert!(seq.len() <= 128, "build_sequence/respects max_len");
}

#[test]
fn build_sequence_truncates_long_state_to_max_len() {
    let tok = CharTok;
    let (long_seq, _) = build_sequence(
        &tok,
        &json!("x".repeat(5000)),
        &training_question(),
        &BuildOptions {
            max_len: 64,
            head_max_len: 32,
            ..default_opts()
        },
    )
    .unwrap();
    assert!(
        long_seq.len() <= 64,
        "build_sequence/truncates long state to max_len (len={})",
        long_seq.len()
    );
}

#[test]
fn build_sequence_noul_always_offers_two_options() {
    let tok = CharTok;
    let (noul_seq, noul_markers) = build_sequence(
        &tok,
        &json!("state"),
        &json!({"t": "noul", "ins": "Is it?", "crit": null}),
        &BuildOptions {
            max_len: 128,
            head_max_len: 64,
            ..default_opts()
        },
    )
    .unwrap();
    assert_eq!(
        noul_markers.len(),
        2,
        "build_sequence/noul always offers two options"
    );
    assert!(noul_seq.len() <= 128);
}

#[test]
fn build_sequence_score_has_one_marker_per_level() {
    let tok = CharTok;
    let (score_seq, score_markers) = build_sequence(
        &tok,
        &json!("state"),
        &json!({"t": "score", "ins": "How bad?", "crit": ["low", "mid", "high"]}),
        &BuildOptions {
            max_len: 128,
            head_max_len: 64,
            ..default_opts()
        },
    )
    .unwrap();
    assert_eq!(
        score_markers.len(),
        3,
        "build_sequence/score has one marker per level"
    );
    assert!(score_seq.len() <= 128);
}
