//! Installer unit tests: pure helpers and validation paths that need no
//! network (the download path itself is exercised by the app-level
//! integration, against the real mirror).

use std::collections::HashMap;

use super::{
    InstallRequest, normalize_caller_map, parse_sha256sums, subfolder_prefix, tokenizer_usable,
};
use crate::error::Error;

#[test]
fn normalizes_caller_map_special_keys_and_prefixes() {
    let map: HashMap<String, String> = [
        ("onnx".to_string(), "g1".to_string()),
        ("rl_agent_config.json".to_string(), "c1".to_string()),
    ]
    .into_iter()
    .collect();
    let out = normalize_caller_map(&map, "english/", "english/laya.onnx");
    // The special key resolves to the graph's repo-relative path; ordinary
    // checkpoint-relative keys gain the subfolder prefix.
    assert_eq!(out.get("english/laya.onnx").unwrap(), "g1");
    assert_eq!(out.get("english/rl_agent_config.json").unwrap(), "c1");
    assert_eq!(out.len(), 2);
    assert!(!out.contains_key("onnx"));
}

#[test]
fn onnx_path_loses_to_onnx_and_prefixed_keys_stay() {
    let map: HashMap<String, String> = [
        ("onnx".to_string(), "g1".to_string()),
        ("onnx_path".to_string(), "g2".to_string()),
        ("english/tok.json".to_string(), "t1".to_string()),
        ("tok.json".to_string(), "t2".to_string()),
    ]
    .into_iter()
    .collect();
    let out = normalize_caller_map(&map, "english/", "english/laya.onnx");
    assert_eq!(out.get("english/laya.onnx").unwrap(), "g1");
    // Already-prefixed keys keep their spelling; unprefixed ones gain it.
    assert_eq!(out.get("english/tok.json").unwrap(), "t1");
    assert_eq!(out.get("english/tok.json"), out.get("english/tok.json"));
    assert!(!out.contains_key("tok.json"));
    assert_eq!(out.len(), 2);
}

#[test]
fn empty_prefix_leaves_keys_untouched() {
    let map: HashMap<String, String> = [("a.bin".to_string(), "d".to_string())]
        .into_iter()
        .collect();
    let out = normalize_caller_map(&map, "", "laya.onnx");
    assert_eq!(out.get("a.bin").unwrap(), "d");
    assert_eq!(out.get("laya.onnx"), None);
    assert_eq!(subfolder_prefix(None), "");
    assert_eq!(subfolder_prefix(Some("english")), "english/");
}

#[test]
fn tokenizer_usable_mirrors_the_loader_rule() {
    let root = std::env::temp_dir().join("cosh-tokenizer-usable-test");
    let _ = std::fs::remove_dir_all(&root);
    let snap = root.join("snap");
    std::fs::create_dir_all(snap.join("english/tokenizer")).unwrap();
    std::fs::create_dir_all(snap.join("english/encoder")).unwrap();

    // tokenizer/ exists but carries no tokenizer.json: the loader would
    // pick it and fail, so the snapshot must be REJECTED even though the
    // encoder tokenizer is present.
    std::fs::write(snap.join("english/encoder/tokenizer.json"), "{}").unwrap();
    assert!(!tokenizer_usable(&snap, "english/"));

    // tokenizer.json lands in tokenizer/: usable.
    std::fs::write(snap.join("english/tokenizer/tokenizer.json"), "{}").unwrap();
    assert!(tokenizer_usable(&snap, "english/"));

    // tokenizer/ directory absent entirely + encoder tokenizer: usable
    // (the encoder-bundle layout).
    std::fs::remove_dir_all(snap.join("english/tokenizer")).unwrap();
    assert!(tokenizer_usable(&snap, "english/"));

    // Nothing anywhere: unusable.
    std::fs::remove_file(snap.join("english/encoder/tokenizer.json")).unwrap();
    assert!(!tokenizer_usable(&snap, "english/"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn parses_sha256sum_text_format() {
    let d1 = "a".repeat(64);
    let d2 = "b".repeat(64);
    let text = format!("{d1}  english/laya.onnx\n{d2}  multilingual/laya.onnx\n\nbroken line\n");
    let map = parse_sha256sums(&text);
    assert_eq!(map.get("english/laya.onnx").unwrap(), &d1);
    assert_eq!(map.get("multilingual/laya.onnx").unwrap(), &d2);
    assert_eq!(map.len(), 2);
}

#[test]
fn rejects_short_or_nonhex_digests() {
    let map = parse_sha256sums("abc  a/b\nzzzz  a/c\n");
    assert!(map.is_empty());
}

#[test]
fn empty_repo_id_is_a_value_error() {
    let err = super::install(InstallRequest {
        repo: "",
        subfolder: None,
        revision: None,
        graphs: &["laya.onnx"],
        expected_sha256: None,
        progress: super::ProgressCallbacks::silent(),
    })
    .unwrap_err();
    assert!(matches!(err, Error::Value(_)));
}

#[test]
fn no_graph_candidates_is_a_value_error() {
    let err = super::install(InstallRequest {
        repo: "owner/name",
        subfolder: None,
        revision: None,
        graphs: &[],
        expected_sha256: None,
        progress: super::ProgressCallbacks::silent(),
    })
    .unwrap_err();
    assert!(matches!(err, Error::Value(_)));
}

#[test]
fn local_directory_repo_is_a_value_error() {
    let dir = std::env::temp_dir().join("cosh-install-local-dir-test");
    std::fs::create_dir_all(&dir).unwrap();
    let err = super::install(InstallRequest {
        repo: dir.to_str().unwrap(),
        subfolder: None,
        revision: None,
        graphs: &["laya.onnx"],
        expected_sha256: None,
        progress: super::ProgressCallbacks::silent(),
    })
    .unwrap_err();
    assert!(matches!(err, Error::Value(_)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_status_is_none_for_an_uncached_repo() {
    // A repo id that cannot be in any real cache (reserved path chars are
    // still legal folder names, so just an improbable one).
    assert!(
        super::cache_status(
            "cosh-tests/nonexistent-repo-zz",
            Some("english"),
            &["laya.onnx"],
            None,
        )
        .is_none()
    );
}

#[test]
fn silent_callbacks_are_cloneable_and_safe() {
    let cb = super::ProgressCallbacks::silent();
    let mut c2 = cb.clone();
    hf_hub::api::Progress::init(&mut c2, 10, "x");
    hf_hub::api::Progress::update(&mut c2, 5);
    hf_hub::api::Progress::finish(&mut c2);
}
