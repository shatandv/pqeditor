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

type PageEntry = (u64, Option<Vec<Vec<CellValue>>>);

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayCell {
    pub value: CellValue,
    /// Column index of the underlying column.
    pub col: usize,
    /// True when the overlay overrides this cell; the grid renders an edit
    /// marker for these.
    pub edited: bool,
}

/// Windowed cache keyed by original row index (== `__rowid`, 0-based here;
/// the engine uses 1-based row_number and subtracts one).
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
    pub fn put_page(&mut self, epoch: u64, page: u64, rows: Vec<Vec<CellValue>>) -> bool {
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
    pub fn page(&mut self, page: u64) -> Option<&Vec<Vec<CellValue>>> {
        if let Some(entry) = self.pages.get_mut(&page) {
            entry.0 = self.tick;
        }
        self.tick += 1;
        self.pages.get(&page).and_then(|(_, v)| v.as_ref())
    }
}

/// A cell address in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub rowid: u64,
    pub col: usize,
}

/// Everything between `anchor` and `head` inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Cell,
    pub head: Cell,
}

impl Sel {
    pub fn single(rowid: u64, col: usize) -> Self {
        let c = Cell { rowid, col };
        Self { anchor: c, head: c }
    }

    /// One row across every column (row-number click).
    pub fn single_full_row(rowid: u64, ncols: usize) -> Self {
        Self {
            anchor: Cell { rowid, col: 0 },
            head: Cell {
                rowid,
                col: ncols.saturating_sub(1),
            },
        }
    }

    /// The selected rectangle as (first_row, last_row, first_col, last_col),
    /// clamped to `ncols`. A whole-column selection (head.rowid = u64::MAX)
    /// spans all rows given by `total`.
    pub fn rect(&self, total: u64, ncols: usize) -> (u64, u64, usize, usize) {
        let r0 = self.anchor.rowid.min(self.head.rowid);
        let r1 = self.anchor.rowid.max(self.head.rowid);
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
}

impl GridState {
    pub fn new(schema: Vec<crate::data::types::ColumnSchema>) -> Self {
        Self {
            schema,
            sort: Vec::new(),
            filter: None,
            cache: RowCache::default(),
            overlay: EditOverlay::default(),
            shown_rows: 0,
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

    /// A cell as the grid should show it: overlay first, cached value second.
    /// Returns None while the row is not fetched yet (grid shows a
    /// placeholder).
    pub fn cell(&mut self, rowid: u64, col: usize) -> Option<DisplayCell> {
        let page = rowid / PAGE_SIZE as u64;
        let cached = self
            .cache
            .page(page)
            .and_then(|rows| rows.get((rowid % PAGE_SIZE as u64) as usize))
            .and_then(|r| r.get(col))
            .cloned();
        match cached {
            Some(base) => {
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
                Some(DisplayCell { value, col, edited })
            }
            None => None,
        }
    }

    /// Original cached value behind an edited cell, for the editor's initial
    /// content (fall back to the overlay value if the page evicted).
    pub fn edit_source_text(&mut self, rowid: u64, col: usize) -> Option<String> {
        if let Some(ev) = self.overlay.get(rowid, col) {
            return Some(match ev {
                EditValue::Null => String::new(),
                EditValue::Text(t) => t.clone(),
            });
        }
        self.cell(rowid, col).map(|c| c.value.display())
    }

    /// The selection rectangle as TSV (Ctrl+C). Missing (unfetched) cells
    /// come out empty; dropped columns are skipped. Returns None for an
    /// empty or oversized selection.
    pub fn copy_rect(&mut self, sel: &Sel, max_cells: usize) -> Option<String> {
        let ncols = self.schema.len();
        let (r0, r1, c0, c1) = sel.rect(self.cache.total(), ncols);
        if r1 < r0 || c1 < c0 {
            return None;
        }
        let nrows = (r1 - r0 + 1) as usize;
        let ncols_n = c1 - c0 + 1;
        if nrows.saturating_mul(ncols_n) > max_cells {
            return None;
        }
        let mut out = String::new();
        for r in r0..=r1 {
            if self.overlay.is_row_deleted(r) {
                continue;
            }
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
        Some(out)
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
        let p0: Vec<Vec<CellValue>> = (0..PAGE_SIZE as i64).map(val).collect();
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
        assert!(!c.put_page(99, 0, vec![val(1)]));
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
        let p0: Vec<Vec<CellValue>> = (0..PAGE_SIZE as i64).map(val).collect();
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
        let p0: Vec<Vec<CellValue>> = (0..PAGE_SIZE as i64).map(val).collect();
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
}
