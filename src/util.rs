//! Small helpers shared across the UI.

/// Thousands-separated decimal formatting for the status bar: 10000000 ->
/// "10,000,000".
pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let digits = s.len();
    for (i, c) in s.chars().enumerate() {
        out.push(c);
        let rest = digits - i - 1;
        if rest > 0 && rest.is_multiple_of(3) {
            out.push(',');
        }
    }
    out
}

/// Swap a path's extension for a new one. Only csv/xlsx extensions are
/// rewritten (the user may have typed something else deliberately); a
/// missing extension is left alone. Used by the export dialog when the
/// format changes.
pub fn swap_export_extension(path: &str, new_ext: &str) -> String {
    // Stay inside the file name: the last separator bounds the search.
    let dir_end = path.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let file = &path[dir_end..];
    let Some(dot) = file.rfind('.').filter(|d| *d > 0) else {
        return path.to_string();
    };
    let ext = file[dot + 1..].to_ascii_lowercase();
    if ext != "csv" && ext != "xlsx" {
        return path.to_string();
    }
    format!("{}{}", &path[..dir_end + dot + 1], new_ext)
}

#[cfg(test)]
mod tests {
    use super::{fmt_count, swap_export_extension};

    #[test]
    fn grouping() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_204), "1,204");
        assert_eq!(fmt_count(10_000_000), "10,000,000");
    }

    #[test]
    fn extension_swap() {
        assert_eq!(
            swap_export_extension("C:\\a\\b\\out.csv", "xlsx"),
            "C:\\a\\b\\out.xlsx"
        );
        assert_eq!(
            swap_export_extension("/tmp/x/out.xlsx", "csv"),
            "/tmp/x/out.csv"
        );
        assert_eq!(
            swap_export_extension("/tmp/x/OUT.CSV", "xlsx"),
            "/tmp/x/OUT.xlsx"
        );
        // No csv/xlsx extension: untouched (user's own extension wins).
        assert_eq!(
            swap_export_extension("/tmp/x/data.parquet", "csv"),
            "/tmp/x/data.parquet"
        );
        assert_eq!(swap_export_extension("/tmp/x/out", "csv"), "/tmp/x/out");
        assert_eq!(
            swap_export_extension("/tmp/x/a.b.csv", "xlsx"),
            "/tmp/x/a.b.xlsx"
        );
    }
}
