use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Virtual option the TUI appends to every `SingleChoice` question so the
/// user can always give a free-text answer. Never sent by the model — the
/// tool description explicitly tells it NOT to invent its own custom/other
/// entry, and [`crate::question::Question::validate_and_normalize`] rejects
/// payloads whose options collide with this reserved label (the TUI renders
/// it as a separate row, so a colliding option would be unreachable).
pub const CUSTOM_RESPONSE_LABEL: &str = "Personalize your response";

/// A single question to ask the user.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuestionItem {
    /// Unique identifier for this question (used to correlate answers).
    pub id: String,
    /// The question text displayed to the user.
    pub question: String,
    /// The type of answer expected.
    #[serde(rename = "type")]
    pub question_type: QuestionType,
    /// Why the agent needs this information. Helps the user provide better answers
    /// and reduces frustration from "obvious" questions.
    pub purpose: Option<String>,
    /// Available options for `single_choice` and `multi_choice` questions.
    pub options: Option<Vec<String>>,
    /// Whether an answer is required. Defaults to `true`.
    #[serde(default = "default_required")]
    pub required: bool,
    /// The option the agent recommends for a `SingleChoice` question.
    ///
    /// Must exactly match one of `options`. The tool moves it to the top
    /// automatically (the model must NOT pre-sort options itself) and the
    /// TUI renders it with a `(Recommended)` badge. Required for
    /// `SingleChoice`; forbidden for every other question type.
    #[serde(default)]
    pub recommended: Option<String>,
}

const fn default_required() -> bool {
    true
}

/// The kind of answer expected for a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum QuestionType {
    /// Free-text input. The user types their answer.
    Text,
    /// Single selection from a list of options (radio buttons).
    SingleChoice,
    /// Multiple selection from a list of options (checkboxes).
    MultiChoice,
    /// A yes/no question. Presented as a choice between "Yes" and "No".
    YesNo,
}

/// A single answer from the user.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnswerItem {
    /// The question ID this answer corresponds to.
    pub id: String,
    /// The answer text (for `Text` and `YesNo` questions).
    pub answer: Option<String>,
    /// Selected option(s) (for `SingleChoice` and `MultiChoice` questions).
    pub selected: Option<Vec<String>>,
}

/// Input for `ask_questions`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuestionInput {
    /// The questions to ask the user. Ask multiple questions at once to
    /// minimize back-and-forth and avoid frustrating the user with
    /// repeated interruptions.
    pub questions: Vec<QuestionItem>,
}

/// Output from `ask_questions`. Contains the user's answers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuestionOutput {
    /// The questions that were asked (echoed back so the TUI can render them).
    pub questions: Vec<QuestionItem>,
    /// The user's answers, in the same order as the questions.
    /// Empty when the tool is first called — the TUI captures answers and
    /// returns them on a subsequent invocation.
    pub answers: Vec<AnswerItem>,
}
