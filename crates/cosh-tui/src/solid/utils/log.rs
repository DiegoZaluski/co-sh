use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_debug(enabled: bool) {
    DEBUG_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_debug() -> bool {
    DEBUG_ENABLED.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! log_reconciler {
    ($($arg:tt)*) => {
        if $crate::solid::utils::log::is_debug() {
            eprintln!("[Reconciler] {}", format!($($arg)*));
        }
    };
}
