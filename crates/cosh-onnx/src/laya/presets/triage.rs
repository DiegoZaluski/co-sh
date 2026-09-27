//! `triage_questions`: preset questions for general support-ticket triage.

use serde_json::{json, Map};

use super::q;
use super::Questions;

pub fn triage_questions() -> Questions {
    let mut out = Map::new();
    out.insert(
        "intent".to_string(),
        q(
            "choice",
            "What does the customer want in `message`?",
            Some(json!({
                "refund": "money returned or a duplicate charge reversed",
                "technical_help": "a bug, outage or integration problem",
                "billing_question": "a question about an invoice, plan or payment method",
                "information": "general information, pricing or how-to",
                "cancellation": "wants to cancel or downgrade",
                "other": "none of the other options fits",
            })),
        ),
    );
    out.insert(
        "is_urgent".to_string(),
        q("noul", "Does `message` communicate time pressure or a deadline?", None),
    );
    out.insert(
        "frustration".to_string(),
        q(
            "score",
            "How frustrated does the customer sound in `message`?",
            Some(json!([
                "calm and neutral",
                "concerned but civil",
                "clearly annoyed",
                "very angry or using strong language",
            ])),
        ),
    );
    out.insert(
        "refund_requested".to_string(),
        q("noul", "Does the customer ask for money back?", None),
    );
    out.insert(
        "churn_risk".to_string(),
        q(
            "noul",
            "Does `message` suggest the customer may leave for a competitor or cancel?",
            None,
        ),
    );
    out
}

