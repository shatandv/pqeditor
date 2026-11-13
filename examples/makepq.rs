//! Generate a small deterministic parquet for interactive testing.
//! usage: makepq <path> <rows>
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "t.parquet".into());
    let rows: i64 = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(100);
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT i AS id, i * 1.5 AS score, 'item_' || i AS label \
         FROM range({rows}) t(i)) TO {}",
        pqeditor::data::types::sql_quote(&path)
    ))
    .unwrap();
}
