//! Parity benchmark, Rust side.
//!
//! Drives the crate facade ([`cosh_onnx::load`] -> `DecisionModel::decide`)
//! over the shared seed-generated dataset and records per-case latency with
//! a monotonic clock. The Python twin (`/tmp/cosh-bench/driver_py.py`) does
//! the same over the upstream `ONNXAgent`; `report.py` joins the two JSONL
//! streams. Nothing here generates cases and nothing outside the `decide`
//! span is timed -- the honesty rules (same ORT version, same graph files,
//! warmup, interleaved rounds, I/O outside the clock) live in the run
//! script and the module docs of the drivers.
//!
//! Env:
//!   PARITY_MODEL_DIR  checkpoint dir (default /tmp/laya-eval/english)
//!   PARITY_ONNX       graph path    (default `<dir>/laya.onnx`)
//!   PARITY_DATASET    dataset path  (default /tmp/cosh-bench/dataset.jsonl)
//!   PARITY_OUT        output JSONL  (default /tmp/cosh-bench/results_rust.jsonl)
//!
//! Args: `--round N` (default 1), `--limit K` (smoke runs: only the first K
//! cases are timed). Rounds append to the output file; `report.py` pairs
//! records by `(id, round)`.

use std::io::{BufRead, Write};
use std::time::Instant;

use serde_json::{Value, json};

use cosh_onnx::{LoadOptions, ModelKind, load};

fn arg(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let round: usize = arg("--round").and_then(|v| v.parse().ok()).unwrap_or(1);
    let limit: Option<usize> = arg("--limit").and_then(|v| v.parse().ok());

    let model_dir =
        std::env::var("PARITY_MODEL_DIR").unwrap_or_else(|_| "/tmp/laya-eval/english".into());
    let onnx_path =
        std::env::var("PARITY_ONNX").unwrap_or_else(|_| format!("{model_dir}/laya.onnx"));
    let dataset =
        std::env::var("PARITY_DATASET").unwrap_or_else(|_| "/tmp/cosh-bench/dataset.jsonl".into());
    let out =
        std::env::var("PARITY_OUT").unwrap_or_else(|_| "/tmp/cosh-bench/results_rust.jsonl".into());

    // Model load: outside the clock.
    let model = load(
        ModelKind::Custom {
            repo: model_dir.clone(),
            subfolder: None,
        },
        &LoadOptions {
            onnx_path: Some(onnx_path.clone()),
            ..LoadOptions::default()
        },
    )?;

    let file = std::fs::File::open(&dataset)?;
    let cases: Vec<Value> = std::io::BufReader::new(file)
        .lines()
        .map(|line| -> Result<Value, Box<dyn std::error::Error>> {
            let line = line?;
            Ok(serde_json::from_str(&line)?)
        })
        .collect::<Result<_, _>>()?;
    let timed = limit.unwrap_or(cases.len());

    // Warmup: five untimed forwards (ORT lazy init, allocator arenas,
    // tokenizer cache). The same five cases are timed again below, as they
    // are on the Python side.
    for case in cases.iter().take(5) {
        let state = &case["state"];
        let questions = case["questions"].as_object().expect("questions map");
        let lang = case["lang"].as_str();
        model.decide(state, questions, lang, None, None)?;
    }

    let mut records: Vec<String> = Vec::with_capacity(timed);
    for case in cases.iter().take(timed) {
        let state = &case["state"];
        let questions = case["questions"].as_object().expect("questions map");
        let lang = case["lang"].as_str();

        let start = Instant::now();
        let result = model.decide(state, questions, lang, None, None)?;
        let elapsed = start.elapsed();

        let answers = result.get("answers").and_then(Value::as_object);
        let loop_answer = answers.and_then(|a| a.get("loop_terminated"));
        let urgency_answer = answers.and_then(|a| a.get("urgency"));
        let record = json!({
            "id": case["id"],
            "lang": case["lang"],
            "round": round,
            "elapsed_ns": elapsed.as_nanos() as u64,
            "choice": loop_answer.and_then(|l| l.get("choice")).and_then(Value::as_str),
            "urgency": urgency_answer.and_then(|u| u.get("score")).and_then(Value::as_f64),
            "conf": loop_answer.and_then(|l| l.get("confidence")).and_then(Value::as_f64),
            "answer_conf": loop_answer.and_then(|l| l.get("answer_confidence")).and_then(Value::as_f64),
            "model": result.get("model").and_then(Value::as_str),
        });
        records.push(record.to_string());
    }

    // I/O outside the clock: one append pass after the timed loop.
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)?;
    for record in &records {
        writeln!(fh, "{record}")?;
    }
    eprintln!(
        "parity(rust): round {round}: {} cases timed -> {out}",
        records.len()
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("parity(rust): {error}");
        std::process::exit(1);
    }
}
