//! Tool for asking structured questions to the user.
//!
//! [`Question`] provides a way for LLM agents to ask the user for
//! information in a structured, efficient way. Supports free-text and
//! choice questions (single/multi select) and allows asking several
//! questions at once to minimize back-and-forth.
//!
//! # Design principles
//!
//! - **Market-standard wire format**: the schema mirrors Claude Code's
//!   `AskUserQuestion` (and its ports — Qwen Code, OpenCode, Crush): a
//!   `questions` array of `{ question, header, options: [{label,
//!   description}], multiSelect }` items. Models trained on any of those
//!   harnesses emit a valid call on the first attempt.
//! - **Lenient decoding of model emission variants**: plain-string options,
//!   string-encoded arrays, the `multiple` alias, and header-only items all
//!   parse — see [`types::QuestionItem`]'s custom deserializer. These are
//!   model behaviors, not alternate schemas.
//! - **Batch questions**: ask multiple questions in a single call.
//! - **Purpose field**: each question may explain *why* the information is
//!   needed.
//! - **Recommendations by convention**: the recommended option is the FIRST
//!   option carrying a `" (Recommended)"` label suffix — the convention
//!   models already know. The suffix is stripped wherever it appears and
//!   turned into the internal `recommended` badge.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::question::Question;
//! use cosh_tools::question::types::QuestionInput;
//!
//! let q = Question::new();
//! let input: QuestionInput = serde_json::from_value(serde_json::json!({
//!     "questions": [{
//!         "question": "Which auth method?",
//!         "header": "Auth method",
//!         "options": [
//!             {"label": "OAuth 2.0 (Recommended)", "description": "Browser flow"},
//!             {"label": "API key", "description": "Static token"}
//!         ]
//!     }]
//! }))?;
//! let output = q.ask(&input)?;
//! ```

pub mod types;

#[cfg(test)]
mod test;

use types::{CUSTOM_RESPONSE_LABEL, QuestionInput, QuestionOutput, QuestionType};

use crate::ToolDescription;

/// Tool for asking structured questions to the user.
///
/// Use the [`ask`](Self::ask) method to present questions and collect
/// answers. The tool is stateless — the TUI layer is responsible for
/// showing the dialog and capturing user input.
pub struct Question {
    /// MCP Tool description for `ask_questions`.
    pub description_ask: ToolDescription,
}

impl Default for Question {
    fn default() -> Self {
        Self::new()
    }
}

/// Canonical example embedded in the tool description. The harness feeds
/// this to the model in schema-rejection hints (`exampleArgs`), so a failed
/// call is corrected with a complete, copy-pasteable payload rather than a
/// type-only skeleton.
const EXAMPLE_ARGS: &str = r#"{"questions": [
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
    "question": "Which sections should I include?",
    "header": "Sections",
    "multiSelect": true,
    "options": [
      {"label": "Introduction", "description": "Opening context"},
      {"label": "Conclusion", "description": "Final summary"}
    ]
  }
]}"#;

impl Question {
    /// Create a new `Question` with the tool description pre-configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            description_ask: serde_json::json!( {
                "name": "ask_questions",
                "description": concat!(
                    "Ask the user one or more questions and collect their answers. ",
                    "Ask ALL questions you need in a single call — batch your ",
                    "questions to minimize back-and-forth and avoid frustrating ",
                    "the user with repeated interruptions. Use the 'purpose' field ",
                    "to explain WHY you need each piece of information so the user ",
                    "understands the context and can give better answers.\n\n",
                    "Each question has:\n",
                    "- **question**: the complete question text, clear, specific, ",
                    "ending with a question mark.\n",
                    "- **header**: very short label displayed as a chip/tag (max 12 ",
                    "chars). Examples: \"Auth method\", \"Library\", \"Approach\".\n",
                    "- **options**: 2-4 distinct choices, each {\"label\": display ",
                    "text (1-5 words), \"description\": what the option means or its ",
                    "trade-off}. Omit 'options' entirely for a free-text question — ",
                    "the user can always type a custom answer instead of picking.\n",
                    "- **multiSelect**: set to true to allow the user to select ",
                    "multiple options (default false — mutually exclusive choices).\n\n",
                    "Do NOT add your own 'Other'/custom option — free text is offered ",
                    "automatically. If you recommend an option, make it the FIRST ",
                    "option and append \" (Recommended)\" to its label.\n\n",
                    "Rule of thumb: if you can reasonably infer the answer from ",
                    "context, do NOT ask — use your existing knowledge or search ",
                    "tools instead. Only ask when you genuinely need user input."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "Questions to ask the user (1-4 questions). Ask ALL questions at once.",
                            "minItems": 1,
                            "maxItems": 4,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": {
                                        "type": "string",
                                        "description": "The complete question to ask the user. Should be clear, specific, and end with a question mark. If multiSelect is true, phrase it accordingly, e.g. \"Which features do you want to enable?\""
                                    },
                                    "header": {
                                        "type": "string",
                                        "description": "Very short label displayed as a chip/tag (max 12 chars). Examples: \"Auth method\", \"Library\", \"Approach\"."
                                    },
                                    "options": {
                                        "type": "array",
                                        "description": "The available choices for this question (2-4 options). Each option: {\"label\": concise display text (1-5 words), \"description\": what this option means or its trade-off}. Omit for a free-text question. Do NOT add an 'Other' option — that is provided automatically.",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": {
                                                    "type": "string",
                                                    "description": "The display text for this option that the user will see and select. Should be concise (1-5 words)."
                                                },
                                                "description": {
                                                    "type": "string",
                                                    "description": "Explanation of what this option means or what will happen if chosen. Useful for providing context about trade-offs or implications."
                                                }
                                            },
                                            "required": ["label"]
                                        }
                                    },
                                    "multiSelect": {
                                        "type": "boolean",
                                        "default": false,
                                        "description": "Set to true to allow the user to select multiple options instead of just one. Use when choices are not mutually exclusive."
                                    },
                                    "purpose": {
                                        "type": "string",
                                        "description": "Why the agent needs this information. Helps the user understand context."
                                    },
                                    "required": {
                                        "type": "boolean",
                                        "default": true,
                                        "description": "Whether an answer is required. Defaults to true."
                                    }
                                },
                                "required": ["question"]
                            }
                        }
                    },
                    "required": ["questions"]
                },
                "exampleArgs": serde_json::from_str::<serde_json::Value>(EXAMPLE_ARGS)
                    .expect("EXAMPLE_ARGS is valid JSON"),
            }),
        }
    }

    /// Present questions to the user and collect their answers.
    ///
    /// Returns the user's answers as a structured [`QuestionOutput`].
    /// The TUI layer intercepts the tool call, renders the dialog,
    /// captures user input, and returns it as the tool result.
    ///
    /// Single-select questions are normalized: the recommended option (the
    /// option carrying a `" (Recommended)"` label suffix, per the market
    /// convention) is stripped, badged, and moved to the top.
    ///
    /// # Errors
    ///
    /// Validation only rejects what cannot be rendered sensibly:
    /// - The questions array is empty.
    /// - A choice question has zero options.
    /// - A single-select option collides with the reserved
    ///   [`types::CUSTOM_RESPONSE_LABEL`] (the TUI renders that label as
    ///   its own virtual row, so such an option would be unreachable).
    ///
    /// Everything else is normalized: empty `options` on a free-text
    /// question are cleared, and a [`types::RECOMMENDED_SUFFIX`] label
    /// naming an option that no longer exists is dropped.
    pub fn ask(&self, input: &QuestionInput) -> Result<QuestionOutput, String> {
        let questions = Self::validate_and_normalize(input)?;
        Ok(QuestionOutput {
            questions,
            answers: Vec::new(),
        })
    }

    /// Validate `input` and return the normalized questions.
    ///
    /// Normalization: derive the per-question kind and stable ids, strip
    /// [`types::RECOMMENDED_SUFFIX`] labels into the internal
    /// `recommended` badge, clear empty option lists on free-text questions,
    /// drop recommendations that name no option, and move the recommended
    /// single-select option to position 0. Shared by [`Self::ask`] and the
    /// harness agent loop so both paths see identical validation and
    /// ordering.
    ///
    /// # Errors
    ///
    /// Same error contract as [`Self::ask`].
    pub fn validate_and_normalize(
        input: &QuestionInput,
    ) -> Result<Vec<types::QuestionItem>, String> {
        if input.questions.is_empty() {
            return Err("No questions provided. Ask at least one question.".into());
        }

        // Derive the canonical fields the wire leaves implicit.
        let mut finalized = input.clone();
        finalized.finalize();
        let mut out = finalized.questions;

        for q in &mut out {
            if q.question_type.is_choice() {
                let opts = q.options.as_ref().ok_or_else(|| {
                    format!(
                        "Question '{}' is a choice question but has no 'options' — \
                         provide 2-4 options or omit 'options' for free text",
                        q.question
                    )
                })?;
                if opts.is_empty() {
                    return Err(format!(
                        "Question '{}' has zero options. Provide at least one option.",
                        q.question
                    ));
                }
                // Only single-select renders the virtual custom-answer row,
                // so only its option list can collide with the reserved label.
                if q.question_type == QuestionType::SingleChoice
                    && let Some(clash) = opts.iter().find(|o| o.label == CUSTOM_RESPONSE_LABEL)
                {
                    return Err(format!(
                        "Question '{}' has option '{}' which collides with the \
                         reserved custom-answer label. Do not add your own custom/other \
                         entry — the UI always offers free text itself.",
                        q.question, clash.label
                    ));
                }
            } else if q.options.as_ref().is_some_and(|o| o.is_empty()) {
                // Free-text questions render no choice list.
                q.options = None;
            }

            // A recommendation must name a real option; a stale/wrong label
            // is dropped (with the batch still accepted) rather than
            // burning an entire attempt.
            if let Some(rec) = &q.recommended
                && let Some(opts) = &q.options
                && !opts.iter().any(|o| o.label == *rec)
            {
                q.recommended = None;
            }
        }

        // Move the recommended single-select option to the top so the TUI
        // renders it first (with the Recommended badge).
        for q in &mut out {
            if q.question_type == QuestionType::SingleChoice
                && let Some(rec) = q.recommended.clone()
                && let Some(opts) = &mut q.options
                && let Some(pos) = opts.iter().position(|o| o.label == rec)
                && pos != 0
            {
                let item = opts.remove(pos);
                opts.insert(0, item);
            }
        }
        Ok(out)
    }
}
