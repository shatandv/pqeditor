//! Generates a test parquet file with realistic mixed types:
//! ints, floats, text (with nulls), booleans, dates, timestamps.
//!
//! Usage: `cargo run --example make_test_parquet -- <out.parquet> [rows]`
//!
//! Handy for smoke-testing pqeditor on any platform.

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "test-data.parquet".into());
    let rows: i64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(100_000);

    let conn = duckdb::Connection::open_in_memory().expect("duckdb");
    conn.execute_batch(&format!(
        "COPY (
            SELECT
                i AS id,
                round(i * 1.37 + (i % 7) * 0.5, 2) AS score,
                CASE WHEN i % 13 = 0 THEN NULL ELSE 'item_' || (i % 997) END AS label,
                CASE WHEN i % 2 = 0 THEN NULL ELSE i % 2 = 1 END AS flag,
                DATE '2024-01-01' + CAST(i % 365 AS INTEGER) AS day,
                TIMESTAMP '2024-01-01 00:00:00' + INTERVAL (i % 31_536_000) SECOND AS seen_at
            FROM range({rows}) t(i)
        ) TO {} (FORMAT PARQUET, COMPRESSION zstd)",
        crate_pq_quote(&out),
    ))
    .expect("write parquet");
    println!("wrote {out} ({rows} rows)");
}

/// Inline quoting (the engine's helper lives in the binary crate, which
/// examples cannot import).
fn crate_pq_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
