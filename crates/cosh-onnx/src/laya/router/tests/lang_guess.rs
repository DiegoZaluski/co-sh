//! Language handling: lang codes that name no language (#359), the
//! caller-supplied lang_guess (#35) and blank-lang abstention.

use super::*;

// ------------------------------------- lang codes that name no language (#359)
// `C`, `POSIX` and `C.UTF-8` are valid `$LANG` values that identify nothing,
// and `C.UTF-8` is the default in the official Python image. Passing one to
// `lang=` used to resolve to "not English" and pin every request to the
// multilingual checkpoint, before detection ever ran. The ISO 639-2 special
// codes say the same thing in the standard's own vocabulary.
#[test]
fn language_agnostic_codes_abstain() {
    let english_state = "Please refund the duplicate charge on invoice 4411";
    for code in [
        "C", "POSIX", "C.UTF-8", "c.utf8", "c", "posix", "und", "zxx", "mul", "UND", "Zxx",
        " und ",
    ] {
        assert_eq!(
            english_from_code(Some(code)),
            None,
            "lang-code/{code:?} abstains at the code level"
        );
        let decision = route(
            &json!(english_state),
            &empty_questions(),
            &RouteOptions { lang: Some(code), ..RouteOptions::default() },
        );
        assert_eq!(
            decision.model, "english",
            "lang-code/{code:?} lets detection name the checkpoint"
        );
        assert!(
            !decision.reason.contains("explicit"),
            "lang-code/{code:?} says the hint was not used"
        );
    }

    // An empty hint is the case this mirrors, so it must still behave the
    // same way.
    assert_eq!(english_from_code(Some("")), None, "lang-code/empty string still abstains");
    assert_eq!(english_from_code(None), None, "lang-code/None still abstains");
    assert_eq!(english_from_code(Some("   ")), None, "lang-code/whitespace still abstains");

    // The change must not touch codes that do name a language: English still
    // routes now, and a non-English code still forces the multilingual
    // checkpoint rather than being second-guessed.
    for code in ["en", "eng", "english", "EN", "en-US", "en_US.UTF-8"] {
        assert_eq!(
            english_from_code(Some(code)),
            Some(true),
            "lang-code/{code:?} is still decisive English"
        );
        let reason = route(
            &json!(english_state),
            &empty_questions(),
            &RouteOptions { lang: Some(code), ..RouteOptions::default() },
        )
        .reason;
        assert_eq!(
            reason.matches("explicit").count(),
            1,
            "lang-code/{code:?} routes without detection"
        );
    }
    for code in ["de", "fr", "zh", "ja", "pt-BR", "de_DE.UTF-8"] {
        assert_eq!(
            english_from_code(Some(code)),
            Some(false),
            "lang-code/{code:?} is still decisive non-English"
        );
        assert_eq!(
            route(
                &json!(english_state),
                &empty_questions(),
                &RouteOptions { lang: Some(code), ..RouteOptions::default() },
            )
            .model,
            "multilingual",
            "lang-code/{code:?} routes to the multilingual checkpoint"
        );
    }

    // A subtag after an agnostic primary is not itself agnostic: the primary
    // subtag is what is compared, so a hypothetical `C-something` also
    // abstains.
    assert_eq!(
        english_from_code(Some("C.UTF-8")),
        None,
        "lang-code/agnostic primary wins over its subtag"
    );
    assert_eq!(
        english_from_code(Some("POSIX-1")),
        None,
        "lang-code/posix with a modifier abstains"
    );
}

// ----------------------------------------------------- lang_guess (#35)
// A caller who already runs a language-identification model can hand routing
// the answer instead of being silently misrouted by the built-in stopword
// heuristic.
#[test]
fn the_caller_supplied_language_hint() {
    // The state the maintainer used on #35: a short Romanian request the
    // heuristic cannot place.
    let romanian = "Care este ora in Tokyo?";
    let generic = json!({"intent": {"type": "choice", "instructions": "x", "criteria": ["a", "b"]}})
        .as_object()
        .unwrap()
        .clone();
    let r0 = Router::new().expect("router");
    let route_with = |lang_guess: Option<LangGuess>,
                      lang: Option<&'static str>,
                      model: Option<&'static str>,
                      task: Option<&'static str>| {
        r0.route(
            &json!(romanian),
            Some(&generic),
            &RouteOptions {
                lang_guess: lang_guess.as_ref(),
                lang,
                model,
                task,
                ..RouteOptions::default()
            },
        )
        .expect("route")
    };

    // the baseline
    assert_eq!(
        route_with(None, None, None, None)
            .detection
            .expect("detection")["language"],
        json!("en"),
        "baseline/Romanian is not identified by the heuristic"
    );
    // pinned, not "either checkpoint": this is the defect the hint exists to
    // fix, so the assertion has to distinguish english from the correct
    // answer to mean anything.
    assert_eq!(
        route_with(None, None, None, None).model,
        "english",
        "baseline/Romanian therefore reaches english"
    );

    // codes
    let code = |value: &'static str| Some(LangGuess::Code(value.to_string()));
    use LangGuess as Lg;
    assert_eq!(
        route_with(code("ro"), None, None, None).model,
        "multilingual",
        "code/Romanian routes multilingual"
    );
    assert_eq!(route_with(code("en"), None, None, None).model, "english", "code/English routes english");
    assert_eq!(
        route_with(code("en_US"), None, None, None).model,
        "english",
        "code/POSIX underscore is read"
    );
    assert_eq!(
        route_with(code("en_US.UTF-8"), None, None, None).model,
        "english",
        "code/POSIX with encoding is read"
    );
    assert_eq!(
        route_with(code("de-DE"), None, None, None).model,
        "multilingual",
        "code/hyphen subtag is read"
    );
    assert_eq!(
        route_with(code("RO"), None, None, None).model,
        "multilingual",
        "code/case is ignored"
    );
    assert_eq!(
        route_with(code("  en  "), None, None, None).model,
        "english",
        "code/whitespace is ignored"
    );
    assert_eq!(
        route_with(code("qq"), None, None, None).model,
        "multilingual",
        "code/unknown code still means non-English"
    );

    // an abstaining hint must fall through to detection, not force a
    // checkpoint
    for empty in [None, Some(""), Some("   ")] {
        let guess = empty.map(|s| Lg::Code(s.to_string()));
        let decision = route_with(guess, None, None, None);
        assert!(
            decision.detection.is_some(),
            "abstain/{empty:?} falls through to detection"
        );
    }

    // callables
    let callable = |f: LangGuessFn| Some(LangGuess::Callable(f));
    assert_eq!(
        route_with(callable(Arc::new(|_| Some("ro".to_string()))), None, None, None).model,
        "multilingual",
        "callable/code is used"
    );
    assert_eq!(
        route_with(
            callable(Arc::new(|s: &Value| {
                if s.as_str().is_some_and(|t| t.contains("Tokyo")) {
                    Some("en".to_string())
                } else {
                    Some("ro".to_string())
                }
            })),
            None,
            None,
            None,
        )
        .model,
        "english",
        "callable/receives the state"
    );
    assert!(
        route_with(callable(Arc::new(|_| None)), None, None, None)
            .detection
            .is_some(),
        "callable/None falls through to detection"
    );
    assert!(
        route_with(callable(Arc::new(|_| Some(String::new()))), None, None, None)
            .detection
            .is_some(),
        "callable/empty string falls through"
    );
    assert_eq!(
        route_with(callable(Arc::new(|_| None)), None, None, None).model,
        route_with(None, None, None, None).model,
        "callable/Romanian model that returns None does not change the default route"
    );

    // installed on the Router
    let r_inst = Router::configure(RouterOptions {
        lang_guess: Some(LangGuess::Code("ro".to_string())),
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_inst.route(&json!(romanian), Some(&generic), &RouteOptions::default())
            .expect("route")
            .model,
        "multilingual",
        "installed/applies without a per-call hint"
    );
    assert_eq!(
        r_inst
            .route(
                &json!(romanian),
                Some(&generic),
                &RouteOptions { lang_guess: code("en").as_ref(), ..RouteOptions::default() },
            )
            .expect("route")
            .model,
        "english",
        "installed/per-call overrides the installed one"
    );
    let r_fn = Router::configure(RouterOptions {
        lang_guess: Some(LangGuess::Callable(Arc::new(|_| Some("ro".to_string())))),
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_fn.route(&json!(romanian), Some(&generic), &RouteOptions::default())
            .expect("route")
            .model,
        "multilingual",
        "installed/callable works too"
    );
    assert!(r0.lang_guess().is_none(), "installed/absent by default");
    assert!(
        Router::configure(RouterOptions {
            lang_guess: Some(LangGuess::Callable(Arc::new(|_| None))),
            ..Default::default()
        })
        .expect("router")
        .route(&json!(romanian), Some(&generic), &RouteOptions::default())
        .expect("route")
        .detection
        .is_some(),
        "installed/a hint that abstains leaves detection intact"
    );

    // precedence
    assert_eq!(
        route_with(code("ro"), None, Some("english"), None).model,
        "english",
        "precedence/explicit model beats the hint"
    );
    assert_eq!(
        route_with(code("ro"), None, None, Some("typed_decisions")).model,
        "typed-decisions",
        "precedence/explicit task beats the hint"
    );
    let explicit = route_with(code("ro"), Some("en"), None, None);
    assert_eq!(explicit.model, "english", "precedence/explicit lang beats the hint");
    assert!(
        explicit.reason.contains("explicit lang"),
        "precedence/an explicit lang is still reported as explicit"
    );

    // the decision payload
    let d = route_with(code("ro"), None, None, None);
    assert_eq!(d.model, "multilingual", "payload/model");
    assert!(d.repo.contains("convaiinnovations/laya"), "payload/repo points at the bundle");
    assert!(
        d.reason.contains("lang_guess"),
        "payload/reason records the caller hint"
    );
    assert!(d.detection.is_none(), "payload/detection is None when the hint decided it");
    assert!(
        Router::configure(RouterOptions {
            lang_guess: Some(LangGuess::Code("ro".to_string())),
            ..Default::default()
        })
        .expect("router")
        .route(&json!(romanian), Some(&generic), &RouteOptions::default())
        .expect("route")
        .reason
        .contains("Router(lang_guess=...)"),
        "payload/installed hint names its source"
    );

    // predict forwards it (stub agent, offline)
    let calls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let r_p = Router::configure(RouterOptions {
        lang_guess: Some(LangGuess::Code("ro".to_string())),
        ..Default::default()
    })
    .expect("router")
    .with_agent_factory(move |_, _, _| {
        Ok(Box::new(RecordingAgent {
            calls: Arc::clone(&recorded),
            lang_temperatures: false,
        }) as Box<dyn AgentLike>)
    });
    let out = r_p
        .predict(&json!(romanian), &generic, &PredictOptions::default())
        .expect("predict");
    assert_eq!(
        out["routing"]["model"], json!("multilingual"),
        "predict/uses the hint"
    );
    assert_eq!(
        r_p
            .predict(
                &json!(romanian),
                &generic,
                &PredictOptions { lang_guess: code("en").as_ref(), ..Default::default() },
            )
            .expect("predict")["routing"]["model"],
        json!("english"),
        "predict/forwards a per-call hint"
    );
    assert_eq!(calls.lock().unwrap_or_else(|e| e.into_inner()).len(), 2, "predict/reaches the model");
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner())[0],
        json!(romanian),
        "predict/passes the state through unchanged"
    );
    let routing = out["routing"].as_object().expect("routing object");
    for key in ["model", "repo", "reason", "detection", "workflow"] {
        assert!(routing.contains_key(key), "predict/keeps the routing block: {key}");
    }

    // the helper itself
    assert_eq!(english_from_code(None), None, "helper/None");
    assert_eq!(english_from_code(Some("")), None, "helper/empty");
    assert_eq!(english_from_code(Some("   ")), None, "helper/spaces");
    assert_eq!(english_from_code(Some("en")), Some(true), "helper/en");
    assert_eq!(english_from_code(Some("en_US")), Some(true), "helper/en_US");
    assert_eq!(english_from_code(Some("zh_CN")), Some(false), "helper/zh_CN");
    assert_eq!(english_from_code(Some("en.UTF-8")), Some(true), "helper/strips after the dot");
    assert_eq!(english_from_code(Some(".")), None, "helper/only a dot");

    // the standalone-repo mapping is untouched
    let r_alone = Router::configure(RouterOptions {
        standalone_repos: true,
        lang_guess: Some(LangGuess::Code("ro".to_string())),
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_alone.route(&json!(romanian), Some(&generic), &RouteOptions::default())
            .expect("route")
            .repo,
        "convaiinnovations/laya-multilingual",
        "standalone/hint still uses the standalone repo"
    );

    // nothing without a hint moves
    for (label, state, want) in [
        ("plain english", "I was charged twice and want a refund", "english"),
        ("German with umlauts", "Mein Konto wurde zweimal belastet, bitte erstatten Sie", "multilingual"),
        ("Hindi", "यह एक हिंदी वाक्य है", "multilingual"),
        ("empty", "", "english"),
        ("digits", "12345", "english"),
    ] {
        assert_eq!(
            r0.route(&json!(state), Some(&generic), &RouteOptions::default())
                .expect("route")
                .model,
            want,
            "unchanged/{label}"
        );
    }
}

/// `_FakeAgent` (test_lang_guess.py) / the batch-suite fake agent: records
/// the `system_one` states it is handed and answers the fixed payload.
struct RecordingAgent {
    calls: Arc<Mutex<Vec<Value>>>,
    lang_temperatures: bool,
}

impl AgentLike for RecordingAgent {
    fn system_one(
        &mut self,
        state: &Value,
        _questions: &Map<String, Value>,
        _lang: Option<&str>,
        _max_len: Option<usize>,
        _head_max_len: Option<usize>,
    ) -> Result<Value> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(state.clone());
        Ok(json!({"answers": {}, "usage": {"input_tokens": 0, "output_tokens": 0}}))
    }

    fn has_lang_temperatures(&self) -> bool {
        self.lang_temperatures
    }
}

// ------------------------------------------------------ blank lang routing
// A blank `lang` must abstain and fall through to detection, not pin the
// multilingual checkpoint; a real code is still decisive
// (`tests/test_blank_lang_routing.py`).
#[test]
fn blank_lang_abstains_and_real_codes_still_win() {
    // A state the built-in detector reads as English. On this state "blank
    // lang must abstain" and "blank lang forced multilingual" are
    // distinguishable, because abstaining keeps the English checkpoint while
    // the bug would jump to multilingual.
    let english = "I was charged twice for invoice 4411";
    // A state the detector reads as non-English, to show a real code is
    // unchanged.
    let german = "Mein Konto wurde zweimal belastet, bitte erstatten Sie";
    let generic = q_generic();
    let r = Router::new().expect("router");
    let route_with = |state: &Value, lang: Option<&'static str>| {
        r.route(state, Some(&generic), &RouteOptions { lang, ..RouteOptions::default() })
            .expect("route")
    };

    // baseline
    assert_eq!(
        route_with(&json!(english), None).model,
        "english",
        "baseline/plain English state routes english"
    );
    assert!(
        !route_with(&json!(english), None).reason.contains("explicit lang="),
        "baseline/no lang emits no explicit reason"
    );

    // blank abstains
    let blank = route_with(&json!(english), Some(""));
    let ws = route_with(&json!(english), Some("   "));
    let none = route_with(&json!(english), None);
    assert_eq!(blank.model, "english", "blank/empty lang falls through to detection (english)");
    assert_eq!(
        ws.model, "english",
        "blank/whitespace lang falls through to detection (english)"
    );
    assert_eq!(none.model, "english", "blank/None lang keeps detected model");
    assert!(
        !blank.reason.contains("explicit lang="),
        "blank/empty lang does not claim an explicit override"
    );
    assert!(
        !ws.reason.contains("explicit lang="),
        "blank/whitespace lang does not claim an explicit override"
    );
    // fall-through means the detection block is present, not just an equal
    // model
    assert!(blank.detection.is_some(), "blank/empty lang keeps the detection block");
    assert!(ws.detection.is_some(), "blank/whitespace lang keeps the detection block");

    // real codes still win
    assert_eq!(route_with(&json!(german), Some("en")).model, "english", "explicit/en forces english");
    assert_eq!(
        route_with(&json!(english), Some("de")).model,
        "multilingual",
        "explicit/de forces multilingual"
    );
    assert!(
        route_with(&json!(german), Some("en")).reason.contains("explicit lang="),
        "explicit/en keeps the explicit reason"
    );
    assert!(
        route_with(&json!(english), Some("de")).reason.contains("explicit lang="),
        "explicit/de keeps the explicit reason"
    );

    for code in ["en", "EN", "en-US", "en_US", "en_US.UTF-8"] {
        assert_eq!(
            route_with(&json!(english), Some(code)).model,
            "english",
            "code/{code} routes english"
        );
    }
    for code in ["de", "fr", "zh_CN", "pt-BR"] {
        assert_eq!(
            route_with(&json!(english), Some(code)).model,
            "multilingual",
            "code/{code} routes multilingual"
        );
    }

    // blank does not mask a hint: with a real hint installed, a blank lang
    // must still abstain rather than pin multilingual.
    let r_hint = Router::configure(RouterOptions {
        lang_guess: Some(LangGuess::Code("de".to_string())),
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_hint
            .route(
                &json!(german),
                Some(&generic),
                &RouteOptions { lang: Some(""), ..RouteOptions::default() },
            )
            .expect("route")
            .model,
        "multilingual",
        "blank/does not mask an installed lang_guess"
    );
    assert_eq!(
        r_hint.route(&json!(german), Some(&generic), &RouteOptions::default())
            .expect("route")
            .model,
        "multilingual",
        "blank/does not break plain detection with a hint installed"
    );
}

