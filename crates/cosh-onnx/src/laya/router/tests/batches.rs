//! Batch prediction: route_batch / predict_batch / predict_many grouping,
//! caching, hooks-timeout forwarding, lang forwarding and concurrent batch
//! load deduplication.

use super::*;

/// One recorded agent call: `(checkpoint, states, questions, batch_size)`
/// upstream, plus the forwarded `lang` the lang-recording suite reads.
#[derive(Clone)]
struct BatchCall {
    checkpoint: String,
    states: Vec<Value>,
    questions: Map<String, Value>,
    batch_size: Option<usize>,
}

/// The `fake_agent` fixture's Agent: records every batch call and answers
/// `{"model": "laya-rl-agent", "answers": {"seen": state}, "usage": {}}` per
/// state; a state of `"raise"` fails the call.
struct FakeBatchAgent {
    checkpoint: String,
    calls: Arc<Mutex<Vec<BatchCall>>>,
}

impl AgentLike for FakeBatchAgent {
    fn predict_batch(
        &self,
        states: &[Value],
        questions: &Map<String, Value>,
        batch_size: Option<usize>,
        _lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Vec<Value>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(BatchCall {
                checkpoint: self.checkpoint.clone(),
                states: states.to_vec(),
                questions: questions.clone(),
                batch_size,
            });
        if states.iter().any(|s| s == &json!("raise")) {
            return Err(Error::Runtime("inference failed".to_string()));
        }
        Ok(states
            .iter()
            .map(|s| json!({"model": "laya-rl-agent", "answers": {"seen": s}, "usage": {}}))
            .collect())
    }

    fn system_one(
        &self,
        state: &Value,
        questions: &Map<String, Value>,
        lang: Option<&str>,
        max_len: Option<usize>,
        head_max_len: Option<usize>,
    ) -> Result<Value> {
        Ok(self
            .predict_batch(
                std::slice::from_ref(state),
                questions,
                None,
                lang,
                max_len,
                head_max_len,
            )?
            .remove(0))
    }
}

/// The fixture: a Router whose factory records builds and installs
/// [`FakeBatchAgent`]s (upstream `monkeypatch.setattr(laya.agent, "Agent", ...)`).
struct BatchFixture {
    router: Router,
    built: Arc<Mutex<Vec<String>>>,
    calls: Arc<Mutex<Vec<BatchCall>>>,
}

impl BatchFixture {
    fn new(opts: RouterOptions) -> Self {
        let built: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let calls: Arc<Mutex<Vec<BatchCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_built = Arc::clone(&built);
        let recorded_calls = Arc::clone(&calls);
        let router = Router::configure(opts).expect("router").with_agent_factory(
            move |_repo, subfolder, _revision| {
                let checkpoint = subfolder.unwrap_or("english").to_string();
                recorded_built
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(checkpoint.clone());
                Ok(Box::new(FakeBatchAgent {
                    checkpoint,
                    calls: Arc::clone(&recorded_calls),
                }) as Box<dyn AgentLike>)
            },
        );
        Self {
            router,
            built,
            calls,
        }
    }

    fn builds(&self) -> Vec<String> {
        self.built.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn calls_snapshot(&self) -> Vec<BatchCall> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

#[test]
fn mixed_groups_keep_order_and_lru() {
    for (capacity, expected_built, expected_loaded) in [
        (
            1usize,
            strings(&["english", "multilingual", "typed-decisions"]),
            strings(&["typed-decisions"]),
        ),
        (
            2,
            strings(&["english", "multilingual", "typed-decisions"]),
            strings(&["multilingual", "typed-decisions"]),
        ),
        (
            3,
            strings(&["english", "multilingual", "typed-decisions"]),
            strings(&["english", "multilingual", "typed-decisions"]),
        ),
    ] {
        let fx = BatchFixture::new(RouterOptions {
            max_loaded: Some(capacity),
            auto_task_detection: true,
            ..Default::default()
        });
        let typed = q_td_auto();
        let items = vec![
            request("English text one"),
            request("مرحبا"),
            request("English text two"),
            json!({"state": "decision", "questions": Value::Object(typed.clone())}),
            request_with("forced", &[("model", json!("ml"))]),
            request_with("forced english", &[("lang", json!("en"))]),
            request_with("explicit task", &[("task", json!("typed_decisions"))]),
        ];
        let decisions = fx.router.route_batch(&items).expect("route_batch");
        assert!(fx.router.loaded().is_empty(), "route_batch loads nothing");
        let results = fx
            .router
            .predict_batch(&items, None, None)
            .expect("predict_batch");
        for (result, item) in results.iter().zip(&items) {
            assert_eq!(result["answers"]["seen"], item["state"], "seen {capacity}");
        }
        for (result, decision) in results.iter().zip(&decisions) {
            assert_eq!(result["routing"], decision.to_value(), "routing {capacity}");
        }
        assert_eq!(fx.builds(), expected_built, "built {capacity}");
        assert_eq!(fx.router.loaded(), expected_loaded, "loaded {capacity}");
        let observed: Vec<(String, Vec<Value>)> = fx
            .calls_snapshot()
            .into_iter()
            .map(|c| (c.checkpoint, c.states))
            .collect();
        assert_eq!(
            observed,
            vec![
                (
                    "english".to_string(),
                    vec![
                        json!("English text one"),
                        json!("English text two"),
                        json!("forced english")
                    ]
                ),
                (
                    "multilingual".to_string(),
                    vec![json!("مرحبا"), json!("forced")]
                ),
                ("typed-decisions".to_string(), vec![json!("decision")]),
                ("typed-decisions".to_string(), vec![json!("explicit task")]),
            ],
            "calls {capacity}"
        );
        assert!(
            fx.router
                .predict_batch(&[], None, None)
                .expect("empty")
                .is_empty(),
            "predict_many([])"
        );
        assert!(
            fx.router.route_batch(&[]).expect("empty").is_empty(),
            "route_batch(())"
        );
    }
}

#[test]
fn invalid_batches_fail_before_loading() {
    // (items, message fragment). The non-sequence request containers upstream
    // also covers (None / {} / "text" with "requests must be a sequence")
    // have no slice analogue; see the module docs.
    let cases: Vec<(Value, &str)> = vec![
        (json!([Value::Null]), "request 0"),
        (
            json!([{"questions": Value::Object(q_batch())}]),
            "request 0 is missing required key 'state'",
        ),
        (
            json!([{"state": "x"}]),
            "request 0 is missing required key 'questions'",
        ),
        (
            json!([request("x"), {"state": "y", "questions": Value::Null}]),
            "request 1 'questions'",
        ),
        (
            json!([
                request("x"),
                request_with("y", &[("model", json!("invalid"))])
            ]),
            "unknown model",
        ),
    ];
    for (items, fragment) in cases {
        let items: Vec<Value> = items.as_array().expect("array").clone();
        let fx = BatchFixture::new(RouterOptions::default());
        let err = fx.router.predict_batch(&items, None, None).unwrap_err();
        assert!(
            matches!(err, Error::Value(_)),
            "error kind for {fragment}: {err:?}"
        );
        assert!(
            err.to_string().contains(fragment),
            "message for {fragment}: {err}"
        );
        assert!(fx.builds().is_empty(), "nothing built for {fragment}");
        assert!(
            fx.router.loaded().is_empty(),
            "nothing loaded for {fragment}"
        );
    }
}

#[test]
fn inference_exception_propagates_and_cache_remains_consistent() {
    let fx = BatchFixture::new(RouterOptions {
        max_loaded: Some(1),
        ..Default::default()
    });
    let items = vec![
        request("first"),
        request_with("raise", &[("lang", json!("ar"))]),
        request_with("unreached", &[("lang", json!("ar"))]),
    ];
    let err = fx.router.predict_batch(&items, None, None).unwrap_err();
    assert!(
        err.to_string().contains("inference failed"),
        "propagates: {err:?}"
    );
    assert_eq!(fx.builds(), strings(&["english", "multilingual"]));
    let observed: Vec<(String, Vec<Value>)> = fx
        .calls_snapshot()
        .into_iter()
        .map(|c| (c.checkpoint, c.states))
        .collect();
    assert_eq!(
        observed,
        vec![
            ("english".to_string(), vec![json!("first")]),
            (
                "multilingual".to_string(),
                vec![json!("raise"), json!("unreached")]
            ),
        ]
    );
    assert_eq!(fx.router.loaded(), strings(&["multilingual"]));
    // `list(router._agents) == router.loaded`: the two lifecycle views agree.
    let residents: Vec<String> = {
        fx.router
            .lock_lifecycle()
            .agents
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    };
    assert_eq!(residents, fx.router.loaded());
    let out = fx
        .router
        .predict(
            &json!("after failure"),
            &q_batch(),
            &PredictOptions {
                lang: Some("ar"),
                ..Default::default()
            },
        )
        .expect("predict after failure");
    assert_eq!(out["answers"]["seen"], json!("after failure"));
}

#[test]
fn warm_cache_and_repeated_batches() {
    let fx = BatchFixture::new(RouterOptions {
        max_loaded: Some(2),
        ..Default::default()
    });
    fx.router.load("multilingual").expect("load");
    let result = fx
        .router
        .predict_batch(
            &[
                request_with("en", &[("model", json!("english"))]),
                request_with("ar", &[("model", json!("multilingual"))]),
                request_with("en again", &[("model", json!("english"))]),
            ],
            None,
            None,
        )
        .expect("predict_batch");
    let seen: Vec<Value> = result
        .iter()
        .map(|r| r["answers"]["seen"].clone())
        .collect();
    assert_eq!(seen, vec![json!("en"), json!("ar"), json!("en again")]);
    assert_eq!(fx.builds(), strings(&["multilingual", "english"]));
    assert_eq!(fx.router.loaded(), strings(&["english", "multilingual"]));
    fx.router
        .predict_batch(
            &[
                request_with("ar", &[("model", json!("multilingual"))]),
                request_with("en", &[("model", json!("english"))]),
            ],
            None,
            None,
        )
        .expect("predict_batch");
    assert_eq!(fx.builds(), strings(&["multilingual", "english"]));
}

#[test]
fn same_checkpoint_same_questions_uses_one_agent_batch() {
    let fx = BatchFixture::new(RouterOptions {
        max_loaded: Some(2),
        ..Default::default()
    });
    let items = vec![
        request_with("one", &[("model", json!("english"))]),
        request_with("two", &[("model", json!("english"))]),
        request_with("three", &[("model", json!("english"))]),
    ];
    let results = fx
        .router
        .predict_batch(&items, Some(2), None)
        .expect("predict_batch");
    let seen: Vec<Value> = results
        .iter()
        .map(|r| r["answers"]["seen"].clone())
        .collect();
    assert_eq!(seen, vec![json!("one"), json!("two"), json!("three")]);
    let calls = fx.calls_snapshot();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].checkpoint, "english");
    assert_eq!(
        calls[0].states,
        vec![json!("one"), json!("two"), json!("three")]
    );
    assert_eq!(calls[0].questions, q_batch());
    assert_eq!(calls[0].batch_size, Some(2));
}

#[test]
fn same_checkpoint_different_questions_split_agent_batches() {
    let fx = BatchFixture::new(RouterOptions {
        max_loaded: Some(2),
        ..Default::default()
    });
    let q2 = json!({"risk": {"type": "noul", "instructions": "Risky?"}})
        .as_object()
        .unwrap()
        .clone();
    let items = vec![
        json!({"state": "one", "questions": Value::Object(q_batch()), "model": "english"}),
        json!({"state": "two", "questions": Value::Object(q2.clone()), "model": "english"}),
        json!({"state": "three", "questions": Value::Object(q_batch()), "model": "english"}),
    ];
    let results = fx
        .router
        .predict_batch(&items, None, None)
        .expect("predict_batch");
    let seen: Vec<Value> = results
        .iter()
        .map(|r| r["answers"]["seen"].clone())
        .collect();
    assert_eq!(seen, vec![json!("one"), json!("two"), json!("three")]);
    let calls = fx.calls_snapshot();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].checkpoint, "english");
    assert_eq!(calls[0].states, vec![json!("one"), json!("three")]);
    assert_eq!(calls[0].questions, q_batch());
    assert_eq!(calls[1].checkpoint, "english");
    assert_eq!(calls[1].states, vec![json!("two")]);
    assert_eq!(calls[1].questions, q2);
}

#[test]
fn predict_batch_honours_hooks_timeout() {
    // Upstream runs this against the fake_agent fixture, so no checkpoint is
    // ever built here either.
    use crate::hooks::PredictHook;
    let slow_hook: PredictHook = Arc::new(|_ctx: &mut crate::hooks::PredictContext| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        Ok(())
    });

    let fx = BatchFixture::new(RouterOptions {
        hooks_timeout: Some(0.05),
        on_predict_start: Some(Arc::clone(&slow_hook)),
        ..Default::default()
    });
    let err = fx
        .router
        .predict_batch(&[request("one")], None, None)
        .unwrap_err();
    assert!(
        matches!(err, Error::Timeout(_)),
        "a slow on_predict_start times out: {err:?}"
    );

    // A per-call override wins, exactly as it does on `predict`.
    let fx = BatchFixture::new(RouterOptions {
        hooks_timeout: Some(0.05),
        on_predict_start: Some(slow_hook),
        ..Default::default()
    });
    let results = fx
        .router
        .predict_batch(&[request("one")], None, Some(5.0))
        .expect("override");
    assert_eq!(results.len(), 1);
}

#[test]
fn route_batch_forwards_lang_guess() {
    let fx = BatchFixture::new(RouterOptions::default());
    let decisions = fx
        .router
        .route_batch(&[
            request_with("hola", &[("lang_guess", json!("es"))]),
            request_with("hello", &[("lang_guess", json!("en-US"))]),
        ])
        .expect("route_batch");
    let models: Vec<&str> = decisions.iter().map(|d| d.model.as_str()).collect();
    assert_eq!(models, ["multilingual", "english"]);
    assert!(fx.builds().is_empty(), "route_batch loads nothing");
}

#[test]
fn equal_questions_with_different_option_order_score_separately() {
    // A question schema that arrives with a different key order must be
    // scored with its own order. Dict equality ignores insertion order, but
    // options are positional in the rendered sequence, so grouping
    // reordered-but-equal schemas would make the second request's batched
    // answers differ from its single-request answers.
    let fx = BatchFixture::new(RouterOptions {
        max_loaded: Some(1),
        default: Some("english".to_string()),
        ..Default::default()
    });
    let ordered = json!({"intent": {"type": "choice", "instructions": "Pick one",
                          "criteria": {"zulu": "last", "alpha": "first"}}});
    let reordered = json!({"intent": {"type": "choice", "instructions": "Pick one",
                            "criteria": {"alpha": "first", "zulu": "last"}}});
    let requests = vec![
        json!({"state": "one", "questions": ordered}),
        json!({"state": "two", "questions": reordered}),
    ];
    let results = fx
        .router
        .predict_batch(&requests, None, None)
        .expect("predict_batch");
    assert_eq!(results.len(), 2);
    // separate agent calls, each carrying its own caller's option order
    let orders: Vec<Vec<String>> = fx
        .calls_snapshot()
        .into_iter()
        .map(|c| {
            c.questions["intent"]["criteria"]
                .as_object()
                .expect("criteria object")
                .keys()
                .cloned()
                .collect()
        })
        .collect();
    assert_eq!(
        orders,
        [vec!["zulu", "alpha"], vec!["alpha", "zulu"]].map(strings2)
    );
}

fn strings2(values: Vec<&str>) -> Vec<String> {
    values.into_iter().map(str::to_string).collect()
}

/// The `_lang_recording_router` fixture of the batch suite: a Router over a
/// fake agent that records the `lang` each call received, with or without
/// per-language temperatures.
type LangCall = (usize, Option<String>);

struct LangRecordingFixture {
    router: Router,
    calls: Arc<Mutex<Vec<LangCall>>>,
}

impl LangRecordingFixture {
    fn new(lang_temperatures: bool) -> Self {
        let calls: Arc<Mutex<Vec<LangCall>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let router = Router::configure(RouterOptions {
            max_loaded: Some(1),
            default: Some("english".to_string()),
            ..Default::default()
        })
        .expect("router")
        .with_agent_factory(move |_repo, _subfolder, _revision| {
            Ok(Box::new(LangRecordingAgent {
                calls: Arc::clone(&recorded),
                lang_temperatures,
            }) as Box<dyn AgentLike>)
        });
        Self { router, calls }
    }

    fn snapshot(&self) -> Vec<LangCall> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The lang-recording agent: `system_one` records `(1, lang)`,
/// `predict_batch` records `(len(states), lang)`.
struct LangRecordingAgent {
    calls: Arc<Mutex<Vec<LangCall>>>,
    lang_temperatures: bool,
}

impl AgentLike for LangRecordingAgent {
    fn predict_batch(
        &self,
        states: &[Value],
        _questions: &Map<String, Value>,
        _batch_size: Option<usize>,
        lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Vec<Value>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((states.len(), lang.map(str::to_string)));
        Ok(states
            .iter()
            .map(|_| json!({"model": "fake", "answers": {}}))
            .collect())
    }

    fn system_one(
        &self,
        _state: &Value,
        _questions: &Map<String, Value>,
        lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Value> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((1, lang.map(str::to_string)));
        Ok(json!({"model": "fake", "answers": {}}))
    }

    fn has_lang_temperatures(&self) -> bool {
        self.lang_temperatures
    }
}

#[test]
fn lang_reaches_the_agent_when_it_carries_lang_temperatures() {
    // `predict` forwards the request's language so per-language temperatures
    // apply; the batch path forwarded only the token budgets, so the same
    // request scored differently depending on which entry point served it.
    // The request is routed as German either way, which is what made the
    // difference hard to see.
    let fx = LangRecordingFixture::new(true);
    fx.router
        .predict_batch(
            &[
                request_with("a", &[("lang", json!("de"))]),
                request_with("b", &[("lang", json!("de"))]),
                request_with("c", &[("lang", json!("fr"))]),
            ],
            None,
            None,
        )
        .expect("predict_batch");
    let langs: Vec<Option<String>> = fx.snapshot().into_iter().map(|(_, lang)| lang).collect();
    let mut sorted = langs.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [Some("de".to_string()), Some("fr".to_string())],
        "sorted(c[\"lang\"] for c in calls)"
    );
    // requests sharing a language still share one forward pass
    assert!(
        fx.snapshot().contains(&(2, Some("de".to_string()))),
        "{{\"n\": 2, \"lang\": \"de\"}} in calls"
    );
    assert!(
        fx.snapshot().contains(&(1, Some("fr".to_string()))),
        "{{\"n\": 1, \"lang\": \"fr\"}} in calls"
    );
}

#[test]
fn lang_is_not_added_to_the_group_key_without_lang_temperatures() {
    // An agent with no per-language temperatures does not use `lang`, so
    // naming it must not split a group that shares one forward pass today.
    let fx = LangRecordingFixture::new(false);
    fx.router
        .predict_batch(
            &[
                request_with("a", &[("model", json!("english")), ("lang", json!("de"))]),
                request_with("b", &[("model", json!("english")), ("lang", json!("fr"))]),
                request_with("c", &[("model", json!("english"))]),
            ],
            None,
            None,
        )
        .expect("predict_batch");
    assert_eq!(fx.snapshot(), [(3, None)], "one group, no lang");
}

#[test]
fn predict_and_predict_batch_pass_the_same_lang() {
    // The invariant that was violated: one request, either entry point, same
    // language.
    let fx = LangRecordingFixture::new(true);
    fx.router
        .predict(
            &json!("a"),
            &q_batch(),
            &PredictOptions {
                model: Some("english"),
                lang: Some("de"),
                ..Default::default()
            },
        )
        .expect("predict");
    let via_predict = fx.snapshot().last().expect("call").1.clone();
    fx.calls.lock().unwrap_or_else(|e| e.into_inner()).clear();
    fx.router
        .predict_batch(
            &[request_with(
                "a",
                &[("model", json!("english")), ("lang", json!("de"))],
            )],
            None,
            None,
        )
        .expect("predict_batch");
    let via_batch = fx.snapshot().last().expect("call").1.clone();
    assert_eq!(via_batch, via_predict, "predict and predict_batch agree");
    assert_eq!(via_predict, Some("de".to_string()));
}

// --------------------------------------- concurrent batch load deduplication
#[test]
fn concurrent_batch_load_deduplicates() {
    // Eight workers race `predict_batch` over the same two checkpoints; the
    // 10 ms construction sleep widens the check-then-build window.
    let built: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&built);
    let router = Router::configure(RouterOptions {
        max_loaded: Some(2),
        ..Default::default()
    })
    .expect("router")
    .with_agent_factory(move |_repo, subfolder, _revision| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(subfolder.unwrap_or("english").to_string());
        Ok(Box::new(FakeBatchAgent {
            checkpoint: subfolder.unwrap_or("english").to_string(),
            calls: Arc::new(Mutex::new(Vec::new())),
        }) as Box<dyn AgentLike>)
    });
    let start = std::sync::Barrier::new(8);
    let results: std::sync::Mutex<Vec<Vec<Value>>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let router = &router;
            let results = &results;
            let start = &start;
            scope.spawn(move || {
                start.wait();
                let out = router
                    .predict_batch(
                        &[
                            request_with("hello", &[("model", json!("english"))]),
                            request_with("مرحبا", &[("model", json!("multilingual"))]),
                        ],
                        None,
                        None,
                    )
                    .expect("predict_batch");
                results.lock().unwrap_or_else(|e| e.into_inner()).push(out);
            });
        }
    });
    let mut builds = built.lock().unwrap_or_else(|e| e.into_inner()).clone();
    builds.sort();
    builds.dedup();
    assert_eq!(builds, strings(&["english", "multilingual"]));
    assert_eq!(router.loaded(), strings(&["english", "multilingual"]));
    for result in results.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        assert_eq!(result[0]["answers"]["seen"], json!("hello"));
        assert_eq!(result[1]["answers"]["seen"], json!("مرحبا"));
    }
}
