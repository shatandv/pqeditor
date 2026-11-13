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

/// Grouped two-column file: grp = i % 5 (repeats), val = i — for
/// multi-column sort pins.
fn make_parquet3(path: &Path, rows: i64) {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT i % 5 AS grp, i AS val FROM range({rows}) t(i)) TO {}",
        pqeditor::data::types::sql_quote(&path.to_string_lossy())
    ))
    .unwrap();
}

fn fixture(name: &str, which: u8, rows: i64) -> (PathBuf, PathBuf, Option<PathBuf>) {
    let dir = tmp(&format!("{name}-fx{which}"));
    let p1 = dir.join("one.parquet");
    let p2 = dir.join("two.parquet");
    let p3 = dir.join("grp.parquet");
    match which {
        1 => make_parquet1(&p1, rows),
        2 => make_parquet2(&p2, rows),
        3 => make_parquet3(&p3, rows),
        _ => unreachable!(),
    }
    (p1, p2, Some(p3))
}

/// Read a scenario from tools/scenarios and substitute the file tokens.
fn load_scenario(name: &str, p1: &Path, p2: Option<&Path>, p3: Option<&Path>) -> String {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(repo.join("tools/scenarios").join(name)).expect("scenario file");
    let fallback = || "nonexistent.parquet".to_string();
    text.replace(
        "<PARQUET3>",
        &p3.map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(fallback),
    )
    .replace(
        "<PARQUET2>",
        &p2.map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(fallback),
    )
    .replace("<PARQUET>", &p1.to_string_lossy())
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

/// Run one scenario file against a fresh app + worker.
type DiskCheck<'a> = Option<&'a dyn Fn(&Path, &Path)>;

fn run_scenario(name: &str, which: u8, rows: i64, on_disk: DiskCheck<'_>) {
    let (p1, p2, p3) = fixture(name, which, rows);
    let text = load_scenario(name, &p1, Some(&p2), p3.as_deref());
    let engine = Engine::spawn().unwrap();
    let mut app = App::with_engine(engine, Vec::new());
    let r = run_with_deadline(&mut app, name, &text);
    expect_pass(name, r, &app);
    if let Some(check) = on_disk {
        check(&p1, &p2);
    }
}

fn parquet_count(path: &Path, extra: &str) -> i64 {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.query_row(
        &format!(
            "SELECT count(*) FROM read_parquet({}) {}",
            pqeditor::data::types::sql_quote(&path.to_string_lossy()),
            extra
        ),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn scenario_edit_and_save() {
    run_scenario("edit-and-save.txt", 1, 100, None);
}

#[test]
fn scenario_drag_select() {
    run_scenario("drag-select.txt", 1, 100, None);
}

#[test]
fn scenario_set_null() {
    run_scenario(
        "set-null.txt",
        1,
        100,
        Some(&|p1: &Path, _| {
            // The file now holds NULL in row 2.
            assert_eq!(parquet_count(p1, "WHERE id IS NULL"), 1);
        }),
    );
}

#[test]
fn scenario_edit_rejected() {
    run_scenario("edit-rejected.txt", 1, 100, None);
}

#[test]
fn scenario_sort_copy_paste() {
    run_scenario("sort-copy-paste.txt", 1, 100, None);
}

#[test]
fn scenario_delete_rows() {
    run_scenario(
        "delete-rows.txt",
        1,
        100,
        Some(&|p1: &Path, _| {
            // The file on disk lost exactly rows 0 and 1.
            assert_eq!(parquet_count(p1, ""), 98);
        }),
    );
}

#[test]
fn scenario_filter_box() {
    run_scenario("filter-box.txt", 1, 100, None);
}

#[test]
fn scenario_bad_filter() {
    run_scenario("bad-filter.txt", 1, 100, None);
}

#[test]
fn scenario_drop_column() {
    run_scenario(
        "drop-column.txt",
        2,
        100,
        Some(&|_: &Path, p2: &Path| {
            // On disk only the id column survives.
            let conn = duckdb::Connection::open_in_memory().unwrap();
            let cols: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM (DESCRIBE SELECT * FROM read_parquet({}))",
                        pqeditor::data::types::sql_quote(&p2.to_string_lossy())
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(cols, 1);
        }),
    );
}

#[test]
fn scenario_save_then_copy_paste() {
    run_scenario("save-then-copy-paste.txt", 1, 100, None);
}

#[test]
fn scenario_delete_save_copy_paste() {
    run_scenario("delete-save-copy-paste.txt", 1, 100, None);
}

#[test]
fn scenario_paste_block() {
    run_scenario("paste-block.txt", 2, 100, None);
}

#[test]
fn scenario_multi_sort() {
    run_scenario("multi-sort.txt", 3, 100, None);
}

#[test]
fn scenario_shift_click() {
    run_scenario("shift-click.txt", 2, 100, None);
}

#[test]
fn scenario_editor_text() {
    run_scenario("editor-text.txt", 1, 100, None);
}

#[test]
fn scenario_filter_popover() {
    run_scenario("filter-popover.txt", 1, 100, None);
}
