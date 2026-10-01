//! Ported tests: the rendering and validation parts of `tests/test_criteria.py`
//! and `tests/test_criteria_normalization.py` from the laya repository, with
//! the same cases and the same expected values.

use serde_json::{Value, json};

use crate::decision::question::{
    check_question, render_criterion, render_options, resolve_noul_labels, to_internal,
};
use crate::pycompat::py_str;

// --------------------------------------------------------------- render_criterion
#[test]
fn criterion_str_passes_through() {
    assert_eq!(
        render_criterion(&json!("phishing or scam")),
        "phishing or scam"
    );
}

#[test]
fn criterion_dict_to_json() {
    assert_eq!(
        render_criterion(&json!({"desc": "phishing"})),
        r#"{"desc": "phishing"}"#
    );
}

#[test]
fn criterion_list_to_json() {
    assert_eq!(render_criterion(&json!(["a", "b"])), r#"["a", "b"]"#);
}

#[test]
fn criterion_int_to_json() {
    assert_eq!(render_criterion(&json!(3)), "3");
}

#[test]
fn criterion_bool_to_json() {
    assert_eq!(render_criterion(&json!(false)), "false");
}

#[test]
fn criterion_non_ascii_kept() {
    assert_eq!(
        render_criterion(&json!({"d": "münchen"})),
        r#"{"d": "münchen"}"#
    );
}

// --------------------------------------------------------------- the reported crash
#[test]
fn noul_dict_criteria_does_not_crash() {
    let q = json!({
        "t": "noul", "ins": "Is this phishing?",
        "crit": {"true": {"desc": "phishing, scam or fraud"}, "false": {"desc": "legitimate"}}
    });
    let out = render_options(&q).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], r#"false: {"desc": "legitimate"}"#);
    assert_eq!(out[1], r#"true: {"desc": "phishing, scam or fraud"}"#);
    // no python repr leaked
    assert!(!out.concat().contains('\''), "{:?}", out);
}

// --------------------------------------------------------------- choice and score
#[test]
fn choice_criteria_rendering() {
    let out = render_options(&json!({
        "t": "choice", "ins": "x",
        "crit": {"billing": {"desc": "payments"}, "tech": null, "sales": ""}
    }))
    .unwrap();
    assert_eq!(out[0], r#"billing: {"desc": "payments"}"#);
    assert_eq!(out[1], "tech");
    assert_eq!(out[2], "sales");
    assert!(!out.concat().contains("{'"), "{:?}", out);
}

#[test]
fn choice_zero_and_false_are_kept() {
    // 0 and False are real criterion values, not "missing"
    let out = render_options(&json!({"t": "choice", "ins": "x", "crit": {"zero": 0, "no": false}}))
        .unwrap();
    assert_eq!(out[0], "zero: 0");
    assert_eq!(out[1], "no: false");
}

#[test]
fn score_criteria_rendering() {
    let out = render_options(&json!({"t": "score", "ins": "x", "crit": [{"d": "low"}, "high", 2]}))
        .unwrap();
    assert_eq!(out[0], r#"level 0: {"d": "low"}"#);
    assert_eq!(out[1], "level 1: high");
    assert_eq!(out[2], "level 2: 2");
}

// --------------------------------------------------------------- noul labels
#[test]
fn noul_default_texts() {
    let q = json!({"t": "noul", "ins": "x", "crit": null});
    let out = render_options(&q).unwrap();
    assert_eq!(out[0], "false: no, the statement does not hold");
    assert_eq!(out[1], "true: yes, the statement holds");
}

#[test]
fn noul_explicit_null_labels_use_defaults() {
    let out =
        render_options(&json!({"t": "noul", "ins": "x", "crit": null, "labels": null})).unwrap();
    assert_eq!(
        out,
        vec![
            "false: no, the statement does not hold",
            "true: yes, the statement holds"
        ]
    );
}

#[test]
fn noul_string_criteria_still_work() {
    let out = render_options(&json!({
        "t": "noul", "ins": "x", "crit": {"true": "yes it is", "false": "no"}
    }))
    .unwrap();
    assert_eq!(out, vec!["false: no", "true: yes it is"]);
}

#[test]
fn noul_custom_labels_preserve_false_then_true_semantics() {
    let custom_question = json!({
        "t": "noul", "ins": "x", "crit": null,
        "labels": {"true": " A ", "false": " B "}
    });
    let out = render_options(&custom_question).unwrap();
    assert_eq!(
        out,
        vec![
            "B: no, the statement does not hold",
            "A: yes, the statement holds"
        ]
    );
    // custom label input is not mutated (a JSON clone cannot be; the assertion
    // lives on the caller side in Python)
    assert_eq!(
        custom_question["labels"],
        json!({"true": " A ", "false": " B "})
    );
}

#[test]
fn noul_invalid_labels() {
    let label_error =
        "noul labels must map exactly 'false' and 'true' to distinct non-empty strings";
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("not a dict", json!(["negative", "positive"])),
        ("missing true", json!({"false": "negative"})),
        (
            "extra key",
            json!({"false": "negative", "true": "positive", "other": "x"}),
        ),
        ("blank value", json!({"false": " ", "true": "positive"})),
        ("duplicate values", json!({"false": "same", "true": "same"})),
        ("non-string value", json!({"false": 0, "true": "positive"})),
    ];
    for (name, labels) in cases {
        let q = json!({"t": "noul", "ins": "x", "labels": labels});
        let err = render_options(&q).unwrap_err().to_string();
        assert_eq!(err, label_error, "case: {}", name);
    }
    // the resolver agrees with the renderer
    assert_eq!(
        resolve_noul_labels(Some(&json!(["negative", "positive"])))
            .unwrap_err()
            .to_string(),
        label_error
    );
}

#[test]
fn choice_and_score_reject_labels() {
    for crit in [json!({"a": null, "b": null}), json!(["low", "high"])] {
        let q = json!({
            "t": if crit.is_object() { "choice" } else { "score" },
            "ins": "x", "crit": crit,
            "labels": {"false": "B", "true": "A"}
        });
        let err = render_options(&q).unwrap_err().to_string();
        assert_eq!(err, "labels is only supported for noul questions");
    }
}

// --------------------------------------------------------------- unchanged choice and score behaviour
#[test]
fn choice_string_criteria_still_work() {
    let out =
        render_options(&json!({"t": "choice", "ins": "x", "crit": {"a": "first", "b": null}}))
            .unwrap();
    assert_eq!(out, vec!["a: first", "b"]);
}

#[test]
fn choice_boolean_word_labels_are_not_rewritten() {
    let out = render_options(&json!({
        "t": "choice", "ins": "x", "crit": {"true": "yes", "false": "no"}
    }))
    .unwrap();
    assert_eq!(out, vec!["true: yes", "false: no"]);
}

#[test]
fn score_string_criteria_still_work() {
    let out = render_options(&json!({"t": "score", "ins": "x", "crit": ["low", "high"]})).unwrap();
    assert_eq!(out, vec!["level 0: low", "level 1: high"]);
}

/// Every rendered option must be a string, whatever went in.
#[test]
fn all_options_are_str() {
    let cases = [
        (
            "choice/objects",
            json!({"t": "choice", "ins": "x", "crit": {"a": {"n": 1}, "b": [1, 2], "c": 3.5}}),
        ),
        (
            "score/objects",
            json!({"t": "score", "ins": "x", "crit": [{"a": 1}, [2], null]}),
        ),
        (
            "noul/objects",
            json!({"t": "noul", "ins": "x", "crit": {"true": [1], "false": {"z": 0}}}),
        ),
        (
            "choice/int labels, no description",
            json!({"t": "choice", "ins": "x", "crit": {"1": null, "2": null, "3": null}}),
        ),
        (
            "choice/float labels, no description",
            json!({"t": "choice", "ins": "x", "crit": {"1.5": null}}),
        ),
        (
            "choice/bool labels, no description",
            json!({"t": "choice", "ins": "x", "crit": {"true": null, "false": null}}),
        ),
        (
            "choice/int labels with descriptions",
            json!({"t": "choice", "ins": "x", "crit": {"1": "one", "2": "two"}}),
        ),
    ];
    for (label, q) in cases {
        let out = render_options(&q).unwrap();
        assert!(out.iter().all(|o| !o.is_empty()), "{} -> {:?}", label, out);
    }
}

// the rendered text of a non-string label is its string form, not its repr
#[test]
fn int_label_renders_as_its_str() {
    assert_eq!(
        render_options(&json!({"t": "choice", "ins": "x", "crit": {"1": null}})).unwrap(),
        vec!["1"]
    );
    // Upstream also checks `crit: {None: None}` -> ["None"] here. A JSON
    // object cannot carry a null key, so that exact case cannot exist on the
    // shared surface; its spelling is covered directly through `py_str`.
    assert_eq!(py_str(&serde_json::Value::Null), "None");
}

#[test]
fn int_label_keeps_its_description_form() {
    assert_eq!(
        render_options(&json!({"t": "choice", "ins": "x", "crit": {"1": "one"}})).unwrap(),
        vec!["1: one"]
    );
}

// the JSON we emit is parseable back
#[test]
fn emitted_json_round_trips() {
    let rendered = render_options(&json!({
        "t": "noul", "ins": "x", "crit": {"true": {"a": 1}, "false": {"b": 2}}
    }))
    .unwrap();
    let payload = rendered[1].split_once("true: ").unwrap().1;
    let parsed: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(parsed, json!({"a": 1}));
}

#[test]
fn noul_criteria_boolean_keys() {
    // Over the JSON surface Python's `True`/`False` keys are the strings
    // "true"/"false"; the normalisation under test is the `.lower()` rule.
    let qdef = json!({
        "type": "noul",
        "instructions": "Is this a refund?",
        "criteria": {"true": "User requests money back",
                     "false": "User does not ask for money back"}
    });
    let internal = to_internal(&qdef).unwrap();
    assert!(internal["crit"].as_object().unwrap().contains_key("true"));
    assert!(internal["crit"].as_object().unwrap().contains_key("false"));
    let opts = render_options(&Value::Object(internal)).unwrap();
    assert_eq!(opts.len(), 2);
    assert!(
        opts[0].starts_with("false: User does not ask for money back"),
        "{}",
        opts[0]
    );
    assert!(
        opts[1].starts_with("true: User requests money back"),
        "{}",
        opts[1]
    );
}

#[test]
fn choice_criteria_list_expansion() {
    let qdef = json!({
        "type": "choice",
        "instructions": "Select urgency",
        "criteria": ["low", "medium", "high"]
    });
    let internal = to_internal(&qdef).unwrap();
    let keys: Vec<&str> = internal["crit"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["low", "medium", "high"]);
}

// --------------------------------------------------------------- #182: malformed shapes
/// The shapes from `test_criteria.py` that `_check_question` must reject,
/// naming the question id. The inference-path half of those cases runs the
/// whole forward pass, which arrives with the ONNX runtime in Phase 3; the
/// validation itself is shared and exercised here.
#[test]
fn rejected_malformed_question_shapes() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "choice without criteria",
            json!({"type": "choice", "instructions": "Which team?"}),
        ),
        (
            "choice with criteria None",
            json!({"type": "choice", "instructions": "Which team?", "criteria": null}),
        ),
        (
            "choice with empty criteria",
            json!({"type": "choice", "instructions": "Which team?", "criteria": {}}),
        ),
        // ("choice with a tuple of labels") — upstream passes a Python tuple,
        // which `isinstance(crit, (dict, list))` rejects; a tuple serialises
        // as a JSON array, which is a valid choice criteria list, so that
        // exact case cannot exist on the shared surface.
        (
            "score without criteria",
            json!({"type": "score", "instructions": "How urgent?"}),
        ),
        (
            "score with an empty list",
            json!({"type": "score", "instructions": "How urgent?", "criteria": []}),
        ),
        (
            "score with a dict of levels",
            json!({"type": "score", "instructions": "How urgent?", "criteria": {"low": "no pressure", "high": "blocking"}}),
        ),
        (
            "score with a null level",
            json!({"type": "score", "instructions": "How urgent?", "criteria": ["low", null, "high"]}),
        ),
        (
            "choice with labels",
            json!({"type": "choice", "instructions": "Which team?", "criteria": ["billing", "tech"], "labels": {"false": "B", "true": "A"}}),
        ),
        (
            "score with labels",
            json!({"type": "score", "instructions": "How urgent?", "criteria": ["low", "high"], "labels": {"false": "B", "true": "A"}}),
        ),
        (
            "noul with list criteria",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": ["a", "b"]}),
        ),
        (
            "noul with string criteria",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": "spam?"}),
        ),
        (
            "noul with incomplete labels",
            json!({"type": "noul", "instructions": "Is it spam?", "labels": {"true": "A"}}),
        ),
        (
            "noul with duplicate labels",
            json!({"type": "noul", "instructions": "Is it spam?", "labels": {"false": "A", "true": "A"}}),
        ),
        (
            "noul with yes/no criteria",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": {"yes": "it is spam", "no": "it is not"}}),
        ),
        (
            "noul with neutral keys",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": {"spam": "it is spam", "ham": "it is not"}}),
        ),
        (
            "noul with alpha/beta criteria",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": {"alpha": "yes", "beta": "no"}}),
        ),
        (
            "noul with a typo'd key",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": {"ture": "yes", "false": "no"}}),
        ),
        (
            "noul with an extra key",
            json!({"type": "noul", "instructions": "Is it spam?", "criteria": {"true": "y", "false": "n", "maybe": "?"}}),
        ),
        (
            "unknown type",
            json!({"type": "bool", "instructions": "Is it spam?"}),
        ),
        ("missing type", json!({"instructions": "Is it spam?"})),
        ("no instructions", json!({"type": "noul"})),
    ];
    for (label, qdef) in cases {
        let err = check_question("q", &qdef).unwrap_err();
        // the message must name the question: a caller with twenty of them
        // needs to know which one
        assert!(err.to_string().contains("'q'"), "{} -> {}", label, err);
        // ...and say what to fix
        assert!(err.to_string().len() > 40, "{} -> {}", label, err);
    }
    // a null score level names the level
    let err = check_question(
        "q",
        &json!({"type": "score", "instructions": "How urgent?", "criteria": ["low", null]}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("level 1"), "{}", err);
    // every question id is validated, not only the first one put in the dict
    let err = check_question(
        "broken",
        &json!({"type": "choice", "instructions": "Which team?"}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("'broken'"), "{}", err);
}

// --------------------------------------------------------------- #156: noul criteria keys
const _DEFAULT_FALSE_TEXT: &str = "false: no, the statement does not hold";
const _DEFAULT_TRUE_TEXT: &str = "true: yes, the statement holds";

#[test]
fn noul_criteria_key_rules() {
    let cases: Vec<(&str, Option<Value>, Vec<&str>)> = vec![
        (
            "both keys given",
            Some(json!({"true": "yes", "false": "no"})),
            vec!["false: no", "true: yes"],
        ),
        (
            "only true given",
            Some(json!({"true": "the review is positive"})),
            vec![_DEFAULT_FALSE_TEXT, "true: the review is positive"],
        ),
        (
            "only false given",
            Some(json!({"false": "the review is negative"})),
            vec!["false: the review is negative", _DEFAULT_TRUE_TEXT],
        ),
        (
            "an empty dict falls back to both defaults",
            Some(json!({})),
            vec![_DEFAULT_FALSE_TEXT, _DEFAULT_TRUE_TEXT],
        ),
        (
            "omitting criteria falls back to both defaults",
            None,
            vec![_DEFAULT_FALSE_TEXT, _DEFAULT_TRUE_TEXT],
        ),
        (
            "a description equal to the default wording still counts as given",
            Some(
                json!({"true": "yes, the statement holds", "false": "no, the statement does not hold"}),
            ),
            vec![_DEFAULT_FALSE_TEXT, _DEFAULT_TRUE_TEXT],
        ),
    ];
    for (label, crit, want) in cases {
        let mut qdef = json!({"type": "noul", "instructions": "Is the review positive?"});
        if let Some(crit) = crit {
            qdef["criteria"] = crit;
        }
        let internal = to_internal(&qdef).unwrap();
        let out = render_options(&Value::Object(internal)).unwrap();
        let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        assert_eq!(out, want, "case: {}", label);
    }
}

#[test]
fn labels_change_only_the_prefix() {
    // `labels` is the supported way to word the answer without touching the
    // option text; the criteria text after the prefixes is unchanged, and the
    // result stays P(true).
    let mixed = to_internal(&json!({
        "type": "noul", "instructions": "Is the review positive?",
        "criteria": {"true": "the review is positive", "false": "the review is negative"},
        "labels": {"true": "positive", "false": "negative"}
    }))
    .unwrap();
    let mixed_value = Value::Object(mixed.clone());
    let out = render_options(&mixed_value).unwrap();
    assert_eq!(
        out,
        vec![
            "negative: the review is negative",
            "positive: the review is positive"
        ]
    );
    // labels are carried through
    assert_eq!(
        mixed["labels"],
        json!({"true": "positive", "false": "negative"})
    );
    // labels keep the false/true slot order
    assert!(out[0].starts_with("negative:"));
    // the criteria descriptions are all `labels` changes
    let plain = to_internal(&json!({
        "type": "noul", "instructions": "Is the review positive?",
        "criteria": {"true": "the review is positive", "false": "the review is negative"}
    }))
    .unwrap();
    let strip = |opts: Vec<String>| -> Vec<String> {
        opts.into_iter()
            .map(|o| o.split_once(": ").unwrap().1.to_string())
            .collect()
    };
    assert_eq!(
        strip(out),
        strip(render_options(&Value::Object(plain)).unwrap())
    );
}

// --------------------------------------------------------------- instructions serialisation
#[test]
fn non_string_instructions_render_as_json_without_escapes() {
    // `_to_internal` serialises non-string `instructions` with json.dumps. The
    // default `ensure_ascii=True` escaped non-ASCII to literal `\uXXXX`, which
    // the tokenizer then read as escape text: on the English checkpoint one
    // German question answered noul=0.1652 as a dict and noul=0.2650 as the
    // identical plain string.
    let internal = to_internal(&json!({
        "type": "noul",
        "instructions": {"frage": "Bittet um eine Rückerstattung?"},
        "criteria": null
    }))
    .unwrap();
    assert_eq!(
        internal["ins"],
        json!("{\"frage\": \"Bittet um eine Rückerstattung?\"}")
    );
    let ins = internal["ins"].as_str().unwrap();
    assert!(!ins.contains("\\u"), "{}", ins);
}

#[test]
fn ascii_instructions_are_unchanged_in_shape() {
    let internal = to_internal(&json!({
        "type": "noul", "instructions": {"asks": "for a refund"}, "criteria": null
    }))
    .unwrap();
    assert_eq!(internal["ins"], json!("{\"asks\": \"for a refund\"}"));
}

#[test]
fn plain_string_instructions_are_untouched() {
    let internal = to_internal(&json!({
        "type": "noul",
        "instructions": "Bittet der Kunde um eine Rückerstattung?",
        "criteria": null
    }))
    .unwrap();
    assert_eq!(
        internal["ins"],
        json!("Bittet der Kunde um eine Rückerstattung?")
    );
}

#[test]
fn list_instructions_render_as_json() {
    let internal = to_internal(&json!({
        "type": "noul", "instructions": ["a", "b"], "criteria": null
    }))
    .unwrap();
    assert_eq!(internal["ins"], json!("[\"a\", \"b\"]"));
}

// --------------------------------------------------------------- labels reach the renderer unchanged
#[test]
fn public_labels_reach_the_renderer_without_changing_caller_data() {
    let public_labels = json!({"true": "A", "false": "B"});
    let public_question = json!({
        "type": "noul", "instructions": "Is this true?", "labels": public_labels
    });
    let internal = to_internal(&public_question).unwrap();
    // forwards noul labels
    assert_eq!(internal["labels"], json!({"true": "A", "false": "B"}));
    // forwarded labels reach renderer
    assert_eq!(
        render_options(&Value::Object(internal)).unwrap(),
        vec![
            "B: no, the statement does not hold",
            "A: yes, the statement holds"
        ]
    );
    // leaves public question unchanged
    assert_eq!(
        public_question,
        json!({"type": "noul", "instructions": "Is this true?", "labels": {"true": "A", "false": "B"}})
    );
}

#[test]
fn custom_labels_keep_boolean_criteria_normalization() {
    // Over the JSON surface the boolean keys arrive as "true"/"false"
    // strings; the lowercase normalisation is what the boolean-key case
    // exercises upstream.
    let boolean_criteria = json!({"TRUE": "yes", "FALSE": "no"});
    let boolean_question = json!({
        "type": "noul", "instructions": "Is this true?", "criteria": boolean_criteria,
        "labels": {"false": "B", "true": "A"}
    });
    let boolean_internal = to_internal(&boolean_question).unwrap();
    // custom labels keep boolean criteria normalization
    assert_eq!(
        boolean_internal["crit"],
        json!({"true": "yes", "false": "no"})
    );
    // boolean criteria render with custom labels
    assert_eq!(
        render_options(&Value::Object(boolean_internal)).unwrap(),
        vec!["B: no", "A: yes"]
    );
    // leaves boolean criteria unchanged
    assert_eq!(
        boolean_question["criteria"],
        json!({"TRUE": "yes", "FALSE": "no"})
    );
}
