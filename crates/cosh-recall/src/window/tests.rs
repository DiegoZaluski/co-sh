use super::*;

#[test]
fn new_window_is_empty() {
    let w = ContextWindow::new(100);
    assert!(w.is_empty());
    assert_eq!(w.len(), 0);
    assert_eq!(w.total_tokens(), 0);
    assert!(!w.is_over_budget());
}

#[test]
#[should_panic(expected = "max_tokens must be greater than zero")]
fn zero_max_tokens_panics() {
    let _ = ContextWindow::new(0);
}

#[test]
fn push_one_entry_stays_within_budget() {
    let mut w = ContextWindow::new(100);
    w.push("hello", 10);
    assert_eq!(w.len(), 1);
    assert_eq!(w.total_tokens(), 10);
    assert!(!w.is_over_budget());
}

#[test]
fn push_exactly_at_budget_triggers_eviction_inline() {
    let mut w = ContextWindow::new(100);

    // Push 10 entries of 10 tokens each.
    // Eviction fires on the 10th push itself (100 >= 100).
    for i in 0..10 {
        w.push(format!("entry-{i}"), 10);
    }

    // After the 10th push: 10 entries, remove ceil(10/2)=5, keep newest 5.
    assert_eq!(w.len(), 5);
    assert_eq!(w.total_tokens(), 50);
    assert!(!w.is_over_budget(), "eviction already ran inside the loop");

    // Push an 11th entry: 60 tokens — under budget, no eviction.
    w.push("new-entry", 10);
    assert_eq!(w.len(), 6);
    assert_eq!(w.total_tokens(), 60);

    // The surviving entries are the 6 newest
    let remaining: Vec<_> = w.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(remaining, vec!["entry-5", "entry-6", "entry-7", "entry-8", "entry-9", "new-entry"]);
}

#[test]
fn eviction_preserves_newest_entries() {
    let mut w = ContextWindow::new(50);

    // Push entries 0..5 (5 entries × 15 tokens = 75 tokens, over the 50 limit)
    for i in 0..5 {
        w.push(format!("entry-{i}"), 15);
    }

    // Eviction fires on push 3 (entry-3: total 60 >= 50).
    //   ceil(4/2)=2 removed → entries [2,3], total=30.
    // Push 4 (entry-4): total=45. No eviction. Entries [2,3,4].
    //
    // Final: 3 entries, newest survive.
    assert_eq!(w.len(), 3);
    let remaining: Vec<_> = w.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(remaining, vec!["entry-2", "entry-3", "entry-4"]);
}

#[test]
fn single_oversized_entry_is_evicted() {
    let mut w = ContextWindow::new(10);
    w.push("big", 100); // single entry, far over budget

    // Only 1 entry. ceil(1/2) = 1. Pop front, window becomes empty.
    assert!(w.is_empty(), "single oversized entry is evicted entirely");
    assert_eq!(w.total_tokens(), 0);
}

#[test]
fn push_many_entries_gradually_shrinks() {
    let mut w = ContextWindow::new(100);
    let mut prev_len = 0usize;

    for i in 0..50 {
        w.push(format!("entry-{i}"), 10);
        if w.len() < prev_len {
            // Eviction happened — the window compacted
            assert!(w.total_tokens() <= w.max_tokens());
        }
        prev_len = w.len();
    }

    // After 50 entries of 10 tokens each with a 100-token budget,
    // the window should have stabilised
    assert!(w.total_tokens() <= w.max_tokens());
    assert!(!w.is_empty(), "window should never become empty after many pushes");
}

#[test]
fn clear_resets_everything() {
    let mut w = ContextWindow::new(100);
    w.push("a", 30);
    w.push("b", 30);
    w.push("c", 30);
    assert_eq!(w.len(), 3);

    w.clear();

    assert!(w.is_empty());
    assert_eq!(w.total_tokens(), 0);
    assert!(!w.is_over_budget());
}

#[test]
fn entries_ordered_oldest_first() {
    let mut w = ContextWindow::new(1000);
    w.push("first", 10);
    w.push("second", 10);
    w.push("third", 10);

    let entries: Vec<_> = w.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(entries, vec!["first", "second", "third"]);
}

#[test]
fn total_tokens_is_accurate_after_eviction() {
    let mut w = ContextWindow::new(30);

    w.push("a", 20); // total: 20
    w.push("b", 20); // total: 40 → over budget
    // 2 entries, remove ceil(2/2)=1 → keep "b" (20 tokens)
    assert_eq!(w.total_tokens(), 20);
    assert_eq!(w.len(), 1);
    assert_eq!(w.iter().next().unwrap().content, "b");
}

#[test]
fn mixed_token_sizes() {
    let mut w = ContextWindow::new(100);

    w.push("small", 5);
    w.push("medium", 15);
    w.push("large", 80); // total: 100, exactly at budget
    w.push("tiny", 2); // total: 102 → over budget

    // 4 entries, remove ceil(4/2)=2 → keep the 2 newest
    assert_eq!(w.len(), 2);
    let remaining: Vec<_> = w.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(remaining, vec!["large", "tiny"]);
    assert_eq!(w.total_tokens(), 82);
}

#[test]
fn eviction_never_empties_with_small_entries() {
    let mut w = ContextWindow::new(100);

    for i in 0..100 {
        w.push(format!("entry-{i}"), 10);
    }

    // Window should stabilise with a handful of entries and never empty.
    assert!(!w.is_empty(), "window should never become empty");
    assert!(w.total_tokens() <= w.max_tokens());
    // With max=100 and entries of 10 tokens, the stable size is ~5-10.
    assert!(w.len() > 0 && w.len() <= 10, "expected 1-10 entries, got {}", w.len());

    // Verify newest entries survive
    let entries: Vec<_> = w.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(entries.last().unwrap(), &"entry-99");
}

#[test]
fn is_over_budget_flag() {
    let mut w = ContextWindow::new(50);

    assert!(!w.is_over_budget());
    w.push("a", 30);
    assert!(!w.is_over_budget());

    // Push "b" (20 tokens): total becomes 50, exactly at budget.
    // Eviction fires: 2 entries, remove ceil(2/2)=1, keep 1 (20 tokens).
    w.push("b", 20);
    assert!(
        !w.is_over_budget(),
        "eviction runs inside push(), so is_over_budget() is never externally visible"
    );
    assert_eq!(w.total_tokens(), 20);
    assert_eq!(w.len(), 1);
}

#[test]
fn clone_and_debug() {
    let mut w = ContextWindow::new(100);
    w.push("test", 10);

    let cloned = w.clone();
    assert_eq!(w.len(), cloned.len());
    assert_eq!(w.total_tokens(), cloned.total_tokens());

    let _debug = format!("{:?}", w);
}

#[test]
fn entries_reference_is_stable() {
    let mut w = ContextWindow::new(100);
    w.push("a", 10);
    w.push("b", 10);

    let entries_ref = w.entries();
    assert_eq!(entries_ref.len(), 2);
}
