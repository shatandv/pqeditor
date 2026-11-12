//! Shared value types exchanged between the DuckDB engine and the UI.
//!
//! Everything crossing the worker-thread boundary is plain data: no duckdb
//! types, no egui types. The engine converts duckdb `Value`s into `CellValue`,
//! the UI renders `CellValue` and hands edits back as `EditValue` (text that
//! has already passed validation for the column's type).

use std::fmt;

/// Physical column type as reported by DuckDB, narrowed to what matters for
/// rendering, editing and export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    /// Any signed/unsigned integer type.
    Integer,
    /// Float/double/decimal.
    Float,
    Boolean,
    Date,
    Timestamp,
    Time,
    Text,
    /// list/struct/map and anything else: rendered as text, read-only.
    Other,
}

impl ColType {
    /// Human name used in the stats strip and tooltips.
    pub fn label(self) -> &'static str {
        match self {
            ColType::Integer => "integer",
            ColType::Float => "float",
            ColType::Boolean => "boolean",
            ColType::Date => "date",
            ColType::Timestamp => "timestamp",
            ColType::Time => "time",
            ColType::Text => "text",
            ColType::Other => "other",
        }
    }

    /// SQL type name used when casting edited values on save.
    pub fn sql_type(self) -> &'static str {
        match self {
            ColType::Integer => "BIGINT",
            ColType::Float => "DOUBLE",
            ColType::Boolean => "BOOLEAN",
            ColType::Date => "DATE",
            ColType::Timestamp => "TIMESTAMP",
            ColType::Time => "TIME",
            ColType::Text => "VARCHAR",
            ColType::Other => "VARCHAR",
        }
    }

    /// Whether cell editing is allowed at all.
    pub fn editable(self) -> bool {
        !matches!(self, ColType::Other)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSchema {
    pub name: String,
    pub ty: ColType,
}

/// A single cell as the engine produced it.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Date/timestamp/time, already formatted for display.
    Temporal(String),
    Text(String),
    /// Non-scalar or unrecognized value, already stringified.
    Other(String),
}

impl CellValue {
    /// Text for the grid, the editor's initial content and CSV/XLSX export.
    pub fn display(&self) -> String {
        match self {
            CellValue::Null => String::new(),
            CellValue::Int(v) => v.to_string(),
            CellValue::Float(v) => format_float(*v),
            CellValue::Bool(v) => v.to_string(),
            CellValue::Temporal(s) | CellValue::Text(s) | CellValue::Other(s) => s.clone(),
        }
    }
}

/// Compact float formatting: no trailing `.0`, no scientific notation for
/// ordinary magnitudes. Mirrors what DuckDB prints.
pub fn format_float(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{v}")
}

impl fmt::Display for CellValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// A pending edit as the UI produced it: text that passed validation for the
/// column type, or `Null` (Ctrl+Delete). Cast to the column's SQL type on save.
#[derive(Debug, Clone, PartialEq)]
pub enum EditValue {
    Null,
    Text(String),
}

/// One sort key: column index + direction. Empty vec = file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKey {
    pub col: usize,
    pub ascending: bool,
}

/// Column statistics for the stats strip.
#[derive(Debug, Clone)]
pub struct ColumnStats {
    pub col: usize,
    pub rows: u64,
    pub nulls: u64,
    pub distinct: Option<u64>,
    /// Numeric-only stats; `None` for other types.
    pub numeric: Option<NumericStats>,
    /// Top values for text-like columns (value, count), longest first computed
    /// by the engine; capped count set by the engine.
    pub top: Vec<(CellValue, u64)>,
}

#[derive(Debug, Clone, Copy)]
pub struct NumericStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub stddev: f64,
}

/// Validate raw editor text against a column type. Returns the normalized
/// text to store in the overlay, or an error message for the status bar.
pub fn validate_edit(ty: ColType, raw: &str) -> Result<EditValue, String> {
    let s = raw.trim();
    if s.is_empty() {
        // An empty field means "set NULL"; Ctrl+Delete produces the same.
        return Ok(EditValue::Null);
    }
    match ty {
        ColType::Other => Err("column type is read-only".into()),
        ColType::Integer => s
            .parse::<i64>()
            .map(|_| EditValue::Text(s.to_string()))
            .map_err(|_| format!("'{}' is not an integer", s)),
        ColType::Float => s
            .parse::<f64>()
            .map(|_| EditValue::Text(s.to_string()))
            .map_err(|_| format!("'{}' is not a number", s)),
        ColType::Boolean => match s.to_ascii_lowercase().as_str() {
            "true" | "false" | "t" | "f" | "1" | "0" => Ok(EditValue::Text(s.to_string())),
            _ => Err(format!("'{}' is not true/false", s)),
        },
        ColType::Date => {
            // Let DuckDB be the authority on save; here just reject obviously
            // wrong text by checking yyyy-mm-dd shape.
            let parts: Vec<&str> = s.splitn(3, '-').collect();
            let ok = parts.len() == 3
                && parts[0].len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            if ok {
                Ok(EditValue::Text(s.to_string()))
            } else {
                Err(format!("'{}' is not a date (yyyy-mm-dd)", s))
            }
        }
        ColType::Timestamp => {
            // Same idea: yyyy-mm-dd[ hh:mm[:ss[.f]]]
            let mut it = s.splitn(2, ' ');
            let d = it.next().unwrap_or("");
            let t = it.next().unwrap_or("");
            let date_ok = validate_edit(ColType::Date, d).is_ok();
            let time_ok = t.is_empty()
                || t.split(':')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit() || c == '.'));
            if date_ok && time_ok {
                Ok(EditValue::Text(s.to_string()))
            } else {
                Err(format!("'{}' is not a timestamp", s))
            }
        }
        ColType::Time => Ok(EditValue::Text(s.to_string())),
        ColType::Text => Ok(EditValue::Text(s.to_string())),
    }
}

/// Escape a string literal for inline use in DuckDB SQL (filter fragments,
/// view definitions).
pub fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Quote an identifier (column names come from parquet schemas and can be
/// arbitrary strings).
pub fn sql_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_int() {
        assert!(validate_edit(ColType::Integer, "42").is_ok());
        assert!(validate_edit(ColType::Integer, "-3").is_ok());
        assert!(validate_edit(ColType::Integer, "4.5").is_err());
        assert!(validate_edit(ColType::Integer, "").is_ok()); // = null
    }

    #[test]
    fn validate_bool_and_float() {
        assert!(validate_edit(ColType::Boolean, "TRUE").is_ok());
        assert!(validate_edit(ColType::Boolean, "yes").is_err());
        assert!(validate_edit(ColType::Float, "1e3").is_ok());
        assert!(validate_edit(ColType::Float, "abc").is_err());
    }

    #[test]
    fn validate_dates() {
        assert!(validate_edit(ColType::Date, "2024-03-02").is_ok());
        assert!(validate_edit(ColType::Date, "24-3-2").is_err());
        assert!(validate_edit(ColType::Timestamp, "2024-03-02 14:30:00").is_ok());
        assert!(validate_edit(ColType::Timestamp, "2024-03-02x").is_err());
    }

    #[test]
    fn quoting() {
        assert_eq!(sql_quote("o'brien"), "'o''brien'");
        assert_eq!(sql_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn float_display() {
        assert_eq!(format_float(128.0), "128");
        assert_eq!(format_float(21.34), "21.34");
        assert_eq!(format_float(f64::NAN), "NaN");
    }
}
