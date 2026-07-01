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

fn main() {
    let mut app = App::new();
    app.show_welcome_toast();
    if let Err(e) = app.run() {
        eprintln!("Error: {e}");
    }
}
