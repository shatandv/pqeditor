//! Dump the tail rows of a parquet file (paste verification).
fn main() {
    let path = std::env::args().nth(1).expect("usage: dumppq <file>");
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, score, label, flag FROM read_parquet({}) WHERE id >= 99990 ORDER BY id",
            pqeditor::data::types::sql_quote(&path)
        ))
        .unwrap();
    let rows: Vec<(i64, f64, Option<String>, Option<bool>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for (id, score, label, flag) in rows {
        println!("id={id} score={score} label={label:?} flag={flag:?}");
    }
}
