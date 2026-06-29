use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

static ID_COUNTER: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Generate a unique ID for the given element type.
///
/// # Panics
/// Panics if the global mutex is poisoned (another thread panicked while holding the lock).
pub fn get_next_id(element_type: &str) -> String {
    let mut map = ID_COUNTER.lock().unwrap();
    let entry = map.entry(element_type.to_string()).or_insert(0);
    *entry += 1;
    format!("{element_type}-{entry}")
}
