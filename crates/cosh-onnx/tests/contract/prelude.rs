//! The prelude resolves every advertised name — `use cosh_onnx::prelude::*;`
//! is the one-import surface.

use serde_json::{json, Map, Value};

use cosh_onnx::prelude::*;

/// Touch each advertised name once; the test passes when it compiles and the
/// values behave as documented. Compile failure here means the prelude and
/// the crate surface have drifted apart.
#[test]
fn the_prelude_resolves_every_advertised_name() {
    // Facade.
    let _: LoadOptions = LoadOptions::default();
    assert_eq!(ModelKind::default(), ModelKind::English);

    // Language helpers (upstream `detect_language` alias).
    let analysis = detect_language(&json!({"body": "Hello there"}));
    assert!(analysis.is_english, "english text analyses as english");
    assert_eq!(detect_script("hello"), "latin");
    assert!(is_english(&json!("hello")));

    // Presets. Each preset carries its own leading key (triage's is
    // `intent`, email's is `category`, guard's `jailbreak`, moderation's
    // `toxic`, router's `difficulty`).
    assert!(triage_questions().contains_key("intent"));
    assert!(email_questions(None).contains_key("category"));
    assert!(guard_questions().contains_key("jailbreak"));
    assert!(moderation_questions().contains_key("toxic"));
    assert!(router_questions().contains_key("difficulty"));

    // Email cleaning.
    assert_eq!(clean_email_body("Hi.\n\nSent from my iPhone", 3000), "Hi.");
    assert_eq!(
        email_state(" s ", "body", None, false, &[]).len(),
        2,
        "subject and body only"
    );

    // Question protocol.
    assert_eq!(QTYPES.len(), 3);
    assert_eq!(
        render_options(&json!({"t": "choice", "ins": "", "crit": {"a": null}})).unwrap(),
        ["a"]
    );
    assert!(to_internal(&json!({"type": "noul", "instructions": "?"})).is_ok());

    // Confidence helpers.
    let probs = [0.75, 0.25];
    assert!((answer_confidence(&probs, 2) - 0.75).abs() < 1e-9);
    assert!(confidence_from_probs(&probs, 2) > 0.0);
    // conf 0.9 against the single correct bin: |0.9 - 1.0| = 0.1.
    assert!((ece_score(&[0.9], &[1.0], 10) - 0.1).abs() < 1e-9);

    // Hooks.
    let _ctx = PredictContext::new(vec![json!("s")], Map::new(), None, None, None);
    let _ph: Option<PredictHook> = None;

    // Shortlist.
    let questions = json!({"dept": {"type": "choice", "instructions": "", "criteria": ["a"]}})
        .as_object()
        .unwrap()
        .clone();
    // k >= n passes through without calling the embedder.
    let out = predict_shortlist(&mut Runner, &json!("q"), &questions, &Stub, 1).unwrap();
    assert!(out["shortlist"]["dept"]["passthrough"] == json!(true));
    let cached = cached_embed_fn(Stub, 4096).unwrap(); // named export present
    drop(cached);

    // Errors.
    let e = Error::Value("x".into());
    let _: Result<()> = Err(e);

    // Schema decisions.
    let result = decide(&mut Runner, &json!("state"), Some(&json!({})), None, false);
    assert!(result.is_err(), "empty schema is an error");
}

/// A minimal [`EmbedFn`] for the prelude resolution check.
struct Stub;

impl EmbedFn for Stub {
    fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f64>>> {
        Ok(Vec::new())
    }
}

/// A minimal `PredictRunner` for `decide`.
struct Runner;

impl cosh_onnx::decision::model::PredictRunner for Runner {
    fn predict(&mut self, _state: &Value, _questions: &Map<String, Value>) -> Result<Value> {
        Ok(json!({}))
    }
}
