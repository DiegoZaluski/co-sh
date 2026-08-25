use crate::app::providers::is_valid_local_url;

#[test]
fn accepts_http_and_https_with_host() {
    assert!(is_valid_local_url("http://127.0.0.1:8080"));
    assert!(is_valid_local_url("https://localhost:11434"));
    assert!(is_valid_local_url("  http://host:1  "));
}

#[test]
fn rejects_missing_scheme_or_empty_host() {
    assert!(!is_valid_local_url("127.0.0.1:8080"));
    assert!(!is_valid_local_url("http://"));
    assert!(!is_valid_local_url("https://"));
    assert!(!is_valid_local_url(""));
    assert!(!is_valid_local_url("ftp://host"));
}
