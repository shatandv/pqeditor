# pqeditor — System Design

The product contract, the component map, and the sync contracts that keep
the UI correct. Every rule here is enforced either by the test battery or
by the pre-push hook. This document is the arbiter when a fix would fight
the structure: change the structure or change the fix, not the rule.

## 1. Product requirements (accumulated, verbatim-intent)

File handling
- R1 Open a parquet file (dialog, CLI arg, drag-drop, recents). Anything
  that is not readable parquet is rejected with a short, presentable
  error.
- R2 10M+ rows must open and scroll smoothly. The UI thread never blocks
  on the file.
- R3 Export the current view to CSV (BOM, delimiter choice) and XLSX
  (bold frozen header, autofilter, autofit, typed cells).

Editing
- R4 Edit a cell in place (double-click, Enter, F2, context menu). Values
  validate against the column type; a rejected editor stays open with a
  visible error; an unchanged commit is a no-op.
- R5 Enter starts editing; Enter commits; Escape cancels; click-away
  commits. The editor must not self-commit in its creation frame.
- R6 Rect selection: click selects, shift+click extends, drag selects,
  arrow keys move, shift+arrows extend, row-number click selects the
  whole row, header menu selects a column.
- R7 Copy (Ctrl+C) = TSV of the selection; Paste (Ctrl+V) fills at the
  anchor; a single value fills the whole selection. Paste is
  all-or-nothing on validation.
- R8 Delete on a row selection deletes those rows; on cells sets NULL.
- R9 Drop a column (header menu). Dropped columns are hidden client-side
  and excluded from save.

Sorting / filtering
- R10 Sort by clicking a header (asc → desc → off cycle, shift appends);
  RMB menu sorts asc/desc; sort chips in the toolbar.
- R11 SQL WHERE filter box: validated by running it (Count) before
  applying; errors are short and keep the previous filter. Submitting an
  empty box clears the filter.
- R12 Per-column value popover (top values, check to filter).

Save
- R13 Ctrl+S saves atomically (tmp + rename). After a plain save the
  view is unchanged on screen (no reload, no flicker); after a save with
  dropped columns the schema refreshes and sort/filter carry over.
- R14 Unsaved-changes confirmation on quit and on switching files.
- R14a Undo (Ctrl+Z) / redo (Ctrl+Shift+Z, Ctrl+Y) with a comfortable
  amount of history (200 actions). Undo never writes the user's file;
  saving deletes or dropped columns ends the history for that file
  (user-accepted trade-off). Sorts/filters are not undoable; the view
  jumps to the row the last step touched.
- R14b Insert rows (toolbar "+ Row", row-number right-click above or
  below). New rows exist only in the overlay until saved (synthetic
  rowids, spliced into the view at their anchors); undoable; save
  materializes them at their anchors.

Status / feedback
- R15 Status bar: total rows, filtered "shown" count, unsaved-edit
  count, transient status/errors. The shown count always matches the
  active filter.
- R16 Column statistics strip; true file rowids in the row-number column
  (stable under sort).

Non-negotiable invariants (see AGENTS.md — unchanged)
- The UI thread never touches DuckDB; everything crossing the worker
  boundary is plain data; edits key off true `__rowid`; row requests
  carry an epoch; SQL is quoted; saves are atomic.

## 2. Components

```
┌────────────────────────────────────────────────────────────┐
│ main.rs        eframe entry, global keys, panel composition │
├────────────────────────────────────────────────────────────┤
│ ui/*           RENDER ONLY: reads state, calls Commands      │
│   grid.rs       table (delegates all pointer logic)          │
│   panels.rs     toolbar, stats, status, start screen         │
│   dialogs.rs    confirm, filter popover, export, goto        │
├────────────────────────────────────────────────────────────┤
│ app.rs         App: Command layer + result application       │
│   App           owns Engine + FileState + flows              │
│   FileState     one open file: view state + model            │
│   interaction   pointer_on_cell/release/click_cell/...       │
│   state machine (the ONLY writers of selection/drag/edit)    │
├────────────────────────────────────────────────────────────┤
│ data/model.rs  GridState: schema, sort, filter, cache,       │
│                overlay, shown_rows (pure data + pure fns)    │
│ data/engine.rs worker thread: DuckDB, Job protocol           │
│ data/types.rs  CellValue/EditValue, validation, SQL quoting  │
└────────────────────────────────────────────────────────────┘
```

Layer rules:
1. `ui/*` never mutates state directly except through `FileState`/`App`
   command methods. Deferred events (HeaderEvent) exist only because
   egui table closures cannot borrow the app; they map 1:1 to commands.
2. `data/model.rs` holds no egui types and no I/O; it is fully
   deterministic and unit-tested.
3. `app.rs` owns ALL effectful decisions: what to send to the worker and
   how to apply each worker result. This is the single file allowed to
   know about the job protocol's lifecycle.

## 3. View-state ownership (the part that went ad-hoc)

One owner per piece of state; nothing duplicated:

| State | Owner | Notes |
|---|---|---|
| schema, sort, filter, cache, overlay, shown_rows | `GridState` | the model |
| selection, drag_select | `FileState` | written only by the interaction state machine |
| editing (pos/rowid/text/initial/had_focus/created_frame) | `FileState` | written by begin/commit/cancel |
| scroll request (`ScrollRequest`) | `FileState` | enum: Center/Edge, never a bare row |
| visible_rows (last rendered range) | `FileState` | written by grid draw each frame |
| filter draft / pending validation | `FileState` | applied filter lives in GridState.filter |
| total_rows (file size) | `FileState` | set once at Open, unchanged by filters |
| count_pending | `FileState` | one count in flight per epoch |
| total_rows vs shown_rows | NEVER duplicated | shown lives only in GridState |

Scroll semantics (R2, R-fix from feedback): single steps scroll only
when the selection leaves the visible area, by one row; jumps center;
animation off; two pages prefetched.

## 4. Worker protocol and the view-sync contract

Jobs: Open, Count, Rows, Stats, TopValues, SetDeletedRows, Save, Export.
Every result carries its epoch (Count/Rows) or completes a flow.

The contract for applying results (implemented in `App::poll`):

- `Opened` → build FileState, seed cache total, apply `reopen_carry`
  (sort/filter) if this Open came from a save-with-schema-change.
- `Count` → if a filter candidate is pending: apply it; else (epoch
  match) update shown_rows + cache total. Stale epochs are dropped.
- `Rows` → put page (epoch-checked). Pages hold `(rowid, cells)`;
  addressing is by view position, rowids resolve overlay/save keys.
- `Done{Save}` → classified by the save outcome (see §5):
  - Plain: patch fetched pages with the saved values (no reload).
  - RowsChanged: reset the worker's deleted-rows filter (rowids of
    survivors are unchanged; positions and pages stay valid).
  - SchemaChanged: re-open, carrying sort/filter.
- `Failed{Filter}` → pending_filter cleared, error surfaced, previous
  filter intact.

## 5. The save flow (one state machine, no ad-hoc branches)

Classification happens when the job is SENT, not when it completes:

```
SaveOutcome { Plain, RowsChanged, SchemaChanged }
  SchemaChanged  iff overlay.deleted_cols non-empty
  RowsChanged    else iff overlay.deleted_rows non-empty
  Plain          otherwise
```

- `App::save()` computes and stores the outcome + the edit snapshot.
- `Done{Save}` consumes the stored outcome. No re-derivation from
  half-cleared state. This killed the flicker class of bugs.
- Rowids are stable across a plain save; positions are stable unless
  rows were deleted; the schema only changes with dropped columns.

## 6. Undo/redo (command history, in-memory only)

Per-file `History` on FileState: two stacks of `HistoryEntry` (label +
inverse `OverlayOp` lists + focus rowid), capped at 200, redo stack
cleared on every new action. Undo applies the undo ops to the overlay;
redo applies the redo ops to the overlay. Nothing in the undo path
touches the worker or the file — the data either exists in the overlay
or the action is not undoable.

- Capture points: commit_editing, set_cell_null, paste_tsv,
  delete_selected_rows, drop_col. Row-delete entries are pure
  rowid lists (edits on deleted rows survive in the overlay by design,
  so restore needs nothing else); column-drop entries carry the purged
  edits so undrop can put them back.
- A row-change step (delete undo/redo) syncs the worker exactly like
  delete_selected_rows: full deleted_rows set via SetDeletedRows +
  requery + refresh_count. Column steps are overlay-only (the worker
  never knows about deleted columns).
- Save interaction: Plain keeps history; Done{Save} Plain rewrites
  "pending = None" before-states into "pending = disk value" (the
  pre-action state is no longer reachable by clearing the overlay).
  RowsChanged/SchemaChanged truncate the history: the save renumbers
  rowids or removes columns, and stale references would edit the wrong
  rows. No remap machinery — truncation was chosen deliberately.
- View jump: after every step, scroll Center to the first affected
  rowid — cached pages answer directly, otherwise Job::Locate returns
  the rowid's position under the current sort/filter (the ORDER BY
  lives inside the window function; an outer one applies too late).
- Keys: Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y in shell.rs, gated on focus-free
  and no dialog; an open cell editor keeps Ctrl+Z for text-level undo.

## 6a. Row insert (new rows exist only in the overlay until saved)

New rows live on BOTH sides with the worker as the display source:

- FileState.new_rows: BTreeMap<synthetic rowid, NewRow{anchor, values}>;
  synthetic rowids count up from the file's row count (never collide
  with real rowids). next_rowid resets to the view count after a save.
- The worker's view unions a temp table of the new rows over the file
  (Job::SetNewRows). Each new row carries a __pq_sk sortkey: real rows
  sort by rowid (= file position), new rows splice at anchor ± 0.5 +
  small offsets (After/Before/End). __pq_sk closes EVERY ordering:
  unsorted queries order by it alone (it IS view order), sorted queries
  use it as the tiebreaker. count() uses FROM+WHERE only (an ORDER BY
  over non-aggregated columns is invalid under aggregation).
- save() builds __pq_src FROM THE VIEW (deleted already excluded, new
  rows present with current values), applies edits by rowid (they hit
  new rows too), writes ORDER BY __pq_sk. SaveOutcome::RowsChanged also
  covers inserts: requery + count + new_rows cleared + next_rowid reset
  + history truncated. save()'s guard counts new rows, not just overlay
  edits.
- Entry points: toolbar "+ Row" (End) and row-number right-click
  (insert above/below — RowEvent deferred like HeaderEvent, since the
  closures released FileState). After the insert the worker locates the
  row and the view scrolls to it and selects its first cell
  (FileState.pending_select consumed by the Located reply).
- Deleting a not-yet-saved row removes it outright (not marked
  deleted); a mixed selection produces one history entry restoring
  both sides.

## 7. Interaction state machine (all pointer semantics)

The grid draw closures call and the headless tests drive the same
methods — one source of truth for "what a click does":

- `pointer_on_cell(pos, col, shift)` — press starts a drag-select
  (collapsing the selection) or extends the rect while held.
- `pointer_release()` — any release ends a drag (a drag's release never
  lands as `clicked()` on the release cell).
- `click_cell(pos, col)` — click-away commits an open editor.
- `double_click_cell(pos, col)` — selection + begin edit.
- `secondary_click_cell(pos, col)` — selection for the context menu.
- `click_rownum(pos, shift)` — whole-row select / extend.

Labels are never selectable (selectable text swallowed clicks in three
different places — pinned by history). The cell editor ignores its
creation frame's Enter and commits on click-away only after it held
focus (`had_focus`).

## 8. Deterministic verification

- `cargo test` (unit + headless App scenarios + the committed scenario
  suite in tests/scenarios.rs) runs on every push via
  `.githooks/pre-push` (fmt, clippy -D warnings, full battery).
  Headless tests drive the REAL worker through the same command methods
  the UI uses.
- `src/driver.rs` is the scenario interpreter; `src/bin/pqdriver.rs` runs
  a scenario file against the app (headless here, and usable under real
  Windows against the actual exe). Committed scenarios live in
  `tools/scenarios/*.txt` and run as `tests/scenarios.rs` on every push.
- Scenario coverage: edit+save (page patching), sort+copy+paste under
  sort (position→rowid), delete rows (view reset, disk count), filter
  box (shown count, clear, bad filter kept previous), drop column
  (re-open with carried sort), drag-select state machine, set-NULL
  (disk-verified), rejected edit (editor stays open).
- Manual per-round pass: the Wine + nested-Xwayland + XTEST harness
  (AGENTS.md §verification tooling; scripted in tools/wine-pass.sh) —
  pixel grabs after each step, like-a-human clicking. Required before
  any exe ships to the user. Cleanup of test windows/processes at the
  end is mandatory.

## 9. Known open items

- Modal surfaces (export dialog, confirm, context menus) render black
  under wine+glow here: environment artifact; verify on real Windows.
  Their logic is exercised headlessly.
- Wine keyboard focus is flaky in the harness; always verify a key took
  effect before trusting a keyboard-driven step.
- Per-job worker timing goes to stderr (diagnostics); make it a status
  bar readout if open-speed questions recur.
- Undo/redo, multi-file tabs: not designed yet, do not bolt on.
