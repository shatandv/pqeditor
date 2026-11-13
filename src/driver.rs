//! Scripted scenario interpreter: the deterministic verification layer.
//!
//! One action per line (see `src/bin/pqdriver.rs` for the grammar, and
//! `tools/scenarios/` for the committed scenarios). The SAME interpreter
//! runs:
//! - `cargo test` integration tests (tests/scenarios.rs) — on every push
//!   via the pre-push hook,
//! - `pqdriver <file>` — headless here, and under real Windows against
//!   the actual exe.
//!
//! It drives the REAL App + worker through the same command methods the
//! UI uses; no rendering is involved.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::App;

/// Run one action line. `out` receives copy results and diagnostics.
pub fn run(app: &mut App, line: &str, out: &mut impl Write) -> Result<(), String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.as_slice() {
        ["open", path] => {
            app.open_path(PathBuf::from(path));
            Ok(())
        }
        ["wait-open"] => poll_until(app, |a| a.file.is_some() && a.opening.is_none(), "open"),
        ["page0"] => {
            let (epoch, sort, filter) = {
                let f = need_file(app)?;
                (
                    f.grid.cache.epoch(),
                    f.grid.sort.clone(),
                    f.grid.filter.clone(),
                )
            };
            app.engine.send(crate::data::engine::Job::Rows {
                epoch,
                page: 0,
                sort,
                filter,
            });
            poll_until(
                app,
                |a| {
                    a.file
                        .as_mut()
                        .map(|f| f.grid.cache.page(0).is_some())
                        .unwrap_or(false)
                },
                "page 0",
            )
        }
        ["sort", col, dir] => {
            let col: usize = col.parse().map_err(|_| "bad column".to_string())?;
            let ascending = parse_sort(dir)?;
            app.set_sort(col, ascending);
            Ok(())
        }
        ["sort-add", col, dir] => {
            let col: usize = col.parse().map_err(|_| "bad column".to_string())?;
            let ascending = parse_sort(dir)?;
            app.add_sort(col, ascending);
            Ok(())
        }
        ["sort-remove", col] => {
            let col: usize = col.parse().map_err(|_| "bad column".to_string())?;
            app.remove_sort_key(col);
            Ok(())
        }
        ["popover", col] => {
            let col: usize = col.parse().map_err(|_| "bad column".to_string())?;
            let f = need_file(app)?;
            f.popover = Some(crate::app::FilterPopover::new(col));
            let filter = f.grid.filter.clone();
            app.engine.send(crate::data::engine::Job::TopValues {
                col,
                limit: 200,
                filter,
            });
            poll_until(
                app,
                |a| {
                    a.file
                        .as_ref()
                        .and_then(|f| f.popover.as_ref())
                        .map(|p| !p.loading)
                        .unwrap_or(false)
                },
                "popover values",
            )
        }
        ["popover-check", value @ ..] => {
            let value = value.join(" ");
            let f = need_file(app)?;
            let pop = f
                .popover
                .as_mut()
                .ok_or_else(|| "no popover open".to_string())?;
            pop.checked.insert(value);
            Ok(())
        }
        ["popover-check-null"] => {
            let f = need_file(app)?;
            let pop = f
                .popover
                .as_mut()
                .ok_or_else(|| "no popover open".to_string())?;
            pop.checked.insert(crate::app::NULL_SENTINEL.into());
            Ok(())
        }
        ["popover-apply"] => {
            app.apply_popover();
            poll_until(
                app,
                |a| {
                    a.file
                        .as_ref()
                        .map(|f| f.pending_filter.is_none())
                        .unwrap_or(false)
                },
                "popover filter",
            )
        }
        ["popover-close"] => {
            need_file(app)?.popover = None;
            Ok(())
        }
        ["sort-append", col, dir] => {
            let col: usize = col.parse().map_err(|_| "bad column".to_string())?;
            let ascending = parse_sort(dir)?;
            app.cycle_sort(col, true);
            if !ascending {
                // cycle appends asc; a desc append needs the direct setter.
                need_file(app)?.set_sort_desc(col);
            }
            Ok(())
        }
        ["filter", rest @ ..] => {
            let sql = rest.join(" ");
            let f = need_file(app)?;
            f.filter_draft = sql;
            app.submit_filter();
            // Validation done = candidate consumed (applied on success,
            // cleared with an error on failure; assert-error checks that).
            poll_until(
                app,
                |a| {
                    a.file
                        .as_ref()
                        .map(|f| f.pending_filter.is_none())
                        .unwrap_or(false)
                },
                "filter",
            )
        }
        ["filter-clear"] => {
            let f = need_file(app)?;
            f.filter_draft.clear();
            app.submit_filter();
            Ok(())
        }
        ["click", pos, col] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let f = need_file(app)?;
            f.pointer_on_cell(pos, col, false);
            f.pointer_release();
            f.click_cell(pos, col);
            Ok(())
        }
        ["click-row", pos] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            need_file(app)?.click_rownum(pos, false);
            Ok(())
        }
        ["shift-click-row", pos] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            need_file(app)?.click_rownum(pos, true);
            Ok(())
        }
        ["drag", pos, col] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            need_file(app)?.pointer_on_cell(pos, col, false);
            Ok(())
        }
        ["press", pos, col] => {
            // Begin a pointer press on a cell (drag-select start).
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            need_file(app)?.pointer_on_cell(pos, col, false);
            Ok(())
        }
        ["release"] => {
            need_file(app)?.pointer_release();
            Ok(())
        }
        ["begin-edit", pos, col] => {
            // Open the editor without touching its text: pins what the
            // editor starts with (the filtered/sorted empty-editor bug).
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let f = need_file(app)?;
            f.begin_edit_at(pos, col);
            if f.editing.is_none() {
                return fail("editor did not open".into());
            }
            Ok(())
        }
        ["editor-close"] => {
            need_file(app)?.editing = None;
            Ok(())
        }
        ["assert-editor-text", expect @ ..] => {
            let expect = expect.join(" ");
            let got = need_file(app)?
                .editing
                .as_ref()
                .map(|e| e.text.clone())
                .unwrap_or_else(|| "<no editor>".into());
            if got != expect {
                return fail(format!("editor text = {got:?} (want {expect:?})"));
            }
            Ok(())
        }
        ["dblclick", pos, col] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let f = need_file(app)?;
            f.double_click_cell(pos, col);
            if f.editing.is_none() {
                return fail("double-click did not open the editor".into());
            }
            Ok(())
        }
        ["edit", pos, col, text @ ..] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let text = unescape(&text.join(" "));
            let f = need_file(app)?;
            f.begin_edit_at(pos, col);
            match f.editing.as_mut() {
                Some(e) => {
                    e.text = text;
                }
                None => return fail("editor did not open (read-only or loading?)".into()),
            }
            f.commit_editing();
            Ok(())
        }
        ["set-null"] => {
            app.set_cell_null();
            Ok(())
        }
        ["press-del"] => {
            // Del semantics live in main.rs; the driver re-states them so
            // the scenarios cover the same behavior.
            let whole_rows = {
                let f = need_file(app)?;
                f.selection
                    .as_ref()
                    .is_some_and(|s| s.is_whole_rows(f.grid.schema.len()))
            };
            if whole_rows {
                app.delete_selected_rows();
            } else {
                app.set_cell_null();
            }
            Ok(())
        }
        ["drop", col] => {
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            need_file(app)?.drop_col(col);
            Ok(())
        }
        ["paste", text @ ..] => {
            app.paste_tsv(&unescape(&text.join(" ")));
            Ok(())
        }
        ["copy"] => {
            let text = {
                let f = need_file(app)?;
                let sel = f
                    .selection
                    .as_ref()
                    .ok_or_else(|| "no selection to copy".to_string())?;
                f.grid.copy_rect(sel, 1_000_000)
            };
            match text {
                Ok(t) => {
                    let _ = writeln!(out, "copied: {t:?}");
                    Ok(())
                }
                Err(msg) => fail(msg),
            }
        }
        ["save"] => {
            app.save();
            Ok(())
        }
        ["wait-save"] => poll_until(
            app,
            |a| {
                !a.saving
                    && a.opening.is_none()
                    && a.file
                        .as_ref()
                        .map(|f| f.grid.overlay.is_empty())
                        .unwrap_or(false)
            },
            "save",
        ),
        ["wait-count", n] => {
            let n: u64 = n.parse().map_err(|_| "bad number".to_string())?;
            poll_until(
                app,
                |a| {
                    a.file
                        .as_ref()
                        .map(|f| f.grid.shown_rows == n)
                        .unwrap_or(false)
                },
                "count",
            )
        }
        ["assert-count", n] => {
            let n: u64 = n.parse().map_err(|_| "bad number".to_string())?;
            let got = need_file(app)?.grid.shown_rows;
            if got != n {
                return fail(format!("shown_rows = {got} (want {n})"));
            }
            Ok(())
        }
        ["assert-total", n] => {
            let n: u64 = n.parse().map_err(|_| "bad number".to_string())?;
            let got = need_file(app)?.total_rows;
            if got != n {
                return fail(format!("total_rows = {got} (want {n})"));
            }
            Ok(())
        }
        ["assert-cell", pos, col, expect @ ..] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let expect = expect.join(" ");
            let got = need_file(app)?
                .grid
                .cell(pos, col)
                .map(|c| c.value.display())
                .unwrap_or_else(|| "<not fetched>".into());
            if got != expect {
                return fail(format!("cell({pos},{col}) = {got:?} (want {expect:?})"));
            }
            Ok(())
        }
        ["assert-page", pos, col, expect @ ..] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let expect = expect.join(" ");
            let got = {
                let f = need_file(app)?;
                let page = pos / crate::data::model::PAGE_SIZE as u64;
                let idx = (pos % crate::data::model::PAGE_SIZE as u64) as usize;
                f.grid
                    .cache
                    .page(page)
                    .and_then(|rows| rows.get(idx))
                    .and_then(|(_, cells)| cells.get(col))
                    .map(|c| c.display())
                    .unwrap_or_else(|| "<not fetched>".into())
            };
            if got != expect {
                return fail(format!("page({pos},{col}) = {got:?} (want {expect:?})"));
            }
            Ok(())
        }
        ["assert-selected", pos, col] => {
            let pos: u64 = pos.parse().map_err(|_| "bad pos".to_string())?;
            let col: usize = col.parse().map_err(|_| "bad col".to_string())?;
            let sel = need_file(app)?
                .selection
                .ok_or_else(|| "no selection".to_string())?;
            if sel.head.pos != pos || sel.head.col != col {
                return fail(format!(
                    "head = ({},{}) (want ({pos},{col}))",
                    sel.head.pos, sel.head.col
                ));
            }
            Ok(())
        }
        ["assert-no-selection"] => {
            if need_file(app)?.selection.is_some() {
                return fail("expected no selection".into());
            }
            Ok(())
        }
        ["assert-editor-open"] => {
            if need_file(app)?.editing.is_none() {
                return fail("expected the editor to stay open".into());
            }
            Ok(())
        }
        ["assert-editor-closed"] => {
            if need_file(app)?.editing.is_some() {
                return fail("expected the editor to be closed".into());
            }
            Ok(())
        }
        ["assert-overlay", n] => {
            let n: usize = n.parse().map_err(|_| "bad number".to_string())?;
            let got = need_file(app)?.grid.overlay.len();
            if got != n {
                return fail(format!("overlay = {got} (want {n})"));
            }
            Ok(())
        }
        ["assert-schema", n] => {
            let n: usize = n.parse().map_err(|_| "bad number".to_string())?;
            let got = need_file(app)?.grid.schema.len();
            if got != n {
                return fail(format!("schema = {got} cols (want {n})"));
            }
            Ok(())
        }
        ["assert-status", expect @ ..] => {
            let expect = expect.join(" ");
            if !app.status.starts_with(&expect) {
                return fail(format!("status = {:?} (want {:?})", app.status, expect));
            }
            Ok(())
        }
        ["assert-error", expect @ ..] => {
            let expect = expect.join(" ");
            match &app.error {
                Some(e) if e.starts_with(&expect) => Ok(()),
                Some(e) => fail(format!("error = {e:?} (want {expect:?})")),
                None => fail("expected an error, got none".into()),
            }
        }
        ["assert-filter-error", expect @ ..] => {
            let expect = expect.join(" ");
            match need_file(app)?.filter_error.as_deref() {
                Some(e) if e.starts_with(&expect) => Ok(()),
                Some(e) => fail(format!("filter error = {e:?} (want {expect:?})")),
                None => fail("expected a filter error, got none".into()),
            }
        }
        _ => fail("unknown action".into()),
    }
}

/// Run a whole scenario (newline-separated actions), fail-fast.
/// Returns the failing line description, or None on success.
pub fn run_scenario(app: &mut App, text: &str, out: &mut impl Write) -> Option<String> {
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(msg) = run(app, line, out) {
            return Some(format!("line {}: {msg}: {line}", i + 1));
        }
    }
    None
}

fn fail<T>(msg: String) -> Result<T, String> {
    Err(msg)
}

fn need_file(app: &mut App) -> Result<&mut crate::app::FileState, String> {
    app.file.as_mut().ok_or_else(|| "no file open".to_string())
}

fn poll_until(
    app: &mut App,
    mut done: impl FnMut(&mut App) -> bool,
    what: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(app) {
        app.poll();
        if Instant::now() > deadline {
            return fail(format!("timeout waiting for {what}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn parse_sort(dir: &str) -> Result<bool, String> {
    match dir {
        "asc" => Ok(true),
        "desc" => Ok(false),
        other => fail(format!("bad sort direction '{other}'")),
    }
}

fn unescape(s: &str) -> String {
    s.replace("\\t", "\t").replace("\\n", "\n")
}
