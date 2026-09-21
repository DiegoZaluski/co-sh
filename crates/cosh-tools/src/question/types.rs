use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// Virtual option the TUI appends to every single-select question so the
/// user can always give a free-text answer. Never sent by the model — the
/// tool description explicitly tells it NOT to invent its own custom/other
/// entry, and [`crate::question::Question::validate_and_normalize`] rejects
/// payloads whose options collide with this reserved label (the TUI renders
/// it as a separate row, so a colliding option would be unreachable).
pub const CUSTOM_RESPONSE_LABEL: &str = "Personalize your response";

/// Marker suffix a model appends to an option label to mark its
/// recommendation (the market convention: recommended option first, with
/// this suffix). Stripped by [`QuestionInput::finalize`]; the stripped label
/// becomes the internal [`QuestionItem::recommended`] badge.
pub const RECOMMENDED_SUFFIX: &str = "(Recommended)";

/// The kind of answer a question renders as, derived from the wire shape:
/// `multiSelect: true` → multi-select; any non-empty `options` → single
/// select; otherwise free text. Internal — never part of the model-facing
/// schema.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub enum QuestionType {
    /// Free-text input. The user types their answer.
    #[default]
    Text,
    /// Single selection from a list of options (radio buttons).
    SingleChoice,
    /// Multiple selection from a list of options (checkboxes).
    MultiChoice,
}

impl QuestionType {
    /// `true` when this kind renders a choice list (`options` must exist).
    pub fn is_choice(self) -> bool {
        matches!(self, Self::SingleChoice | Self::MultiChoice)
    }
}

/// One choice inside a question's `options` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionOption {
    /// Display text the user will see and select. Concise (1-5 words).
    pub label: String,
    /// Explanation of what this option means or what will happen if chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl QuestionOption {
    /// Wrap a bare label.
    pub fn from_label(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
        }
    }
}

/// A single question to ask the user.
///
/// Wire format — the industry-standard shape used by Claude Code's
/// `AskUserQuestion` and its ports (Qwen Code, OpenCode, Crush):
///
/// ```json
/// {"question": "...", "header": "...",
///  "options": [{"label": "...", "description": "..."}], "multiSelect": false}
/// ```
///
/// The kind is derived, never declared: `multiSelect` selects the multi
/// kind, any non-empty `options` the single kind, absence of options the
/// free-text kind. `question_type` and `id` are internal (serialized for
/// the TUI and answer correlation, excluded from the model-facing schema).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct QuestionItem {
    /// The complete question text displayed to the user. Clear, specific,
    /// ends with a question mark.
    pub question: String,
    /// Very short label displayed as a chip/tag (max 12 chars). Examples:
    /// "Auth method", "Library", "Approach".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The available choices for this question (2-4 options). Omit for a
    /// free-text question. Do NOT add an "Other" option — free text is
    /// always offered automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<QuestionOption>>,
    /// Set to `true` to allow the user to select multiple options instead
    /// of just one. Defaults to `false`.
    #[serde(
        rename = "multiSelect",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub multi_select: bool,
    /// Why the agent needs this information. Helps the user understand the
    /// context and give better answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Whether an answer is required. Defaults to `true`.
    #[serde(default = "default_required")]
    pub required: bool,
    /// Internal: derived kind, per the struct doc. Never part of the schema.
    #[serde(default)]
    #[schemars(skip)]
    pub question_type: QuestionType,
    /// Internal: stable identifier used to correlate answers. Derived from
    /// position when absent. Never part of the schema.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schemars(skip)]
    pub id: String,
    /// Internal: label of the recommended single-select option (marked via
    /// [`RECOMMENDED_SUFFIX`], moved to the top by the tool). Never part of
    /// the schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub recommended: Option<String>,
}

impl Default for QuestionItem {
    fn default() -> Self {
        Self {
            question: String::new(),
            header: None,
            options: None,
            multi_select: false,
            purpose: None,
            required: true,
            question_type: QuestionType::Text,
            id: String::new(),
            recommended: None,
        }
    }
}

impl QuestionItem {
    /// Free-text constructor (no options).
    pub fn text(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            ..Self::default()
        }
    }

    /// Single-select constructor from plain labels.
    pub fn single_choice(question: impl Into<String>, labels: &[&str]) -> Self {
        Self {
            question: question.into(),
            options: Some(
                labels
                    .iter()
                    .map(|l| QuestionOption::from_label(*l))
                    .collect(),
            ),
            question_type: QuestionType::SingleChoice,
            ..Self::default()
        }
    }

    /// Multi-select constructor from plain labels.
    pub fn multi_choice(question: impl Into<String>, labels: &[&str]) -> Self {
        Self {
            question: question.into(),
            options: Some(
                labels
                    .iter()
                    .map(|l| QuestionOption::from_label(*l))
                    .collect(),
            ),
            question_type: QuestionType::MultiChoice,
            multi_select: true,
            ..Self::default()
        }
    }
}

const fn default_required() -> bool {
    true
}

/// Lenient wire decoder for one question item.
///
/// Accepts the canonical shape plus the emission variants models produce
/// regardless of the schema they were shown:
///
/// - `options` as plain strings instead of `{label}` objects
/// - `options` as a JSON-encoded string (double-encoded emission)
/// - `multiple` as an alias of `multiSelect`
/// - an item with only `header` and no `question` (the header carries the
///   prompt — same relaxation OpenCode shipped for its question tool)
///
/// Returns `None` when there is neither a `question` nor a `header` to show.
fn question_item_from_wire(value: &JsonValue) -> Option<QuestionItem> {
    let obj = value.as_object()?;
    let string_field = |key: &str| {
        obj.get(key)
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };

    let question = string_field("question").or_else(|| string_field("header"))?;
    let header = string_field("header");

    let multi_select = obj
        .get("multiSelect")
        .or_else(|| obj.get("multiple"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let options = decode_options(obj.get("options"));

    Some(QuestionItem {
        question,
        header,
        options,
        multi_select,
        purpose: string_field("purpose"),
        required: obj
            .get("required")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true),
        question_type: QuestionType::Text,
        id: String::new(),
        recommended: None,
    })
}

/// Decode `options` accepting canonical `[{label, description}]`, bare
/// `[string]`, and a JSON-encoded string of either. `None` when the field is
/// absent or not a list.
fn decode_options(value: Option<&JsonValue>) -> Option<Vec<QuestionOption>> {
    let arr: &Vec<JsonValue> = match value? {
        JsonValue::Array(arr) => arr,
        // Models sometimes double-encode the whole array as a string.
        JsonValue::String(s) => match serde_json::from_str::<Vec<JsonValue>>(s) {
            Ok(parsed) => {
                return Some(parsed.iter().filter_map(decode_option_item).collect());
            }
            Err(_) => return None,
        },
        _ => return None,
    };
    Some(arr.iter().filter_map(decode_option_item).collect())
}

/// Decode one option entry: a bare string or a `{label, description}` object.
/// Blank labels are rejected — they would render as empty selectable rows.
fn decode_option_item(item: &JsonValue) -> Option<QuestionOption> {
    match item {
        JsonValue::String(label) => {
            if label.trim().is_empty() {
                None
            } else {
                Some(QuestionOption::from_label(label.clone()))
            }
        }
        JsonValue::Object(_) => serde_json::from_value(item.clone())
            .ok()
            .filter(|o: &QuestionOption| !o.label.trim().is_empty()),
        _ => None,
    }
}

impl<'de> serde::de::Deserialize<'de> for QuestionItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let value = <JsonValue as serde::de::Deserialize>::deserialize(deserializer)?;
        question_item_from_wire(&value).ok_or_else(|| {
            serde::de::Error::custom(
                "each question needs a non-empty `question` (or `header`) field",
            )
        })
    }
}

/// A single answer from the user.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnswerItem {
    /// The question ID this answer corresponds to.
    pub id: String,
    /// The answer text (for free-text questions).
    pub answer: Option<String>,
    /// Selected option(s) (for choice questions).
    pub selected: Option<Vec<String>>,
}

/// Input for `ask_questions`.
///
/// Decodes through the lenient per-item decoder ([`question_item_from_wire`])
/// so canonical shapes and common model emission variants parse into the
/// same canonical struct.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QuestionInput {
    /// Questions to ask the user (1-4 per call). Ask ALL questions you need
    /// in a single call to minimize back-and-forth.
    pub questions: Vec<QuestionItem>,
}

impl QuestionInput {
    /// Internal: derive the canonical fields the wire leaves implicit — the
    /// per-question kind (from `multiSelect`/options presence), stable ids
    /// (from position, never colliding with ids already present), and the
    /// recommendation (from a [`RECOMMENDED_SUFFIX`] label, wherever it
    /// appears — models often place it on a non-first option).
    pub(crate) fn finalize(&mut self) {
        let mut used: std::collections::HashSet<String> = self
            .questions
            .iter()
            .filter(|q| !q.id.is_empty())
            .map(|q| q.id.clone())
            .collect();
        for (idx, q) in self.questions.iter_mut().enumerate() {
            if q.id.is_empty() {
                let mut candidate = format!("q{idx}");
                let mut n = 1;
                while used.contains(&candidate) {
                    candidate = format!("q{idx}-{n}");
                    n += 1;
                }
                used.insert(candidate.clone());
                q.id = candidate;
            }

            if q.multi_select {
                q.question_type = QuestionType::MultiChoice;
            } else if q.options.as_ref().is_some_and(|o| !o.is_empty()) {
                q.question_type = QuestionType::SingleChoice;
            } else {
                q.question_type = QuestionType::Text;
            }

            if let Some(opts) = &mut q.options {
                let mut found: Option<String> = None;
                for opt in opts.iter_mut() {
                    if let Some(stripped) = opt
                        .label
                        .strip_suffix(RECOMMENDED_SUFFIX)
                        .map(str::trim_end)
                        .filter(|l| !l.is_empty())
                    {
                        opt.label = stripped.to_string();
                        found.get_or_insert_with(|| opt.label.clone());
                    }
                }
                // A recommendation only means something on single-select;
                // multi-select just gets the cleaned label.
                if q.recommended.is_none() && q.question_type == QuestionType::SingleChoice {
                    q.recommended = found;
                }
            }
        }
    }
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
