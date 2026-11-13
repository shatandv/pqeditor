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

## MORNING RUNBOOK (2026-11-13, execute after 09:00 UTC+3 — no heavy CPU
## was run overnight; everything below needs the compile-verify cycle)

STATE AT SLEEP:
- All fixes through "save rework" are COMMITTED AND PUSHED (138e5a5):
  interaction state machine, save/paste/scroll fixes, panic hook, build
  stamp, timing logs. dist/ had the build they tested (md5 c3c7b3d6).
- UNCOMMITTED, REVIEWED-BY-READING ONLY (no compile overnight): the
  SaveOutcome/PendingSave rework per DESIGN.md §5 — save() classifies
  Plain/RowsChanged/SchemaChanged at send time and snapshots the
  sort/filter carry for SchemaChanged; Done{Save} consumes it; Opened
  consumes pending_save (prevents a stale carry leaking to a later
  open). DESIGN.md added (requirements inventory + component contracts
  + sync contracts).
- The user wants: design-first refactor continued, all issues gone,
  deterministic checks committed and gating every push, a like-a-human
  Wine pass per round, and a WORKING exe in dist/ in the morning.

RUNBOOK (in order):
1. cargo fmt && cargo check --all-targets && cargo clippy
   --all-targets -- -D warnings && cargo test  (fix what the compiler
   finds; the overnight work — SaveOutcome rework, lib split, driver,
   scenarios — compiled zero times so far). tests/scenarios.rs runs the
   committed scenarios through src/driver.rs on the real worker.
2. Commit the rework once green ("design rework: lib split + driver +
   SaveOutcome"). Push only after the hook passes.
3. Refactor slices per DESIGN.md, one commit each, battery green
   between slices:
   a. Filter state: move draft/pending/error handling into one
      FileState block (it is already single-owner; just group the
      methods and pin with the existing filter tests).
   b. Scenario tests to add: save_with_deleted_rows (RowsChanged arm:
      overlay cleared, SetDeletedRows sent, count correct), then
      save_with_dropped_col (SchemaChanged arm: reopen, sort carried,
      schema len correct on disk).
   c. The driver mode (DESIGN §7/AGENTS item B) is OPTIONAL — the
      headless scenario tests cover most of it; only build it if time
      remains.
4. Wine + XTEST like-a-human pass on the REAL exe (build first,
   COPY TO dist/ AND VERIFY THE HASH — this bit twice now):
   open -> sort asc/desc/cycle -> edit under sort (Enter) -> commit ->
   save (expect NO flicker, values correct, sort intact) -> copy 2x2 ->
   paste -> delete row (rownum+Del) -> save -> drop column (RMB) ->
   save (re-open + sort carried) -> filter box (shown count, empty
   clear) -> export dialog (width) -> recents. Grab pixels per step.
   ALSO verify the save-reopen path can't stall like it did overnight
   (status transitions saving -> saved -> opened; the panic hook now
   logs to dist/pqeditor-panic.log if the worker dies).
5. Refresh dist/, report with the new md5 + size + build stamp.
6. CLEANUP at the end of every Wine pass: kill the exe, `wineserver
   -k`, kill the nested `Xwayland :99` — the user's monitor otherwise
   keeps showing stray test windows (they noticed one overnight).

CONSTRAINTS: no uncapped builds ever (machine froze once at 16-way);
.cargo/config.toml caps at 2 jobs. The user sleeps near the box until
09:00 UTC+3 — keep builds short and tell them what ran.
USER REPORT (stale exe): the user tested dist/pqeditor.exe from Nov 12
23:34 which predated the Enter/header/drag/clipboard/sort-count fixes —
every reported symptom matches the pre-fix behavior. dist/ has since
been refreshed with the final build (md5 b36da1180bbd5067f9ebde3a0318468d).
LESSON: refresh dist/ after EVERY rebuild round, before reporting (the
pitfall note exists; it was still missed). Add a visible BUILD STAMP so
a stale exe is instantly recognizable (prep below).

OBSERVATIONS FROM THE OVERNIGHT SESSION (all need clean reproduction —
they may be wine+glow artifacts, but treat as real until proven):

1. SAVE -> AUTO-REOPEN DIDN'T REFRESH THE VIEW (possible real bug):
   edit under sort -> Ctrl+S -> status showed "saved (1 edits applied)"
   but the grid kept the pre-save data and sort; disk WAS correctly
   rewritten (verified with a fresh instance: rowid 99999 score=1).
   So the Done{Save} -> Job::Open -> Opened -> replace FileState flow
   silently stalled. Suspicion: the worker died after save (panic is
   invisible: GUI-subsystem exe has no stderr) OR the Open job hung.
   ACTION: (a) add a panic hook writing to a log file next to the exe
   (Windows has no stderr!); (b) reproduce in the deterministic driver;
   (c) check whether save()'s atomic rewrite + immediate re-open races.

2. RENDER DEATH: instance 1's window went permanently black while the
   process stayed alive (after being fully occluded by another window
   on the WM-less Xserver). Could be a glow/egl artifact of this setup,
   could be an app-side context-loss bug. On real Windows with a WM
   this is unlikely to reproduce the same way, but note it.

3. INPUT DEATH: after a multi-step interaction, clicks stopped
   registering on an instance that still rendered fine. Suspect a stale
   egui modal-layer or focus state swallowing input, or harness
   flakiness (overlapping windows at 0,0 on the nested Xserver — XTEST
   clicks raced). The deterministic driver settles this.

4. Home/End with NO selection anchor at (0,0) instead of jumping
   (pre-existing: the None arm of move_selection ignores drow/dcol).
   Fix: when selection is None, treat drow/dcol as the target directly.

DETERMINISTIC VERIFICATION PLAN (the user explicitly asked for this —
priority for the next working session):

A. Extract the interaction state machine out of the grid draw closures
   into FileState/App methods (click_cell(pos,col,shift), drag_to,
   release_pointer, click_rownum, click_header, open_editor, ...).
   grid.rs closures become thin wrappers. Then the whole interaction
   layer is headless-testable: app tests drive clicks/drags/keys and
   assert selection/editing/overlay/status — no rendering, fully
   deterministic. This is the single highest-value change.

B. Scripted driver mode (env PQEDITOR_DRIVER=path/to/scenario.txt):
   actions per line (open <file> / click <pos> <col> / drag / key / save
   / wait_job / assert_status <text> / assert_selection ...); runs the
   REAL draw loop under eframe so layer/modal paths get exercised too,
   dumps per-step state (status, selection, overlay len, row under
   head) to stdout and exits nonzero on assert failure. Runs under
   Wine/XTEST-free; the Wine harness then only needs to launch it and
   read the result file.

C. Per-job timing logs: the worker logs every job's duration to the
   progress channel; status bar can show "opened in 0.8s". Settles the
   "slow file open" class of reports with numbers. Consider making
   Open async-cheap: return the schema fast, let count(*) fill in
   async (it already does everywhere else) so big/pathological files
   render rows before the count answers.

D. Visible build stamp: window title "pqeditor 0.1.0 (build
   2026-11-13 00:32)" via env!("BUILD_TS") written by build.rs — makes
   a stale exe instantly recognizable (this incident's root cause).

SMALL PREPS ALREADY CODED (uncommitted, compile+test in the morning):
- panic hook logging to <exe_dir>/pqeditor-panic.log (main.rs) + build
  stamp via build.rs shown in the window title
- Home/End with no selection jumps to the target (app.rs
  move_selection None arm) + pin test home_end_jump_without_selection
- INTERACTION STATE MACHINE EXTRACTED (app.rs FileState):
  pointer_on_cell / pointer_release / click_cell / double_click_cell /
  secondary_click_cell / click_rownum; grid.rs draw closures now just
  call these. Regression pins written: drag_then_click_does_not_extend
  (the user's stale-build symptom), click_away_commits_editor,
  rownum_click_selects_row_and_shift_extends.
- lib.rs note: build.rs now exists — `cargo test` compiles it; make
  sure the stamp shows in the title under Wine.
Do NOT push until the full battery + a Wine pass on the driver run
green.

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
- app.error lingers on the status bar's right even after a later
  successful action overwrote app.status (error has display priority).
  Clear app.error when a new action succeeds.
- Enter-to-edit opens the editor with the caret at the end, so typing
  appends to the old value (observed repeatedly under Wine). egui has
  no public select-all-on-open API; either plumb TextCursorState or
  open editors empty on Enter.
- Edit under sort keeps the row's old position until the next explicit
  sort; the header's arrow then disagrees with the visible order.
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
- Build stamp: build.rs stamps the WALL CLOCK and reruns when
  `.build-stamp` is touched. Both shipped-build flows (cross/
  build-windows.sh, tools/morning-run.sh) touch it. A plain `cargo
  build` may reuse the previous stamp. Verify a fresh exe with
  `strings dist/pqeditor.exe | grep -E "2026-11-13 [0-9]{2}:"`.
- Harness pitfalls (round Nov 13 morning): topng.py previously split
  the raw grab on \n bytes and truncated any frame whose pixels
  contain 0x0a (this caused the false "modals render black" note;
  modals render fine). Click offsets: ALL clicks need real = grab +
  (4, 30), including toolbar buttons at the top — a toolbar click at
  grab-y lands in the title bar otherwise.
