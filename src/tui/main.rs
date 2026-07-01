#![allow(clippy::cast_possible_truncation)]
#![allow(dead_code)]

mod app;
mod config;
mod keymap;
mod state;
mod theme;
mod types;
mod routes;
mod component;
mod ui;
mod util;

use app::App;

fn main() {
    let mut app = App::new();
    if let Err(e) = app.run() {
        eprintln!("Error: {e}");
    }
}
