//! pqdriver: run a scripted scenario against the real application logic
//! (no UI). See `src/driver.rs` for the grammar and `tools/scenarios/`
//! for the committed scenarios.
//!
//! Usage: pqdriver <scenario.txt>

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: pqdriver <scenario.txt>");
            std::process::exit(2);
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read scenario {path:?}: {e}");
            std::process::exit(2);
        }
    };
    let engine = pqeditor::data::engine::Engine::spawn().expect("engine");
    let mut app = pqeditor::app::App::with_engine(engine, Vec::new());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match pqeditor::driver::run_scenario(&mut app, &text, &mut out) {
        None => {
            let _ = writeln!(out, "scenario PASSED");
        }
        Some(failure) => {
            let _ = writeln!(out, "FAIL {failure}");
            std::process::exit(1);
        }
    }
}
