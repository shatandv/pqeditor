# pqeditor

A barebones, spreadsheet-style desktop editor for parquet files. Built for
one job: open a parquet file, look at it, filter/sort it, fix values,
export it, go home. Handles tens of millions of rows without loading them.

## Build and run

You need Rust (1.85+) and CMake + a C++17 toolchain (DuckDB is compiled
from source and linked into the binary; no other runtime dependencies).

```
cargo run --release            # build + run
cargo build --release          # binary in target/release/pqeditor
cargo test                     # unit + integration tests
```

The release binary is fully self-contained: DuckDB is embedded, the GUI
font is bundled. Copy it anywhere and run it.

## What it does

- Opens parquet files by drag-and-drop, file dialog, or double-click
  (via file association you set up yourself)
- Virtualized grid: scroll through 10M rows; only visible rows are fetched
- Sort: click a header (asc -> desc -> off); shift+click to add secondary
  sort keys; sort chips in the toolbar show and prune the stack
- Filter: SQL WHERE fragments in the toolbar box (validated before
  applying), or a per-column value-picker popover from the header menu
- Edit: double-click a cell (or F2), Enter commits, Esc cancels,
  Ctrl+Delete sets NULL. Edits are held in an overlay (amber markers) and
  survive sorting/filtering
- Save (Ctrl+S): rewrites the file in place, atomic (tmp file + rename),
  preserving original row order
- Column statistics: right-click a header; count, nulls, distinct, and
  min/max/avg/stddev for numerics or top values for text
- Export (Ctrl+E): filtered rows or everything, as CSV (delimiter
  selectable, default `;`, UTF-8 with BOM) or XLSX (bold frozen header,
  autofit columns, autofilter, typed cells)

## Architecture in one paragraph

One DuckDB connection runs on a worker thread and answers a small job
protocol (count / rows / stats / top-values / open / save / export) over
channels; the UI thread never blocks. The grid is an egui_extras table
that renders only visible rows and asks the worker for 2000-row pages,
cached in a windowed LRU. Pending edits live in an overlay keyed by
`row_number() OVER ()` (the original file order), so sorting and filtering
never lose them. Save replays the overlay into a full COPY rewrite with an
atomic rename. Details and conventions in [AGENTS.md](AGENTS.md).

## Platform notes

Windows is the primary target; Linux builds from the same code. macOS
should work but is untested. Building on Windows needs CMake and MSVC;
the bundled DuckDB build takes a few minutes on first build.
