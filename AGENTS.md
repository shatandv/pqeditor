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

## FEEDBACK ROUND 2 (Nov 13 late) — DIAGNOSIS + PLAN (tests first!)

USER RULE (permanent, this is my codebase): before fixing anything, add
tests that pin currently-CORRECT behavior; run the full battery after
every round; do a full functional pass (open/sort/filter/edit/save/copy/
paste/delete/drop/export) via the Wine+xgrab loop before shipping.

ROOT CAUSE identified (fix FIRST, explains #3/#4/#6 + latent sort bug):
the grid addresses cells by VIEW POSITION (row_idx from egui_extras) but
the model treats it as the ORIGINAL rowid. Under sort (display order !=
file order) and after row deletion (view excludes rows) every cell
lookup hits the wrong row: copy/delete act on wrong rows, sorted data is
silently wrong. FIX: Job::Rows must return the true __rowid per row
(engine: add __rowid to the rows SELECT and split it out), cache pages
store (rowid, cells), model addresses by position and keys overlay by
true rowid; Sel = view positions; delete_selected_rows maps positions
to rowids through the cache. Add an App-level test: sort by column, then
assert the first displayed row's DATA actually changed.

Other items:
- #1 Enter-starts-edit flashes and disappears + phantom edit marker:
  not yet explained. commit_editing no-ops on unchanged text, so
  something sets text != initial. Reproduce via Wine + cua-driver keys
  (wine windows ARE X11, XTEST works) + xgrab screenshots per step.
- #2 LMB click/double-click dead: suspect the drag-select press block
  (is_pointer_button_down_on) fighting clicks + micro-movement extends
  the rect to neighbors (user's screenshot shows exactly that). 
  Rework: press sets anchor only if drag not active; only extend when
  the pointer actually moves to a different cell while down.
- #3 copy/paste: copy uses the rect (root cause above). Paste: egui
  Event::Paste may not fire on Windows — plan: use the `arboard` crate
  for clipboard read/write instead of relying on egui events.
- #5 drag-select selects text like a paragraph: egui labels are text-
  selectable now; use Label::selectable(false) on cell labels (verify
  the API in egui 0.37).
- #6 sort/RMB-sort blanks the table: verify after the mapping fix; add
  App-level test asserting first-row data changes after sort.
- #7 shown count wrong + empty-filter Enter doesn't clear: add app test
  for submit_filter -> shown_rows; empty draft with an active filter
  must clear it (currently early-returns because both are None).
- #8 Export modal grows on every input: auto-sized Modal + TextEdit
  desired_width(INFINITY) = runaway width. Fix: Modal with fixed size
  (Modal::area(Area::new(id).fixed_size([480, 0]))) and finite TextEdit
  widths.

PUSHED: https://github.com/shatandv/pqeditor.git (main). dist/ is
gitignored (57MB binary); cross/static .a files are committed.

GITHUB PUSH PROCEDURE: git remote add origin
https://github.com/shatandv/pqeditor.git && git push -u origin main
(auth via gh or stored credentials — check what works).


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
