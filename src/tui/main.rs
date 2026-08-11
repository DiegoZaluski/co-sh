#![allow(dead_code)]

mod app;
mod component;
mod config;
mod fallback;
mod keymap;
mod logo;
mod routes;
mod session_store;
mod state;
mod theme;
mod types;
mod ui;
mod util;

#[cfg(test)]
#[path = "test/rag_abort.rs"]
mod test;

use app::App;
use dotenvy::dotenv;

#[tokio::main]
async fn main() {
    dotenv().ok();

    // Initialize debug file logger (no-op in release builds).
    // Logs are written to /tmp/tui_main.log.
    cosh::util::logger::init("tui_main");

    if std::env::args().any(|a| a == "--check-keyring") {
        check_keyring();
        return;
    }

    crate::util::selection::init_clipboard();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "?".to_string());
    let mut app = App::new(cwd);
    app.show_welcome_toast();
    if let Err(e) = app.run() {
        eprintln!("Error: {e}");
    }
}

/// Diagnostic for the OS credential store backing the API keys.
///
/// Read-only: reports, per provider, whether a key is available from the
/// environment and/or from the keyring (service `cosh`), plus a write/read/
/// delete roundtrip through the native store to prove end-to-end connectivity.
fn check_keyring() {
    use cosh_sdk::connector::{COSH_SERVICE, known_providers_with_env};
    use keyring::Entry;

    let mut rows: Vec<(String, bool, bool)> = Vec::new();
    let mut read_errors = 0usize;
    for (provider, env_var) in known_providers_with_env() {
        let env = std::env::var(env_var).is_ok();
        let keyring = match Entry::new(COSH_SERVICE, env_var) {
            Ok(entry) => entry.get_password().is_ok(),
            Err(err) => {
                read_errors += 1;
                eprintln!("  keyring error for {provider}: {err}");
                false
            }
        };
        rows.push((provider.to_string(), env, keyring));
    }

    println!("cosh keyring status");
    println!(
        "  dbus : {:?}",
        std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .unwrap_or_else(|| "<unset>".to_string())
    );
    println!(
        "  keys : {} stored under service \"{COSH_SERVICE}\"",
        rows.iter().filter(|(_, _, kr)| *kr).count()
    );
    println!();
    println!("{:<16} {:<6} {:<8} source", "provider", "env", "keyring");
    for (p, env, kr) in rows {
        let source = if kr {
            "keyring"
        } else if env {
            "env"
        } else {
            "-"
        };
        println!(
            "{p:<16} {:<6} {:<8} {source}",
            if env { "yes" } else { "no" },
            if kr { "yes" } else { "no" }
        );
    }

    // Write/read/delete roundtrip to prove the native store works end to end.
    let selftest_key = "__cosh_selftest";
    let roundtrip = (|| -> Result<(), keyring::Error> {
        let entry = Entry::new(COSH_SERVICE, selftest_key)?;
        entry.set_password("roundtrip-ok")?;
        let value = entry.get_password()?;
        let ok = value == "roundtrip-ok";
        entry.delete_credential()?;
        if ok {
            Ok(())
        } else {
            Err(keyring::Error::NoEntry)
        }
    })();
    println!();
    match roundtrip {
        Ok(()) => println!("roundtrip : OK (write/read/delete via the native OS store)"),
        Err(err) => {
            println!("roundtrip : FAILED: {err}");
            std::process::exit(2);
        }
    }
    if read_errors > 0 {
        std::process::exit(2);
    }
}
