//! The checkpoint specs: workflow signatures, name normalisation and the
//! bundle-vs-standalone model maps.

use super::*;

// ------------------------------------------------------- workflow signatures
#[test]
fn typed_decision_workflow_signatures() {
    // profile anchors upstream keeps next to the workflow table
    assert_eq!(
        analyse(&json!("Հայերեն")).script_profile,
        vec![("armenian".to_string(), 1.0)],
        "profile/armenian"
    );
    assert_eq!(
        analyse(&json!("Հայերեն abc")).non_latin_fraction,
        0.7,
        "profile/armenian mixed with Latin"
    );
    assert_eq!(
        analyse(&json!("ələ")).script_profile,
        vec![("latin".to_string(), 1.0)],
        "profile/azerbaijani schwa is latin"
    );

    let td: &[(&str, &[&str])] = &[
        (
            "agent_trace_observability",
            &["action", "needs_review", "outcome", "risk", "urgency"],
        ),
        (
            "customer_service",
            &["action", "category", "churn_risk", "needs_human", "urgency"],
        ),
        (
            "invoice_processing",
            &[
                "discrepancy_severity",
                "disposition",
                "duplicate",
                "matches_order",
                "urgency",
            ],
        ),
        (
            "security_incidents",
            &[
                "credential_compromise",
                "disposition",
                "severity",
                "true_positive",
                "urgency",
            ],
        ),
    ];
    for (wf, ids) in td {
        let questions: Map<String, Value> =
            ids.iter().map(|id| (id.to_string(), json!({}))).collect();
        assert_eq!(
            match_typed_decisions_workflow(&questions),
            Some(*wf),
            "workflow/{wf}"
        );
    }
    assert_eq!(
        match_typed_decisions_workflow(
            &json!({"urgency": {}, "category": {}})
                .as_object()
                .unwrap()
                .clone()
        ),
        None,
        "workflow/partial overlap"
    );
    let superset: Map<String, Value> = td[1]
        .1
        .iter()
        .chain(["extra"].iter())
        .map(|id| (id.to_string(), json!({})))
        .collect();
    assert_eq!(
        match_typed_decisions_workflow(&superset),
        None,
        "workflow/superset"
    );
    assert_eq!(
        match_typed_decisions_workflow(&Map::new()),
        None,
        "workflow/empty"
    );
}

// --------------------------------------------------------- name normalisation
#[test]
fn name_normalisation_and_aliases() {
    for (alias, want) in [
        ("en", "english"),
        ("laya", "english"),
        ("multi", "multilingual"),
        ("ML", "multilingual"),
        ("typed", "typed-decisions"),
        ("typed_decisions", "typed-decisions"),
        ("English", "english"),
        // "convaiinnovations/laya".split("/")[-1]
        ("laya", "english"),
    ] {
        assert_eq!(normalise_name(alias).expect("known"), want, "alias/{alias}");
    }
    let err = normalise_name("nope").expect_err("alias/unknown raises");
    assert!(
        matches!(err, Error::Value(_)),
        "alias/unknown raises: {err:?}"
    );
    assert!(
        err.to_string().starts_with("unknown model"),
        "alias/unknown message: {err}"
    );
}

// ------------------------------------------------- bundle vs standalone
#[test]
fn bundle_and_standalone_model_maps() {
    let defaults = super::default_models();
    let standalone = super::standalone_models();
    assert_eq!(
        defaults
            .iter()
            .find(|(k, _)| *k == "english")
            .map(|(_, v)| v.clone()),
        Some(super::ModelSpec::Repo(super::BUNDLE_REPO.to_string())),
        "bundle/english is repo root"
    );
    assert_eq!(
        defaults
            .iter()
            .find(|(k, _)| *k == "multilingual")
            .map(|(_, v)| v.clone()),
        Some(super::ModelSpec::Bundled(
            super::BUNDLE_REPO.to_string(),
            "multilingual".to_string()
        )),
        "bundle/multilingual subfolder"
    );
    assert_eq!(
        defaults
            .iter()
            .find(|(k, _)| *k == "typed-decisions")
            .map(|(_, v)| v.clone()),
        Some(super::ModelSpec::Bundled(
            super::BUNDLE_REPO.to_string(),
            "typed-decisions".to_string()
        )),
        "bundle/typed subfolder"
    );
    assert_eq!(
        repo_str(&super::ModelSpec::Repo(super::BUNDLE_REPO.to_string())),
        "convaiinnovations/laya",
        "repo_str/root"
    );
    assert_eq!(
        repo_str(&super::ModelSpec::Bundled(
            super::BUNDLE_REPO.to_string(),
            "multilingual".to_string()
        )),
        "convaiinnovations/laya/multilingual",
        "repo_str/sub"
    );
    assert_eq!(
        repo_str(&super::ModelSpec::Repo("some/repo".to_string())),
        "some/repo",
        "repo_str/plain string"
    );

    let mut default_names: Vec<&str> = defaults.iter().map(|(k, _)| *k).collect();
    let mut standalone_names: Vec<&str> = standalone.iter().map(|(k, _)| *k).collect();
    default_names.sort();
    standalone_names.sort();
    assert_eq!(standalone_names, default_names, "standalone map complete");

    let r_bundle = Router::new().expect("router");
    let r_alone = Router::configure(RouterOptions {
        standalone_repos: true,
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_bundle
            .route(
                &json!({"m": "मुझसे दो बार"}),
                Some(&q_generic()),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        "convaiinnovations/laya/multilingual",
        "bundle/default router uses bundle"
    );
    assert_eq!(
        r_alone
            .route(
                &json!({"m": "मुझसे दो बार"}),
                Some(&q_generic()),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        "convaiinnovations/laya-multilingual",
        "standalone/opt-in uses own repo"
    );
    assert_eq!(
        r_alone
            .route(
                &json!({"m": "I was charged twice"}),
                Some(&q_generic()),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        "convaiinnovations/laya",
        "standalone/english unchanged"
    );
    // a local-path override must still work (the Space and tests rely on it)
    let r_local = Router::configure(RouterOptions {
        models: vec![
            (
                "english".to_string(),
                super::ModelSpec::Repo("/tmp/en".to_string()),
            ),
            (
                "multilingual".to_string(),
                super::ModelSpec::Repo("/tmp/ml".to_string()),
            ),
        ],
        ..Default::default()
    })
    .expect("router");
    assert_eq!(
        r_local
            .route(
                &json!({"m": "मुझसे दो बार"}),
                Some(&q_generic()),
                &RouteOptions::default()
            )
            .expect("route")
            .repo,
        "/tmp/ml",
        "override/local path kept"
    );
}
