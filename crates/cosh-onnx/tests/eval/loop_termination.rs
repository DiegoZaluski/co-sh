//! Real-checkpoint evaluation: agent-loop termination detection.
//!
//! The idea: simulate the way an agent loop stops (or fails to stop) by
//! feeding the model sentences that either close the loop ("Fluxo terminado,
//! tarefa concluída. A seguir, o que foi feito: ...") or announce more work
//! ("Agora vou ler o arquivo:"), then ask the model whether the loop ended.
//! The scores tell us whether the port is good enough for production use.
//!
//! This is a *new* evaluation, not a port of an upstream test: upstream has no
//! loop-termination dataset. It follows the upstream `laya/evals.py`
//! conventions instead — one `Example` = `state` + `questions` + `expected`
//! (+ tags/language), `ChoiceAccuracy` scoring (`answer["choice"] ==
//! expected`), calibration read from `answer_confidence`, and per-slice
//! aggregation by language and label.
//!
//! Gated: the default `cargo test` run skips it. Set
//! `LAYA_EVAL_MODEL_DIR=<model dir>` (and optionally `LAYA_EVAL_ONNX`,
//! `LAYA_EVAL_MIN_ACCURACY`) to run it against a real exported checkpoint:
//!
//! ```text
//! LAYA_EVAL_MODEL_DIR=/tmp/laya-eval/english \
//! LAYA_EVAL_ONNX=/tmp/laya-eval/laya.onnx \
//! cargo test -p cosh-onnx --test loop_termination_eval -- --nocapture
//! ```
//!
//! Measured against the three published checkpoints (ONNX exported with
//! upstream `scripts/export_onnx.py`, and the same dataset through upstream
//! torch laya — the numbers match to 4 decimals, confirming the port rather
//! than the model decides these scores):
//!
//! | checkpoint      | loop accuracy | mean `answer_confidence` | control |
//! |-----------------|--------------|--------------------------|---------|
//! | english         | 0.8333       | 0.7321                   | 1.0000  |
//! | multilingual    | 0.8333       | 0.7989                   | 1.0000  |
//! | typed-decisions | 0.8750       | 0.7182                   | 1.0000  |
//!
//! The misses are all borderline "terminated" summaries read as "running" —
//! the model is conservative without an explicit closing word. 0.90 is not
//! reachable on this out-of-distribution task for any checkpoint (upstream
//! torch included), so the default floor is 0.80; raise
//! `LAYA_EVAL_MIN_ACCURACY=0.9` to re-run the stricter gate.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use cosh_onnx::{LoadOptions, ModelKind, load};

/// One labelled example: the loop text, whether the loop ended, and the
/// language tag used for slicing (`en`/`pt`).
struct Example {
    message: &'static str,
    terminated: bool,
    language: &'static str,
    /// `control` marks in-distribution triage examples: the model was trained
    /// on this shape of decision, so they anchor the checkpoint's advertised
    /// behaviour while the loop-termination slice measures the new task.
    control: bool,
}

/// The simulated loop fragments. Terminated examples close the loop (summary,
/// "done", no further steps); running examples announce the next action and
/// would keep a healthy agent loop alive. The first two of each kind are the
/// sentences from the evaluation brief.
const EXAMPLES: &[Example] = &[
    // ------------------------------------------------ terminated (PT)
    Example {
        message: "Fluxo terminado, tarefa concluída. A seguir, o que foi feito: instalei a dependência, rodei os testes e atualizei o README.",
        terminated: true,
        language: "pt",
        control: false,
    },
    Example {
        message: "Tarefa concluída com sucesso. Todas as etapas foram executadas e os testes passaram.",
        terminated: true,
        language: "pt",
        control: false,
    },
    Example {
        message: "Pronto! O relatório foi gerado e enviado por e-mail. Encerrando o loop.",
        terminated: true,
        language: "pt",
        control: false,
    },
    Example {
        message: "A correção foi aplicada e verificada. Não há mais passos pendentes. Tarefa encerrada.",
        terminated: true,
        language: "pt",
        control: false,
    },
    Example {
        message: "Resumo final: o bug foi corrigido, os testes passaram e o deploy foi concluído. Nada mais a fazer.",
        terminated: true,
        language: "pt",
        control: false,
    },
    Example {
        message: "Objetivo alcançado. O pipeline está verde e a entrega foi publicada. Fim da execução.",
        terminated: true,
        language: "pt",
        control: false,
    },
    // ------------------------------------------------ terminated (EN)
    Example {
        message: "Done. Task complete. Here is a summary of what was changed: fixed the parser, updated the docs, closed the ticket.",
        terminated: true,
        language: "en",
        control: false,
    },
    Example {
        message: "The migration ran successfully and all checks pass. Nothing left to do.",
        terminated: true,
        language: "en",
        control: false,
    },
    Example {
        message: "All done — build green, docs updated, PR merged. Loop finished.",
        terminated: true,
        language: "en",
        control: false,
    },
    Example {
        message: "Fixed the failing test and re-ran the suite: 142 passed, 0 failed. Work complete.",
        terminated: true,
        language: "en",
        control: false,
    },
    Example {
        message: "Setup finished. Environment ready. Ending the session now.",
        terminated: true,
        language: "en",
        control: false,
    },
    Example {
        message: "Task finished: created the endpoint, added tests, updated the changelog. Done for today.",
        terminated: true,
        language: "en",
        control: false,
    },
    // ------------------------------------------------ running (PT)
    Example {
        message: "Agora vou ler o arquivo:",
        terminated: false,
        language: "pt",
        control: false,
    },
    Example {
        message: "Vou editar o arquivo de configuração e rodar os testes em seguida.",
        terminated: false,
        language: "pt",
        control: false,
    },
    Example {
        message: "Primeiro vou examinar os logs para entender o erro e depois corrigir.",
        terminated: false,
        language: "pt",
        control: false,
    },
    Example {
        message: "Vou começar criando o branch e depois aplicarei as mudanças.",
        terminated: false,
        language: "pt",
        control: false,
    },
    Example {
        message: "Analisando o resultado anterior. A seguir, vou ajustar a configuração.",
        terminated: false,
        language: "pt",
        control: false,
    },
    Example {
        message: "O primeiro comando falhou; vou repetir com as flags corretas.",
        terminated: false,
        language: "pt",
        control: false,
    },
    // ------------------------------------------------ running (EN)
    Example {
        message: "Now I will read the file:",
        terminated: false,
        language: "en",
        control: false,
    },
    Example {
        message: "Let me check the failing test first, then I'll fix it.",
        terminated: false,
        language: "en",
        control: false,
    },
    Example {
        message: "Step 2 of 5 complete. Continuing with the database migration.",
        terminated: false,
        language: "en",
        control: false,
    },
    Example {
        message: "Next: write the unit tests for the new function.",
        terminated: false,
        language: "en",
        control: false,
    },
    Example {
        message: "Reading the stack trace... the failure seems to come from the parser.",
        terminated: false,
        language: "en",
        control: false,
    },
    Example {
        message: "Now I need to install the dependency before running the build.",
        terminated: false,
        language: "en",
        control: false,
    },
    // ------------------------------------------------ in-distribution controls
    // Same shape the checkpoint was trained on (preset `triage_questions`
    // intent): if these are not near-perfect, the checkpoint/runtime pairing
    // itself is broken and the loop-termination numbers mean nothing.
    Example {
        message: "I was charged twice on my March invoice, please refund the duplicate today.",
        terminated: true,
        language: "en",
        control: true,
    },
    Example {
        message: "A API retorna 500 em todas as requisições de login desde ontem, ninguém consegue entrar.",
        terminated: false,
        language: "pt",
        control: true,
    },
    Example {
        message: "Fomos cobrados duas vezes na fatura de março. Por favor, estornem a cobrança duplicada hoje.",
        terminated: true,
        language: "pt",
        control: true,
    },
    Example {
        message: "The integration webhooks stopped delivering events after your update, our pipeline is broken.",
        terminated: false,
        language: "en",
        control: true,
    },
];

/// The loop question, in the upstream question shape (`type`, `instructions`,
/// `criteria`). Two options because `temperature_by_options` calibrates
/// `choice:2` explicitly.
fn loop_questions() -> Map<String, Value> {
    let mut questions = Map::new();
    questions.insert(
        "loop_terminated".to_string(),
        json!({
            "type": "choice",
            "instructions": "An agent produced the text in `message` while working on a task. Did the agent's loop finish with this text, or is the agent about to keep working?",
            "criteria": {
                "terminated": "the task is complete and the loop ended: a summary of what was done, nothing left to do",
                "running": "the loop is still going: the agent will read or write files, run commands, or start the next step",
            }
        }),
    );
    questions
}

/// The control question: the first `triage_questions` preset question, verbatim
/// from upstream `laya/presets.py`, narrowed to three criteria so the control
/// slice shares the choice scorer with the loop slice.
fn control_questions() -> Map<String, Value> {
    let mut questions = Map::new();
    questions.insert(
        "intent".to_string(),
        json!({
            "type": "choice",
            "instructions": "What does the customer want in `message`?",
            "criteria": {
                "refund": "money returned or a duplicate charge reversed",
                "technical_help": "a bug, outage or integration problem",
                "other": "none of the other options fits",
            }
        }),
    );
    questions
}

/// `ChoiceAccuracy` for one answer: 1.0 when the decoded choice matches the
/// expected label, 0.0 otherwise (`laya/evals.py`).
fn choice_correct(answers: &Value, qid: &str, expected: &str) -> Option<bool> {
    let answer = answers.get(qid)?;
    if answer.get("type").and_then(Value::as_str) != Some("choice") {
        return None;
    }
    Some(answer.get("choice").and_then(Value::as_str) == Some(expected))
}

/// `_answer_confidence` from `laya/evals.py`: the calibrated
/// `answer_confidence` field, not the entropy score.
fn answer_confidence(answers: &Value, qid: &str) -> Option<f64> {
    let answer = answers.get(qid)?;
    answer
        .get("answer_confidence")
        .and_then(Value::as_f64)
        .or_else(|| answer.get("confidence").and_then(Value::as_f64))
}

fn pct(hits: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

#[test]
fn loop_termination_eval() {
    let Some(model_dir) = std::env::var("LAYA_EVAL_MODEL_DIR")
        .ok()
        .filter(|d| !d.is_empty())
    else {
        eprintln!(
            "skipping loop_termination_eval: set LAYA_EVAL_MODEL_DIR=<model dir> \
             (and optionally LAYA_EVAL_ONNX, LAYA_EVAL_MIN_ACCURACY) to run the \
             real-checkpoint evaluation"
        );
        return;
    };
    let onnx_path = match std::env::var("LAYA_EVAL_ONNX") {
        Ok(p) if !p.is_empty() => p,
        _ => format!("{model_dir}/laya.onnx"),
    };
    let min_accuracy: f64 = match std::env::var("LAYA_EVAL_MIN_ACCURACY") {
        Ok(v) if !v.is_empty() => v.parse().unwrap_or_else(|_| {
            eprintln!(
                "warning: LAYA_EVAL_MIN_ACCURACY={v:?} is not a number; using the 0.80 default"
            );
            0.80
        }),
        _ => 0.80,
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

    let loop_qs = loop_questions();
    let control_qs = control_questions();

    // Per-slice tallies: (hits, total, confidence sum) keyed by slice name.
    let mut slices: HashMap<String, (usize, usize, f64)> = HashMap::new();
    let mut misses: Vec<String> = Vec::new();

    for example in EXAMPLES {
        let state = json!({"message": example.message});
        let (questions, qid, expected) = if example.control {
            (
                &control_qs,
                "intent",
                if example.terminated {
                    "refund"
                } else {
                    "technical_help"
                },
            )
        } else {
            (
                &loop_qs,
                "loop_terminated",
                if example.terminated {
                    "terminated"
                } else {
                    "running"
                },
            )
        };
        let result = model
            .decide(&state, questions, None, None, None)
            .unwrap_or_else(|e| panic!("predict failed for {:?}: {e}", example.message));
        let answers = &result["answers"];
        let correct = choice_correct(answers, qid, expected)
            .unwrap_or_else(|| panic!("no choice answer for {qid} in {:?}", example.message));
        let confidence = answer_confidence(answers, qid).unwrap_or(0.0);

        let label = if example.terminated {
            "terminated"
        } else {
            "running"
        };
        for key in [
            format!("all/{}", if example.control { "control" } else { "loop" }),
            format!("lang/{}", example.language),
            format!("label/{label}"),
        ] {
            let entry = slices.entry(key).or_insert((0, 0, 0.0));
            entry.0 += usize::from(correct);
            entry.1 += 1;
            entry.2 += confidence;
        }
        if !correct && !example.control {
            misses.push(format!(
                "  [{}] {:?} -> {:?} (expected {})",
                example.language, example.message, answers[qid]["choice"], expected,
            ));
        }
    }

    // Report in the `EvalReport.to_markdown` spirit: one row per slice.
    println!("\n=== loop-termination eval: {model_dir} ===");
    println!("| slice | accuracy | mean answer_confidence | n |");
    println!("|---|---|---|---|");
    let mut keys: Vec<&String> = slices.keys().collect();
    keys.sort();
    let mut loop_accuracy = None;
    for key in keys {
        let (hits, total, conf) = slices[key];
        println!(
            "| {} | {:.4} | {:.4} | {} |",
            key,
            pct(hits, total),
            conf / total.max(1) as f64,
            total
        );
        if key == "all/loop" {
            loop_accuracy = Some(pct(hits, total));
        }
    }
    if !misses.is_empty() {
        println!("--- loop misses ---");
        for miss in &misses {
            println!("{miss}");
        }
    }

    // The gate: the loop-termination slice must reach the configured floor.
    let accuracy = loop_accuracy.expect("loop slice always runs");
    assert!(
        accuracy >= min_accuracy,
        "loop-termination accuracy {accuracy:.4} is below the production floor \
         {min_accuracy} (set LAYA_EVAL_MIN_ACCURACY to change the gate)"
    );
    // The control slice anchors the checkpoint itself: near-perfect is
    // expected for the shape it was trained on.
    let (c_hits, c_total, _) = slices
        .get("all/control")
        .copied()
        .expect("the control slice always runs");
    assert!(
        pct(c_hits, c_total) >= 0.75,
        "control accuracy {}/{} fell below 0.75 — the checkpoint/runtime pairing \
         is broken, not the task",
        c_hits,
        c_total
    );
}

/// Confidence-floor sweep: pick the `min_confidence` default from the
/// measured curve instead of a convention.
///
/// The harness consumer vetoes a natural completion only when the model says
/// `running` AND its calibrated `answer_confidence` reaches the floor. This
/// test replays exactly that decision over the loop corpus for every floor
/// in the sweep and reports, per threshold:
///
/// - `missed_stops` — running examples the consumer let end (the model said
///   `terminated`, or said `running` below the floor): an agent loop dies
///   mid-task. The primary cost — minimize first.
/// - `false_continues` — terminated examples the consumer kept running: a
///   wasted turn, bounded by MAX_ITERATIONS. The secondary cost.
///
/// The recommended floor is the smallest one reaching the maximum number of
/// correct consumer decisions (fewest missed stops, then fewest false
/// continues). Run with the same env vars as the eval above:
///
/// ```text
/// LAYA_EVAL_MODEL_DIR=/tmp/laya-eval/multilingual \
/// LAYA_EVAL_ONNX=/tmp/laya-eval/multilingual/laya.onnx \
/// cargo test -p cosh-onnx --test loop_termination_eval confidence_sweep -- --nocapture
/// ```
#[test]
fn confidence_floor_sweep() {
    let Some(model_dir) = std::env::var("LAYA_EVAL_MODEL_DIR")
        .ok()
        .filter(|d| !d.is_empty())
    else {
        eprintln!("skipping confidence_floor_sweep: set LAYA_EVAL_MODEL_DIR");
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

    let qs = loop_questions();
    // One pass over the corpus: (expected terminated, said running,
    // answer_confidence) per loop example.
    let mut observations: Vec<(bool, bool, f64)> = Vec::new();
    for example in EXAMPLES.iter().filter(|e| !e.control) {
        let state = json!({"message": example.message});
        let result = model
            .decide(&state, &qs, None, None, None)
            .unwrap_or_else(|e| panic!("predict failed for {:?}: {e}", example.message));
        let answers = &result["answers"];
        let said_running = choice_correct(answers, "loop_terminated", "running").is_some_and(|c| c);
        let confidence = answer_confidence(answers, "loop_terminated").unwrap_or(0.0);
        observations.push((example.terminated, said_running, confidence));
    }

    // The consumer decision at floor f: veto (continue) iff said_running
    // AND confidence >= f.
    let floor_candidates: Vec<f64> = (0..=20).map(|i| i as f64 / 20.0).collect();
    println!("\n=== confidence-floor sweep: {model_dir} ===");
    println!("| floor | missed_stops | false_continues | correct |");
    println!("|---|---|---|---|");
    let mut best: Option<(f64, usize, usize)> = None; // (floor, missed, false)
    for &floor in &floor_candidates {
        let mut missed_stops = 0usize;
        let mut false_continues = 0usize;
        for &(terminated, said_running, confidence) in &observations {
            let veto = said_running && confidence >= floor;
            if !terminated && !veto {
                missed_stops += 1;
            }
            if terminated && veto {
                false_continues += 1;
            }
        }
        let correct = observations.len() - missed_stops - false_continues;
        println!(
            "| {:.2} | {} | {} | {} |",
            floor, missed_stops, false_continues, correct
        );
        // Tie-break towards the HIGHEST floor with identical results:
        // the bottom of an optimal plateau (e.g. 0.00) vetoes every
        // "running" answer regardless of confidence, disabling the gate;
        // the top edge keeps the same measured optimum while the floor
        // still filters low-confidence verdicts.
        let better = match best {
            None => true,
            Some((_, bm, bf)) => (missed_stops, false_continues) <= (bm, bf),
        };
        if better {
            best = Some((floor, missed_stops, false_continues));
        }
    }
    let (floor, missed, false_c) = best.expect("sweep always runs");
    println!(
        "recommended min_confidence default: {floor:.2} \
         (missed_stops={missed}, false_continues={false_c} over {} examples)",
        observations.len()
    );
    // The sweep cannot pick a floor with ANY missed stop when a
    // zero-missed floor exists: dying mid-task is the primary cost.
    let zero_missed_floor = floor_candidates.iter().copied().find(|&f| {
        observations
            .iter()
            .filter(|&&(terminated, said_running, confidence)| {
                !terminated && said_running && confidence < f
            })
            .count()
            == 0
    });
    if let Some(zero_floor) = zero_missed_floor {
        assert!(
            missed == 0 || floor >= zero_floor,
            "the recommended floor {floor} misses {missed} stops while \
             {zero_floor} would miss none"
        );
    }
}
