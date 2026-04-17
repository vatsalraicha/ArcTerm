// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // env_logger respects RUST_LOG. Default to info so PTY lifecycle is
    // visible during development without the user setting anything.
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info,arcterm_desktop_lib=debug");
    }
    env_logger::init();

    arcterm_desktop_lib::run();
}
