//! EMPIRICAL PROBE (temporary, gated): which decision-state shape can the
//! published checkpoints actually score?
//!
//! The enrichment shipped a two-field state (`{"user_request",
//! "assistant_response"}`) that the review found the published English
//! checkpoint cannot read (every closing summary flips to `running`). This
//! probe runs a small labelled matrix over candidate shapes so the consumer
//! ships a shape measured against the real weights, not assumed.
//!
//! ```text
//! LAYA_EVAL_MODEL_DIR=/path/to/checkpoint \
//! cargo test -p cosh-onnx --test eval shape_probe -- --nocapture
//! ```
//!
//! KEPT as the standing record of the measured negative result: the
//! enrichment shapes above were rejected on these numbers, and the doc
//! comments in `src/harness/checkup.rs` cite this probe as evidence. Re-run
//! it when a request-aware checkpoint ships (its result may flip).

use std::collections::HashMap;

use serde_json::{Value, json};

use cosh_onnx::{LoadOptions, ModelKind, load};

/// One labelled pair: what the user asked, and the agent's final text.
/// Kept minimal (one terminated, one running) — the shape comparison, not
/// the accuracy measurement, is the goal; `loop_termination` scores the
/// winner on the full corpus.
struct Pair {
    request: &'static str,
    response: &'static str,
    terminated: bool,
}

const PAIRS: &[Pair] = &[
    // terminated: a closing summary (the shape the consumer most sees).
    Pair {
        request: "Instale a dependência do projeto, rode os testes e atualize o README.",
        response: "Fluxo terminado, tarefa concluída. A seguir, o que foi feito: instalei a dependência, rodei os testes e atualizei o README.",
        terminated: true,
    },
    // running: a next-step announcement.
    Pair {
        request: "Diagnostique o erro no parser: leia o arquivo e me diga o que encontrou.",
        response: "Agora vou ler o arquivo:",
        terminated: false,
    },
    // terminated: the direct-answer case that motivated the enrichment.
    Pair {
        request: "Quando é 2+2?",
        response: "2+2 é 4",
        terminated: true,
    },
    // running: mid-task narration.
    Pair {
        request: "The build is failing; find the root cause and fix it.",
        response: "Reading the stack trace... the failure seems to come from the parser.",
        terminated: false,
    },
];

/// A candidate state shape: a function from pair → state value.
type ShapeFn = fn(&Pair) -> Value;

struct Shape {
    name: &'static str,
    build: ShapeFn,
    /// The question instructions to pair with this shape (verbatim candidate).
    instructions: &'static str,
    /// The criteria to pair with this shape (verbatim candidate).
    criteria: (&'static str, &'static str),
}

/// The consumer's current instructions (names both fields).
const INSTR_TWO_FIELD: &str = "An agent worked on the task in `user_request` (each `[user] ` line is one user turn, oldest first) and produced the text in `assistant_response`. Did the agent's loop finish with this text, or is the agent about to keep working?";

/// The ORIGINAL instructions (the eval's pre-enrichment wording): names a
/// single `message`.
const INSTR_MESSAGE: &str = "An agent produced the text in `message` while working on a task. Did the agent's loop finish with this text, or is the agent about to keep working?";

/// A task-shaped rewrite of the message wording: the request is IN the
/// message, the loop question unchanged.
const INSTR_MESSAGE_TASK: &str = "`message` is an agent's final response to the task quoted inside it. Did the agent's loop finish with this text, or is the agent about to keep working?";

/// The consumer's current two-field criteria.
const CRITERIA_CURRENT: (&str, &str) = (
    "the task is complete and the loop ended: the response fully answers the user's request — a direct self-contained answer, or a summary of what was done with nothing left to do",
    "the loop is still going: the agent will read or write files, run commands, or start the next step",
);

/// Direct-answer-aware criteria for the BARE-MESSAGE state: the request is
/// not visible, so the criterion describes endings without referencing it.
const CRITERIA_DIRECT: (&str, &str) = (
    "the task is complete and the loop ended: the response is a complete ending — a direct self-contained answer, or a summary of what was done with nothing left to do",
    "the loop is still going: the agent will read or write files, run commands, or start the next step",
);

/// Question/answer-closure criteria: reframes BOTH options around whether
/// the exchange is closed rather than around a work summary.
const CRITERIA_QA: (&str, &str) = (
    "the exchange is closed: the response fully settles what was asked or reports the work as done, nothing left to do",
    "the exchange is open: the response announces or implies further work — reading or writing files, running commands, starting the next step",
);

fn shapes() -> Vec<Shape> {
    vec![
        // Baseline: the pre-enrichment shape the checkpoints were validated on.
        Shape {
            name: "A. bare message (baseline)",
            build: |p| json!({ "message": p.response }),
            instructions: INSTR_MESSAGE,
            criteria: CRITERIA_CURRENT,
        },
        // The shipped (failing) two-field shape.
        Shape {
            name: "B. user_request + assistant_response",
            build: |p| json!({
                "user_request": p.request,
                "assistant_response": p.response,
            }),
            instructions: INSTR_TWO_FIELD,
            criteria: CRITERIA_CURRENT,
        },
        // Concatenated into ONE message: the request quoted, then the
        // response — a single string the bare-message tokenizer path reads.
        Shape {
            name: "C. message-concat (task: … / response)",
            build: |p| json!({ "message": format!("Task: {}\n\nResponse: {}", p.request, p.response) }),
            instructions: INSTR_MESSAGE_TASK,
            criteria: CRITERIA_CURRENT,
        },
        // Same concat with the [user] prefix the consumer renders.
        Shape {
            name: "D. message-concat with [user] marker",
            build: |p| json!({ "message": format!("[user] {}\n{}", p.request, p.response) }),
            instructions: INSTR_MESSAGE_TASK,
            criteria: CRITERIA_CURRENT,
        },
        // Both fields but state as an ARRAY of strings (serialize_state
        // stringifies each element; truncate_left = state.is_array() also
        // flips, so the TAIL is kept when over budget).
        Shape {
            name: "E. array state [request, response]",
            build: |p| json!([p.request, p.response]),
            instructions: INSTR_TWO_FIELD,
            criteria: CRITERIA_CURRENT,
        },
        // Bare message (in-distribution state) + direct-answer-aware
        // criteria: does the wording alone flip the direct-answer case
        // WITHOUT dragging the request into the state?
        Shape {
            name: "F. bare message + direct-answer criteria",
            build: |p| json!({ "message": p.response }),
            instructions: INSTR_MESSAGE,
            criteria: CRITERIA_DIRECT,
        },
        // Bare message + exchange-closure criteria: both options reframed
        // around whether the exchange is closed.
        Shape {
            name: "G. bare message + QA-closure criteria",
            build: |p| json!({ "message": p.response }),
            instructions: INSTR_MESSAGE,
            criteria: CRITERIA_QA,
        },
        // The shipped two-field shape + QA-closure criteria: worst case
        // crossed with the reframe, for the full picture.
        Shape {
            name: "H. two-field + QA-closure criteria",
            build: |p| json!({
                "user_request": p.request,
                "assistant_response": p.response,
            }),
            instructions: INSTR_TWO_FIELD,
            criteria: CRITERIA_QA,
        },
    ]
}

fn questions(instructions: &str, criteria: (&str, &str)) -> serde_json::Map<String, Value> {
    let mut questions = serde_json::Map::new();
    questions.insert(
        "loop_terminated".to_string(),
        json!({
            "type": "choice",
            "instructions": instructions,
            "criteria": {
                "terminated": criteria.0,
                "running": criteria.1,
            }
        }),
    );
    questions
}

#[test]
fn shape_probe() {
    let Some(model_dir) = std::env::var("LAYA_EVAL_MODEL_DIR")
        .ok()
        .filter(|d| !d.is_empty())
    else {
        eprintln!("skipping shape_probe: set LAYA_EVAL_MODEL_DIR=<model dir>");
        return;
    };
    let onnx_path = match std::env::var("LAYA_EVAL_ONNX") {
        Ok(p) if !p.is_empty() => p,
        _ => format!("{model_dir}/laya.onnx"),
    };
    let model = load(
        ModelKind::Custom {
            repo: model_dir.clone(),
            subfolder: None,
        },
        &LoadOptions {
            onnx_path: Some(onnx_path),
            hooks_concurrent: Some(false),
            ..LoadOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("failed to load {model_dir}: {e}"));

    // Per-shape: hits + per-pair verdict detail.
    let mut tally: HashMap<&str, (usize, usize, Vec<String>)> = HashMap::new();
    for shape in shapes() {
        let qs = questions(shape.instructions, shape.criteria);
        let (mut hits, mut total) = (0usize, 0usize);
        let mut detail = Vec::new();
        for pair in PAIRS {
            let state = (shape.build)(pair);
            let result = model
                .decide(&state, &qs, None, None, None)
                .unwrap_or_else(|e| panic!("decide failed [{}/{}]: {e}", shape.name, pair.response));
            let answers = &result["answers"];
            let said = answers["loop_terminated"]["choice"]
                .as_str()
                .unwrap_or("<none>")
                .to_string();
            let conf = answers["loop_terminated"]["answer_confidence"]
                .as_f64()
                .unwrap_or(0.0);
            let correct = said == if pair.terminated { "terminated" } else { "running" };
            hits += usize::from(correct);
            total += 1;
            detail.push(format!(
                "    [{}{:2}] said={said:<10} conf={conf:.3} — “{}”",
                if correct { " " } else { "!" },
                if pair.terminated { "T" } else { "R" },
                &pair.response[..pair.response.len().min(50)],
            ));
        }
        tally.insert(shape.name, (hits, total, detail));
    }

    println!("\n=== shape probe: {model_dir} ===");
    for (name, (hits, total, detail)) in &tally {
        println!("{name}: {hits}/{total}");
        for line in detail {
            println!("{line}");
        }
    }
}
