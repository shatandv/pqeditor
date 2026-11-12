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

#[cfg(test)]
mod tests {
    use super::fmt_count;

    #[test]
    fn grouping() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_204), "1,204");
        assert_eq!(fmt_count(10_000_000), "10,000,000");
    }
}
