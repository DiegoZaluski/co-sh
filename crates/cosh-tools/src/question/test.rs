use super::Question;
use super::types::{QuestionInput, QuestionItem, QuestionType};

#[test]
fn new_creates_tool_with_description() {
    let q = Question::new();
    let desc = &q.description_ask;
    assert_eq!(desc["name"], "ask_questions");
    assert!(desc["description"].as_str().unwrap_or("").len() > 20);
    assert!(desc["inputSchema"].is_object());
}

#[test]
fn ask_validates_non_empty_questions() {
    let q = Question::new();
    let input = QuestionInput { questions: vec![] };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("No questions provided"));
}

#[test]
fn ask_validates_duplicate_ids() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![
            QuestionItem {
                id: "q1".into(),
                question: "First?".into(),
                question_type: QuestionType::Text,
                purpose: None,
                options: None,
                required: true,
                recommended: None,
            },
            QuestionItem {
                id: "q1".into(),
                question: "Second?".into(),
                question_type: QuestionType::Text,
                purpose: None,
                options: None,
                required: true,
                recommended: None,
            },
        ],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("Duplicate question id"));
}

#[test]
fn ask_validates_single_choice_has_options() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "q1".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: None,
            required: true,
            recommended: Some("A".into()),
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("no 'options' field"));
}

#[test]
fn ask_validates_multi_choice_has_options() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "q1".into(),
            question: "Pick many?".into(),
            question_type: QuestionType::MultiChoice,
            purpose: None,
            options: None,
            required: true,
            recommended: None,
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("no 'options' field"));
}

#[test]
fn ask_validates_choices_have_non_empty_options() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "q1".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec![]),
            required: true,
            recommended: Some("A".into()),
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("zero options"));
}

#[test]
fn ask_validates_yesno_no_custom_options() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "q1".into(),
            question: "Yes or no?".into(),
            question_type: QuestionType::YesNo,
            purpose: None,
            options: Some(vec!["Maybe".into()]),
            required: true,
            recommended: None,
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(err.contains("YesNo"));
}

#[test]
fn ask_valid_text_single_question() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "lang".into(),
            question: "What language?".into(),
            question_type: QuestionType::Text,
            purpose: Some("To scaffold the project".into()),
            options: None,
            required: true,
            recommended: None,
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 1);
    assert_eq!(output.questions[0].id, "lang");
    assert!(output.answers.is_empty());
}

#[test]
fn ask_valid_single_choice() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "color".into(),
            question: "Favorite color?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec!["Red".into(), "Blue".into(), "Green".into()]),
            required: true,
            recommended: Some("Red".into()),
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 1);
    assert_eq!(
        output.questions[0].question_type,
        QuestionType::SingleChoice
    );
}

#[test]
fn ask_valid_multi_choice() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "toppings".into(),
            question: "Pizza toppings?".into(),
            question_type: QuestionType::MultiChoice,
            purpose: None,
            options: Some(vec![
                "Cheese".into(),
                "Pepperoni".into(),
                "Mushrooms".into(),
            ]),
            required: false,
            recommended: None,
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 1);
    assert!(!output.questions[0].required);
}

#[test]
fn ask_valid_yesno() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "confirm".into(),
            question: "Proceed?".into(),
            question_type: QuestionType::YesNo,
            purpose: Some("Need confirmation before continuing".into()),
            options: None,
            required: true,
            recommended: None,
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 1);
}

#[test]
fn ask_multiple_questions() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![
            QuestionItem {
                id: "name".into(),
                question: "Your name?".into(),
                question_type: QuestionType::Text,
                purpose: None,
                options: None,
                required: true,
                recommended: None,
            },
            QuestionItem {
                id: "role".into(),
                question: "Your role?".into(),
                question_type: QuestionType::SingleChoice,
                purpose: Some("To set permissions".into()),
                options: Some(vec!["Admin".into(), "User".into(), "Viewer".into()]),
                required: true,
                recommended: Some("Admin".into()),
            },
            QuestionItem {
                id: "notify".into(),
                question: "Send notifications?".into(),
                question_type: QuestionType::YesNo,
                purpose: None,
                options: None,
                required: true,
                recommended: None,
            },
        ],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 3);
    // IDs are preserved
    assert_eq!(output.questions[0].id, "name");
    assert_eq!(output.questions[1].id, "role");
    assert_eq!(output.questions[2].id, "notify");
}

#[test]
fn ask_single_choice_requires_recommended() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "pick".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec!["A".into(), "B".into()]),
            required: true,
            recommended: None,
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(
        err.contains("recommended"),
        "missing recommended must be rejected: {err}"
    );
}

#[test]
fn ask_single_choice_recommended_must_match_options() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "pick".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec!["A".into(), "B".into()]),
            required: true,
            recommended: Some("C".into()),
        }],
    };
    let err = q.ask(&input).unwrap_err();
    assert!(
        err.contains("not one of its options"),
        "unknown recommended must be rejected: {err}"
    );
}

#[test]
fn ask_single_choice_moves_recommended_to_top() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "refactor".into(),
            question: "Deltas or snapshots?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec![
                "Deltas because x".into(),
                "Snapshots because y".into(),
                "Neither".into(),
            ]),
            required: true,
            recommended: Some("Snapshots because y".into()),
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(
        output.questions[0].options.as_deref(),
        Some(
            vec![
                "Snapshots because y".to_string(),
                "Deltas because x".to_string(),
                "Neither".to_string(),
            ]
            .as_slice()
        )
    );
    // Recommended field is preserved so the TUI can badge it.
    assert_eq!(
        output.questions[0].recommended.as_deref(),
        Some("Snapshots because y")
    );
}

#[test]
fn ask_single_choice_recommended_first_stays_stable() {
    let q = Question::new();
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "pick".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec!["A".into(), "B".into(), "C".into()]),
            required: true,
            recommended: Some("A".into()),
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(
        output.questions[0].options.as_deref(),
        Some(vec!["A".to_string(), "B".to_string(), "C".to_string()].as_slice())
    );
}

#[test]
fn ask_rejects_recommended_for_other_types() {
    let q = Question::new();
    for (id, qt) in [
        ("t", QuestionType::Text),
        ("m", QuestionType::MultiChoice),
        ("y", QuestionType::YesNo),
    ] {
        let input = QuestionInput {
            questions: vec![QuestionItem {
                id: id.into(),
                question: "Q?".into(),
                question_type: qt,
                purpose: None,
                options: if qt == QuestionType::MultiChoice {
                    Some(vec!["A".into()])
                } else {
                    None
                },
                required: true,
                recommended: Some("A".into()),
            }],
        };
        let err = q.ask(&input).unwrap_err();
        assert!(
            err.contains("only allowed for SingleChoice"),
            "{id:?} must reject recommended: {err}"
        );
    }
}

#[test]
fn ask_recommended_deserializes_with_default_none() {
    // Old payloads without `recommended` still parse (then fail validation
    // for SingleChoice with a clear message instead of a schema error).
    let v = serde_json::json!({
        "questions": [{
            "id": "pick",
            "question": "Pick one?",
            "type": "SingleChoice",
            "options": ["A", "B"],
            "required": true
        }]
    });
    let input: QuestionInput = serde_json::from_value(v).unwrap();
    assert_eq!(input.questions[0].recommended, None);
    let err = Question::new().ask(&input).unwrap_err();
    assert!(err.contains("recommended"));
}

#[test]
fn ask_rejects_option_colliding_with_custom_label() {
    // The TUI renders `CUSTOM_RESPONSE_LABEL` as its own virtual row, so an
    // option with that exact name would be unreachable in the dialog.
    let input = QuestionInput {
        questions: vec![QuestionItem {
            id: "s1".into(),
            question: "Pick one?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: None,
            options: Some(vec![
                crate::question::types::CUSTOM_RESPONSE_LABEL.into(),
                "B".into(),
            ]),
            required: true,
            recommended: Some("B".into()),
        }],
    };
    let err = Question::new().ask(&input).unwrap_err();
    assert!(
        err.contains("reserved custom-answer label"),
        "must reject colliding option: {err}"
    );
}
