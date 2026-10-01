//! `moderation_questions`: preset questions for content safety and moderation.

use serde_json::{Map, json};

use super::Questions;
use super::q;

pub fn moderation_questions() -> Questions {
    let mut out = Map::new();
    out.insert(
        "toxic".to_string(),
        q(
            "noul",
            "Is `post` toxic: rude, disrespectful or likely to make someone leave the discussion?",
            None,
        ),
    );
    out.insert(
        "harassment".to_string(),
        q(
            "noul",
            "Does `post` target or harass a specific person?",
            None,
        ),
    );
    out.insert(
        "threat".to_string(),
        q(
            "noul",
            "Does `post` threaten violence, harm or intimidation?",
            None,
        ),
    );
    out.insert(
        "spam".to_string(),
        q("noul", "Is `post` spam or advertising?", None),
    );
    out.insert(
        "severity".to_string(),
        q(
            "score",
            "How severe is any rule-breaking in `post`?",
            Some(json!([
                "no rule-breaking: ordinary on-topic post",
                "mild: rude tone or off-topic, no target",
                "clear violation: insults, harassment or spam aimed at someone",
                "severe: threats, hate speech or calls for violence",
            ])),
        ),
    );
    out
}
