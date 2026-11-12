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

## FEEDBACK ROUND 2 (Nov 13-14) — ALL FIXED, verified via Wine+XTEST loop

USER RULE (permanent): before fixing anything, add tests that pin
correct behavior; run the full battery after every round; do a full
functional pass (open/sort/filter/edit/save/copy/paste/delete/drop/
export) via the Wine loop before shipping. There is a pre-push hook now
(.githooks/pre-push via `git config core.hooksPath .githooks`): fmt +
clippy -D warnings + full test suite run on every push.

WHAT THE ROUND FOUND (root causes, all fixed + pinned by tests):
- Position-vs-rowid mapping: the grid addressed rows by display position
  but keyed edits/deletes by rowid. Job::Rows now returns true __rowid
  per row; cache pages store (rowid, cells); Cell.pos is the view
  position and actions resolve pos -> rowid via GridState::rowid_at.
  Row-number column displays the true rowid (stable under sort).
  App tests: sorted_edit_lands_on_the_row_you_see,
  sorted_delete_removes_the_row_you_see, sorted_view_shows_sorted_values.
- Sort cycle could never leave ascending (cycle_sort cleared the stack
  first). Fixed; header RMB has a real sort-descending now. Test:
  sort_click_cycles_asc_desc_off.
- Enter-to-start-edit flashed and committed itself: the editor gained
  focus in its creation frame while the initiating Enter was still in
  the input state -> immediate commit. EditState.created_frame makes the
  editor ignore its creation frame's Enter. Focus guard: click-away
  commit only after the editor really held focus (EditState.had_focus).
- Header clicks AND row-number clicks dead: selectable text labels
  swallowed the clicks. ALL grid labels (cells, headers, rownum) are now
  Label::selectable(false).sense(hover).
- Drag-select never extended: is_pointer_button_down_on is only true for
  the press-origin widget. Now: primary_down + rect_contains_pointer;
  drag_select is cleared on any pointer release (a drag's release never
  lands as clicked()).
- Copy/paste never fired: egui-winit converts Ctrl+C/X/V into
  Event::Copy/Cut/Paste and emits NO key event. handle_keys now reacts
  to those events (when no text field is focused). egui's own clipboard
  is used (arboard dropped from Cargo.toml).
- Sort/RMB-sort blanked the table: the header apply path called
  FileState::cycle_sort which requeries but never sends count(*) ->
  cache total stayed 0 -> "no rows". grid.rs now sends the count after
  sort events. Also the earlier broken cycle hid this.
- "shown" count: there were TWO shown_rows fields (FileState + GridState);
  the filter path updated the grid one, the status bar read the file one.
  Now only GridState::shown_rows exists. Empty-draft submit clears the
  active filter directly (no validation roundtrip). Tests:
  filter_updates_shown_count, empty_submit_clears_active_filter.
- Export modal runaway width: Modal auto-sizes to content; the path
  TextEdit at desired_width(INFINITY) fed back into the width every
  frame. Now ui.set_width(480.0) pins the content width.

VERIFICATION TOOLING (rebuild if /tmp was wiped):
- /tmp/opencode/xgrab.c -> xgrab: `gcc xgrab.c -o xgrab -lX11`; grabs an
  X11 window by id to raw RGB ("XRGB W H\n" header). /tmp/opencode/
  topng.py wraps RGB into PNG. /tmp/opencode/xtest.c -> xtest:
  `gcc xtest.c -o xtest -lX11 -lXtst` — XTEST injector (move/click/drag/
  key/combo/type/down/up/focus/activate/getfocus/moveresize).
- IMPORTANT: this box is Wayland; cua-driver cannot target XWayland
  windows and XTEST on the main :0 display does NOT reach wine. The
  working setup: nested `Xwayland :99 -geometry 1280x800`, run wine
  apps with DISPLAY=:99, grab/inject on :99. Wine windows may need
  `moveresize <wid> 0 0 1270 790` to fit; after XMoveResizeWindow the
  app's cached position is stale by the OLD offset — clicks must be
  real_x = egui_x + 4, real_y = egui_y + 30. Keyboard focus is flaky:
  verify a key took effect (e.g. Down creates a selection) before
  trusting any keyboard-driven test; refocus and retry if not.
- Modal/context-menu surfaces do not render under wine+glow on :99
  (black body) — environment artifact; modal width is verifiable from
  the frame outline. eframe __screenshot saves a white buffer here.

KNOWN LEFTOVERS (candidates for next round):
- Wine keyboard focus flakiness is harness-side, but the app could
  re-request focus after clicks if real Windows shows anything similar.
- Unsaved-changes confirm dialog renders (logic verified round 1); the
  Wine loop can't paint modals so re-verify on real Windows.
- Status bar "rows" shows the file total; after unsaved deletions the
  view total only appears via "shown" with a filter. Consider showing
  the live view count.
- Visual polish toward mockup.html (#10) remains soft.


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
