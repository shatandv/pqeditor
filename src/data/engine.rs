//! DuckDB engine wrapper and the worker-thread job protocol.
//!
//! Rules that keep this codebase sane:
//! - The UI thread never touches duckdb. Everything runs on one worker
//!   thread that owns the `Connection`.
//! - Requests are fire-and-forget (`Job`), answers poll-based (`JobResult`
//!   via `try_recv` each frame). Row requests carry an `epoch` so answers
//!   from a superseded query are dropped by the model.
//! - All SQL is built through `sql_quote`/`sql_ident` or prepared params.
//!   Filter fragments typed by the user are validated by the engine before
//!   use (see `Job::Count`).
//! - Long operations (save, export) stream progress through `progress_tx`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{anyhow, bail, Context, Result};

use crate::data::types::{
    sql_ident, sql_quote, CellValue, ColType, ColumnSchema, ColumnStats, EditValue, NumericStats,
    SortKey,
};

/// Kind of a long operation, reported back in `JobResult::Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Save,
    Export,
}

/// Where a failure came from, so the UI routes the message correctly
/// (filter validation errors show next to the filter box, row errors in the
/// status bar, and so on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailSource {
    Filter,
    Rows,
    General,
}

#[derive(Debug)]
pub enum Job {
    /// `count(*)` for the proposed filter. A bad fragment answers
    /// `JobResult::Failed` with a short error message.
    Count { epoch: u64, filter: Option<String> },
    /// Rows for one page (0-based page index, `PAGE_SIZE` rows).
    Rows { epoch: u64, page: u64, sort: Vec<SortKey>, filter: Option<String> },
    /// Column statistics.
    Stats { col: usize, filter: Option<String> },
    /// Top distinct values of a column for the filter popover.
    TopValues { col: usize, limit: u32, filter: Option<String> },
    /// Open a parquet file, validate it, return schema + row count.
    Open { path: PathBuf },
    /// Replace the worker's deleted-row set (and therefore the view).
    SetDeletedRows { rows: Vec<u64> },
    /// Rewrite the open file with the overlay applied (atomic tmp+rename).
    /// `drop_cols` are schema indices of user-deleted columns.
    Save {
        overlay: Vec<(u64, usize, EditValue)>,
        drop_cols: Vec<usize>,
        progress: mpsc::Sender<String>,
    },
    /// Export current view (filtered or not) to csv or xlsx.
    Export {
        path: PathBuf,
        filter: Option<String>,
        format: ExportFormat,
        delimiter: char,
        progress: mpsc::Sender<String>,
    },
}

#[derive(Debug)]
pub enum JobResult {
    Count { epoch: u64, total: u64 },
    Rows(RowBatch),
    Stats(ColumnStats),
    TopValues { col: usize, values: Vec<(CellValue, u64)>, truncated: bool },
    Opened { schema: Vec<ColumnSchema>, total: u64 },
    /// A long operation finished successfully.
    Done { kind: JobKind, message: String },
    /// Anything failed; `message` is user-presentable.
    Failed { source: FailSource, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Xlsx,
}

/// One row batch. `start_row` is the 0-based rowid of the first row.
#[derive(Debug)]
pub struct RowBatch {
    pub epoch: u64,
    pub start_row: u64,
    pub rows: Vec<Vec<CellValue>>,
}

/// Owned worker handle: send jobs, poll for results and progress.
pub struct Engine {
    job_tx: Option<mpsc::Sender<Job>>,
    /// Cloned per Save/Export job so progress messages stream back.
    progress_tx: Option<mpsc::Sender<String>>,
    pub results: mpsc::Receiver<JobResult>,
    pub progress: mpsc::Receiver<String>,
}

impl Engine {
    pub fn spawn() -> Result<Self> {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, res_rx) = mpsc::channel::<JobResult>();
        let (prog_tx, prog_rx) = mpsc::channel::<String>();
        let prog_tx_for_worker = prog_tx.clone();
        std::thread::Builder::new()
            .name("duckdb-worker".into())
            .spawn(move || {
                if let Err(e) = worker_loop(job_rx, res_tx, prog_tx_for_worker) {
                    eprintln!("pqeditor: worker stopped: {e:#}");
                }
            })
            .context("spawning duckdb worker")?;
        Ok(Self {
            job_tx: Some(job_tx),
            progress_tx: Some(prog_tx),
            results: res_rx,
            progress: prog_rx,
        })
    }

    /// Degenerate engine for the (rare) case where duckdb could not start:
    /// accepts jobs, never answers. The UI shows the startup error instead.
    pub fn broken() -> Self {
        let (_, res_rx) = mpsc::channel::<JobResult>();
        let (_, prog_rx) = mpsc::channel::<String>();
        Self { job_tx: None, progress_tx: None, results: res_rx, progress: prog_rx }
    }

    pub fn send(&self, job: Job) {
        // The worker runs forever; send only fails if it has panicked.
        if self
            .job_tx
            .as_ref()
            .is_some_and(|tx| tx.send(job).is_err())
        {
            eprintln!("pqeditor: worker is gone; job dropped");
        }
    }

    /// A progress-sender clone to attach to a Save/Export job.
    pub fn progress_sender(&self) -> mpsc::Sender<String> {
        match &self.progress_tx {
            Some(tx) => tx.clone(),
            None => mpsc::channel::<String>().0, // sender, never written to
        }
    }
}

// ---------------------------------------------------------------- worker ---

struct Worker {
    conn: duckdb::Connection,
    /// Absolute path of the open parquet file.
    path: Option<PathBuf>,
    schema: Vec<ColumnSchema>,
    /// Monotonic suffix so reopened views never collide.
    seq: u64,
    /// Rowids excluded from the view (user-deleted rows, not yet saved).
    /// The view is recreated whenever this changes so every query
    /// (count/rows/stats/top-values/export) sees the deletion.
    deleted_rows: Vec<u64>,
}

impl Worker {
    fn new() -> Result<Self> {
        let conn = duckdb::Connection::open_in_memory()?;
        Ok(Self {
            conn,
            path: None,
            schema: Vec::new(),
            seq: 0,
            deleted_rows: Vec::new(),
        })
    }

    fn view_name(&self) -> String {
        format!("__pq_view_{}", self.seq)
    }

    fn open(&mut self, path: &Path) -> Result<(Vec<ColumnSchema>, u64)> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let p = path_string(&path);

        // Reject non-parquet input with a short message before anything else.
        let total: i64 = self
            .conn
            .query_row(&format!("SELECT count(*) FROM read_parquet({})", sql_quote(&p)), [], |r| {
                r.get(0)
            })
            .with_context(|| format!("cannot read parquet file: {}", path.display()))?;

        self.seq += 1;
        self.conn.execute_batch(&format!(
            "CREATE OR REPLACE VIEW {} AS SELECT row_number() OVER () - 1 AS __rowid, * FROM read_parquet({})",
            self.view_name(),
            sql_quote(&p),
        ))?;

        let mut stmt = self.conn.prepare(&format!(
            "DESCRIBE SELECT * EXCLUDE (__rowid) FROM {}",
            self.view_name()
        ))?;
        let mapped = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let ty_raw: String = r.get(1)?;
            Ok(ColumnSchema { name, ty: map_col_type(&ty_raw) })
        })?;
        let mut schema = Vec::new();
        for row in mapped {
            schema.push(row?);
        }
        if schema.is_empty() {
            bail!("parquet file has no columns");
        }
        self.schema = schema;
        self.path = Some(path);
        self.deleted_rows.clear();
        self.recreate_view()?;
        Ok((self.schema.clone(), total as u64))
    }

    /// Recreate the view, excluding user-deleted rows. Cheap: it is just a
    /// view definition over read_parquet.
    fn recreate_view(&mut self) -> Result<()> {
        let p = self
            .path
            .as_ref()
            .map(|p| path_string(p))
            .ok_or_else(|| anyhow!("no file open"))?;
        let mut sql = format!(
            "CREATE OR REPLACE VIEW {} AS SELECT row_number() OVER () - 1 AS __rowid, * FROM read_parquet({})",
            self.view_name(),
            sql_quote(&p),
        );
        if !self.deleted_rows.is_empty() {
            let ids = self
                .deleted_rows
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" WHERE __rowid NOT IN ({ids})"));
        }
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    /// FROM/WHERE/ORDER BY suffix shared by count/rows/export queries.
    fn select_suffix(&self, sort: &[SortKey], filter: Option<&str>) -> String {
        let mut sql = format!(" FROM {}", self.view_name());
        if let Some(f) = filter {
            sql.push_str(" WHERE ");
            sql.push_str(f);
        }
        if !sort.is_empty() {
            sql.push_str(" ORDER BY ");
            let keys: Vec<String> = sort
                .iter()
                .map(|k| {
                    let col = self
                        .schema
                        .get(k.col)
                        .map(|c| sql_ident(&c.name))
                        .unwrap_or_else(|| format!("__rowid"));
                    format!("{col} {}", if k.ascending { "ASC" } else { "DESC" })
                })
                .collect();
            sql.push_str(&keys.join(", "));
        }
        sql
    }

    fn count(&self, filter: Option<&str>) -> Result<u64> {
        let sql = format!("SELECT count(*){}", self.select_suffix(&[], filter));
        let n: i64 = self.conn.query_row(&sql, [], |r| r.get(0))?;
        Ok(n as u64)
    }

    fn rows(&self, epoch: u64, page: u64, sort: &[SortKey], filter: Option<&str>) -> Result<RowBatch> {
        let start = page * crate::data::model::PAGE_SIZE as u64;
        let len = crate::data::model::PAGE_SIZE as u64;
        let mut sql = String::from("SELECT ");
        for (i, col) in self.schema.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&sql_ident(&col.name));
        }
        sql.push_str(&self.select_suffix(sort, filter));
        sql.push_str(&format!(" LIMIT {len} OFFSET {start}"));

        let mut stmt = self.conn.prepare(&sql)?;
        let n_cols = self.schema.len();
        let mapped = stmt.query_map([], |r| {
            let mut row = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                let v: duckdb::types::Value = r.get(i)?;
                row.push(value_to_cell(&v));
            }
            Ok(row)
        })?;
        let mut rows = Vec::with_capacity(len as usize);
        for row in mapped {
            rows.push(row?);
        }
        Ok(RowBatch { epoch, start_row: start, rows })
    }

    fn stats(&self, col: usize, filter: Option<&str>) -> Result<ColumnStats> {
        let schema = self
            .schema
            .get(col)
            .ok_or_else(|| anyhow!("column {col} out of range"))?
            .clone();
        let ident = sql_ident(&schema.name);
        let view = self.view_name();
        let where_sql = filter_clause(filter);

        let mut stats = ColumnStats {
            col,
            rows: 0,
            nulls: 0,
            distinct: None,
            numeric: None,
            top: Vec::new(),
        };
        {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT count(*), count({ident}) FROM {view}{where_sql}"))?;
            let (rows, nonnull): (i64, i64) = stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            stats.rows = rows as u64;
            stats.nulls = (rows - nonnull) as u64;
        }
        {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT count(DISTINCT {ident}) FROM {view}{where_sql}"))?;
            let d: i64 = stmt.query_row([], |r| r.get(0))?;
            stats.distinct = Some(d as u64);
        }
        match schema.ty {
            ColType::Integer | ColType::Float => {
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT min({ident}), max({ident}), avg({ident}), stddev_samp({ident}) FROM {view}{where_sql}"
                ))?;
                let (min, max, avg, sd): (Option<f64>, Option<f64>, Option<f64>, Option<f64>) =
                    stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                if let (Some(min), Some(max), Some(avg)) = (min, max, avg) {
                    stats.numeric = Some(NumericStats {
                        min,
                        max,
                        avg,
                        stddev: sd.unwrap_or(f64::NAN),
                    });
                }
            }
            _ => {
                let (top, _) = self.top_values(col, 10, filter)?;
                stats.top = top;
            }
        }
        Ok(stats)
    }

    /// Distinct values with counts (for the filter popover). Returns one more
    /// than `limit` only via the `truncated` flag; values are capped.
    fn top_values(&self, col: usize, limit: u32, filter: Option<&str>) -> Result<(Vec<(CellValue, u64)>, bool)> {
        let schema = self
            .schema
            .get(col)
            .ok_or_else(|| anyhow!("column {col} out of range"))?
            .clone();
        let ident = sql_ident(&schema.name);
        let sql = format!(
            "SELECT {ident}, count(*) AS n FROM {}{} GROUP BY 1 ORDER BY n DESC, 1 LIMIT {}",
            self.view_name(),
            filter_clause(filter),
            limit + 1,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mapped = stmt.query_map([], |r| {
            let v: duckdb::types::Value = r.get(0)?;
            let n: i64 = r.get(1)?;
            Ok((value_to_cell(&v), n as u64))
        })?;
        let mut values = Vec::new();
        for row in mapped {
            values.push(row?);
        }
        let truncated = values.len() as u32 > limit;
        values.truncate(limit as usize);
        Ok((values, truncated))
    }

    /// Rewrite the file with overlay applied. Atomic: write
    /// `<target>.tmp`, then rename over the target. Always saves the file
    /// currently open (Save has no Save-As dialog in v1).
    fn save(
        &mut self,
        overlay: &[(u64, usize, EditValue)],
        drop_cols: &[usize],
        progress: &mpsc::Sender<String>,
    ) -> Result<String> {
        let target = self
            .path
            .clone()
            .ok_or_else(|| anyhow!("no file open"))?;
        let tmp = target.with_extension("parquet.tmp");
        let _ = std::fs::remove_file(&tmp);
        progress.send(format!("saving {}", target.display())).ok();

        // 1. Edits table: rowid, column name, text value (VARCHAR here; the
        //    UPDATE casts to the column type).
        self.conn.execute_batch(
            "CREATE OR REPLACE TEMP TABLE __pq_edits(rowid BIGINT, col VARCHAR, val VARCHAR)",
        )?;
        {
            let mut ins = self.conn.prepare("INSERT INTO __pq_edits VALUES (?, ?, ?)")?;
            for (rowid, col, val) in overlay {
                let name = self
                    .schema
                    .get(*col)
                    .map(|c| c.name.clone())
                    .ok_or_else(|| anyhow!("edit references unknown column {col}"))?;
                let v = match val {
                    EditValue::Null => None::<String>,
                    EditValue::Text(t) => Some(t.clone()),
                };
                ins.execute(duckdb::params![*rowid as i64, name, v])?;
            }
        }

        // 2. Source snapshot with stable rowids.
        self.conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE __pq_src AS SELECT row_number() OVER () - 1 AS __rowid, * FROM read_parquet({})",
            sql_quote(&path_string(&target)),
        ))?;

        // 3. Apply edits column-wise (one UPDATE per edited column).
        let mut edited: Vec<usize> = overlay.iter().map(|(_, c, _)| *c).collect();
        edited.sort_unstable();
        edited.dedup();
        for col in &edited {
            let name = self.schema[*col].name.clone();
            let ty = self.schema[*col].ty.sql_type();
            let ident = sql_ident(&name);
            self.conn.execute_batch(&format!(
                "UPDATE __pq_src SET {ident} = (SELECT CAST(val AS {ty}) FROM __pq_edits WHERE __pq_edits.rowid = __pq_src.__rowid AND __pq_edits.col = {}) WHERE __rowid IN (SELECT rowid FROM __pq_edits WHERE col = {})",
                sql_quote(&name),
                sql_quote(&name),
            ))?;
        }

        // 3b. Delete user-deleted rows from the snapshot.
        if !self.deleted_rows.is_empty() {
            let ids = self
                .deleted_rows
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.conn
                .execute_batch(&format!("DELETE FROM __pq_src WHERE __rowid IN ({ids})"))?;
        }

        // 4. Write in original file order, then atomic swap. Dropped
        //    columns are excluded from the output.
        let drop_names: std::collections::BTreeSet<String> = drop_cols
            .iter()
            .filter_map(|c| self.schema.get(*c).map(|s| s.name.clone()))
            .collect();
        let cols: Vec<String> = self
            .schema
            .iter()
            .filter(|c| !drop_names.contains(&c.name))
            .map(|c| sql_ident(&c.name))
            .collect();
        if cols.is_empty() {
            bail!("cannot save a file with no columns left");
        }
        self.conn.execute_batch(&format!(
            "COPY (SELECT {} FROM __pq_src ORDER BY __rowid) TO {} (FORMAT PARQUET, COMPRESSION zstd)",
            cols.join(", "),
            sql_quote(&path_string(&tmp)),
        ))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))?;
        self.open(&target)?; // fresh view; keeps schema accurate
        let msg = format!("saved {} ({} edits applied)", target.display(), overlay.len());
        progress.send(msg.clone()).ok();
        Ok(msg)
    }

    fn export(
        &self,
        path: &Path,
        filter: Option<&str>,
        format: ExportFormat,
        delimiter: char,
        progress: &mpsc::Sender<String>,
    ) -> Result<String> {
        match format {
            ExportFormat::Csv => self.export_csv(path, filter, delimiter, progress),
            ExportFormat::Xlsx => self.export_xlsx(path, filter, progress),
        }
    }

    /// Build the SELECT list + suffix used by both exporters.
    fn export_query(&self, filter: Option<&str>) -> String {
        let mut sql = String::from("SELECT ");
        for (i, col) in self.schema.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&sql_ident(&col.name));
        }
        sql.push_str(&self.select_suffix(&[], filter));
        sql
    }

    fn export_csv(
        &self,
        path: &Path,
        filter: Option<&str>,
        delimiter: char,
        progress: &mpsc::Sender<String>,
    ) -> Result<String> {
        use std::io::Write;

        let mut stmt = self.conn.prepare(&self.export_query(filter))?;
        let n_cols = self.schema.len();
        let mapped = stmt.query_map([], |r| {
            let mut row = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                let v: duckdb::types::Value = r.get(i)?;
                row.push(value_to_cell(&v));
            }
            Ok(row)
        })?;

        // UTF-8 BOM so Windows Excel detects the encoding.
        let mut file = std::fs::File::create(path)?;
        file.write_all(&[0xEF, 0xBB, 0xBF])?;
        let mut writer = csv::WriterBuilder::new()
            .delimiter(delimiter as u8)
            .from_writer(file);
        writer.write_record(self.schema.iter().map(|c| c.name.clone()))?;

        let mut n: u64 = 0;
        for row in mapped {
            let row = row?;
            writer.write_record(row.iter().map(|c| c.display()))?;
            n += 1;
            if n % 100_000 == 0 {
                progress.send(format!("exporting\u{2026} {n} rows")).ok();
            }
        }
        writer.flush()?;
        let msg = format!("exported {n} rows to {}", path.display());
        progress.send(msg.clone()).ok();
        Ok(msg)
    }

    fn export_xlsx(
        &self,
        path: &Path,
        filter: Option<&str>,
        progress: &mpsc::Sender<String>,
    ) -> Result<String> {
        use rust_xlsxwriter::{Format, Workbook};

        let mut stmt = self.conn.prepare(&self.export_query(filter))?;
        let n_cols = self.schema.len();
        let mapped = stmt.query_map([], |r| {
            let mut row = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                let v: duckdb::types::Value = r.get(i)?;
                row.push(value_to_cell(&v));
            }
            Ok(row)
        })?;

        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        let header_fmt = Format::new().set_bold();
        for (c, col) in self.schema.iter().enumerate() {
            ws.write_with_format(0, c as u16, &col.name, &header_fmt)?;
        }
        ws.set_freeze_panes(1, 0)?;
        ws.autofilter(0, 0, 1_048_575, n_cols as u16 - 1)?;
        // Excel hard limit: 1,048,576 rows including the header.
        let max_rows: u64 = 1_048_575;

        // "Autofit": rust_xlsxwriter's autofit() needs the whole sheet in
        // memory, so we track the longest value per column while streaming.
        let mut widths: Vec<f64> = self
            .schema
            .iter()
            .map(|c| (c.name.chars().count() + 2) as f64)
            .collect();
        let mut n: u64 = 0;
        for row in mapped {
            if n >= max_rows {
                break;
            }
            let row = row?;
            for (c, cell) in row.iter().enumerate() {
                let text = cell.display();
                widths[c] = widths[c].max((text.chars().count() + 2) as f64);
                match cell {
                    CellValue::Int(v) => {
                        ws.write(n as u32 + 1, c as u16, *v)?;
                    }
                    CellValue::Float(v) => {
                        ws.write(n as u32 + 1, c as u16, *v)?;
                    }
                    CellValue::Bool(v) => {
                        ws.write(n as u32 + 1, c as u16, *v)?;
                    }
                    CellValue::Null => {}
                    CellValue::Temporal(s) | CellValue::Text(s) | CellValue::Other(s) => {
                        ws.write(n as u32 + 1, c as u16, s)?;
                    }
                }
            }
            n += 1;
            if n % 100_000 == 0 {
                progress.send(format!("exporting\u{2026} {n} rows")).ok();
            }
        }
        for (c, w) in widths.iter().enumerate() {
            ws.set_column_width(c as u16, w.min(80.0))?;
        }
        wb.save(path)?;
        let msg = if n >= max_rows {
            format!(
                "exported {n} rows to {} (Excel row limit reached, truncated)",
                path.display()
            )
        } else {
            format!("exported {n} rows to {}", path.display())
        };
        progress.send(msg.clone()).ok();
        Ok(msg)
    }
}

fn filter_clause(filter: Option<&str>) -> String {
    match filter {
        Some(f) => format!(" WHERE {f}"),
        None => String::new(),
    }
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Short first-line error for status bar display.
fn short_error(e: anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.lines().next().unwrap_or("error").to_string()
}

/// Map a DuckDB type name (from DESCRIBE) to our ColType.
fn map_col_type(ty: &str) -> ColType {
    let t = ty.trim().to_ascii_uppercase();
    if t.contains("INT") || t.contains("HUGEINT") {
        ColType::Integer
    } else if t.starts_with("DECIMAL")
        || t.contains("FLOAT")
        || t.contains("DOUBLE")
        || t.contains("REAL")
    {
        ColType::Float
    } else if t.starts_with("BOOL") {
        ColType::Boolean
    } else if t.starts_with("DATE") {
        ColType::Date
    } else if t.starts_with("TIMESTAMP") {
        ColType::Timestamp
    } else if t.starts_with("TIME") {
        ColType::Time
    } else if t.starts_with("VARCHAR") || t.starts_with("CHAR") || t == "TEXT" || t == "STRING" {
        ColType::Text
    } else {
        ColType::Other
    }
}

fn value_to_cell(v: &duckdb::types::Value) -> CellValue {
    use duckdb::types::Value as V;
    match v {
        V::Null => CellValue::Null,
        V::Boolean(b) => CellValue::Bool(*b),
        V::TinyInt(x) => CellValue::Int(i64::from(*x)),
        V::SmallInt(x) => CellValue::Int(i64::from(*x)),
        V::Int(x) => CellValue::Int(i64::from(*x)),
        V::BigInt(x) => CellValue::Int(*x),
        V::UTinyInt(x) => CellValue::Int(i64::from(*x)),
        V::USmallInt(x) => CellValue::Int(i64::from(*x)),
        V::UInt(x) => CellValue::Int(i64::from(*x)),
        V::UBigInt(x) => (*x)
            .try_into()
            .map(CellValue::Int)
            .unwrap_or_else(|_| CellValue::Other(x.to_string())),
        V::HugeInt(x) => (*x)
            .try_into()
            .map(CellValue::Int)
            .unwrap_or_else(|_| CellValue::Other(x.to_string())),
        V::UHugeInt(x) => (*x)
            .try_into()
            .map(CellValue::Int)
            .unwrap_or_else(|_| CellValue::Other(x.to_string())),
        V::Float(x) => CellValue::Float(f64::from(*x)),
        V::Double(x) => CellValue::Float(*x),
        V::Text(s) => CellValue::Text(s.clone()),
        V::Blob(b) | V::Geometry(b) => CellValue::Other(format!("blob[{}]", b.len())),
        V::Date32(d) => CellValue::Temporal(format_date(*d)),
        V::Time64(unit, raw) => CellValue::Temporal(format_time(unit.to_micros(*raw) as u64)),
        V::Timestamp(unit, raw) => CellValue::Temporal(format_timestamp(unit.to_micros(*raw))),
        V::Interval { months, days, nanos } => {
            CellValue::Other(format!("interval {months}m {days}d {nanos}ns"))
        }
        V::Decimal(d) => CellValue::Float(d.to_string().parse().unwrap_or(f64::NAN)),
        V::Enum(s) => CellValue::Text(s.clone()),
        other => CellValue::Other(format!("{other:?}")),
    }
}

/// duckdb `Date32` is days since epoch.
fn format_date(days: i32) -> String {
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Microseconds since midnight.
fn format_time(micros: u64) -> String {
    let secs = micros / 1_000_000;
    let sub = micros % 1_000_000;
    if sub == 0 {
        format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!(
            "{:02}:{:02}:{:02}.{:06}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            sub
        )
    }
}

/// Microseconds since epoch.
fn format_timestamp(micros: i64) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let rem = micros.rem_euclid(86_400_000_000);
    let (y, m, d) = civil_from_days(days);
    let secs = rem / 1_000_000;
    let sub = rem % 1_000_000;
    if sub == 0 {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60
        )
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:06}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            sub
        )
    }
}

/// Days-since-epoch to civil date (Howard Hinnant's algorithm; no chrono dep).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ------------------------------------------------------------ worker loop ---

fn worker_loop(
    jobs: mpsc::Receiver<Job>,
    results: mpsc::Sender<JobResult>,
    _progress: mpsc::Sender<String>,
) -> Result<()> {
    let mut worker = Worker::new().context("starting duckdb")?;

    for job in jobs {
        let result = match job {
            Job::Open { path } => match worker.open(&path) {
                Ok((schema, total)) => JobResult::Opened { schema, total },
                Err(e) => JobResult::Failed { source: FailSource::General, message: short_error(e) },
            },
            Job::Count { epoch, filter } => match worker.count(filter.as_deref()) {
                Ok(total) => JobResult::Count { epoch, total },
                Err(e) => JobResult::Failed { source: FailSource::Filter, message: short_error(e) },
            },
            Job::Rows { epoch, page, sort, filter } => {
                match worker.rows(epoch, page, &sort, filter.as_deref()) {
                    Ok(batch) => JobResult::Rows(batch),
                    Err(e) => JobResult::Failed { source: FailSource::Rows, message: short_error(e) },
                }
            }
            Job::Stats { col, filter } => match worker.stats(col, filter.as_deref()) {
                Ok(s) => JobResult::Stats(s),
                Err(e) => JobResult::Failed { source: FailSource::General, message: short_error(e) },
            },
            Job::TopValues { col, limit, filter } => match worker.top_values(col, limit, filter.as_deref()) {
                Ok((values, truncated)) => JobResult::TopValues { col, values, truncated },
                Err(e) => JobResult::Failed { source: FailSource::General, message: short_error(e) },
            },
            Job::Save { overlay, progress } => match worker.save(&overlay, &progress) {
                Ok(message) => JobResult::Done { kind: JobKind::Save, message },
                Err(e) => JobResult::Failed { source: FailSource::General, message: short_error(e) },
            },
            Job::Export { path, filter, format, delimiter, progress } => {
                match worker.export(&path, filter.as_deref(), format, delimiter, &progress) {
                    Ok(message) => JobResult::Done { kind: JobKind::Export, message },
                    Err(e) => JobResult::Failed { source: FailSource::General, message: short_error(e) },
                }
            }
        };
        if results.send(result).is_err() {
            return Ok(()); // UI closed
        }
    }
    Ok(())
}

#[cfg(test)]
mod integration_tests {
    //! End-to-end tests against the real worker: create a parquet file,
    //! open it through the engine, sort/filter, save edits, export.

    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Unique temp dir per test. `tempfile_dir` is a tiny helper crate;
    /// if unavailable, fall back to std::env::temp_dir with a unique name.
    fn tmp(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir();
        let unique = format!(
            "pqeditor-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn recv(engine: &Engine) -> JobResult {
        engine
            .results
            .recv_timeout(Duration::from_secs(30))
            .expect("worker answered")
    }

    fn make_parquet(path: &std::path::Path, rows: i64) {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT i AS id, i * 1.5 AS score, CASE WHEN i % 3 = 0 THEN NULL ELSE 'row_' || i END AS label, DATE '2024-01-01' + CAST(i % 30 AS INTEGER) AS day FROM range({rows}) t(i)) TO {}",
            crate::data::types::sql_quote(&path.to_string_lossy()),
        ))
        .unwrap();
    }

    struct TestApp {
        engine: Engine,
        path: std::path::PathBuf,
    }

    impl TestApp {
        fn open(path: &std::path::Path) -> Self {
            let engine = Engine::spawn().unwrap();
            engine.send(Job::Open { path: path.to_path_buf() });
            match recv(&engine) {
                JobResult::Opened { schema, total } => {
                    assert_eq!(total, 100);
                    assert_eq!(schema.len(), 4);
                    assert_eq!(schema[0].name, "id");
                    assert!(matches!(schema[0].ty, ColType::Integer));
                    assert!(matches!(schema[2].ty, ColType::Text));
                    assert!(matches!(schema[3].ty, ColType::Date));
                    Self { engine, path: path.to_path_buf() }
                }
                other => panic!("expected Opened, got {other:?}"),
            }
        }

        fn count(&self, filter: Option<String>) -> u64 {
            self.engine.send(Job::Count { epoch: 0, filter });
            match recv(&self.engine) {
                JobResult::Count { epoch: 0, total } => total,
                other => panic!("expected Count, got {other:?}"),
            }
        }

        fn rows(&self, page: u64, sort: Vec<SortKey>, filter: Option<String>) -> RowBatch {
            self.engine.send(Job::Rows { epoch: 0, page, sort, filter });
            match recv(&self.engine) {
                JobResult::Rows(b) => b,
                other => panic!("expected Rows, got {other:?}"),
            }
        }
    }

    #[test]
    fn open_count_rows() {
        let dir = tmp("open-count-rows");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);

        assert_eq!(app.count(None), 100);
        assert_eq!(app.count(Some("id > 90".into())), 9);
        assert_eq!(app.count(Some("label IS NULL".into())), 34);

        let batch = app.rows(0, Vec::new(), None);
        assert_eq!(batch.start_row, 0);
        assert_eq!(batch.rows.len(), crate::data::model::PAGE_SIZE.min(100) as usize);
        // First row, first column: id = 0.
        assert_eq!(batch.rows[0][0], CellValue::Int(0));
        // Row 0: i % 3 == 0 -> label null.
        assert_eq!(batch.rows[0][2], CellValue::Null);
        // Row 1 label text.
        assert_eq!(batch.rows[1][2], CellValue::Text("row_1".into()));

        // Sorted descending by id: first row is id = 99.
        let batch = app.rows(
            0,
            vec![SortKey { col: 0, ascending: false }],
            None,
        );
        assert_eq!(batch.rows[0][0], CellValue::Int(99));

        // Sorted ascending by score: first is id 0, score 0.0.
        let batch = app.rows(0, vec![SortKey { col: 1, ascending: true }], None);
        assert_eq!(batch.rows[0][0], CellValue::Int(0));
    }

    #[test]
    fn bad_filter_fails_short() {
        let dir = tmp("bad-filter");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);
        app.engine.send(Job::Count { epoch: 0, filter: Some("id >".into()) });
        match recv(&app.engine) {
            JobResult::Failed { source: FailSource::Filter, message } => {
                assert!(!message.is_empty());
                assert!(message.lines().count() <= 2, "should be short: {message}");
            }
            other => panic!("expected filter failure, got {other:?}"),
        }
    }

    #[test]
    fn stats_and_top_values() {
        let dir = tmp("stats");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);

        app.engine.send(Job::Stats { col: 0, filter: None });
        match recv(&app.engine) {
            JobResult::Stats(s) => {
                assert_eq!(s.rows, 100);
                assert_eq!(s.nulls, 0);
                assert_eq!(s.distinct, Some(100));
                let num = s.numeric.expect("numeric stats for id");
                assert_eq!(num.min, 0.0);
                assert_eq!(num.max, 99.0);
            }
            other => panic!("expected Stats, got {other:?}"),
        }

        app.engine.send(Job::TopValues { col: 2, limit: 200, filter: None });
        match recv(&app.engine) {
            JobResult::TopValues { values, truncated, .. } => {
                // 33 non-null labels of 100 (i % 3 != 0) + 1 null group.
                assert_eq!(values.len(), 67, "33 distinct labels + null");
                assert!(!truncated);
                assert!(values.iter().any(|(v, _)| *v == CellValue::Null));
            }
            other => panic!("expected TopValues, got {other:?}"),
        }
    }

    #[test]
    fn save_applies_edits_and_preserves_order() {
        let dir = tmp("save");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);

        // Edit row 5: id 5 -> 500; label -> null; score -> 99.5.
        let overlay = vec![
            (5, 0, EditValue::Text("500".into())),
            (5, 2, EditValue::Null),
            (5, 1, EditValue::Text("99.5".into())),
        ];
        let (tx, _rx) = mpsc::channel::<String>();
        app.engine.send(Job::Save { overlay, progress: tx });
        match recv(&app.engine) {
            JobResult::Done { kind: JobKind::Save, message } => {
                assert!(message.contains("saved"));
            }
            JobResult::Failed { message, .. } => panic!("save failed: {message}"),
            other => panic!("expected Done, got {other:?}"),
        }

        // Verify on disk with a fresh connection.
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let (id, score, label): (i64, f64, Option<String>) = conn
            .query_row(
                &format!(
                    "SELECT id, score, label FROM (SELECT row_number() OVER () - 1 AS rownum, * FROM read_parquet({})) WHERE rownum = 5",
                    crate::data::types::sql_quote(&path.to_string_lossy())
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(id, 500);
        assert_eq!(score, 99.5);
        assert_eq!(label, None);
        // Order preserved: rowid 0 is still id 0, rowid 99 is id 99.
        let first: i64 = conn
            .query_row(
                &format!("SELECT id FROM (SELECT row_number() OVER () - 1 AS rownum, * FROM read_parquet({})) WHERE rownum = 0", crate::data::types::sql_quote(&path.to_string_lossy())),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(first, 0);
        let last: i64 = conn
            .query_row(
                &format!("SELECT id FROM (SELECT row_number() OVER () - 1 AS rownum, * FROM read_parquet({})) WHERE rownum = 99", crate::data::types::sql_quote(&path.to_string_lossy())),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last, 99);
    }

    #[test]
    fn export_csv_bom_and_delimiter() {
        let dir = tmp("export-csv");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);
        let out = dir.join("out.csv");
        let (tx, _rx) = mpsc::channel::<String>();
        app.engine.send(Job::Export {
            path: out.clone(),
            filter: Some("id > 40".into()),
            format: ExportFormat::Csv,
            delimiter: ';',
            progress: tx,
        });
        match recv(&app.engine) {
            JobResult::Done { kind: JobKind::Export, message } => {
                assert!(message.contains("59 rows"), "{message}");
            }
            JobResult::Failed { message, .. } => panic!("export failed: {message}"),
            other => panic!("expected Done, got {other:?}"),
        }
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM required");
        let text = String::from_utf8_lossy(&bytes[3..]);
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert_eq!(header, "id;score;label;day");
        let first = lines.next().unwrap();
        assert!(first.starts_with("41;61.5;"), "got: {first}");
    }

    #[test]
    fn export_xlsx_basic() {
        let dir = tmp("export-xlsx");
        let path = dir.join("t.parquet");
        make_parquet(&path, 100);
        let app = TestApp::open(&path);
        let out = dir.join("out.xlsx");
        let (tx, _rx) = mpsc::channel::<String>();
        app.engine.send(Job::Export {
            path: out.clone(),
            filter: None,
            format: ExportFormat::Xlsx,
            delimiter: ';',
            progress: tx,
        });
        match recv(&app.engine) {
            JobResult::Done { kind: JobKind::Export, message } => {
                assert!(message.contains("100 rows"), "{message}");
            }
            JobResult::Failed { message, .. } => panic!("export failed: {message}"),
            other => panic!("expected Done, got {other:?}"),
        }
        let meta = std::fs::metadata(&out).unwrap();
        assert!(meta.len() > 1000, "xlsx should be a real zip");
        // Zip magic check (xlsx is a zip container).
        let magic = std::fs::read(&out).unwrap()[..2].to_vec();
        assert_eq!(magic, vec![0x50, 0x4b]);
    }

    #[test]
    fn open_rejects_non_parquet() {
        let dir = tmp("bad-file");
        let path = dir.join("not.parquet");
        std::fs::write(&path, b"this is not parquet").unwrap();
        let engine = Engine::spawn().unwrap();
        engine.send(Job::Open { path: path.clone() });
        match recv(&engine) {
            JobResult::Failed { message, .. } => {
                assert!(message.contains("cannot read"), "{message}");
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }
}

