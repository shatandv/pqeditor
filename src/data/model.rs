//! UI-side data model: windowed row cache + edit overlay.
//!
//! Lives on the UI thread, holds no duckdb handles. Rows arrive as pages from
//! the worker thread; edits are kept per original rowid so they survive
//! sorting and filtering. `model::Tests` covers the pure logic.

use std::collections::HashMap;

use crate::data::types::{CellValue, EditValue, SortKey};

/// How many rows a fetched page holds. Small enough that a page query is
/// always fast, big enough that scrolling rarely waits on the worker.
pub const PAGE_SIZE: u32 = 2_000;

/// Keep at most this many pages in memory, then evict least-recently-used.
const MAX_PAGES: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayCell {
    /// The row's true file rowid (stable across sort/filter).
    pub rowid: u64,
    pub value: CellValue,
    /// Column index of the underlying column.
    pub col: usize,
    /// True when the overlay overrides this cell; the grid renders an edit
    /// marker for these.
    pub edited: bool,
}

/// One fetched row as the cache stores it: the row's true file rowid plus
/// the cell values in schema order.
pub type PageRow = (u64, Vec<CellValue>);

/// page index -> (last used tick, rows or None while fetch failed)
type PageEntry = (u64, Option<Vec<PageRow>>);

/// Windowed cache over the CURRENT view (sort/filter order). Pages hold
/// display positions; each row keeps its true rowid so the UI can address
/// the overlay, deletions and saves correctly under any sort.
#[derive(Default)]
pub struct RowCache {
    epoch: u64,
    total: u64,
    /// page index -> (last used tick, rows or None while fetch failed)
    pages: HashMap<u64, PageEntry>,
    /// pages asked from the worker, not yet answered
    in_flight: std::collections::HashSet<u64>,
    tick: u64,
}

impl RowCache {
    pub fn clear(&mut self, epoch: u64, total: u64) {
        self.epoch = epoch;
        self.total = total;
        self.pages.clear();
        self.in_flight.clear();
    }

    /// Update the row count without discarding cached pages (a fresh
    /// `count(*)` arrived; pagination stays valid).
    pub fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Which pages are missing for the visible range [from, to)?
    /// Marks returned pages in-flight so they are not re-requested.
    pub fn missing_pages(&mut self, from: u64, to: u64) -> Vec<u64> {
        let first_page = from / PAGE_SIZE as u64;
        let last_page = (to.saturating_sub(1).max(from)) / PAGE_SIZE as u64;
        let mut out = Vec::new();
        for p in first_page..=last_page {
            if !self.pages.contains_key(&p) && self.in_flight.insert(p) {
                out.push(p);
            }
        }
        out
    }

    /// Store a fetched page. Rejects stale epochs.
    pub fn put_page(&mut self, epoch: u64, page: u64, rows: Vec<PageRow>) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.in_flight.remove(&page);
        self.tick += 1;
        self.pages.insert(page, (self.tick, Some(rows)));
        self.evict();
        true
    }

    /// Drop all in-flight marks (after a bulk failure such as a broken
    /// filter): pages become requestable again.
    pub fn clear_in_flight(&mut self) {
        self.in_flight.clear();
    }

    fn evict(&mut self) {
        if self.pages.len() <= MAX_PAGES {
            return;
        }
        let mut order: Vec<(u64, u64)> = self.pages.iter().map(|(k, (t, _))| (*k, *t)).collect();
        order.sort_by_key(|(_, t)| *t);
        let drop_n = self.pages.len() - MAX_PAGES + MAX_PAGES / 10;
        for (k, _) in order.into_iter().take(drop_n) {
            self.pages.remove(&k);
        }
    }

    /// Read access for the grid. Bumps the tick so the page is not evicted.
    pub fn page(&mut self, page: u64) -> Option<&Vec<PageRow>> {
        if let Some(entry) = self.pages.get_mut(&page) {
            entry.0 = self.tick;
        }
        self.tick += 1;
        self.pages.get(&page).and_then(|(_, v)| v.as_ref())
    }
}

/// A cell address in the GRID: `pos` is the view position (sort/filter
/// order), `col` the schema column. Actions resolve `pos` to the true file
/// rowid through the cache before touching the overlay or worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub pos: u64,
    pub col: usize,
}

/// Everything between `anchor` and `head` inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Cell,
    pub head: Cell,
}

impl Sel {
    pub fn single(pos: u64, col: usize) -> Self {
        let c = Cell { pos, col };
        Self { anchor: c, head: c }
    }

    /// One row across every column (row-number click).
    pub fn single_full_row(pos: u64, ncols: usize) -> Self {
        Self {
            anchor: Cell { pos, col: 0 },
            head: Cell {
                pos,
                col: ncols.saturating_sub(1),
            },
        }
    }

    /// The selected rectangle as (first_row, last_row, first_col, last_col),
    /// clamped to `ncols` and `total`.
    pub fn rect(&self, total: u64, ncols: usize) -> (u64, u64, usize, usize) {
        let r0 = self.anchor.pos.min(self.head.pos);
        let r1 = self.anchor.pos.max(self.head.pos);
        let c0 = self.anchor.col.min(self.head.col);
        let c1 = self.anchor.col.max(self.head.col);
        let r1 = r1.min(total.saturating_sub(1));
        let c0 = c0.min(ncols.saturating_sub(1));
        let c1 = c1.min(ncols.saturating_sub(1));
        (r0, r1, c0, c1)
    }

    /// True when the selection covers every column of one or more rows
    /// (i.e. it came from row-number clicks).
    pub fn is_whole_rows(&self, ncols: usize) -> bool {
        let (_, _, c0, c1) = self.rect(u64::MAX, ncols);
        c0 == 0 && c1 + 1 == ncols
    }
}

/// Pending changes: cell edits, deleted rows, dropped columns.
#[derive(Default)]
pub struct EditOverlay {
    edits: HashMap<u64, std::collections::BTreeMap<usize, EditValue>>,
    pub deleted_rows: std::collections::BTreeSet<u64>,
    pub deleted_cols: std::collections::BTreeSet<usize>,
}

impl EditOverlay {
    /// Total pending change count for the status bar.
    pub fn len(&self) -> usize {
        self.edits.values().map(|m| m.len()).sum::<usize>()
            + self.deleted_rows.len()
            + self.deleted_cols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.deleted_rows.is_empty() && self.deleted_cols.is_empty()
    }

    pub fn set(&mut self, rowid: u64, col: usize, val: EditValue) {
        self.edits.entry(rowid).or_default().insert(col, val);
    }

    pub fn get(&self, rowid: u64, col: usize) -> Option<&EditValue> {
        self.edits.get(&rowid).and_then(|m| m.get(&col))
    }

    pub fn delete_rows(&mut self, rows: impl IntoIterator<Item = u64>) {
        self.deleted_rows.extend(rows);
    }

    pub fn drop_col(&mut self, col: usize) {
        self.deleted_cols.insert(col);
        // Edits on a dropped column are meaningless.
        for m in self.edits.values_mut() {
            m.remove(&col);
        }
    }

    pub fn is_row_deleted(&self, rowid: u64) -> bool {
        self.deleted_rows.contains(&rowid)
    }

    pub fn is_col_deleted(&self, col: usize) -> bool {
        self.deleted_cols.contains(&col)
    }

    pub fn clear(&mut self) {
        self.edits.clear();
        self.deleted_rows.clear();
        self.deleted_cols.clear();
    }

    pub fn all(&self) -> impl Iterator<Item = (u64, usize, &EditValue)> {
        self.edits
            .iter()
            .flat_map(|(r, m)| m.iter().map(move |(c, v)| (*r, *c, v)))
    }
}

/// Everything the grid needs to render and edit.
pub struct GridState {
    pub schema: Vec<crate::data::types::ColumnSchema>,
    pub sort: Vec<SortKey>,
    pub filter: Option<String>,
    pub cache: RowCache,
    pub overlay: EditOverlay,
    pub shown_rows: u64,
    /// Column widths (points, per schema column), resized from the
    /// header row. Plain data so it can live in the view state.
    pub col_widths: Vec<f32>,
}

/// Default column width until the user resizes it from the header.
pub const DEFAULT_COL_WIDTH: f32 = 130.0;

impl GridState {
    pub fn new(schema: Vec<crate::data::types::ColumnSchema>) -> Self {
        let ncols = schema.len();
        Self {
            schema,
            sort: Vec::new(),
            filter: None,
            cache: RowCache::default(),
            overlay: EditOverlay::default(),
            shown_rows: 0,
            col_widths: vec![DEFAULT_COL_WIDTH; ncols],
        }
    }

    /// Re-query after a sort/filter change. Keeps the overlay (keyed by
    /// rowid) and bumps the epoch so stale row answers are dropped.
    pub fn requery(&mut self, sort: Vec<SortKey>, filter: Option<String>) {
        self.sort = sort;
        self.filter = filter;
        self.cache.clear(self.cache.epoch + 1, 0);
        self.shown_rows = 0;
    }

    /// Write saved edits into the cached pages so the display matches the
    /// file after a save without a refetch (no flicker). A plain save does
    /// not change rowids, so the pages stay valid.
    pub fn patch_saved_edits(&mut self, edits: &[(u64, usize, EditValue)]) {
        if edits.is_empty() {
            return;
        }
        let types: Vec<crate::data::types::ColType> = self.schema.iter().map(|s| s.ty).collect();
        let convert = |ty: Option<crate::data::types::ColType>, ev: &EditValue| match ev {
            EditValue::Null => CellValue::Null,
            EditValue::Text(t) => match ty {
                Some(crate::data::types::ColType::Integer) => t
                    .parse::<i64>()
                    .map(CellValue::Int)
                    .unwrap_or_else(|_| CellValue::Text(t.clone())),
                Some(crate::data::types::ColType::Float) => t
                    .parse::<f64>()
                    .map(CellValue::Float)
                    .unwrap_or_else(|_| CellValue::Text(t.clone())),
                _ => CellValue::Text(t.clone()),
            },
        };
        let mut page_ids: Vec<u64> = self.cache.pages.keys().copied().collect();
        page_ids.sort_unstable();
        for page in page_ids {
            let Some((_, Some(rows))) = self.cache.pages.get_mut(&page) else {
                continue;
            };
            for (rowid, col, ev) in edits {
                let ty = types.get(*col).copied();
                let value = convert(ty, ev);
                // Pages hold view positions; rowids are NOT sorted within a
                // page under sort/filter, so scan linearly.
                for (r, cells) in rows.iter_mut() {
                    if *r == *rowid {
                        if let Some(cell) = cells.get_mut(*col) {
                            *cell = value;
                        }
                        break;
                    }
                }
            }
        }
    }

    /// A cell as the grid should show it: overlay first, cached value second.
    /// `pos` is the view position; the returned `rowid` is the row's true
    /// file rowid. Returns None while the row is not fetched yet (grid
    /// shows a placeholder).
    pub fn cell(&mut self, pos: u64, col: usize) -> Option<DisplayCell> {
        let page = pos / PAGE_SIZE as u64;
        let idx = (pos % PAGE_SIZE as u64) as usize;
        let cached = self
            .cache
            .page(page)
            .and_then(|rows| rows.get(idx))
            .and_then(|(rowid, cells)| Some((*rowid, cells.get(col)?.clone())));
        match cached {
            Some((rowid, base)) => {
                let edited = self.overlay.get(rowid, col).is_some();
                let value = match self.overlay.get(rowid, col) {
                    Some(EditValue::Null) => CellValue::Null,
                    Some(EditValue::Text(t)) => {
                        let ty = self.schema.get(col).map(|s| s.ty);
                        match ty {
                            Some(crate::data::types::ColType::Integer) => t
                                .parse::<i64>()
                                .map(CellValue::Int)
                                .unwrap_or_else(|_| CellValue::Text(t.clone())),
                            Some(crate::data::types::ColType::Float) => t
                                .parse::<f64>()
                                .map(CellValue::Float)
                                .unwrap_or_else(|_| CellValue::Text(t.clone())),
                            _ => CellValue::Text(t.clone()),
                        }
                    }
                    None => base,
                };
                Some(DisplayCell {
                    rowid,
                    value,
                    col,
                    edited,
                })
            }
            _ => None,
        }
    }

    /// The true file rowid behind a view position, if the row is fetched.
    pub fn rowid_at(&mut self, pos: u64) -> Option<u64> {
        let page = pos / PAGE_SIZE as u64;
        let idx = (pos % PAGE_SIZE as u64) as usize;
        self.cache
            .page(page)
            .and_then(|rows| rows.get(idx))
            .map(|(rowid, _)| *rowid)
    }

    /// Original cached value behind an edited cell, for the editor's initial
    /// content (fall back to the overlay value if the page evicted).
    pub fn edit_source_text(&mut self, pos: u64, col: usize) -> Option<String> {
        // `pos` is the view position; the overlay is keyed by the true
        // rowid. (Passing the rowid into `cell()` read the wrong row
        // whenever the view was sorted or filtered — pos and rowid only
        // coincide in a pristine unsorted view.)
        let rowid = self.rowid_at(pos)?;
        if let Some(ev) = self.overlay.get(rowid, col) {
            return Some(match ev {
                EditValue::Null => String::new(),
                EditValue::Text(t) => t.clone(),
            });
        }
        self.cell(pos, col).map(|c| c.value.display())
    }

    /// The selection rectangle as TSV (Ctrl+C). Positions are view
    /// positions; missing (unfetched) cells come out empty; dropped columns
    /// and deleted rows are skipped. Returns None for an empty or
    /// oversized selection.
    /// Copy the selection as TSV. Fails loudly (Err) instead of silently
    /// producing a short/partial clip when the selection spans rows that
    /// are not fetched yet or the block exceeds the copy cap — a silent
    /// partial clip leaves the OLD clipboard in place and a later paste
    /// then replays stale data (user-visible as "transposed" pastes).
    pub fn copy_rect(&mut self, sel: &Sel, max_cells: usize) -> Result<String, String> {
        let ncols = self.schema.len();
        let (r0, r1, c0, c1) = sel.rect(self.cache.total(), ncols);
        if r1 < r0 || c1 < c0 {
            return Err("empty selection".into());
        }
        let nrows = (r1 - r0 + 1) as usize;
        let ncols_n = c1 - c0 + 1;
        if nrows.saturating_mul(ncols_n) > max_cells {
            return Err("selection too large to copy (max 1,000,000 cells)".into());
        }
        for r in r0..=r1 {
            let rowid = self.rowid_at(r);
            let deleted = rowid.is_some_and(|id| self.overlay.is_row_deleted(id));
            if rowid.is_none() || deleted {
                return Err(if deleted {
                    "selection contains a row deleted (unsaved)".into()
                } else {
                    "selection spans rows that are still loading — try again".into()
                });
            }
        }
        let mut out = String::new();
        for r in r0..=r1 {
            let mut line = String::new();
            for c in c0..=c1 {
                if self.overlay.is_col_deleted(c) {
                    continue;
                }
                if !line.is_empty() {
                    line.push('\t');
                }
                line.push_str(
                    &self
                        .cell(r, c)
                        .map(|v| v.value.display())
                        .unwrap_or_default(),
                );
            }
            out.push_str(&line);
            out.push('\n');
        }
        // Drop the trailing newline.
        out.pop();
        Ok(out)
    }

    /// True while row pages are still being fetched.
    pub fn cache_has_pending(&self) -> bool {
        !self.cache.in_flight.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::types::EditValue as EV;

    fn val(v: i64) -> Vec<CellValue> {
        vec![CellValue::Int(v), CellValue::Text(format!("s{v}"))]
    }

    fn filled_cache() -> RowCache {
        let mut c = RowCache::default();
        c.clear(1, PAGE_SIZE as u64 * 2);
        let p0: Vec<PageRow> = (0..PAGE_SIZE as i64).map(|i| (i as u64, val(i))).collect();
        assert!(c.put_page(1, 0, p0));
        c
    }

    #[test]
    fn pages_roundtrip() {
        let mut c = filled_cache();
        assert!(c.page(0).is_some());
        assert!(c.page(1).is_none());
        let miss = c.missing_pages(1_999, 2_001);
        // Row 1,999 is on page 0 (already present); rows 2,000+ are on
        // page 1, which is missing.
        assert_eq!(miss, vec![1]);
    }

    #[test]
    fn stale_epoch_rejected() {
        let mut c = RowCache::default();
        c.clear(1, 10);
        assert!(!c.put_page(99, 0, vec![(7u64, val(1))]));
        assert!(c.page(0).is_none());
    }

    #[test]
    fn missing_pages_deduped_while_in_flight() {
        let mut c = RowCache::default();
        c.clear(1, PAGE_SIZE as u64 * 3);
        let a = c.missing_pages(0, 10);
        assert_eq!(a, vec![0]);
        let b = c.missing_pages(0, 10);
        assert!(b.is_empty(), "same page must not be re-requested");
    }

    #[test]
    fn overlay_overrides_display() {
        use crate::data::types::{ColType, ColumnSchema};
        let mut g = GridState::new(vec![
            ColumnSchema {
                name: "a".into(),
                ty: ColType::Integer,
            },
            ColumnSchema {
                name: "b".into(),
                ty: ColType::Text,
            },
        ]);
        g.cache.clear(1, PAGE_SIZE as u64);
        let p0: Vec<PageRow> = (0..PAGE_SIZE as i64).map(|i| (i as u64, val(i))).collect();
        assert!(g.cache.put_page(1, 0, p0));

        let before = g.cell(5, 0).unwrap();
        assert_eq!(before.value, CellValue::Int(5));
        assert!(!before.edited);

        g.overlay.set(5, 0, EV::Text("77".into()));
        let after = g.cell(5, 0).unwrap();
        assert_eq!(after.value, CellValue::Int(77));
        assert!(after.edited);

        g.overlay.set(5, 1, EV::Null);
        assert_eq!(g.cell(5, 1).unwrap().value, CellValue::Null);

        // Overlay survives a requery (new epoch): the cache is cleared, but
        // the overlay lookup is unaffected.
        g.requery(
            vec![SortKey {
                col: 0,
                ascending: false,
            }],
            None,
        );
        assert!(g.overlay.get(5, 0).is_some());
        // And once the page is re-fetched, display still shows the edit.
        let p0: Vec<PageRow> = (0..PAGE_SIZE as i64).map(|i| (i as u64, val(i))).collect();
        assert!(g.cache.put_page(2, 0, p0));
        assert_eq!(g.cell(5, 0).unwrap().value, CellValue::Int(77));
    }

    #[test]
    fn overlay_count() {
        let mut o = EditOverlay::default();
        o.set(1, 0, EV::Null);
        o.set(1, 1, EV::Text("x".into()));
        o.set(2, 0, EV::Text("y".into()));
        assert_eq!(o.len(), 3);
        o.clear();
        assert!(o.is_empty());
    }

    /// A copyable grid: one page fetched, rows i -> (i, "s{i}").
    fn copy_grid() -> GridState {
        use crate::data::types::{ColType, ColumnSchema};
        let mut g = GridState::new(vec![
            ColumnSchema {
                name: "a".into(),
                ty: ColType::Integer,
            },
            ColumnSchema {
                name: "b".into(),
                ty: ColType::Text,
            },
        ]);
        g.cache.clear(1, PAGE_SIZE as u64 * 2);
        let p0: Vec<PageRow> = (0..PAGE_SIZE as i64).map(|i| (i as u64, val(i))).collect();
        assert!(g.cache.put_page(1, 0, p0));
        g
    }

    fn sel_at(anchor: (u64, usize), head: (u64, usize)) -> Sel {
        Sel {
            anchor: Cell {
                pos: anchor.0,
                col: anchor.1,
            },
            head: Cell {
                pos: head.0,
                col: head.1,
            },
        }
    }

    #[test]
    fn copy_vertical_block_is_column_tsv() {
        let mut g = copy_grid();
        let text = g.copy_rect(&sel_at((0, 0), (2, 0)), 1_000_000).unwrap();
        assert_eq!(text, "0\n1\n2");
        // A 2x2 block copies both columns row-wise.
        let text = g.copy_rect(&sel_at((0, 0), (1, 1)), 1_000_000).unwrap();
        assert_eq!(text, "0\ts0\n1\ts1");
    }

    #[test]
    fn copy_with_unfetched_row_fails_loudly() {
        let mut g = copy_grid();
        // Page 1 is not fetched: position PAGE_SIZE+1 is in it.
        let r = g.copy_rect(
            &sel_at((PAGE_SIZE as u64 - 1, 0), (PAGE_SIZE as u64 + 1, 0)),
            1_000_000,
        );
        let err = r.expect_err("unfetched rows must fail, not copy partial data");
        assert!(err.contains("still loading"), "{err}");
    }

    #[test]
    fn copy_with_deleted_row_in_selection_fails_loudly() {
        let mut g = copy_grid();
        g.overlay.delete_rows(1..2);
        let r = g.copy_rect(&sel_at((0, 0), (2, 0)), 1_000_000);
        let err = r.expect_err("a pending-deleted row must fail the copy");
        assert!(err.contains("deleted"), "{err}");
    }

    #[test]
    fn copy_too_large_fails_loudly() {
        let mut g = copy_grid();
        let r = g.copy_rect(&sel_at((0, 0), (PAGE_SIZE as u64 * 2, 0)), 10);
        let err = r.expect_err("over-cap copy must fail");
        assert!(err.contains("too large"), "{err}");
    }
}
