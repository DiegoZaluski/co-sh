//! `ModelKind` semantics: names, defaults and resolution.

use cosh_onnx::decision::model::ModelKind;

#[test]
fn default_kind_is_english() {
    assert_eq!(ModelKind::default(), ModelKind::English);
}

#[test]
fn named_kinds_carry_the_router_names() {
    assert_eq!(ModelKind::English.name(), Some("english"));
    assert_eq!(ModelKind::Multilingual.name(), Some("multilingual"));
    assert_eq!(ModelKind::TypedDecisions.name(), Some("typed-decisions"));
    assert_eq!(
        ModelKind::Custom {
            repo: "local".to_string(),
            subfolder: None,
        }
        .name(),
        None
    );
}

#[test]
fn kinds_are_comparable_and_clonable() {
    let kind = ModelKind::Custom {
        repo: "convaiinnovations/laya".to_string(),
        subfolder: Some("multilingual".to_string()),
    };
    assert_eq!(kind.clone(), kind);
    assert_ne!(kind, ModelKind::TypedDecisions);
}

#[test]
fn custom_accepts_a_local_path_and_a_bundled_subfolder() {
    // The two shapes a caller holds: a plain directory, and one checkpoint
    // out of a bundling repo. Both pass through unchanged (the Router's
    // `ModelSpec` is the richer surface; the facade needs only these two).
    let local = ModelKind::Custom {
        repo: "/data/checkpoints/laya".to_string(),
        subfolder: None,
    };
    let bundled = ModelKind::Custom {
        repo: "someorg/some-repo".to_string(),
        subfolder: Some("typed-decisions".to_string()),
    };
    assert_eq!(local.name(), None);
    assert_eq!(bundled.name(), None);
}
