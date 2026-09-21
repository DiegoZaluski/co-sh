//! Demonstrate `question`: the market-standard wire format, the lenient
//! decoding of model emission variants, and the validation contract behind
//! `ask`. The example is deterministic — `ask` validates the batch and
//! echoes the questions back; the user interaction itself happens in the
//! UI layer.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example question
//! ```

use cosh_tools::question::Question;
use cosh_tools::question::types::{QuestionInput, QuestionType};

fn ask(label: &str, result: Result<cosh_tools::question::types::QuestionOutput, String>) {
    println!("== {label} ==");
    match result {
        Ok(out) => {
            println!(
                "  accepted {} question(s), answers: {}",
                out.questions.len(),
                out.answers.len()
            );
            for q in &out.questions {
                println!(
                    "  - [{}] {} ({})",
                    q.id,
                    q.question,
                    serde_json::to_string(&q.question_type).unwrap()
                );
                if let Some(p) = &q.purpose {
                    println!("      purpose: {p}");
                }
                println!("      required: {}", q.required);
                if let Some(rec) = &q.recommended {
                    println!("      recommended: {rec}");
                }
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

    // ── 2. The canonical market-standard wire shape (Claude Code style) -------
    let canonical: QuestionInput = serde_json::from_value(serde_json::json!({
        "questions": [
            {
                "question": "Which authentication method should we use?",
                "header": "Auth method",
                "multiSelect": false,
                "options": [
                    {"label": "OAuth 2.0 (Recommended)", "description": "Industry standard, supports multiple providers"},
                    {"label": "API key", "description": "Stateless, good for simple server-to-server calls"}
                ]
            },
            {
                "question": "What should the project be called?",
                "header": "Name",
                "purpose": "To name the crate"
            }
        ]
    }))
    .expect("canonical shape must decode");
    ask("2. canonical market shape", question.ask(&canonical));

    // ── 3. The output serializes to JSON (what the harness returns) ------------
    let out = question.ask(&canonical).unwrap();
    println!("== 3. serialized output ==");
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    println!();

    // ── 4. Lenient decoding: model emission variants still parse ----------------
    let string_options: QuestionInput = serde_json::from_value(serde_json::json!({
        "questions": [{
            "question": "Pick one?",
            "options": "[\"A\", \"B\"]"
        }]
    }))
    .expect("string-encoded options must decode");
    ask("4a. string-encoded options", question.ask(&string_options));

    let bare_strings: QuestionInput = serde_json::from_value(serde_json::json!({
        "questions": [{
            "question": "Which features?",
            "multiple": true,
            "options": ["Lint", "Format"]
        }]
    }))
    .expect("bare-string options with the `multiple` alias must decode");
    ask(
        "4b. bare-string options + `multiple` alias",
        question.ask(&bare_strings),
    );

    let header_only: QuestionInput = serde_json::from_value(serde_json::json!({
        "questions": [{"header": "Pick mode"}]
    }))
    .expect("header-only item must decode");
    ask("4c. header-only item", question.ask(&header_only));

    // ── 5. Validation errors ----------------------------------------------------
    ask(
        "5a. no questions",
        question.ask(&QuestionInput { questions: vec![] }),
    );

    ask(
        "5b. choice question without options",
        question.ask(
            &serde_json::from_value::<QuestionInput>(serde_json::json!({
                "questions": [{"question": "Pick some?", "multiSelect": true}]
            }))
            .unwrap(),
        ),
    );

    ask(
        "5c. option colliding with the reserved custom-answer label",
        question.ask(
            &serde_json::from_value::<QuestionInput>(serde_json::json!({
                "questions": [{
                    "question": "Pick one?",
                    "options": [
                        {"label": "Personalize your response"},
                        {"label": "B"}
                    ]
                }]
            }))
            .unwrap(),
        ),
    );

    let _ = QuestionType::Text; // keep the import honest for doc examples
}
