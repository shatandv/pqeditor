//! Committed deterministic scenarios: the same files `pqdriver` runs
//! stand-in for integration tests here, so every push verifies the
//! exact behaviors a user (or the Wine pass) exercises. Run by the
//! pre-push hook via `cargo test`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pqeditor::app::App;
use pqeditor::data::engine::Engine;

/// Unique temp dir per test.
fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pqeditor-scn-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One-column file: id = 0..rows.
fn make_parquet1(path: &Path, rows: i64) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT i AS id FROM range({rows}) t(i)) TO {}",
        pqeditor::data::types::sql_quote(&path.to_string_lossy())
    ))
    .unwrap();
}

/// Two-column file: id, label.
fn make_parquet2(path: &Path, rows: i64) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT i AS id, 'item_' || i AS label FROM range({rows}) t(i)) TO {}",
        pqeditor::data::types::sql_quote(&path.to_string_lossy())
    ))
    .unwrap();
}

/// Read a scenario from tools/scenarios and substitute the file tokens.
fn load_scenario(name: &str, p1: &Path, p2: Option<&Path>) -> String {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(repo.join("tools/scenarios").join(name)).expect("scenario file");
    let token1 = "<PARQUET>".to_string();
    let token2 = "<PARQUET2>".to_string();
    text.replace(
        &token2,
        &p2.map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "nonexistent.parquet".into()),
    )
    .replace(&token1, &p1.to_string_lossy())
}

/// Fail with the scenario's failure description and a state dump.
fn expect_pass(scenario: &str, result: Option<String>, app: &App) {
    if let Some(fail) = result {
        let mut buf = String::new();
        let _ = writeln!(buf, "scenario {scenario} FAILED: {fail}");
        let _ = writeln!(buf, "status: {:?}", app.status);
        let _ = writeln!(buf, "error: {:?}", app.error);
        if let Some(f) = app.file.as_ref() {
            let _ = writeln!(
                buf,
                "shown={} total={} overlay={} sort={:?} sel={:?}",
                f.grid.shown_rows,
                f.total_rows,
                f.grid.overlay.len(),
                f.grid.sort,
                f.selection.map(|s| (s.head.pos, s.head.col))
            );
        }
        panic!("{buf}");
    }
}

const TIMEOUT: Duration = Duration::from_secs(120);

/// Guard so a hung worker cannot stall the push indefinitely.
fn run_with_deadline(app: &mut App, scenario: &str, text: &str) -> Option<String> {
    let started = Instant::now();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let _ = writeln!(out, "[{scenario}] {line}");
        if let Err(e) = pqeditor::driver::run(app, line, &mut out) {
            return Some(format!("{e} (at: {line})"));
        }
        // Keep polling non-blocking so in-flight jobs continue; the
        // scenario actions themselves block up to 30s each on waits.
        if started.elapsed() > TIMEOUT {
            return Some("overall scenario timeout".into());
        }
        if matches!(
            line.split_whitespace().next(),
            Some("wait-save") | Some("wait-open") | Some("wait-count")
        ) {
            continue;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn scenario_drag_select() {
    let dir = tmp("drag-select");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("drag-select.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "drag-select", &text);
    expect_pass("drag-select", r, &app);
}

#[test]
fn scenario_set_null() {
    let dir = tmp("set-null");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("set-null.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "set-null", &text);
    expect_pass("set-null", r, &app);
    // The file now holds NULL in row 2.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let nulls: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet({}) WHERE id IS NULL",
                pqeditor::data::types::sql_quote(&p1.to_string_lossy())
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 1);
}

#[test]
fn scenario_edit_rejected() {
    let dir = tmp("edit-rejected");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("edit-rejected.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "edit-rejected", &text);
    expect_pass("edit-rejected", r, &app);
}

#[test]
fn scenario_edit_and_save() {
    let dir = tmp("edit-save");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("edit-and-save.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "edit-and-save", &text);
    expect_pass("edit-and-save", r, &app);
}

#[test]
fn scenario_sort_copy_paste() {
    let dir = tmp("sort-copy-paste");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("sort-copy-paste.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "sort-copy-paste", &text);
    expect_pass("sort-copy-paste", r, &app);
}

#[test]
fn scenario_delete_rows() {
    let dir = tmp("delete-rows");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("delete-rows.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "delete-rows", &text);
    expect_pass("delete-rows", r, &app);
    // The file on disk lost exactly rows 0 and 1.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let n: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet({})",
                pqeditor::data::types::sql_quote(&p1.to_string_lossy())
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 98);
}

#[test]
fn scenario_filter_box() {
    let dir = tmp("filter");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("filter-box.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "filter-box", &text);
    expect_pass("filter-box", r, &app);
}

#[test]
fn scenario_bad_filter() {
    let dir = tmp("bad-filter");
    let p1 = dir.join("one.parquet");
    make_parquet1(&p1, 100);
    let text = load_scenario("bad-filter.txt", &p1, None);
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "bad-filter", &text);
    expect_pass("bad-filter", r, &app);
}

#[test]
fn scenario_drop_column() {
    let dir = tmp("drop-col");
    let p1 = dir.join("two.parquet");
    make_parquet2(&p1, 100);
    let text = load_scenario("drop-column.txt", &p1, Some(&p1));
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, "drop-column", &text);
    expect_pass("drop-column", r, &app);
    // On disk only the id column survives.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let cols: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM (DESCRIBE SELECT * FROM read_parquet({}))",
                pqeditor::data::types::sql_quote(&p1.to_string_lossy())
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cols, 1);
}
