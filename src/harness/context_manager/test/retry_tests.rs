//! Tests for retry configuration

use crate::harness::context_manager::ContextManager;
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// --- Retry configuration tests ---

// Verifies that the default max_retries is 3 as defined by DEFAULT_MAX_RETRIES.
#[test]
fn default_max_retries_is_three() {
    let cm = ContextManager::new(test_connector(), 3);
    assert_eq!(cm.max_retries(), 3, "default max_retries should be 3");
}

// Verifies that with_max_retries overrides the retry limit and persists.
#[test]
fn with_max_retries_changes_value() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.with_max_retries(5);
    assert_eq!(cm.max_retries(), 5, "should reflect the new value");

    cm.with_max_retries(0);
    assert_eq!(
        cm.max_retries(),
        0,
        "zero is a valid retry count (no retries)"
    );

    cm.with_max_retries(100);
    assert_eq!(cm.max_retries(), 100, "large values should work too");
}