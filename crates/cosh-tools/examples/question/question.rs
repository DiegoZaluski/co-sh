//! Demonstrate `question`: building a structured question batch and the
//! validation contract behind `ask`. The example is deterministic — `ask`
//! validates the batch and echoes the questions back; the user interaction
//! itself happens in the UI layer.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example question
//! ```

use cosh_tools::question::Question;
use cosh_tools::question::types::{QuestionInput, QuestionItem, QuestionType};

fn ask(label: &str, result: Result<cosh_tools::question::types::QuestionOutput, String>) {
    println!("== {label} ==");
    match result {
        Ok(out) => {
            println!("  accepted {} question(s), answers: {}", out.questions.len(), out.answers.len());
            for q in &out.questions {
                println!(
                    "  - [{}] {} ({}",
                    q.id,
                    q.question,
                    serde_json::to_string(&q.question_type).unwrap()
                );
                if let Some(p) = &q.purpose {
                    println!("      purpose: {p}");
                }
                println!("      required: {}", q.required);
            }
        }
        Err(e) => println!("  rejected: {e}"),
    }
    println!();
}

fn main() {
    // ── 1. The tool description ----------------------------------------------
    let question = Question::new();
    println!("== 1. ask_questions schema ==");
    println!(
        "  name: {}, requires: {:?}\n",
        question.description_ask["name"].as_str().unwrap(),
        question.description_ask["inputSchema"]["required"]
    );

    // ── 2. A valid batch covering all four question types ---------------------
    let input = QuestionInput {
        questions: vec![
            QuestionItem {
                id: "name".into(),
                question: "What should the project be called?".into(),
                question_type: QuestionType::Text,
                purpose: Some("To name the crate".into()),
                options: None,
                required: true,
            },
            QuestionItem {
                id: "lang".into(),
                question: "Which language?".into(),
                question_type: QuestionType::SingleChoice,
                purpose: Some("To scaffold the right project".into()),
                options: Some(vec!["Rust".into(), "TypeScript".into()]),
                required: true,
            },
            QuestionItem {
                id: "features".into(),
                question: "Which features matter?".into(),
                question_type: QuestionType::MultiChoice,
                purpose: None,
                options: Some(vec!["Speed".into(), "Tooling".into(), "Ecosystem".into()]),
                required: false,
            },
            QuestionItem {
                id: "confirm".into(),
                question: "Start now?".into(),
                question_type: QuestionType::YesNo,
                purpose: Some("Need the go-ahead".into()),
                options: None,
                required: true,
            },
        ],
    };
    ask("2. valid batch (Text, SingleChoice, MultiChoice, YesNo)", question.ask(&input));

    // ── 3. The output serializes to JSON (what the harness returns) ------------
    let out = question.ask(&input).unwrap();
    println!("== 3. serialized output ==");
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    println!();

    // ── 4. Validation errors ----------------------------------------------------
    ask("4a. no questions", question.ask(&QuestionInput { questions: vec![] }));

    ask(
        "4b. duplicate ids",
        question.ask(&QuestionInput {
            questions: vec![
                item("q1", "First?", QuestionType::Text),
                item("q1", "Second?", QuestionType::Text),
            ],
        }),
    );

    ask(
        "4c. SingleChoice without options",
        question.ask(&QuestionInput {
            questions: vec![QuestionItem {
                options: None,
                ..item("pick", "Pick one?", QuestionType::SingleChoice)
            }],
        }),
    );

    ask(
        "4d. MultiChoice with zero options",
        question.ask(&QuestionInput {
            questions: vec![QuestionItem {
                options: Some(vec![]),
                ..item("pick", "Pick some?", QuestionType::MultiChoice)
            }],
        }),
    );

    ask(
        "4e. YesNo with custom options",
        question.ask(&QuestionInput {
            questions: vec![QuestionItem {
                options: Some(vec!["Maybe".into()]),
                ..item("yn", "Yes or no?", QuestionType::YesNo)
            }],
        }),
    );
}

fn item(id: &str, question: &str, question_type: QuestionType) -> QuestionItem {
    QuestionItem {
        id: id.into(),
        question: question.into(),
        question_type,
        purpose: None,
        options: None,
        required: true,
    }
}
