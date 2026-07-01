#![allow(clippy::cast_possible_truncation)]
#![allow(dead_code)]

mod app;
mod config;
mod dialogs;
mod footer;
mod home;
mod keymap;
mod permission;
mod prompt;
mod question;
mod session;
mod sidebar;
mod state;
mod subagent_footer;
mod theme;
mod types;

use app::App;

fn main() {
    let mut app = App::new();
    if let Err(e) = app.run() {
        eprintln!("Error: {e}");
    }
}
