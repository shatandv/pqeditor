# pqeditor design

Agreed design (Nov 2026). This is the source of truth for UX decisions;
`mockup.html` is the visual reference and `src/theme.rs` implements its
palette in egui.

## Stack

Rust + eframe/egui 0.37 + egui_extras + DuckDB (bundled, via the `duckdb`
crate) + rfd (file dialogs) + rust_xlsxwriter + csv. Chosen so the output
is a single self-contained binary per platform, no runtime, no installer.

## Interface

Five zones, no tabs, no sidebars:

1. Title bar: OS-native, shows `pqeditor · filename.parquet`.
2. Toolbar: Open, Save, Export, filter box (SQL WHERE fragment), clear
   button, sort chips (one per sort key, click to remove).
3. Grid: row-number column (original rowid, always meaningful), columns
   with sort arrows and multi-sort order badges. Numbers right-aligned.
   Nulls render as a dim `·` (never blank). Edited cells: amber left-edge
   bar + amber text.
4. Stats strip: one line per requested column, closable, filled
   asynchronously.
5. Status bar: total rows, shown rows (when filtered), unsaved edits
   (amber when nonzero), saving/exporting progress, errors in red.

## Main flows

- Open: drag-and-drop (primary), Ctrl+O, file dialog, recents list on the
  start screen. Opening is instant (a view definition, not a load).
- Browse: scroll freely through all rows; pages of 2000 rows are fetched
  around the viewport.
- Sort: click header cycles asc -> desc -> off (replaces the stack);
  shift+click appends a secondary key; order badges 1..n on headers.
- Filter:
  - SQL mode: type a WHERE fragment in the box, Enter applies. Validated
    by a `count(*)` probe first; errors show in red and keep the previous
    filter.
  - Popover mode: header right-click -> "filter values": searchable list
    of top-200 distinct values with counts, checkboxes (multi-select),
    plus an explicit null checkbox.
- Edit: double-click or F2. Enter commits, Esc cancels, focus-loss
  cancels (no accidental commits), Ctrl+Delete = explicit NULL. Empty
  input = NULL. Values are validated against the column type at commit
  time; invalid input is rejected with a red status message.
- Save (Ctrl+S): overlay spliced into a full rewrite; writes
  `<file>.parquet.tmp` then renames over the original. Original row order
  is preserved. After save the overlay clears.
- Stats: header right-click -> "column statistics". Numeric columns get
  count/nulls/distinct/min/max/avg/stddev; text columns get top values
  with counts.
- Export (Ctrl+E): dialog with scope (filtered/all), format (csv/xlsx),
  csv delimiter (`;` default, also `,` tab `|`), destination path. CSV is
  UTF-8 with BOM (Windows Excel compatibility), `;` avoids Excel's
  list-separator quirk. XLSX: bold + frozen header row, autofilter,
  column widths computed from content (autofit), typed cells (numbers as
  numbers, dates as dates), hard-capped at Excel's 1,048,576 rows with a
  warning. Exports run in the background with progress in the status bar.

## Keyboard map

- Ctrl+O open, Ctrl+S save, Ctrl+E export, Ctrl+L focus filter box,
  Ctrl+G go-to-row
- Arrows / PgUp / PgDn / Home / End move the selection (file order)
- F2 or Enter: edit cell; Ctrl+Delete: set NULL; Ctrl+C: copy cell
- Esc closes, in priority order: popover -> go-to-row -> stats -> cell
  editor -> clears selection

## Data model decisions (hard to retrofit; agreed deliberately)

- No in-place patching: save always rewrites the whole file. DuckDB
  cannot UPDATE parquet in place; atomic full rewrite is simpler and
  safe. Seconds for 10M rows.
- `__rowid = row_number() OVER () - 1` over the file is the stable row
  key. All edits key off it; sorting/filtering are just query
  decorations.
- One file open at a time (v1). No undo beyond "close without saving";
  the overlay is the single pending-change surface.
- Filter fragments are raw SQL (DBeaver-style), not a builder. Popover
  filters generate `col IN (...) OR col IS NULL` fragments.
- Row fetches are synchronous paged queries on the worker thread; the UI
  shows placeholders for unfetched rows instead of blocking.

## Known limitations (v1, accepted)

- Navigation keys move in file order, not visual order under sort/filter.
- List/struct/map columns render as text and are read-only.
- Save-As is not implemented (Save always writes the open file).
- The "close with unsaved edits" prompt is not wired to the window close
  button yet.
- Row-number column scrolls away with horizontal scrolling.
- CSV/XLSX export streams through Rust rather than a DuckDB COPY for CSV
  (BOM + delimiter control), so it is slower than raw COPY but bounded.

## Roadmap candidates (in rough priority)

- Close-with-unsaved-edits prompt (window close button)
- Visual-order navigation under sort/filter (ordered rowid pages)
- Save As / export selected rows only
- Multiple tabs / files
- Undo stack for the overlay
