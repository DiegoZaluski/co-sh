use super::Question;
use super::types::{CUSTOM_RESPONSE_LABEL, QuestionInput, QuestionType};

/// Decode a payload the way the harness does (serde over the wire JSON).
fn parse(v: serde_json::Value) -> QuestionInput {
    serde_json::from_value(v).expect("payload must decode")
}

#[test]
fn new_creates_tool_with_market_schema() {
    let q = Question::new();
    let desc = &q.description_ask;
    assert_eq!(desc["name"], "ask_questions");
    assert!(desc["description"].as_str().unwrap_or("").len() > 20);
    let schema = &desc["inputSchema"];
    assert_eq!(schema["required"][0], "questions");
    let item = &schema["properties"]["questions"]["items"];
    // Market-standard shape: only `question` is required; options are
    // {label, description} objects; multiSelect is the type discriminator.
    assert_eq!(item["required"][0], "question");
    assert_eq!(
        item["properties"]["multiSelect"]["type"],
        serde_json::json!("boolean")
    );
    assert_eq!(
        item["properties"]["options"]["items"]["required"][0],
        "label"
    );
    // The curated example must be present and valid JSON.
    assert!(desc["exampleArgs"]["questions"].is_array());
}

#[test]
fn example_args_pass_own_schema_validation() {
    let q = Question::new();
    let example = q.description_ask["exampleArgs"].clone();
    let input: QuestionInput = serde_json::from_value(example).expect("example must decode");
    assert!(Question::validate_and_normalize(&input).is_ok());
}

#[test]
fn ask_validates_non_empty_questions() {
    let q = Question::new();
    let input = QuestionInput { questions: vec![] };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("No questions provided"));
}

#[test]
fn ask_choice_without_options_is_rejected() {
    let q = Question::new();
    // multiSelect: true with no options cannot render a choice list.
    let input = parse(serde_json::json!({
        "questions": [{"question": "Pick some?", "multiSelect": true}]
    }));
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("no 'options'"), "got: {err}");
}

#[test]
fn ask_choice_with_zero_options_is_rejected() {
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{"question": "Pick some?", "multiSelect": true, "options": []}]
    }));
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("zero options"), "got: {err}");
}

#[test]
fn canonical_market_shape_decodes_and_accepts() {
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [
            {
                "question": "Which authentication method should we use?",
                "header": "Auth method",
                "multiSelect": false,
                "options": [
                    {"label": "OAuth 2.0 (Recommended)", "description": "Industry standard"},
                    {"label": "API key", "description": "Static token"}
                ]
            },
            {
                "question": "Which sections should I include?",
                "header": "Sections",
                "multiSelect": true,
                "options": [
                    {"label": "Introduction", "description": "Opening context"},
                    {"label": "Conclusion", "description": "Final summary"}
                ]
            }
        ]
    }));
    let out = q.ask(&input).unwrap();
    assert_eq!(out.questions.len(), 2);

    let first = &out.questions[0];
    assert_eq!(first.question_type, QuestionType::SingleChoice);
    assert_eq!(first.header.as_deref(), Some("Auth method"));
    // The "(Recommended)" suffix is stripped into the internal badge and the
    // option keeps its clean label.
    assert_eq!(first.recommended.as_deref(), Some("OAuth 2.0"));
    assert_eq!(first.options.as_ref().unwrap()[0].label, "OAuth 2.0");
    assert_eq!(
        first.options.as_ref().unwrap()[0].description.as_deref(),
        Some("Industry standard")
    );

    let second = &out.questions[1];
    assert_eq!(second.question_type, QuestionType::MultiChoice);
    assert!(second.multi_select);
}

#[test]
fn ids_are_derived_from_position() {
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [
            {"question": "First?"},
            {"question": "Second?"},
            {"question": "Third?"}
        ]
    }));
    let out = q.ask(&input).unwrap();
    let ids: Vec<_> = out.questions.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(ids, vec!["q0", "q1", "q2"]);
}

#[test]
fn string_encoded_options_array_is_repaired() {
    // Models sometimes double-encode the whole options array.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Pick one?",
            "options": "[\"A\", \"B\"]"
        }]
    }));
    let out = q.ask(&input).unwrap();
    let labels: Vec<_> = out.questions[0]
        .options
        .as_ref()
        .unwrap()
        .iter()
        .map(|o| o.label.clone())
        .collect();
    assert_eq!(labels, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn bare_string_options_decode() {
    // Plain-string options are the most common model emission variant.
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Which features?",
            "multiSelect": true,
            "options": ["Lint", "Format"]
        }]
    }));
    let out = Question::new().ask(&input).unwrap();
    assert_eq!(out.questions[0].question_type, QuestionType::MultiChoice);
    let opts = out.questions[0].options.as_ref().unwrap();
    assert_eq!(opts[0].label, "Lint");
    assert_eq!(opts[0].description, None);
}

#[test]
fn multiple_alias_decodes_as_multiselect() {
    // `multiple` is the alias some harnesses/models use for multiSelect.
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Which features?",
            "multiple": true,
            "options": ["Lint", "Format"]
        }]
    }));
    assert!(input.questions[0].multi_select);
    let out = Question::new().ask(&input).unwrap();
    assert_eq!(out.questions[0].question_type, QuestionType::MultiChoice);
}

#[test]
fn recommended_suffix_on_non_first_option_is_stripped() {
    // Models often place the (Recommended) marker on a non-first option;
    // wherever it appears it becomes the badge and the label is cleaned.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Deltas or snapshots?",
            "options": [
                {"label": "Deltas"},
                {"label": "Snapshots (Recommended)"}
            ]
        }]
    }));
    let out = q.ask(&input).unwrap();
    let first = &out.questions[0];
    assert_eq!(first.recommended.as_deref(), Some("Snapshots"));
    // The recommended option is moved to the top for the TUI badge.
    assert_eq!(first.options.as_ref().unwrap()[0].label, "Snapshots");
    assert_eq!(first.options.as_ref().unwrap()[1].label, "Deltas");
}

#[test]
fn recommended_first_stays_stable() {
    // A first-option suffix needs no reordering.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Pick one?",
            "options": [
                {"label": "A (Recommended)"},
                {"label": "B"},
                {"label": "C"}
            ]
        }]
    }));
    let out = q.ask(&input).unwrap();
    let labels: Vec<_> = out.questions[0]
        .options
        .as_ref()
        .unwrap()
        .iter()
        .map(|o| o.label.clone())
        .collect();
    assert_eq!(labels, vec!["A", "B", "C"]);
    assert_eq!(out.questions[0].recommended.as_deref(), Some("A"));
}

#[test]
fn recommended_suffix_on_multiselect_is_stripped_without_badge() {
    // A recommendation has no UI meaning on a multi-select: the label is
    // still cleaned (no literal suffix reaches the user) but no badge is set.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Which features?",
            "multiSelect": true,
            "options": [
                {"label": "Lint (Recommended)"},
                {"label": "Format"}
            ]
        }]
    }));
    let out = q.ask(&input).unwrap();
    assert_eq!(out.questions[0].recommended, None);
    assert_eq!(out.questions[0].options.as_ref().unwrap()[0].label, "Lint");
}

#[test]
fn question_falls_back_to_header() {
    // An item with only `header` still renders (models trained on harnesses
    // that require header emit it alone — the same relaxation OpenCode
    // shipped for its question tool).
    let input = parse(serde_json::json!({
        "questions": [{"header": "Pick mode"}]
    }));
    assert_eq!(input.questions[0].question, "Pick mode");
    assert_eq!(input.questions[0].header.as_deref(), Some("Pick mode"));
    // Neither question nor header → decode error.
    let err = serde_json::from_value::<QuestionInput>(serde_json::json!({
        "questions": [{"options": ["A"]}]
    }))
    .unwrap_err();
    assert!(err.to_string().contains("question"));
}

#[test]
fn blank_option_labels_are_filtered() {
    // A blank label would render as an empty selectable row, so it is
    // dropped at decode time instead of shown.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{"question": "Pick one?", "options": ["", "B"]}]
    }));
    let out = q.ask(&input).unwrap();
    let labels: Vec<_> = out.questions[0]
        .options
        .as_ref()
        .unwrap()
        .iter()
        .map(|o| o.label.clone())
        .collect();
    assert_eq!(labels, vec!["B".to_string()]);
}

#[test]
fn free_text_options_normalized_away() {
    let input = parse(serde_json::json!({
        "questions": [{"question": "Name?", "options": []}]
    }));
    assert_eq!(input.questions[0].question_type, QuestionType::Text);
    let out = Question::new().ask(&input).unwrap();
    assert_eq!(out.questions[0].options, None);
}

#[test]
fn ask_rejects_option_colliding_with_custom_label() {
    // The TUI renders `CUSTOM_RESPONSE_LABEL` as its own virtual row, so an
    // option with that exact name would be unreachable in the dialog.
    let q = Question::new();
    let input = parse(serde_json::json!({
        "questions": [{
            "question": "Pick one?",
            "options": [CUSTOM_RESPONSE_LABEL, "B"]
        }]
    }));
    let err = q.ask(&input).unwrap_err();
    assert!(
        err.contains("reserved custom-answer label"),
        "must reject colliding option: {err}"
    );
}
