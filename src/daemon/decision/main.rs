//! The `cosh-decisiond` binary.
//!
//! A thin shim over [`cosh::daemon::decision::run`]: all the logic (bind,
//! serve, idle exit, the bind-race outcome) lives in the library so the
//! integration tests exercise the exact code the daemon runs.

fn main() {
    let code = cosh::daemon::decision::run();
    std::process::exit(code);
}
