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

## STATUS NOTE (keep updated; compaction at 50% context)

All 11 feedback items from Nov 13 are DONE (22 tests green, exe rebuilt
in dist/). The big ones: #5 rect selection + TSV copy/paste, #6 row
deletion (view-level exclusion + atomic save) / column drop (client-side
hide + EXCLUDE on save; indices stay stable), toolbar layout bug
(panels are top-down by default — ui.horizontal fixed it), sort sending
a fresh count(*), unsaved-changes dialog, stable cell-editor id.
Rowids are stable through deletion because the view computes
row_number() in a subquery and filters outside it.

Still open / candidate next steps:
- #10 visual polish toward mockup.html (toolbar spacing, stats strip
  styling, start screen).
- Close-with-unsaved-edits is wired; verify once more on real Windows.
- No "shown/filtered" split in the status bar for deleted-row counts
  (the view already excludes them; counts are correct).
- Wine+X11 grab loop is the visual verification tool for the exe
  (see cross/ and /tmp/opencode/xgrab.c — rebuild if /tmp wiped).
- multi-file tabs, undo stack, visual-order navigation remain future work.


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
