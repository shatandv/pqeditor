# AGENTS.md — working on pqeditor

This is the maintainer's guide (for humans and AI agents). Read it before
changing anything. DESIGN.md holds the product decisions; this file holds
the engineering conventions.

## Non-negotiable invariants

1. **The UI thread never touches DuckDB.** All SQL runs on the single
   worker thread in `src/data/engine.rs` behind the `Job`/`JobResult`
   channel protocol. No exceptions: adding a direct `Connection` call in
   UI code reintroduces freezes.
2. **Everything crossing the worker boundary is plain data** from
   `src/data/types.rs`. No duckdb types, no egui types outside `ui/`.
3. **Edits key off `__rowid`** (`row_number() OVER () - 1` over the open
   file). The overlay in `src/data/model.rs` is the only pending-edit
   surface; save replays it and rewrites the file atomically
   (tmp + rename). Never write the user's file non-atomically.
4. **Row requests carry an `epoch`**; answers for a stale epoch are
   dropped. Any sort/filter change bumps the epoch via `GridState::requery`.
   If you add a cache, give it the same treatment.
5. **SQL is built through `sql_quote`/`sql_ident` or prepared params.**
   User filter fragments are the exception (that is the feature); they
   are validated by running them (`Job::Count`) before being applied, and
   failures route back as `FailSource::Filter`.

## Layout

```
src/
  main.rs        eframe entry, global keyboard shortcuts, panel composition
  app.rs         App/FileState state + actions + worker-result polling
  theme.rs       palette + egui styling (design system; mirrors mockup.html)
  util.rs        small helpers (fmt_count)
  data/
    types.rs     CellValue/ColType/EditValue + validation + SQL quoting
    engine.rs    worker thread, Job protocol, SQL building, save/export
    model.rs     RowCache (windowed pages) + EditOverlay + GridState
  ui/
    grid.rs      the table (header events, cells, inline editing)
    panels.rs    toolbar, stats strip, status bar, start screen
    dialogs.rs   filter popover, export dialog, go-to-row
assets/fonts/    JetBrains Mono (OFL license), bundled
mockup.html      visual spec of the UI (open in a browser)
```

## Conventions

- egui 0.37 API quirks that matter here: `egui::CornerRadius` (not
  `Rounding`), `Frame::NONE` const, `Response::context_menu` +
  `ui.close()` for menus, `TableBuilder::body.rows` for the virtualized
  grid, `RichText` has no `append` (build strings first).
- The header closures in `ui/grid.rs` cannot borrow `FileState` (the body
  closure holds it), so header interactions push `HeaderEvent`s applied
  after the draw. Follow that pattern for anything touching file state
  from inside table closures.
- Errors are user-presentable one-liners (`short_error`). Route them:
  filter errors to `filter_error`, cell errors to `commit_error`,
  everything else to `App.error`. Never panic on bad user input.
- Tests: pure logic in-module (`types`, `model`, `util`); DuckDB
  integration in `tests/` (writes a temp parquet via the engine, then
  exercises open/sort/filter/save/export).
- Colors: use the `theme::` constants; the accent (`ACCENT`) means
  "user data state" (edit, sort, filter, pending) — do not spend it on
  decoration.
- Keep `Cargo.toml` versions pinned to what builds; the bundled DuckDB
  compile is the long pole (`~3-5 min` cold). Requires CMake + C++17
  (bundled feature). On this machine cmake lives in `~/.local/bin`.
- **Low-RAM builds (this machine, 12 GB):** parallelism is capped in
  `.cargo/config.toml` (`[build] jobs = 2` plus `NUM_JOBS`/`CMAKE_BUILD_PARALLEL_LEVEL`
  for the CMake step), so plain `cargo build` is safe. The bundled DuckDB
  build at 16-way parallelism spawns huge `cc1plus` processes that exhaust
  memory and freeze the machine. Raise the cap only on machines with more
  RAM.

## Verification checklist before declaring anything "works"

1. `cargo check` clean, `cargo test` green.
2. Generate a test parquet (see examples/), open in the app: rows appear,
   scroll to the far end fast, sort/filter, edit a cell, Ctrl+S, reopen:
   edit persisted, order unchanged.
3. Export CSV with a `;` delimiter: BOM present, delimiter correct.
   Export XLSX: opens in LibreOffice/Excel, filter row present.
4. Bad filter input: red error, previous filter intact.
5. Edit rejected for type mismatch: editor stays open, red message.

## CURRENT WORK (Nov 13 evening) — finishing feedback items #5 and #6

Everything else from the user's 11-point feedback list is DONE and
verified (see git log 546e181/9954153 and below). #5 + #6 are HALF-DONE;
the code does NOT compile right now. State:

DONE in this refactor:
- model.rs: `Sel { anchor: Cell, head: Cell }` rect selection
  (Sel::single, Sel::rect(total, ncols), is_whole_rows);
  EditOverlay gained deleted_rows/deleted_cols (BTreeSets), delete_rows,
  drop_col, is_row/col_deleted; GridState::copy_rect -> TSV;
  edit_source_text kept.
- engine.rs: Worker.deleted_rows + recreate_view() (view gets
  `WHERE __rowid NOT IN (...)` so count/rows/stats/top-values/export all
  see deletions); Job::SetDeletedRows { rows } added; Job::Save gained
  drop_cols; save() deletes rows from __pq_src and EXCLUDEs dropped
  columns; open() clears deleted_rows + calls recreate_view().
  NOTE: open() still has its own inline CREATE VIEW before
  recreate_view — harmless duplication.

STILL TO DO (compile blockers marked !!):
- !! worker_loop: arm for Job::SetDeletedRows { rows } (set
  worker.deleted_rows = rows; worker.recreate_view()).
- !! worker_loop: Job::Save arm must pass drop_cols through to
  worker.save(&overlay, &drop_cols, &progress).
- !! app.rs: Sel shape changed -> FileState::selection users break:
  begin_edit_at reads sel.rowid/col (Sel::single), App::move_selection,
  App::set_cell_null (make it apply to the whole rect), App
  copy_selection in main.rs (use grid.copy_rect), app_tests use
  Sel { rowid, col } -> Sel::single(..).
- app.rs new actions: delete_selected_rows() (overlay.delete_rows +
  Job::SetDeletedRows with the union + cycle requery + refresh_count),
  drop_selected_col(), paste_from_clipboard(text) (parse TSV, anchor at
  head, validate per column type, fill rect if 1-cell clipboard),
  copy via grid.copy_rect(sel, max_cells=1_000_000).
- !! grid.rs: draw_cell/rownum/header use file.selection fields —
  rework to Sel::rect rendering (rect cells BG_SELECT, head cell
  BG_SELECT_CELL + accent stroke); row-number click = select whole row
  (Sel from (r,0) to (r,ncols-1)); skip deleted cols in header loop +
  cells; Del key handling lives in main.rs.
- !! main.rs: handle_keys — Ctrl+C uses copy_rect; Del = delete rows
  (whole-row sel) or set NULL rect (cell sel); paste via
  `ctx.input(|i| i.raw.events.clone())` filter Event::Paste(text).
- Job::Save send in App::save must include drop_cols from overlay.
- Done{Save}: replace manual requery+refresh_count with
  open_path(path) (re-open refreshes schema after column drops;
  open_path no longer clears self.file so the grid doesn't flash —
  if not done yet, check).
- Confirm-discard (dialogs.rs): after overlay.clear() also send
  Job::SetDeletedRows { rows: vec![] } + refresh_count.
- grid.rs: begin_edit_at must refuse on is_row_deleted.

THEN: cargo check/test green (keep 20 tests), visual verify via the
Wine loop, rebuild exe, refresh dist/, commit, report.

VERIFICATION TOOLING (rebuild if /tmp was wiped):
- /tmp/opencode/xgrab.c -> xgrab: `gcc xgrab.c -o xgrab -lX11`;
  grabs an X11 window by id to raw RGB ("XRGB W H\n" header).
- Loop: ./cross/build-windows.sh (or cargo rustc --target
  x86_64-pc-windows-gnullvm -- -C link-arg=-L$PWD/cross/static
  -C link-arg=-Wl,-Bstatic), COPY exe to dist/ (stale-copy bit me),
  run `wine pqeditor.exe test-data.parquet` in background, find WID:
  `xwininfo -root -tree | grep pqeditor.exe` — grab the CHILD window
  named "pqeditor" (1280x800), NOT the 1x1 one; python+zlib to wrap
  RGB into PNG; then read the PNG.
- cua-driver get_window_state works for the LINUX binary (AT-SPI) but
  NOT for wine; screenshots don't work on this box otherwise.
- eframe __screenshot saves a white buffer here (Wayland) — useless.

CONTEXT: user's 11 feedback items: 1-4,7-11 done; 5 = rect select +
copy/paste; 6 = row/col select + delete rows/drop column. User likes
fast feedback loops; compaction may happen.


## Build/run

```
cargo check / cargo test / cargo run --release
```

Release binary is self-contained (bundled DuckDB, bundled font).

## Windows cross-build (from this Linux box)

Toolchain: llvm-mingw in `~/.local/llvm-mingw-*` (self-contained tarball
from github.com/mstorsjo/llvm-mingw, no root), rust target
`x86_64-pc-windows-gnullvm` (installed via rustup). Linker config lives in
`.cargo/config.toml`. Build with the wrapper script, which also refreshes
`dist/`:

```
./cross/build-windows.sh
```

Output: a fully self-contained `dist/pqeditor.exe` — C++ runtime
(libc++/libunwind) is statically linked via an early `-L` to a
dir holding only the static `.a` archives + `-Wl,-Bstatic`; the only DLL
imports left are standard Windows system DLLs (Win10+ ucrt set). PE
subsystem is WINDOWS_GUI, so double-clicking opens just the app, no
console.

Pitfalls (all hit, all documented so they stay fixed):
- Do NOT use the plain `x86_64-pc-windows-gnu` target: GNU libstdc++/
  libgcc do not exist in llvm-mingw. `gnullvm` is the supported pairing.
- `duckdb` needs the `parquet` feature or `read_parquet` is missing from
  the catalog entirely (would otherwise rely on a silent network
  autoload into ~/.duckdb).
- Forcing `-lc++` static via a `-Wl,-Bstatic,-lc++` group duplicates
  symbols with the libstdc++ shim path; the early `-L` static-dir trick
  (script above) is the working approach.
- Wine smoke test: `cd dist && timeout 30 wine pqeditor.exe` — exit 124
  or a clean window = pass; grep the log for `panicked`/`import_dll`.
