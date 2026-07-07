#![allow(clippy::cast_possible_truncation)]
#![allow(dead_code)]

mod app;
mod component;
mod config;
mod keymap;
mod routes;
mod state;
mod theme;
mod types;
mod ui;
mod util;

use app::App;
use dotenvy::dotenv;

#[tokio::main]
async fn main() {
    dotenv().ok();

    // Initialize debug file logger (no-op in release builds).
    // Logs are written to /tmp/cosh_debug.log.
    cosh::util::logger::init();

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
