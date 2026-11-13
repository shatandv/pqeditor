//! eframe entry point. Application logic lives in the `pqeditor`
//! library; the window shell in `pqeditor::shell` (see DESIGN.md).

fn main() {
    pqeditor::shell::install_panic_log();

    // Optional CLI: open a parquet file at startup.
    let open_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    pqeditor::shell::run_native(open_path);
}
