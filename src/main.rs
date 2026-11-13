//! eframe entry point. Application logic lives in the `pqeditor`
//! library; the window shell in `pqeditor::shell` (see DESIGN.md).

// GUI subsystem: no console window pops up alongside the app on Windows.
// (Dropped in the lib-split rewrite of this file — regression the user
// spotted as "the terminal is opening together with the app".)
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    pqeditor::shell::install_panic_log();

    // Optional CLI: open a parquet file at startup.
    let open_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    pqeditor::shell::run_native(open_path);
}
