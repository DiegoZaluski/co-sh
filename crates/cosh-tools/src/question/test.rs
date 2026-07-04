use super::types::{QuestionInput, QuestionItem, QuestionType};
use super::Question;

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
            },
            QuestionItem {
                id: "q1".into(),
                question: "Second?".into(),
                question_type: QuestionType::Text,
                purpose: None,
                options: None,
                required: true,
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
        }],
    };
    let output = q.ask(&input).unwrap();
    assert_eq!(output.questions.len(), 1);
    assert_eq!(output.questions[0].question_type, QuestionType::SingleChoice);
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
            options: Some(vec!["Cheese".into(), "Pepperoni".into(), "Mushrooms".into()]),
            required: false,
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
            },
            QuestionItem {
                id: "role".into(),
                question: "Your role?".into(),
                question_type: QuestionType::SingleChoice,
                purpose: Some("To set permissions".into()),
                options: Some(vec!["Admin".into(), "User".into(), "Viewer".into()]),
                required: true,
            },
            QuestionItem {
                id: "notify".into(),
                question: "Send notifications?".into(),
                question_type: QuestionType::YesNo,
                purpose: None,
                options: None,
                required: true,
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
