# `question` data types

The module has one question model, one answer model, and two container types
— all deriving `Serialize` + `Deserialize` + `JsonSchema`, so the same
structs both carry the tool-call JSON and describe the schema.

---

## `QuestionType` — the kinds of answers

```rust
pub enum QuestionType {
    Text,          // free-text input; the user types an answer
    SingleChoice,  // pick ONE option (radio buttons)
    MultiChoice,   // pick ZERO OR MORE options (checkboxes)
    YesNo,         // built-in "Yes" / "No" choice
}
```

The type selects both how the question is rendered and what the answer
looks like (see [`AnswerItem`](#answeritem)).

## `QuestionItem` — one question

```rust
pub struct QuestionItem {
    pub id: String,              // unique per call; correlates answers
    pub question: String,        // the text shown to the user
    #[serde(rename = "type")]
    pub question_type: QuestionType,
    pub purpose: Option<String>, // WHY the agent needs this
    pub options: Option<Vec<String>>, // choices for Single/MultiChoice
    #[serde(default = "default_required")]
    pub required: bool,          // defaults to true
}
```

Field-by-field:

- **`id`** — the correlation key. Must be unique within one call (duplicates
  are rejected by [`ask`](question.md#validation-the-contract)), and the user's
  answer for this question comes back tagged with the same id.
- **`question`** — the visible text; keep it short and self-contained.
- **`question_type`** — serialized as `"type"` in JSON (the `#[serde(rename
  = "type")]` attribute), matching the tool schema's `type` property.
- **`purpose`** — optional explanation of why the information is needed. The
  tool description urges the model to always fill this in.
- **`options`** — required for `SingleChoice`/`MultiChoice` (and must be
  non-empty); forbidden for `YesNo`; ignored for `Text`.
- **`required`** — whether the user must answer; defaults to `true` when the
  field is omitted (via `#[serde(default = "default_required")]`).

## `AnswerItem` — one answer

```rust
pub struct AnswerItem {
    pub id: String,                        // the question id this answers
    pub answer: Option<String>,            // Text and YesNo answers
    pub selected: Option<Vec<String>>,     // SingleChoice / MultiChoice picks
}
```

An answer references its question by `id` and carries exactly one payload
shape, depending on the question type:

| Question type | Populated field |
|---|---|
| `Text` | `answer` — the typed text |
| `YesNo` | `answer` — "Yes" or "No" |
| `SingleChoice` | `selected` — one option |
| `MultiChoice` | `selected` — the chosen options |

## `QuestionInput` and `QuestionOutput`

```rust
pub struct QuestionInput {
    pub questions: Vec<QuestionItem>,
}

pub struct QuestionOutput {
    pub questions: Vec<QuestionItem>,  // echoed back for rendering
    pub answers: Vec<AnswerItem>,      // empty on the first call
}
```

`QuestionInput` is the tool-call argument: just the batch. `QuestionOutput`
is what [`ask`](question.md#the-ask-method) returns: the questions echoed back
(unmodified) plus the answers. On the initial validation call `answers` is
**empty** — the UI layer captures the user's input and returns it as the
tool result, correlated by `id`.

---

## JSON shape

Because of the renames and defaults, the JSON form of a question is:

```json
{
  "id": "lang",
  "question": "Which language?",
  "type": "SingleChoice",
  "purpose": "To scaffold the project",
  "options": ["Rust", "TypeScript"],
  "required": true
}
```

Note `"type"` (not `"question_type"`) and that `required` may be omitted
(it defaults to `true`).
