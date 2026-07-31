//! Tests for context formatting and display functionality

use crate::harness::context_manager::ContextManager;
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// Verifies that format_context preserves exhibition order after simulated compression.
// When a context is "compressed" (rebuilt at the same exhibition slot), its visual
// position in the formatted output must not move — only the content/lv updates.
#[test]
fn format_context_preserves_exhibition_order() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("alpha".to_string(), None, 1, 2, Some(1));
    cm.build_context("beta".to_string(), None, 1, 3, Some(2));
    cm.build_context("gamma".to_string(), None, 1, 2, Some(3));

    // Simulate compression: re-build at exhibition slots 1 and 3
    cm.build_context("ALPHA".to_string(), Some(1), 2, 1, Some(1));
    cm.build_context("GAMMA".to_string(), Some(3), 2, 1, Some(3));

    let output = cm.format_context().unwrap();

    // Slot-1 content must appear before slot-2, which must appear before slot-3
    let p1 = output.find("ALPHA").unwrap();
    let p2 = output.find("beta").unwrap();
    let p3 = output.find("GAMMA").unwrap();
    assert!(p1 < p2, "exhibition slot 1 must appear before slot 2");
    assert!(p2 < p3, "exhibition slot 2 must appear before slot 3");

    // Compressed slots should hold the new content, not the old
    assert!(
        !output.contains("alpha"),
        "slot 1 should be 'ALPHA', not 'alpha'"
    );
    assert!(
        !output.contains("gamma"),
        "slot 3 should be 'GAMMA', not 'gamma'"
    );
}

// Verifies that format_context handles an empty exhibitions array gracefully.
#[test]
fn format_context_empty_exhibitions_returns_some_empty() {
    let mut cm = ContextManager::new(test_connector(), 3);

    let output = cm.format_context();

    assert!(
        output.is_some(),
        "format_context should return Some even empty"
    );
    let text = output.unwrap();
    assert!(
        text.is_empty() || text.trim().is_empty(),
        "empty exhibitions should produce empty output"
    );
}