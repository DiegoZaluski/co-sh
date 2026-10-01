//! The facade is implementable and callable from outside the crate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use cosh_onnx::decision::model::{DecisionModel, LoadOptions, ModelKind};
use cosh_onnx::error::{Error, Result};
use cosh_onnx::load;

/// An out-of-crate backend: records the questions it was handed and answers
/// the first choice with a fixed label. Pins the trait contract a second
/// backend would have to satisfy.
#[derive(Default)]
struct ExternalModel {
    calls: Mutex<Vec<Map<String, Value>>>,
}

impl DecisionModel for ExternalModel {
    fn model_id(&self) -> &str {
        "external/test-model"
    }

    fn revision(&self) -> Option<String> {
        None
    }

    fn decide(
        &self,
        _state: &Value,
        questions: &Map<String, Value>,
        _lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Value> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(questions.clone());
        let mut answers = Map::new();
        for qid in questions.keys() {
            answers.insert(qid.clone(), json!({"type": "choice", "choice": "external"}));
        }
        Ok(json!({"model": "external", "answers": answers}))
    }
}

#[test]
fn a_second_backend_can_implement_the_trait() {
    let model: Arc<dyn DecisionModel> = Arc::new(ExternalModel::default());
    let questions =
        json!({"dept": {"type": "choice", "instructions": "Which?", "criteria": {"a": null}}})
            .as_object()
            .unwrap()
            .clone();
    let out = model
        .decide(&json!("state"), &questions, None, None, None)
        .unwrap();
    assert_eq!(out["answers"]["dept"]["choice"], json!("external"));
    assert_eq!(model.model_id(), "external/test-model");
    assert_eq!(model.revision(), None);
}

#[test]
fn the_default_predict_batch_keeps_input_order() {
    let model: Arc<dyn DecisionModel> = Arc::new(ExternalModel::default());
    let questions =
        json!({"dept": {"type": "choice", "instructions": "Which?", "criteria": {"a": null}}})
            .as_object()
            .unwrap()
            .clone();
    let states = vec![json!("one"), json!("two"), json!("three")];
    let results = model
        .predict_batch(&states, &questions, None, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 3);
    for (i, result) in results.iter().enumerate() {
        assert_eq!(
            result["answers"]["dept"]["choice"],
            json!("external"),
            "results[{i}] answers"
        );
    }
}

#[test]
fn load_options_defaults_carry_the_upstream_signature_defaults() {
    // The struct default must stand in for the upstream keyword defaults:
    // no graph override, no revision, no digests, no hooks, raise on error,
    // concurrent hooks, no timeout.
    let options = LoadOptions::default();
    assert!(options.onnx_path.is_none());
    assert!(options.revision.is_none());
    assert!(options.expected_sha256.is_none());
    assert!(options.lang_temperatures.is_none());
    assert!(options.hooks.is_empty());
    assert!(options.on_predict_start.is_none());
    assert!(options.on_predict_end.is_none());
    assert_eq!(options.hooks_raise, None);
    assert_eq!(options.hooks_concurrent, None);
    assert_eq!(options.hooks_timeout, None);
}

#[test]
fn load_of_a_missing_local_directory_names_the_path() {
    // Upstream: "Local model path not found: '/no/such/dir'." A local path
    // (absolute) that does not exist fails before any Hub lookup.
    let err = load(
        ModelKind::Custom {
            repo: "/no/such/checkpoint/dir".to_string(),
            subfolder: None,
        },
        &LoadOptions::default(),
    )
    .err()
    .expect("a missing local path is an error");
    match &err {
        Error::Runtime(msg) => {
            assert_eq!(
                msg,
                "Local model path not found: '/no/such/checkpoint/dir'."
            );
        }
        other => panic!("expected Error::Runtime, got {other:?}"),
    }
}

#[test]
fn model_kind_name_and_hash_map_options_stay_backend_agnostic() {
    // `Custom` carries a plain repo id / local path, and an optional digest
    // map flows through `LoadOptions` untouched.
    let kind = ModelKind::Custom {
        repo: "./checkpoints/laya".to_string(),
        subfolder: Some("multilingual".to_string()),
    };
    assert_eq!(kind.name(), None);
    assert_eq!(
        kind,
        ModelKind::Custom {
            repo: "./checkpoints/laya".to_string(),
            subfolder: Some("multilingual".to_string()),
        }
    );
    let mut digests = HashMap::new();
    digests.insert("laya.onnx".to_string(), "deadbeef".to_string());
    let options = LoadOptions {
        expected_sha256: Some(digests),
        ..LoadOptions::default()
    };
    assert!(options.expected_sha256.is_some());
}
