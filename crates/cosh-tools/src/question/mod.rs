//! Tool for asking structured questions to the user.
//!
//! [`Question`] provides a way for LLM agents to ask the user for
//! information in a structured, efficient way. Supports multiple
//! question types (text, single choice, multi choice, yes/no) and
//! allows asking several questions at once to minimize back-and-forth.
//!
//! # Design principles
//!
//! - **Batch questions**: Ask multiple questions in a single call to
//!   avoid frustrating the user with repeated interruptions.
//! - **Purpose field**: Each question includes an explanation of *why*
//!   the information is needed, so the user can give better answers
//!   and the agent doesn't ask obvious questions.
//! - **Structured options**: For choice questions, the agent provides
//!   the options upfront so the user can pick rather than type.
//!
//! # Example
//!
//! ```ignore
//! use cosh_tools::question::Question;
//! use cosh_tools::question::types::{QuestionItem, QuestionType, QuestionInput};
//!
//! let q = Question::new();
//! let input = QuestionInput {
//!     questions: vec![
//!         QuestionItem {
//!             id: "lang".into(),
//!             question: "What programming language?".into(),
//!             question_type: QuestionType::Text,
//!             purpose: Some("To scaffold the project".into()),
//!             options: None,
//!             required: true,
//!             recommended: None,
//!         },
//!     ],
//! };
//! let output = q.ask(&input)?;
//! ```

pub mod types;

#[cfg(test)]
mod test;

use types::{QuestionInput, QuestionOutput};

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
                    "the user with repeated interruptions. ",
                    "Use the 'purpose' field to explain WHY you need each piece ",
                    "of information so the user understands the context and can ",
                    "give better answers.\n\n",
                    "Supported question types:\n",
                    "- **Text**: Free-text input (the user types an answer).\n",
                    "- **SingleChoice**: Pick exactly ONE option from a list. ",
                    "You MUST tell which option you recommend via the ",
                    "'recommended' field (it must exactly match one of 'options'). ",
                    "Do NOT pre-sort 'options' yourself — the tool moves the ",
                    "recommended option to the top automatically and the UI shows ",
                    "it with a '(Recommended)' badge. Do NOT add your own ",
                    "'custom/other/personalize' option — the UI always appends ",
                    "a 'Personalize your response' entry for SingleChoice.\n",
                    "- **MultiChoice**: Pick zero or more options from a list.\n",
                    "- **YesNo**: A simple yes/no choice.\n\n",
                    "Rule of thumb: if you can reasonably infer the answer from ",
                    "context, do NOT ask — use your existing knowledge or search ",
                    "tools instead. Only ask when you genuinely need user input."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "The questions to ask the user. Ask ALL questions at once.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {
                                        "type": "string",
                                        "description": "Unique identifier for this question (used to correlate answers)"
                                    },
                                    "question": {
                                        "type": "string",
                                        "description": "The question text displayed to the user"
                                    },
                                    "type": {
                                        "type": "string",
                                        "description": "Type of answer: 'Text', 'SingleChoice', 'MultiChoice', or 'YesNo'",
                                        "enum": ["Text", "SingleChoice", "MultiChoice", "YesNo"]
                                    },
                                    "purpose": {
                                        "type": "string",
                                        "description": "Why the agent needs this information. Helps the user understand context."
                                    },
                                    "options": {
                                        "type": "array",
                                        "description": "Available options for SingleChoice and MultiChoice questions",
                                        "items": { "type": "string" }
                                    },
                                    "required": {
                                        "type": "boolean",
                                        "description": "Whether an answer is required. Defaults to true.",
                                        "default": true
                                    },
                                    "recommended": {
                                        "type": "string",
                                        "description": "REQUIRED for SingleChoice: the option you recommend (must exactly match one entry of 'options'). Forbidden for every other type. The tool moves it to the top automatically."
                                    }
                                },
                                "required": ["id", "question", "type"]
                            }
                        }
                    },
                    "required": ["questions"]
                }
            }),
        }
    }

    /// Present questions to the user and collect their answers.
    ///
    /// Returns the user's answers as a structured [`QuestionOutput`].
    /// The TUI layer intercepts the tool call, renders the dialog,
    /// captures user input, and returns it as the tool result.
    ///
    /// SingleChoice questions are normalized: the `recommended` option is
    /// moved to the top automatically so the model never has to pre-sort
    /// `options` itself. The TUI renders that first entry with a
    /// `(Recommended)` badge.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The questions array is empty.
    /// - A `SingleChoice` or `MultiChoice` question has no options.
    /// - A `YesNo` question has custom options (it uses built-in Yes/No).
    /// - Question IDs are not unique.
    /// - A `SingleChoice` question has no `recommended` field, or the
    ///   recommended value does not exactly match one of `options`.
    /// - A `SingleChoice` option collides with the reserved
    ///   [`types::CUSTOM_RESPONSE_LABEL`] (the TUI renders that label as its
    ///   own virtual row, so such an option would be unreachable).
    /// - A non-`SingleChoice` question sets `recommended` (it is forbidden
    ///   for `Text`, `MultiChoice` and `YesNo`).
    pub fn ask(&self, input: &QuestionInput) -> Result<QuestionOutput, String> {
        let questions = Self::validate_and_normalize(input)?;
        Ok(QuestionOutput {
            questions,
            answers: Vec::new(),
        })
    }

    /// Validate `input` and return the normalized questions.
    ///
    /// Normalization currently means: for every `SingleChoice` question, move
    /// the `recommended` option to position 0 (stable for the rest). Shared
    /// by [`Self::ask`] and the harness agent loop so both the direct tool
    /// path and the `QuestionRequest` path see identical validation and
    /// ordering.
    ///
    /// # Errors
    ///
    /// Same error contract as [`Self::ask`].
    pub fn validate_and_normalize(
        input: &QuestionInput,
    ) -> Result<Vec<types::QuestionItem>, String> {
        // Validate questions
        if input.questions.is_empty() {
            return Err("No questions provided. Ask at least one question.".into());
        }

        let mut ids = std::collections::HashSet::new();
        for q in &input.questions {
            // Check for duplicate IDs
            if !ids.insert(&q.id) {
                return Err(format!("Duplicate question id: '{}'", q.id));
            }

            // Validate question type
            match q.question_type {
                types::QuestionType::SingleChoice => {
                    let opts = q.options.as_ref().ok_or_else(|| {
                        format!(
                            "Question '{}' is {:?} but has no 'options' field",
                            q.id, q.question_type
                        )
                    })?;
                    if opts.is_empty() {
                        return Err(format!(
                            "Question '{}' has zero options. Provide at least one option.",
                            q.id
                        ));
                    }
                    let rec = q.recommended.as_ref().ok_or_else(|| {
                        format!(
                            "Question '{}' is SingleChoice but has no 'recommended' field. \
                             Tell which option you recommend (it must exactly match one of 'options'); \
                             the tool moves it to the top automatically.",
                            q.id
                        )
                    })?;
                    if !opts.iter().any(|o| o == rec) {
                        return Err(format!(
                            "Question '{}' recommends '{rec}' which is not one of its options. \
                             'recommended' must exactly match one entry of 'options'.",
                            q.id
                        ));
                    }
                    if let Some(clash) = opts.iter().find(|o| *o == types::CUSTOM_RESPONSE_LABEL) {
                        return Err(format!(
                            "Question '{}' has option '{clash}' which collides with the \
                             reserved custom-answer label. Do not add your own custom/other \
                             entry — the UI always offers free text itself.",
                            q.id
                        ));
                    }
                }
                types::QuestionType::MultiChoice => {
                    let opts = q.options.as_ref().ok_or_else(|| {
                        format!(
                            "Question '{}' is {:?} but has no 'options' field",
                            q.id, q.question_type
                        )
                    })?;
                    if opts.is_empty() {
                        return Err(format!(
                            "Question '{}' has zero options. Provide at least one option.",
                            q.id
                        ));
                    }
                    if q.recommended.is_some() {
                        return Err(format!(
                            "Question '{}' is MultiChoice but sets 'recommended'. \
                             'recommended' is only allowed for SingleChoice.",
                            q.id
                        ));
                    }
                }
                types::QuestionType::YesNo => {
                    if let Some(ref opts) = q.options
                        && !opts.is_empty()
                    {
                        return Err(format!(
                            "Question '{}' is YesNo but has custom options. \
                             YesNo uses built-in 'Yes' and 'No'.",
                            q.id
                        ));
                    }
                    if q.recommended.is_some() {
                        return Err(format!(
                            "Question '{}' is YesNo but sets 'recommended'. \
                             'recommended' is only allowed for SingleChoice.",
                            q.id
                        ));
                    }
                }
                types::QuestionType::Text => {
                    // Text has no restrictions on options
                    if q.recommended.is_some() {
                        return Err(format!(
                            "Question '{}' is Text but sets 'recommended'. \
                             'recommended' is only allowed for SingleChoice.",
                            q.id
                        ));
                    }
                }
            }
        }

        // Normalize: move the recommended SingleChoice option to the top.
        let mut out = input.questions.clone();
        for q in &mut out {
            if q.question_type == types::QuestionType::SingleChoice
                && let Some(ref rec) = q.recommended.clone()
                && let Some(ref mut opts) = q.options
                && let Some(pos) = opts.iter().position(|o| o == rec)
                && pos != 0
            {
                let item = opts.remove(pos);
                opts.insert(0, item);
            }
        }
        Ok(out)
    }
}
