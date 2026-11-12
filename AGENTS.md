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

## PAUSED WORK (2026-11-12, resume here)

State: 18/18 tests green on Linux. Windows cross-build proven end-to-end.

FINDINGS (Nov 13):
- Root cause of the user's Windows failure found: the cc-based bundled
  build only compiles the parquet extension when the `parquet` feature
  is on (duckdb Cargo.toml). Without it, `read_parquet` is missing from
  the catalog on any machine that cannot autoload it from the network
  (Linux "worked" only because DuckDB silently downloaded
  parquet.duckdb_extension into ~/.duckdb at first query — a hidden
  network dependency, now removed by enabling the feature).
- Second issue: the static-init extension-registration constructor
  crashed on Windows (page fault at the same address in test binary and
  probe). Re-test after the parquet fix; if it still crashes, localize
  with a probe that calls duckdb_register_static_extension on each
  describe fn individually (core_functions/httplib/loadable syms exist
  in the windows lib).
Resume checklist:
1. Wait for the windows test build (with parquet feature); run under
   Wine from a dir with libc++.dll + libunwind.dll beside the test exe.
   Expect 18/18 green and no page fault.
2. Rebuild main exe (glow renderer + windows_subsystem=noconsole already
   in source). Wine-test the GUI: `cd dist && timeout 30 wine pqeditor.exe`
   (exit 124 or clean window = pass; grep log for `panicked`).
3. Self-contained exe experiment: stub libstdc++ (empty archive, llvm-ar
   rcs), RUSTFLAGS="-C link-arg=-static -C link-arg=-L<stubdir>".
   The duplicate-symbol failure earlier came from static libc++ colliding
   with the libstdc++ shim; -static + stub avoids the shim. Verify with
   llvm-objdump that libc++.dll/libunwind.dll are gone from imports, then
   Wine-test. If it fails, ship dist/ with the 2 DLLs (current state).
4. Commit, refresh dist/, report to user.
5. Linux: `cargo test` (parquet now static; no more ~/.duckdb download
   dependency) and `cargo run` once to sanity-check glow on X11.


## Build/run

```
cargo check / cargo test / cargo run --release
```

Release binary is self-contained (bundled DuckDB, bundled font).

## Windows cross-build (from this Linux box)

Toolchain: llvm-mingw in `~/.local/llvm-mingw-*` (self-contained tarball
from github.com/mstorsjo/llvm-mingw, no root), rust target
`x86_64-pc-windows-gnullvm` (installed via rustup). Linker config lives in
`.cargo/config.toml`. Build:

```
export PATH="$HOME/.local/llvm-mingw-20240619-ucrt-ubuntu-20.04-x86_64/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++
cargo build --release --target x86_64-pc-windows-gnullvm
```

Output: `target/x86_64-pc-windows-gnullvm/release/pqeditor.exe`. Do NOT
use the plain `x86_64-pc-windows-gnu` target: its link step wants GNU
libstdc++/libgcc which llvm-mingw does not ship. The C++ runtime for
DuckDB comes from llvm-mingw's libc++ (`-lc++`, emitted by
libduckdb-sys). Smoke-test locally with `wine pqeditor.exe`.
