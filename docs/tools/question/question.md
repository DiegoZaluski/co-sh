# The `question` module: asking the user for input

`question` is the module that lets the agent talk back to the human. It
presents structured questions — text, single choice, multi choice, yes/no —
and collects the answers, so the agent can ask for exactly the information it
needs instead of guessing.

| Tool | What it does |
|---|---|
| [`ask_questions`](#the-question-wrapper) | Present one or more structured questions and collect the user's answers. |

The module is deliberately small: one wrapper ([`Question`](#the-question-wrapper)),
one operation ([`ask`](#the-ask-method)), and a handful of types. There are no
free functions and no engines — the interesting part is the validation
contract and how the tool plugs into the UI layer.

---

## Design principles

Three principles shape the API, and they are baked into the tool description
so the model follows them:

1. **Batch questions.** Ask *all* the questions you need in a single call —
   repeated single-question interruptions frustrate users.
2. **Explain the purpose.** Every question can (and should) carry a
   `purpose`: *why* the agent needs the answer. Users answer better when they
   understand the context, and the agent avoids asking obviously inferable
   things.
3. **Offer structured options.** For choice questions, the options are
   provided up front so the user picks instead of types.

---

## The `Question` wrapper

```rust,ignore
use cosh_tools::question::Question;
use cosh_tools::question::types::{QuestionInput, QuestionItem, QuestionType};

let question = Question::new();
let output = question.ask(&QuestionInput {
    questions: vec![
        QuestionItem {
            id: "lang".into(),
            question: "Which language should I scaffold?".into(),
            question_type: QuestionType::SingleChoice,
            purpose: Some("To generate the right project structure".into()),
            options: Some(vec!["Rust".into(), "TypeScript".into()]),
            required: true,
            recommended: Some("Rust".into()),
        },
        // …more questions…
    ],
})?;
```

`Question::new()` pre-configures `description_ask` — the ready-to-serve MCP
tool description for `ask_questions`. `Question::default()` is
`Question::new()`.

### The `ask` method

```rust,ignore
pub fn ask(&self, input: &QuestionInput) -> Result<QuestionOutput, String>
```

`ask` is **synchronous and stateless** — it validates the input and returns a
`QuestionOutput` echoing the questions back with an empty `answers` list. The
*actual* interaction with the user happens in the UI layer: the harness
intercepts the `ask_questions` tool call, renders the dialog, captures the
user's input, and returns the answers as the tool result (see
[below](#how-answers-come-back)).

---

## Validation: the contract

`ask` rejects malformed question sets with a human-readable error before
anything is presented. The rules:

| Rule | Error |
|---|---|
| At least one question is required | `"No questions provided. Ask at least one question."` |
| Question IDs must be unique | `"Duplicate question id: 'q1'"` |
| `SingleChoice` / `MultiChoice` must have an `options` field | `"Question 'q1' is SingleChoice but has no 'options' field"` |
| …and at least one option | `"Question 'q1' has zero options. Provide at least one option."` |
| `SingleChoice` must set `recommended` (exactly one of `options`) | `"Question 'q1' is SingleChoice but has no 'recommended' field..."` |
| `SingleChoice` `recommended` must match one of `options` | `"Question 'q1' recommends 'X' which is not one of its options..."` |
| `SingleChoice` option must not collide with the reserved custom-answer label (`"Personalize your response"`) | `"Question 'q1' has option 'X' which collides with the reserved custom-answer label..."` |
| `YesNo` must not have custom options (it uses built-in Yes/No) | `"Question 'q1' is YesNo but has custom options. YesNo uses built-in 'Yes' and 'No'."` |
| `recommended` is forbidden for `Text` / `MultiChoice` / `YesNo` | `"Question 'q1' is ... but sets 'recommended'..."` |
| `Text` | No restrictions on options (they are ignored) |

`SingleChoice` questions are normalized: the `recommended` option is moved
to position 0 automatically (the model must NOT pre-sort `options` itself)
and the TUI renders it with a `(Recommended)` badge. The TUI also always
appends a virtual `Personalize your response` entry to `SingleChoice` — the
model must NOT invent its own custom/other option.

Every error names the offending question id, so the model can fix exactly
that item and retry. An empty `questions` array and duplicate ids fail the
whole call — validation is all-or-nothing.

---

## How answers come back

The tool is **stateless by design**: `Question` holds no answers. The flow is:

1. The model calls `ask_questions` with the batch of questions.
2. `ask` validates and returns `QuestionOutput { questions, answers: [] }`.
3. The harness/TUI intercepts the call, renders the questions as a dialog,
   and captures the user's answers.
4. The answers are returned to the model as the tool result — correlated by
   question `id` via [`AnswerItem`](types.md#answeritem).

The `id` field is the correlation key: it must be unique per call, and each
answer references the question it belongs to by that id.

---

## The harness and where asking is allowed

- `ask_questions` is available in **Ask mode** as well as Build/Yolo — asking
  the user questions is a read-only, conversational operation.
- The **internal sub-agent** cannot ask: `ask_questions` is in the sub-agent's
  blocklist (`SUBAGENT_BLOCKED_TOOLS`), because a headless sub-agent would
  hang waiting for a human it cannot reach. Only the main agent may ask.

---

## Summary

- One tool, one operation: `Question::new().ask(&input)`.
- Batch everything, explain `purpose`, offer `options` — the description
  teaches the model these habits.
- `ask` validates (non-empty, unique ids, options rules) and echoes the
  questions; the UI layer collects the answers.
- Answers correlate to questions by `id`.

Next: the [data types](types.md) — the question/answer model.
