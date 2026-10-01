//! `router_questions`: preset questions for intelligent model routing.

use serde_json::{Map, json};

use super::Questions;
use super::q;

pub fn router_questions() -> Questions {
    let mut out = Map::new();
    out.insert(
        "difficulty".to_string(),
        q(
            "score",
            "How hard is `request` for a language model?",
            Some(json!([
                "trivial: a lookup or one-liner",
                "easy: short answer, no reasoning",
                "moderate: several steps",
                "hard: long multi-step reasoning or specialist knowledge",
            ])),
        ),
    );
    out.insert(
        "domain".to_string(),
        q(
            "choice",
            "What domain does `request` belong to?",
            Some(json!({
                "code": "software engineering, programming, refactoring, architecture, debugging",
                "math_or_logic": "mathematics, logic puzzles, proofs, complex calculation",
                "writing": "creative writing, essays, emails, blog posts, copywriting",
                "factual_lookup": "facts, definitions, trivia, history",
                "data_analysis": "statistics, SQL, data manipulation, metrics",
                "chitchat": "casual conversation, greetings, small talk",
            })),
        ),
    );
    out.insert(
        "needs_tools".to_string(),
        q(
            "noul",
            "Does answering `request` require external tools, search or private data?",
            None,
        ),
    );
    out.insert(
        "is_sensitive".to_string(),
        q(
            "noul",
            "Does `request` involve money, legal, medical or safety consequences?",
            None,
        ),
    );
    out
}
