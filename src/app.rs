//! Application state: windows, files, selection, edits, dialogs.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use crate::data::engine::ExportFormat;
use crate::data::engine::{Engine, FailSource, Job, JobKind, JobResult};
pub use crate::data::model::{Cell, Sel};
use crate::data::model::{GridState, PAGE_SIZE};
use crate::data::types::{ColumnSchema, ColumnStats, EditValue, SortKey};

/// Where per-file state lives. One file open at a time (v1).
/// eframe persistence key for the recent-files list.
pub const STORAGE_KEY: &str = "pqeditor/recents";

pub struct FileState {
    pub path: PathBuf,
    pub grid: GridState,
    /// Rows matching the current filter; 0 while the count is in flight.
    /// Total rows in the file, independent of the filter.
    pub total_rows: u64,
    pub count_pending: bool,
    pub selection: Option<Sel>,
    pub editing: Option<EditState>,
    /// Text currently in the filter box (draft; the applied filter lives in
    /// `grid.filter`).
    pub filter_draft: String,
    pub filter_error: Option<String>,
    /// Filter candidate awaiting validation by the engine.
    pub pending_filter: Option<String>,
    pub stats: Vec<ColumnStats>,
    pub popover: Option<FilterPopover>,
    pub export_dialog: Option<ExportDialog>,
    /// Go-to-row dialog state: `None` = closed, `Some(draft)` = open.
    pub goto_row: Option<String>,
    /// Row to scroll to on the next grid draw (keyboard navigation).
    /// Scroll request for the table: jump-center (PageUp/Down/Home/End,
    /// goto-row) or a minimal edge scroll (arrow keys: only when the moved
    /// cell left the visible area, Excel-style).
    pub scroll_to_row: Option<ScrollRequest>,
    /// The row-position range rendered last frame (from the grid draw).
    pub visible_rows: Option<(u64, u64)>,
    /// Cell edit validation error, surfaced in the status bar.
    pub commit_error: Option<String>,
    /// Cell where a drag-select started; None when not dragging.
    pub drag_select: Option<Cell>,
}

impl FileState {
    pub fn new(path: PathBuf, schema: Vec<ColumnSchema>, total_rows: u64) -> Self {
        Self {
            path,
            grid: GridState::new(schema),
            total_rows,
            count_pending: false,
            selection: None,
            editing: None,
            filter_draft: String::new(),
            filter_error: None,
            pending_filter: None,
            stats: Vec::new(),
            popover: None,
            export_dialog: None,
            goto_row: None,
            scroll_to_row: None,
            visible_rows: None,
            commit_error: None,
            drag_select: None,
        }
    }

    /// Adjust the sort stack: plain click replaces the other keys and
    /// cycles the clicked column asc -> desc -> off; shift+click appends.
    pub fn cycle_sort(&mut self, col: usize, append: bool) {
        let mut sort = if append {
            self.grid.sort.clone()
        } else {
            // Keep only this column's key so the cycle can continue.
            self.grid
                .sort
                .iter()
                .filter(|k| k.col == col)
                .cloned()
                .collect()
        };
        match sort.iter().position(|k| k.col == col) {
            Some(i) => {
                if sort[i].ascending {
                    sort[i].ascending = false;
                } else {
                    sort.remove(i);
                }
            }
            None => sort.push(SortKey {
                col,
                ascending: true,
            }),
        }
        let filter = self.grid.filter.clone();
        self.grid.requery(sort, filter);
        self.selection = None;
    }

    /// Set the sort stack to exactly this column (header menu commands).
    pub fn set_sort(&mut self, col: usize, ascending: bool) {
        let sort = vec![SortKey { col, ascending }];
        let filter = self.grid.filter.clone();
        self.grid.requery(sort, filter);
        self.selection = None;
    }

    /// Sort this column descending (header menu), replacing other keys.
    pub fn set_sort_desc(&mut self, col: usize) {
        self.set_sort(col, false);
    }

    /// Add a sort key (or set its direction if the column is already in
    /// the stack), keeping every other key: multi-column sort.
    pub fn add_sort(&mut self, col: usize, ascending: bool) {
        let mut sort = self.grid.sort.clone();
        match sort.iter().position(|k| k.col == col) {
            Some(i) => sort[i].ascending = ascending,
            None => sort.push(SortKey { col, ascending }),
        }
        let filter = self.grid.filter.clone();
        self.grid.requery(sort, filter);
        self.selection = None;
    }

    /// Remove one column's key from the sort stack (keeps the rest).
    pub fn remove_sort_key(&mut self, col: usize) {
        let mut sort = self.grid.sort.clone();
        sort.retain(|k| k.col != col);
        let filter = self.grid.filter.clone();
        self.grid.requery(sort, filter);
    }

    /// Start editing the cell at a view position (type permitting).
    /// Deleted rows cannot be edited. The overlay is keyed by the row's
    /// true rowid, resolved through the cache.
    pub fn begin_edit_at(&mut self, pos: u64, col: usize) {
        let Some(rowid) = self.grid.rowid_at(pos) else {
            self.commit_error = Some("row is still loading".into());
            return;
        };
        if self.grid.overlay.is_row_deleted(rowid) {
            self.commit_error = Some("row is deleted (unsaved)".into());
            return;
        }
        let Some(schema) = self.grid.schema.get(col) else {
            return;
        };
        if self.grid.overlay.is_col_deleted(col) {
            self.commit_error = Some("column is dropped (unsaved)".into());
            return;
        }
        if !schema.ty.editable() {
            self.commit_error = Some(format!(
                "column '{}' ({}) is read-only",
                schema.name,
                schema.ty.label()
            ));
            return;
        }
        let text = self.grid.edit_source_text(pos, col).unwrap_or_default();
        self.editing = Some(EditState {
            pos,
            rowid,
            col,
            initial: text.clone(),
            text,
            had_focus: false,
            created_frame: u64::MAX,
        });
    }

    // ---- interaction state machine ---------------------------------------
    // The grid draw closures call these; headless tests drive them directly,
    // which keeps the click/drag behavior deterministic and unit-tested.

    /// Primary button is down over a cell: starts a drag-select (collapsing
    /// the selection to this cell) or extends the rect while dragging.
    pub fn pointer_on_cell(&mut self, pos: u64, col: usize, shift: bool) {
        match self.drag_select {
            None => {
                self.drag_select = Some(Cell { pos, col });
                if !shift {
                    self.selection = Some(Sel::single(pos, col));
                }
            }
            Some(d) if d.pos != pos || d.col != col => {
                if let Some(sel) = self.selection.as_mut() {
                    sel.head = Cell { pos, col };
                }
                self.drag_select = Some(Cell { pos, col });
            }
            _ => {}
        }
    }

    /// Primary button released: a drag-select ends. A drag's release never
    /// lands as `clicked()` on the release cell (the press started
    /// elsewhere), so this must run on any release frame.
    pub fn pointer_release(&mut self) {
        self.drag_select = None;
    }

    /// Plain click on a cell: an open editor on another cell commits
    /// (click-away = commit, like Excel). With shift and an existing
    /// selection, the head moves to the clicked cell — Excel-style
    /// range extension (anchor stays where the selection started).
    pub fn click_cell(&mut self, pos: u64, col: usize, shift: bool) {
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.pos != pos || e.col != col)
        {
            self.commit_editing();
        }
        self.drag_select = None;
        if shift {
            // Excel semantics: extend from the anchor; with no selection
            // yet, a shift+click simply selects the clicked cell.
            match self.selection.as_mut() {
                Some(sel) => sel.head = Cell { pos, col },
                None => self.selection = Some(Sel::single(pos, col)),
            }
        }
    }

    /// Double-click on a cell opens the editor.
    pub fn double_click_cell(&mut self, pos: u64, col: usize) {
        self.selection = Some(Sel::single(pos, col));
        self.begin_edit_at(pos, col);
    }

    /// Right-click on a cell selects it (the context menu follows).
    pub fn secondary_click_cell(&mut self, pos: u64, col: usize) {
        self.selection = Some(Sel::single(pos, col));
    }

    /// Row-number click: select the whole row; shift extends the current
    /// row selection to this row.
    pub fn click_rownum(&mut self, pos: u64, shift: bool) {
        if self.editing.is_some() {
            self.commit_editing();
        }
        let ncols = self.grid.schema.len();
        if !shift {
            self.selection = Some(Sel::single_full_row(pos, ncols));
        } else if let Some(sel) = self.selection.as_mut() {
            sel.head = Cell {
                pos,
                col: ncols.saturating_sub(1),
            };
        }
    }

    /// Drop a column: hidden client-side, excluded from save. Indices stay
    /// stable.
    pub fn drop_col(&mut self, col: usize) {
        if self.grid.schema.len()
            <= self
                .grid
                .schema
                .len()
                .saturating_sub(self.grid.overlay.deleted_cols.len())
        {
            // Only refuse when dropping would leave zero visible columns.
        }
        let visible = self.grid.schema.len() - self.grid.overlay.deleted_cols.len();
        if visible <= 1 {
            self.commit_error = Some("cannot drop the last visible column".into());
            return;
        }
        if self.grid.overlay.is_col_deleted(col) {
            return;
        }
        self.grid.overlay.drop_col(col);
        self.selection = None;
    }

    /// Commit the active edit: validate against the column type, store in
    /// the overlay. Unchanged text is a no-op. On validation failure the
    /// editor stays open and the error is recorded.
    pub fn commit_editing(&mut self) {
        self.commit_error = None;
        let Some(edit) = self.editing.clone() else {
            return;
        };
        let Some(schema) = self.grid.schema.get(edit.col) else {
            self.editing = None;
            return;
        };
        // No change since the editor opened: discard silently.
        if edit.text.trim() == edit.initial.trim() {
            self.editing = None;
            return;
        }
        match crate::data::types::validate_edit(schema.ty, &edit.text) {
            Ok(val) => {
                self.grid.overlay.set(edit.rowid, edit.col, val);
                self.editing = None;
            }
            Err(msg) => self.commit_error = Some(msg),
        }
    }

    /// Apply the pending filter (engine already validated it).
    pub fn apply_filter(&mut self, filter: Option<String>) {
        self.filter_draft = filter.clone().unwrap_or_default();
        self.filter_error = None;
        self.pending_filter = None;
        let sort = self.grid.sort.clone();
        self.grid.requery(sort, filter);
    }
}

/// Scroll request for the table: jump-center (PageUp/Down/Home/End,
/// goto-row) or a minimal edge scroll (arrow keys move one row only when
/// the selection left the visible area, Excel-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollRequest {
    /// Put the row at the vertical center of the view.
    Center(u64),
    /// Scroll minimally so the row is visible: aligned to the top edge
    /// when scrolling up, bottom edge when scrolling down.
    Edge(u64, bool),
}

#[derive(Debug, Clone)]
pub struct EditState {
    /// View position of the cell being edited (grid addressing).
    pub pos: u64,
    /// True file rowid behind that position (overlay keying, save).
    pub rowid: u64,
    pub col: usize,
    pub text: String,
    /// What the cell showed when the editor opened; an unchanged commit is
    /// a no-op (no overlay entry, no unsaved-edit count).
    pub initial: String,
    /// Set once the editor's TextEdit actually held keyboard focus; the
    /// click-away commit only fires after this, so a focus race cannot
    /// silently commit a half-open editor.
    pub had_focus: bool,
    /// Frame the editor first rendered in. The Enter that STARTED the edit
    /// is still in the input state of this frame; the editor must not
    /// consume it as a commit.
    pub created_frame: u64,
}

/// The per-column filter popover.
pub struct FilterPopover {
    pub col: usize,
    pub search: String,
    pub values: Vec<(String, u64)>,
    pub truncated: bool,
    /// Requested but not answered yet.
    pub loading: bool,
    /// Checked value display-strings (including the null sentinel).
    pub checked: HashSet<String>,
    /// egui `Window::open` flag: true while the window should show; the
    /// title-bar X sets it false and the popover state is then dropped.
    /// (A fresh local false made the window never render while the state
    /// stayed `Some` — which also silently gated copy/paste forever.)
    pub open: bool,
}

pub const NULL_SENTINEL: &str = "· null";

impl FilterPopover {
    pub fn new(col: usize) -> Self {
        Self {
            col,
            search: String::new(),
            values: Vec::new(),
            truncated: false,
            loading: true,
            checked: HashSet::new(),
            open: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    Filtered,
    All,
}

#[derive(Clone)]
pub struct ExportDialog {
    pub scope: ExportScope,
    pub format: ExportFormat,
    pub delimiter: char,
    pub path: String,
}

/// Classification of an in-flight save, decided when the job is SENT
/// (never re-derived from half-cleared state at completion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Only cell edits: display is patched in place, no reload.
    Plain,
    /// Rows were deleted: the worker's view filter is reset (survivor
    /// rowids are unchanged, positions and pages stay valid).
    RowsChanged,
    /// Columns were dropped: the on-disk schema changed, the file is
    /// re-opened and the view (sort/filter) carries over.
    SchemaChanged,
}

/// Everything Done{Save} needs to apply a finished save.
#[derive(Debug, Clone)]
pub struct PendingSave {
    pub outcome: SaveOutcome,
    /// The edits that were applied, for page patching (Plain).
    pub edits: Vec<(u64, usize, EditValue)>,
    /// Sort/filter to restore after the re-open (SchemaChanged).
    pub carry: Option<(Vec<SortKey>, Option<String>)>,
}

pub struct App {
    pub engine: Engine,
    pub file: Option<FileState>,
    /// Path whose Open job is in flight.
    pub opening: Option<PathBuf>,
    /// Everything the completion of the in-flight save needs.
    pub pending_save: Option<PendingSave>,
    /// File to open after the user resolves the unsaved-edits confirm.
    pub pending_open: Option<PathBuf>,
    /// Active unsaved-changes confirmation, if any.
    pub confirm: Option<Confirm>,
    /// Set when the user chose "Save" in a Quit confirm: close once the
    /// save finishes.
    pub quit_after_save: bool,
    /// Main.rs consumes this to send the actual close command.
    pub close_requested_flag: bool,
    pub recents: Vec<Recent>,
    /// Transient green/neutral status message (bottom bar).
    pub status: String,
    /// Error shown in the status bar until the next action.
    pub error: Option<String>,
    pub saving: bool,
    pub exporting: bool,
    /// egui id to focus next frame (filter box).
    pub focus_filter_next: bool,
}

impl App {
    /// True when there are unsaved edits.
    pub fn dirty(&self) -> bool {
        self.file
            .as_ref()
            .is_some_and(|f| !f.grid.overlay.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Opening another file while dirty.
    OpenFile,
    /// Quitting while dirty.
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recent {
    pub path: String,
    pub rows: u64,
}

/// One paste target: resolve position->rowid, validate the raw value.
/// First rejection wins (`rejected`); every field is checked before any
/// overlay write (all-or-nothing).
fn push_target(
    f: &mut FileState,
    targets: &mut Vec<(u64, usize, EditValue)>,
    rejected: &mut Option<String>,
    pos: u64,
    col: usize,
    raw: &str,
) {
    let total = f.grid.cache.total();
    if pos >= total {
        rejected.get_or_insert_with(|| "paste overflows the row count".into());
        return;
    }
    let Some(rowid) = f.grid.rowid_at(pos) else {
        rejected.get_or_insert_with(|| "row still loading".to_string());
        return;
    };
    let Some(schema) = f.grid.schema.get(col) else {
        return;
    };
    if !schema.ty.editable() {
        return;
    }
    match crate::data::types::validate_edit(schema.ty, raw) {
        Ok(val) => targets.push((rowid, col, val)),
        Err(msg) => {
            rejected.get_or_insert_with(|| format!("col '{}': {msg}", schema.name));
        }
    }
}

/// All-or-nothing: every field is validated BEFORE anything is
/// applied, so a type error never leaves a half-applied paste.
impl App {
    /// Headless constructor for tests and tool binaries.
    pub fn with_engine(engine: Engine, recents: Vec<Recent>) -> Self {
        Self {
            engine,
            file: None,
            opening: None,
            pending_save: None,
            pending_open: None,
            confirm: None,
            quit_after_save: false,
            close_requested_flag: false,
            recents,
            status: "ready".into(),
            error: None,
            saving: false,
            exporting: false,
            focus_filter_next: false,
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::theme::install(&cc.egui_ctx);
        crate::theme::install_fonts(&cc.egui_ctx);
        let recents: Vec<Recent> = cc
            .storage
            .and_then(|s| eframe::get_value(s, STORAGE_KEY))
            .unwrap_or_default();
        match Engine::spawn() {
            Ok(engine) => Self::with_engine(engine, recents),
            Err(e) => {
                // App without a worker: show the error and nothing else.
                Self::with_engine(Engine::broken(), recents)
                    .with_status(format!("duckdb failed to start: {e:#}"))
            }
        }
    }

    fn with_status(mut self, status: String) -> Self {
        self.status = status;
        self
    }

    // ---- actions ---------------------------------------------------------

    pub fn open_path(&mut self, path: PathBuf) {
        // Unsaved edits: ask before throwing them away.
        if self.dirty() {
            self.pending_open = Some(path);
            if self.confirm.is_none() {
                self.confirm = Some(Confirm::OpenFile);
            }
            return;
        }
        self.error = None;
        self.status = format!("opening {}", path.display());
        // Keep the old grid visible until Opened replaces it (matters for
        // the save-and-continue flow, where the file is unchanged).
        self.opening = Some(path.clone());
        self.engine.send(Job::Open { path });
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Parquet", &["parquet", "parq", "pq"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    pub fn save(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.saving || file.grid.overlay.is_empty() {
            return;
        }
        let overlay: Vec<(u64, usize, EditValue)> = file
            .grid
            .overlay
            .all()
            .map(|(r, c, v)| (r, c, v.clone()))
            .collect();
        let drop_cols: Vec<usize> = file.grid.overlay.deleted_cols.iter().copied().collect();
        let outcome = if !drop_cols.is_empty() {
            SaveOutcome::SchemaChanged
        } else if !file.grid.overlay.deleted_rows.is_empty() {
            SaveOutcome::RowsChanged
        } else {
            SaveOutcome::Plain
        };
        self.saving = true;
        self.status = "saving\u{2026}".into();
        // For SchemaChanged the view is snapshotted NOW: the re-open will
        // restore exactly this sort/filter (the worker is FIFO, so the
        // re-open's Opened is the next one to arrive).
        let carry = if outcome == SaveOutcome::SchemaChanged {
            Some((file.grid.sort.clone(), file.grid.filter.clone()))
        } else {
            None
        };
        self.pending_save = Some(PendingSave {
            outcome,
            edits: overlay.clone(),
            carry,
        });
        self.engine.send(Job::Save {
            overlay,
            drop_cols,
            progress: self.engine.progress_sender(),
        });
    }

    pub fn open_export_dialog(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.exporting {
            return;
        }
        let filter_active = file.grid.filter.is_some();
        let default_path = file.path.with_extension("csv");
        self.error = None;
        file.export_dialog = Some(ExportDialog {
            scope: if filter_active {
                ExportScope::Filtered
            } else {
                ExportScope::All
            },
            format: ExportFormat::Csv,
            delimiter: ';',
            path: default_path.to_string_lossy().into_owned(),
        });
    }

    pub fn start_export(&mut self) {
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let Some(dialog) = file.export_dialog.as_ref() else {
            return;
        };
        if self.exporting || self.saving {
            return;
        }
        let path = PathBuf::from(dialog.path.trim());
        if path.as_os_str().is_empty() {
            self.error = Some("export path is empty".into());
            return;
        }
        let filter = match (dialog.scope, file.grid.filter.as_deref()) {
            (ExportScope::Filtered, Some(f)) => Some(f.to_string()),
            _ => None,
        };
        self.exporting = true;
        self.status = "exporting\u{2026}".into();
        self.engine.send(Job::Export {
            path,
            filter,
            format: dialog.format,
            delimiter: dialog.delimiter,
            progress: self.engine.progress_sender(),
        });
        if let Some(f) = self.file.as_mut() {
            f.export_dialog = None;
        }
    }

    /// Submit the filter box contents for validation + application.
    pub fn submit_filter(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let draft = file.filter_draft.trim().to_string();
        let filter = if draft.is_empty() { None } else { Some(draft) };
        // Same filter as applied -> nothing to do.
        if filter.as_deref() == file.grid.filter.as_deref() {
            file.filter_error = None;
            return;
        }
        if filter.is_none() {
            // Clearing needs no validation: drop the filter right away.
            file.apply_filter(None);
            self.refresh_count();
            self.status = "filter cleared".into();
            return;
        }
        file.pending_filter = filter;
        file.filter_error = None;
        let epoch = file.grid.cache.epoch() + 1;
        self.engine.send(Job::Count {
            epoch,
            filter: file.pending_filter.clone(),
        });
        self.status = "checking filter\u{2026}".into();
    }

    pub fn clear_filter(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        file.pending_filter = None;
        file.filter_error = None;
        if file.grid.filter.is_none() {
            file.filter_draft.clear();
            return;
        }
        file.apply_filter(None);
        self.refresh_count();
        self.status = "filter cleared".into();
    }

    pub fn cycle_sort(&mut self, col: usize, append: bool) {
        if let Some(f) = self.file.as_mut() {
            f.cycle_sort(col, append);
        }
        self.refresh_count();
    }

    /// Set the sort directly (driver/tools commands mirror the menu).
    pub fn set_sort(&mut self, col: usize, ascending: bool) {
        if let Some(f) = self.file.as_mut() {
            f.set_sort(col, ascending);
        }
        self.refresh_count();
    }

    /// Add a key to the sort stack (header menu / shift+click semantics).
    pub fn add_sort(&mut self, col: usize, ascending: bool) {
        if let Some(f) = self.file.as_mut() {
            f.add_sort(col, ascending);
        }
        self.refresh_count();
    }

    /// Drop one column's key from the sort stack (header menu).
    pub fn remove_sort_key(&mut self, col: usize) {
        if let Some(f) = self.file.as_mut() {
            f.remove_sort_key(col);
        }
        self.refresh_count();
    }

    /// After any requery (sort/filter change): the grid reads its row count
    /// from the cache, which requery resets — so always send a fresh
    /// `count(*)` for the current filter.
    pub fn refresh_count(&mut self) {
        let Some(f) = self.file.as_ref() else { return };
        let epoch = f.grid.cache.epoch();
        let filter = f.grid.filter.clone();
        self.engine.send(Job::Count { epoch, filter });
        if let Some(f) = self.file.as_mut() {
            f.count_pending = true;
        }
    }

    pub fn apply_popover(&mut self) {
        // Snapshot the popover state, then release the borrow before
        // requerying.
        let (col, checked, values) = {
            let Some(file) = self.file.as_ref() else {
                return;
            };
            let Some(pop) = file.popover.as_ref() else {
                return;
            };
            (pop.col, pop.checked.clone(), pop.values.clone())
        };
        if let Some(f) = self.file.as_mut() {
            f.popover = None;
        }
        let Some(schema) = self.file.as_ref().and_then(|f| f.grid.schema.get(col)) else {
            return;
        };
        let ty = schema.ty;
        let ident = crate::data::types::sql_ident(&schema.name);

        let mut conds: Vec<String> = Vec::new();
        let picked: Vec<&(String, u64)> = values
            .iter()
            .filter(|(v, _)| checked.contains(v.as_str()))
            .collect();
        let null_picked = checked.contains(NULL_SENTINEL);
        if !picked.is_empty() {
            let quoted: Vec<String> = picked
                .iter()
                .map(|(v, _)| match ty {
                    crate::data::types::ColType::Integer
                    | crate::data::types::ColType::Float
                    | crate::data::types::ColType::Boolean => v.clone(),
                    _ => crate::data::types::sql_quote(v),
                })
                .collect();
            conds.push(format!("{ident} IN ({})", quoted.join(", ")));
        }
        if null_picked {
            conds.push(format!("{ident} IS NULL"));
        }
        let fragment = match conds.len() {
            0 => None,
            _ => Some(conds.join(" OR ")),
        };
        // A popover filter replaces the box filter: too clever to merge into
        // arbitrary SQL; the box filter is cleared when the popover applies.
        if fragment.is_some()
            && let Some(f) = self.file.as_mut()
        {
            f.filter_draft.clear();
        }
        if let Some(f) = self.file.as_mut() {
            let sort = f.grid.sort.clone();
            f.grid.requery(sort, fragment.clone());
        }
        self.refresh_count();
        self.status = match fragment {
            Some(_) => "filter applied".into(),
            None => "filter cleared".into(),
        };
    }

    pub fn begin_edit(&mut self) {
        let Some(f) = self.file.as_mut() else { return };
        let Some(sel) = f.selection else { return };
        let (pos, col) = (sel.head.pos, sel.head.col);
        f.begin_edit_at(pos, col);
    }

    /// Set NULL on every selected cell. Positions resolve to true rowids
    /// through the cache; unfetched rows are skipped.
    pub fn set_cell_null(&mut self) {
        let Some(f) = self.file.as_mut() else { return };
        let Some(sel) = f.selection else { return };
        let total = f.grid.cache.total();
        let (r0, r1, c0, c1) = sel.rect(total, f.grid.schema.len());
        for r in r0..=r1 {
            let Some(rowid) = f.grid.rowid_at(r) else {
                continue;
            };
            for c in c0..=c1 {
                if f.grid.schema.get(c).is_some_and(|s| s.ty.editable()) {
                    f.grid.overlay.set(rowid, c, EditValue::Null);
                }
            }
        }
    }

    /// Delete the selected rows (whole-row selection): the overlay marks
    /// them (by true rowid), the worker's view excludes them, save rewrites
    /// without them.
    pub fn delete_selected_rows(&mut self) {
        let Some(f) = self.file.as_ref() else { return };
        let Some(sel) = f.selection else { return };
        let total = f.grid.cache.total();
        let (r0, r1, _, _) = sel.rect(total, f.grid.schema.len());
        if !sel.is_whole_rows(f.grid.schema.len()) {
            self.error = Some("click a row number to select whole rows first".into());
            return;
        }
        let rows: Vec<u64> = {
            let f = self.file.as_mut().unwrap();
            (r0..=r1).filter_map(|r| f.grid.rowid_at(r)).collect()
        };
        let all: Vec<u64> = {
            let f = self.file.as_mut().unwrap();
            f.grid.overlay.delete_rows(rows.iter().copied());
            f.grid.overlay.deleted_rows.iter().copied().collect()
        };
        let Some(f) = self.file.as_mut() else { return };
        let sort = f.grid.sort.clone();
        let filter = f.grid.filter.clone();
        f.grid.requery(sort, filter);
        f.selection = None;
        let n = all.len();
        self.engine.send(Job::SetDeletedRows { rows: all });
        self.refresh_count();
        self.status = format!("{} rows deleted (unsaved)", n);
    }

    pub fn paste_tsv(&mut self, text: &str) {
        // Paste a TSV block (or a single value, which fills the whole
        // selection) anchored at the selection's top-left corner.
        let Some(f) = self.file.as_mut() else { return };
        let Some(sel) = f.selection else { return };
        let total = f.grid.cache.total();
        let ncols = f.grid.schema.len();
        let (r0, r1, c0, c1) = sel.rect(total, ncols);
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return;
        }
        let one_cell = lines.len() == 1 && !lines[0].contains('\t');
        // Build the target cell list first (position, col, value). Nothing
        // touches the overlay until every field validated.
        let mut targets: Vec<(u64, usize, EditValue)> = Vec::new();
        let mut rejected: Option<String> = None;
        if one_cell {
            // A single value fills every cell of the selection rect:
            // TILE the rect (rows x cols), do not run the value across a
            // single row — that transposed a vertical selection.
            let width = c1 - c0 + 1;
            let height = (r1 - r0 + 1) as usize;
            for i in 0..height as u64 {
                for j in 0..width {
                    push_target(f, &mut targets, &mut rejected, r0 + i, c0 + j, lines[0]);
                }
            }
        } else {
            for (i, line) in lines.iter().enumerate() {
                for (j, raw) in line.split('\t').enumerate() {
                    push_target(f, &mut targets, &mut rejected, r0 + i as u64, c0 + j, raw);
                }
            }
        }
        if let Some(msg) = rejected {
            self.error = Some(msg);
            return;
        }
        let applied = targets.len();
        for (rowid, col, val) in targets {
            f.grid.overlay.set(rowid, col, val);
        }
        self.status = format!("pasted {} cells", applied);
    }

    /// Move the selection by (drow, dcol), clamped to the grid. With
    /// `extend` (shift held), only the head moves; otherwise anchor and
    /// head move together. Scrolls the view to the new row.
    pub fn move_selection(&mut self, drow: i64, dcol: i64, extend: bool) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let total = file.grid.shown_rows.max(file.grid.cache.total());
        if total == 0 {
            return;
        }
        let ncols = file.grid.schema.len() as i64;
        let (row, col, anchor) = match file.selection {
            Some(sel) => {
                let base = sel.head;
                (base.pos as i64 + drow, base.col as i64 + dcol, sel.anchor)
            }
            // No selection yet: the step targets the destination directly
            // (End/Down jump from the top, Left/Right clamp to col 0).
            None => (drow.max(0), dcol.max(0), Cell { pos: 0, col: 0 }),
        };
        let row = row.clamp(0, total as i64 - 1) as u64;
        let col = col.clamp(0, ncols - 1) as usize;
        let anchor = if extend {
            anchor
        } else {
            Cell { pos: row, col }
        };
        file.selection = Some(Sel {
            anchor,
            head: Cell { pos: row, col },
        });
        // Excel-style: single steps scroll only when the cell left the
        // visible area (one-row motion); jumps (PageUp/Down/Home/End)
        // center the row.
        if drow.abs() > 2 {
            file.scroll_to_row = Some(ScrollRequest::Center(row));
        } else if let Some((lo, hi)) = file.visible_rows {
            if row < lo {
                file.scroll_to_row = Some(ScrollRequest::Edge(row, false));
            } else if row > hi {
                file.scroll_to_row = Some(ScrollRequest::Edge(row, true));
            }
        }
    }

    // ---- polling ---------------------------------------------------------

    /// Drain worker results and progress. Called once per frame.
    pub fn poll(&mut self) {
        // Progress strings (export/save status).
        while let Ok(msg) = self.engine.progress.try_recv() {
            self.status = msg;
        }
        while let Ok(result) = self.engine.results.try_recv() {
            match result {
                JobResult::Opened { schema, total } => {
                    let path = self.opening.take().unwrap_or_default();
                    self.status = format!("opened {}", path.display());
                    let mut file = FileState::new(path, schema, total);
                    // The grid renders from the cache's total; seed it with
                    // the real row count right away. Without a filter the
                    // shown count IS the file count. A file always opens at
                    // the top: egui persists scroll offsets across runs and
                    // would otherwise restore the previous session's
                    // position on a file the user just opened.
                    file.grid.cache.set_total(total);
                    file.grid.shown_rows = total;
                    file.scroll_to_row = Some(crate::app::ScrollRequest::Center(0));
                    // A save-with-drops re-open carries the view over so
                    // the grid doesn't jump back to unsorted file order.
                    // Consumed here: later opens must not inherit it.
                    if let Some((sort, filter)) = self.pending_save.take().and_then(|p| p.carry) {
                        file.grid.requery(sort, filter);
                        // The requery zeroed the count; answer it now.
                        self.refresh_count();
                    }
                    self.file = Some(file);
                    self.push_recent();
                }
                JobResult::Count { epoch, total } => {
                    if let Some(f) = self.file.as_mut() {
                        if f.pending_filter.is_some() {
                            // Validation success for the candidate filter.
                            let filter = f.pending_filter.clone();
                            f.apply_filter(filter);
                            f.grid.shown_rows = total;
                            f.grid.cache.set_total(total);
                            self.status = format!("{} rows match", crate::util::fmt_count(total));
                        } else if epoch == f.grid.cache.epoch() {
                            f.grid.shown_rows = total;
                            f.grid.cache.set_total(total);
                            f.count_pending = false;
                        }
                    }
                }
                JobResult::Rows(batch) => {
                    if let Some(f) = self.file.as_mut() {
                        let rows = batch.rowids.iter().cloned().zip(batch.rows).collect();
                        f.grid.cache.put_page(
                            batch.epoch,
                            batch.start_row / PAGE_SIZE as u64,
                            rows,
                        );
                    }
                }
                JobResult::Stats(s) => {
                    if let Some(f) = self.file.as_mut() {
                        f.stats.retain(|old| old.col != s.col);
                        f.stats.push(s);
                    }
                }
                JobResult::TopValues {
                    col,
                    values,
                    truncated,
                } => {
                    if let Some(pop) = self.file.as_mut().and_then(|f| f.popover.as_mut())
                        && pop.col == col
                    {
                        pop.values = values.into_iter().map(|(v, n)| (v.display(), n)).collect();
                        pop.truncated = truncated;
                        pop.loading = false;
                    }
                }
                JobResult::Done { kind, message } => match kind {
                    JobKind::Save => {
                        self.saving = false;
                        // The classification was made when the job was
                        // sent; the overlay is already cleared here.
                        let pending = self.pending_save.take();
                        if let Some(f) = self.file.as_mut() {
                            f.grid.overlay.clear();
                            f.editing = None;
                            // Keep the selection, sort and fetched pages:
                            // a plain save does not change what the grid
                            // shows (dropped columns were already hidden).
                        }
                        self.status = message.clone();
                        match pending.as_ref().map(|p| p.outcome) {
                            Some(SaveOutcome::Plain) => {
                                // Write the saved values into the fetched
                                // pages so the display matches the file
                                // with zero flicker.
                                if let Some(f) = self.file.as_mut() {
                                    f.grid.patch_saved_edits(
                                        &pending.map(|p| p.edits).unwrap_or_default(),
                                    );
                                }
                            }
                            Some(SaveOutcome::RowsChanged) => {
                                // The saved file no longer contains the
                                // deleted rows; clear the worker's view
                                // filter. Rowids of the surviving rows are
                                // unchanged, so pages stay valid, and the
                                // file's row count is now the view count.
                                // The cache total must follow: a stale one
                                // made tail copy/paste address positions
                                // the new view no longer has.
                                if let Some(f) = self.file.as_mut() {
                                    f.total_rows = f.grid.shown_rows;
                                    f.grid.cache.set_total(f.grid.shown_rows);
                                }
                                self.engine.send(Job::SetDeletedRows { rows: vec![] });
                            }
                            Some(SaveOutcome::SchemaChanged) => {
                                // The on-disk schema changed: re-open.
                                // pending_save (with its carry snapshot)
                                // stays until the Opened reply consumes it.
                                if let Some(f) = self.file.as_ref() {
                                    let path = f.path.clone();
                                    self.opening = Some(path.clone());
                                    self.pending_save = pending;
                                    self.engine.send(Job::Open { path });
                                }
                            }
                            None => {}
                        }
                        // Save & continue flows (confirm dialog).
                        if self.quit_after_save {
                            self.quit_after_save = false;
                            self.close_requested_flag = true;
                        }
                        if let Some(p) = self.pending_open.take() {
                            self.open_path(p);
                        }
                    }
                    JobKind::Export => {
                        self.exporting = false;
                        self.status = message;
                    }
                },
                JobResult::Failed { source, message } => match source {
                    FailSource::Filter => {
                        if let Some(f) = self.file.as_mut() {
                            f.pending_filter = None;
                            f.filter_error = Some(message.clone());
                        }
                        self.status = "invalid filter".into();
                    }
                    FailSource::Rows => {
                        // Row fetch failed: drop in-flight marks so the grid
                        // can retry.
                        if let Some(f) = self.file.as_mut() {
                            f.grid.cache.clear_in_flight();
                        }
                        self.error = Some(message);
                    }
                    FailSource::General => {
                        self.saving = false;
                        self.exporting = false;
                        self.error = Some(message);
                    }
                },
            }
        }
    }

    fn push_recent(&mut self) {
        let Some(f) = self.file.as_ref() else { return };
        let entry = Recent {
            path: f.path.to_string_lossy().into_owned(),
            rows: f.total_rows,
        };
        self.recents.retain(|r| r.path != entry.path);
        self.recents.insert(0, entry);
        self.recents.truncate(8);
        // Persisted once per frame via eframe storage (see update()).
    }
}

#[cfg(test)]
mod app_tests {
    //! Headless end-to-end tests for App-level flows (real worker).

    use super::*;
    use crate::data::types::CellValue;
    use std::path::Path;
    use std::time::Duration;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pqeditor-app-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_parquet(path: &Path, rows: i64) {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT i AS id FROM range({rows}) t(i)) TO {}",
            crate::data::types::sql_quote(&path.to_string_lossy())
        ))
        .unwrap();
    }

    /// Two-column file for the schema-change (drop column) scenarios.
    fn make_parquet2(path: &Path, rows: i64) {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT i AS id, 'item_' || i AS label FROM range({rows}) t(i)) TO {}",
            crate::data::types::sql_quote(&path.to_string_lossy())
        ))
        .unwrap();
    }

    fn drain(app: &mut App) {
        for _ in 0..50 {
            app.poll();
            if app.file.is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("file never opened");
    }

    #[test]
    fn paste_is_all_or_nothing() {
        // A type error in ANY field must reject the whole paste: no
        // half-applied block left in the overlay.
        let dir = tmp("paste-atomic");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);
        let f = app.file.as_mut().unwrap();
        f.grid.cache.clear(1, 10);
        let rows: Vec<(u64, Vec<CellValue>)> = (0..10u64)
            .map(|i| (i, vec![CellValue::Int(i as i64)]))
            .collect();
        assert!(f.grid.cache.put_page(1, 0, rows));

        f.selection = Some(Sel::single(1, 0));
        // 12 lines into a 10-row file starting at row 1 overflows.
        app.paste_tsv("5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16");
        let f = app.file.as_mut().unwrap();
        assert!(app.error.is_some(), "overflow must be reported");
        assert!(
            f.grid.overlay.is_empty(),
            "a rejected paste must not apply anything"
        );
    }

    #[test]
    fn save_keeps_view_state_when_no_schema_change() {
        // A plain save (no drops) must NOT re-open the file: sort,
        // selection and fetched pages survive, no flicker.
        let dir = tmp("save-keep-state");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);
        load_page0(app);

        app.cycle_sort(0, false); // id ascending
        load_page0(app);
        let epoch_before = app.file.as_ref().unwrap().grid.cache.epoch();

        let f = app.file.as_mut().unwrap();
        f.selection = Some(Sel::single(3, 0));
        app.begin_edit();
        let f = app.file.as_mut().unwrap();
        f.editing.as_mut().unwrap().text = "500".into();
        f.commit_editing();
        assert!(!f.grid.overlay.is_empty());

        app.save();
        wait_save(app);

        let f = app.file.as_ref().unwrap();
        assert_eq!(
            f.grid.cache.epoch(),
            epoch_before,
            "plain save must not requery or re-open"
        );
        assert_eq!(f.grid.sort.len(), 1, "sort survives");
        assert!(f.selection.is_some(), "selection survives");
        assert!(f.grid.overlay.is_empty(), "edits applied");
        assert!(
            app.file.as_mut().unwrap().grid.cache.page(0).is_some(),
            "pages stay valid across a plain save"
        );
        // The edited value is visible on the row that was edited.
        let v = app.file.as_mut().unwrap().grid.cell(3, 0).unwrap();
        assert_eq!(v.value, CellValue::Int(500));
        assert_eq!(parquet_int_count(&path, "id = 500"), 1);
    }

    #[test]
    fn save_with_deleted_rows_resets_worker_view() {
        // RowsChanged arm: after save the file lacks the deleted rows and
        // the worker's view filter must be cleared, keeping the count
        // correct (no double-exclusion).
        let dir = tmp("save-delrows");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);
        load_page0(app);

        // Select rows 0..=1 via row-number clicks and delete them.
        let f = app.file.as_mut().unwrap();
        f.click_rownum(0, false);
        f.click_rownum(1, true);
        app.delete_selected_rows();
        // The count round-trips after the requery; wait for it.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.file.as_ref().unwrap().grid.shown_rows == 98 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "count never updated");
            std::thread::sleep(Duration::from_millis(10));
        }

        app.save();
        wait_save(app);

        assert!(app.file.as_ref().unwrap().grid.overlay.is_empty());
        assert_eq!(parquet_row_count(&path), 98);
        assert_eq!(parquet_int_count(&path, "id = 0"), 0);
        assert_eq!(parquet_int_count(&path, "id = 1"), 0);
        assert_eq!(parquet_int_count(&path, "id = 2"), 1);
        // The on-screen count must stay 98 (not 96 = double exclusion).
        assert_eq!(app.file.as_ref().unwrap().grid.shown_rows, 98);
        assert_eq!(app.file.as_ref().unwrap().grid.cache.total(), 98);
    }

    #[test]
    fn save_with_dropped_col_reopens_with_carry() {
        // SchemaChanged arm: the re-open refreshes the schema and the
        // sort survives (the view is carried over).
        let dir = tmp("save-dropcol");
        let path = dir.join("t.parquet");
        make_parquet2(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);
        assert_eq!(app.file.as_ref().unwrap().grid.schema.len(), 2);

        app.cycle_sort(0, false); // sort by id asc
        assert_eq!(app.file.as_ref().unwrap().grid.sort.len(), 1);

        let f = app.file.as_mut().unwrap();
        f.drop_col(1); // drop 'label'
        app.save();
        wait_save(app);

        let f = app.file.as_ref().unwrap();
        assert_eq!(
            f.grid.schema.len(),
            1,
            "re-opened schema has only the surviving column"
        );
        assert_eq!(f.grid.sort.len(), 1, "sort carried over the re-open");
        // The sort was by col 0 which is still index 0; the carried filter
        // is empty. Sort keys pointing past the new schema would be a bug.
        assert!(f.grid.sort[0].col < f.grid.schema.len());
        // On disk the label column is gone.
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let cols: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM (DESCRIBE SELECT * FROM read_parquet({}))",
                    crate::data::types::sql_quote(&path.to_string_lossy())
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cols, 1);
    }

    #[test]
    fn sort_and_filter_refresh_grid_row_count() {
        // Regression: cycle_sort/apply_popover requiered but never sent a
        // fresh count(*), so the grid rendered zero rows after any sort.
        let dir = tmp("sort-count");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);
        assert_eq!(app.file.as_ref().unwrap().grid.cache.total(), 100);

        app.cycle_sort(0, false);
        // Count answer must arrive and fill the cache total.
        for _ in 0..50 {
            app.poll();
            if app.file.as_ref().unwrap().grid.cache.total() == 100 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.file.as_ref().unwrap().grid.cache.total(), 100);
    }

    #[test]
    fn unchanged_edit_is_a_noop() {
        let dir = tmp("noop-edit");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);

        let f = app.file.as_mut().unwrap();
        f.grid.cache.clear(1, 10);
        let rows: Vec<(u64, Vec<CellValue>)> = (0..10u64)
            .map(|i| (i, vec![CellValue::Int(i as i64)]))
            .collect();
        assert!(f.grid.cache.put_page(1, 0, rows));

        f.selection = Some(Sel::single(3, 0));
        f.begin_edit_at(3, 0);
        // Commit without touching the text: no overlay entry.
        f.commit_editing();
        assert!(f.editing.is_none());
        assert!(f.grid.overlay.is_empty());

        // Changing the text does produce an edit.
        f.begin_edit_at(3, 0);
        f.editing.as_mut().unwrap().text = "42".into();
        f.commit_editing();
        assert_eq!(f.grid.overlay.len(), 1);
    }

    /// Drive the app like the grid does: ask the worker for page 0 with the
    /// file's current epoch/sort/filter, then poll so the cache is filled.
    fn load_page0(app: &mut App) {
        let (epoch, sort, filter) = {
            let f = app.file.as_ref().unwrap();
            (
                f.grid.cache.epoch(),
                f.grid.sort.clone(),
                f.grid.filter.clone(),
            )
        };
        app.engine.send(crate::data::engine::Job::Rows {
            epoch,
            page: 0,
            sort,
            filter,
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.file.as_mut().unwrap().grid.cache.page(0).is_none() {
            app.poll();
            assert!(std::time::Instant::now() < deadline, "rows never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        app.file.as_mut().unwrap().grid.cache.clear_in_flight();
    }

    /// Wait until a Job::Save round-trip completed (overlay cleared and the
    /// automatic re-open finished).
    fn wait_save(app: &mut App) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            app.poll();
            // The optional post-save re-open (dropped columns) must also be
            // finished, or state assertions race with the Opened reply.
            let done = !app.saving
                && app.opening.is_none()
                && app
                    .file
                    .as_ref()
                    .map(|f| f.grid.overlay.is_empty())
                    .unwrap_or(false);
            if done {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "save never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn parquet_int_count(path: &Path, pred: &str) -> i64 {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.query_row(
            &format!(
                "SELECT count(*) FROM read_parquet({}) WHERE {pred}",
                crate::data::types::sql_quote(&path.to_string_lossy())
            ),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn parquet_row_count(path: &Path) -> i64 {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.query_row(
            &format!(
                "SELECT count(*) FROM read_parquet({})",
                crate::data::types::sql_quote(&path.to_string_lossy())
            ),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn sort_click_cycles_asc_desc_off() {
        // Pin: plain clicks on one header cycle asc -> desc -> off; other
        // keys are replaced.
        let dir = tmp("sort-cycle");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);

        app.cycle_sort(0, false);
        assert_eq!(
            app.file.as_ref().unwrap().grid.sort,
            vec![SortKey {
                col: 0,
                ascending: true
            }]
        );
        app.cycle_sort(0, false);
        assert_eq!(
            app.file.as_ref().unwrap().grid.sort,
            vec![SortKey {
                col: 0,
                ascending: false
            }]
        );
        app.cycle_sort(0, false);
        assert!(app.file.as_ref().unwrap().grid.sort.is_empty());

        // Shift+click appends; a later plain click on the same column
        // continues that column's cycle (asc -> desc), dropping other keys.
        app.cycle_sort(0, false);
        app.cycle_sort(1, true);
        assert_eq!(app.file.as_ref().unwrap().grid.sort.len(), 2);
        app.cycle_sort(1, false);
        assert_eq!(
            app.file.as_ref().unwrap().grid.sort,
            vec![SortKey {
                col: 1,
                ascending: false
            }]
        );
    }

    #[test]
    fn drag_then_click_does_not_extend() {
        // Regression (user-visible on the stale build): after a drag
        // release, the NEXT plain click must select that single cell — not
        // extend the old drag rect from the drag start to the click.
        let dir = tmp("drag-click");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);
        let f = app.file.as_mut().unwrap();
        f.grid.cache.clear(1, 10);
        let rows: Vec<(u64, Vec<CellValue>)> = (0..10u64)
            .map(|i| (i, vec![CellValue::Int(i as i64)]))
            .collect();
        assert!(f.grid.cache.put_page(1, 0, rows));

        // Drag from (1,0) to (3,0): press, drag, release.
        f.pointer_on_cell(1, 0, false);
        f.pointer_on_cell(2, 0, false);
        f.pointer_on_cell(3, 0, false);
        assert_eq!(f.selection.unwrap().rect(10, 1).0, 1, "drag rect start");
        assert_eq!(f.selection.unwrap().rect(10, 1).1, 3, "drag rect end");

        f.pointer_release();
        // A later click on another cell: press collapses to that cell,
        // release ends the (non-)drag, the click finalizes.
        f.pointer_on_cell(6, 0, false);
        f.pointer_release();
        f.click_cell(6, 0, false);
        let sel = f.selection.unwrap();
        assert_eq!(sel.anchor, sel.head, "plain click after drag = single cell");
        assert_eq!(sel.head.pos, 6);
    }

    #[test]
    fn click_away_commits_editor() {
        // Clicking another cell commits an open editor (Excel behavior);
        // clicking the edited cell does not.
        let dir = tmp("click-away");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);
        let f = app.file.as_mut().unwrap();
        f.grid.cache.clear(1, 10);
        let rows: Vec<(u64, Vec<CellValue>)> = (0..10u64)
            .map(|i| (i, vec![CellValue::Int(i as i64)]))
            .collect();
        assert!(f.grid.cache.put_page(1, 0, rows));

        f.double_click_cell(2, 0);
        assert!(f.editing.is_some(), "double-click opens the editor");
        f.editing.as_mut().unwrap().text = "77".into();

        // Click the SAME cell: editor stays.
        f.click_cell(2, 0, false);
        assert!(f.editing.is_some(), "click on edited cell keeps it open");

        // Click another cell: commit lands in the overlay.
        f.click_cell(5, 0, false);
        assert!(f.editing.is_none(), "click-away commits");
        assert!(!f.grid.overlay.is_empty(), "edited value stored");
    }

    #[test]
    fn rownum_click_selects_row_and_shift_extends() {
        let dir = tmp("rownum-select");
        let path = dir.join("t.parquet");
        make_parquet(&path, 10);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);
        let f = app.file.as_mut().unwrap();

        f.click_rownum(2, false);
        let sel = f.selection.unwrap();
        assert_eq!(
            sel.rect(10, 1),
            (2, 2, 0, 0),
            "row-number click selects the whole row"
        );
        assert!(sel.is_whole_rows(1), "counted as whole rows");

        f.click_rownum(5, true);
        let sel = f.selection.unwrap();
        assert_eq!(sel.rect(10, 1), (2, 5, 0, 0), "shift extends the row rect");
    }

    #[test]
    fn home_end_jump_without_selection() {
        // End/Down with no selection must create a selection at the TARGET
        // (previously the None arm anchored everything at row 0).
        let dir = tmp("end-jump");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);

        app.move_selection(i64::MAX / 2, 0, false);
        assert_eq!(
            app.file.as_ref().unwrap().selection.unwrap().head.pos,
            99,
            "End with no selection jumps to the last row"
        );

        // And from a selection, End moves the head to the last row.
        app.move_selection(i64::MAX / 2, 0, false);
        assert_eq!(app.file.as_ref().unwrap().selection.unwrap().head.pos, 99);
    }

    #[test]
    fn sorted_view_shows_sorted_values() {
        // Pin: the grid displays the rows the worker returned, first
        // position first. Under sort the top of the view must show the
        // extreme value, not the file-order row that used to sit there.
        let dir = tmp("sorted-display");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);

        app.cycle_sort(0, false); // id ascending
        load_page0(app);
        let v0 = app.file.as_mut().unwrap().grid.cell(0, 0).unwrap().value;
        assert_eq!(v0, CellValue::Int(0));
        let v9 = app.file.as_mut().unwrap().grid.cell(9, 0).unwrap().value;
        assert_eq!(v9, CellValue::Int(9));

        // Descending: position 0 shows the largest id.
        app.cycle_sort(0, false); // asc -> desc
        load_page0(app);
        let v0 = app.file.as_mut().unwrap().grid.cell(0, 0).unwrap().value;
        assert_eq!(v0, CellValue::Int(99));
    }

    #[test]
    fn sorted_edit_lands_on_the_row_you_see() {
        // Bug pin: editing the first displayed row while sorted descending
        // must edit that row (id 99), not the file-order row that happens
        // to share its position.
        let dir = tmp("sorted-edit");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);

        app.cycle_sort(0, false); // asc
        app.cycle_sort(0, false); // desc
        load_page0(app);

        let f = app.file.as_mut().unwrap();
        f.selection = Some(Sel::single(0, 0));
        app.begin_edit();
        let f = app.file.as_mut().unwrap();
        f.editing.as_mut().unwrap().text = "999".into();
        f.commit_editing();
        assert!(!f.grid.overlay.is_empty(), "edit must land in the overlay");

        app.save();
        wait_save(app);

        assert_eq!(parquet_int_count(&path, "id = 999"), 1, "edited row");
        assert_eq!(parquet_int_count(&path, "id = 99"), 0, "old row replaced");
        assert_eq!(parquet_int_count(&path, "id = 0"), 1, "other rows intact");
    }

    #[test]
    fn sorted_delete_removes_the_row_you_see() {
        // Bug pin: deleting the first displayed row while sorted descending
        // must delete id 99, not the file-order first row.
        let dir = tmp("sorted-delete");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path.clone());
        drain(app);

        app.cycle_sort(0, false); // asc
        app.cycle_sort(0, false); // desc
        load_page0(app);

        let f = app.file.as_mut().unwrap();
        f.selection = Some(Sel::single_full_row(0, 1));
        app.delete_selected_rows();
        assert!(
            !app.file.as_ref().unwrap().grid.overlay.is_empty(),
            "deletion must be pending"
        );

        app.save();
        wait_save(app);

        assert_eq!(parquet_row_count(&path), 99);
        assert_eq!(parquet_int_count(&path, "id = 99"), 0, "seen row deleted");
        assert_eq!(parquet_int_count(&path, "id = 0"), 1, "first row intact");
    }

    #[test]
    fn filter_updates_shown_count() {
        // Bug pin: the status bar's "shown" count must reflect the filter,
        // not the unfiltered total.
        let dir = tmp("filter-shown");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);

        app.file.as_mut().unwrap().filter_draft = "id > 90".into();
        app.submit_filter();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            let f = app.file.as_ref().unwrap();
            if f.grid.filter.is_some() && f.grid.shown_rows == 9 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "filter never applied");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.file.as_ref().unwrap().grid.shown_rows, 9);
    }

    #[test]
    fn empty_submit_clears_active_filter() {
        // Bug pin: submitting an empty filter box while a filter is active
        // must clear the filter.
        let dir = tmp("filter-clear");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = &mut App::with_engine(Engine::spawn().unwrap(), Vec::new());
        app.open_path(path);
        drain(app);

        app.file.as_mut().unwrap().filter_draft = "id > 90".into();
        app.submit_filter();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            if app.file.as_ref().unwrap().grid.filter.is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "filter never applied");
            std::thread::sleep(Duration::from_millis(10));
        }

        app.file.as_mut().unwrap().filter_draft.clear();
        app.submit_filter();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            app.poll();
            let f = app.file.as_ref().unwrap();
            if f.grid.filter.is_none() && f.grid.shown_rows == 100 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "filter never cleared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
