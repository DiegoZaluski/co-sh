//! Tests for advanced expansion functionality (multi-level expansion)

use crate::harness::context_manager::ContextManager;
use cosh_sdk::connector::Connector;

fn test_connector() -> Connector {
    Connector::new("ollama").expect("ollama provider should be known")
}

// Verifies model can expand directly to LV3 from LV5.
#[test]
fn expand_directly_to_mid_level() {
    let mut cm = ContextManager::new(test_connector(), 3);

    cm.build_context("level 1 original".to_string(), None, 1, 100, Some(1));
    cm.build_context("level 2 summary".to_string(), None, 2, 80, Some(1));
    cm.build_context("level 3 summary".to_string(), None, 3, 60, Some(1));
    cm.build_context("level 4 summary".to_string(), None, 4, 40, Some(1));
    cm.build_context("level 5 seed".to_string(), None, 5, 20, Some(1));

    let hashes: Vec<u64> = cm.queue.iter().map(|c| c.hash_id).collect();
    // hashes: [(1<<24)|1, (1<<24)|2, (1<<24)|3, (1<<24)|4, (1<<24)|5] for ex=1, lv=1..5
    cm.layers.insert(hashes[4], vec![cm.queue[3].clone()]);
    cm.layers.insert(hashes[3], vec![cm.queue[2].clone()]);
    cm.layers.insert(hashes[2], vec![cm.queue[1].clone()]);
    cm.layers.insert(hashes[1], vec![cm.queue[0].clone()]);

    // Target LV3: (1<<24) | 3
    cm.expand_slot((1 << 24) | 3);

    let output = cm.format_context().unwrap();

    assert!(
        output.contains("level 3 summary"),
        "should expand to level 3"
    );
    assert!(!output.contains("level 5 seed"), "should NOT show level 5");
    assert!(
        !output.contains("level 1 original"),
        "should NOT show level 1"
    );
    assert!(
        !output.contains("level 4 summary"),
        "should NOT show level 4"
    );
    assert!(
        !output.contains("level 2 summary"),
        "should NOT show level 2"
    );
    assert!(
        output.contains("[EXPANDED]"),
        "should show [EXPANDED] marker"
    );
}
