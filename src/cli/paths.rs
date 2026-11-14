//! `cosh paths` — print the directories the app reads and writes.
//!
//! Support and composition both need this: "where are my sessions?" is
//! answered by the store's own resolution (including the `COSH_DATA_DIR`
//! override), not by a second, drifting copy of the path rules.

use std::path::PathBuf;

use cosh::session::store::SessionStore;

use crate::cli::style::Palette;

/// One printed row: the label and the resolved path.
pub fn entries() -> Vec<(&'static str, PathBuf)> {
    let logs = cosh::harness::truncate::scratch_log_dir().join("log");
    let sessions = SessionStore::new().sessions_dir().to_path_buf();
    let binary = std::env::current_exe().unwrap_or_default();
    vec![
        ("data", cosh::setup::data_dir_override()),
        ("config", cosh::setup::config_dir_override()),
        ("logs", logs),
        ("sessions", sessions),
        ("binary", binary),
    ]
}

/// Print every path and return the process exit code.
pub fn run(palette: &Palette) -> i32 {
    println!("{}", palette.title("cosh paths"));
    println!();
    for (label, path) in entries() {
        println!(
            "  {}{}",
            palette.item(&format!("{label:<9}")),
            palette.body(&path.display().to_string())
        );
    }
    println!();
    println!(
        "{}",
        palette.muted("data and config can be overridden with COSH_DATA_DIR / COSH_CONFIG_DIR.")
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_row_has_a_path() {
        let entries = entries();
        let labels: Vec<&str> = entries.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["data", "config", "logs", "sessions", "binary"]);
        assert!(
            entries.iter().all(|(_, path)| !path.as_os_str().is_empty()),
            "a documented row resolved to an empty path: {entries:?}"
        );
    }
}
