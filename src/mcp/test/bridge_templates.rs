//! RFC 6570 template expansion and matching ([`super::super::bridge`]).
//!
//! Covers expanding raw `uri_template`s with parameters and routing expanded
//! URIs back to the server that advertised the template.

use super::super::bridge::{expand_resource_template, match_resource_template};

#[test]
fn template_expand_and_match_roundtrip() {
    let mut params = std::collections::HashMap::new();
    params.insert("id".to_string(), "42".to_string());
    assert_eq!(
        expand_resource_template("file:///docs/{id}", &params),
        "file:///docs/42"
    );
    // Missing keys leave the placeholder untouched.
    assert_eq!(
        expand_resource_template("file:///docs/{id}", &std::collections::HashMap::new()),
        "file:///docs/{id}"
    );
    assert!(match_resource_template(
        "file:///docs/{id}",
        "file:///docs/42"
    ));
    assert!(!match_resource_template(
        "file:///docs/{id}",
        "file:///other/42"
    ));
    assert!(!match_resource_template(
        "file:///docs/{id}",
        "file:///docs/"
    ));
}

#[test]
fn template_match_rejects_literal_slash_for_simple_vars() {
    // Simple `{name}` is one path segment (MCP SDK `UriTemplate.match`
    // semantics): a literal `/` in the candidate value belongs to the
    // URI structure and must not match.
    assert!(match_resource_template(
        "file:///docs/{name}",
        "file:///docs/readme.txt"
    ));
    assert!(!match_resource_template(
        "file:///docs/{name}",
        "file:///docs/a/b"
    ));
    // A percent-encoded `%2F` contains no literal slash, so it still
    // matches (decoding is the server's job).
    assert!(match_resource_template(
        "file:///docs/{name}",
        "file:///docs/a%2Fb"
    ));
    // Reserved expansion spans segments (the MCP spec's canonical
    // `file:///{path}` form).
    assert!(match_resource_template(
        "file:///docs/{+path}",
        "file:///docs/a/b"
    ));
    assert!(!match_resource_template(
        "file:///docs/{+path}",
        "file:///docs/"
    ));
}

#[test]
fn template_expand_fills_reserved_operators_from_base_var() {
    let mut params = std::collections::HashMap::new();
    params.insert("path".to_string(), "a/b".to_string());
    assert_eq!(
        expand_resource_template("file:///docs/{+path}", &params),
        "file:///docs/a/b"
    );
}
