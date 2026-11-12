//! pqeditor library: the application logic (DuckDB worker, view model,
//! command layer) separated from the eframe shell, so integration tests
//! and tool binaries can drive the exact same code the UI runs.

pub mod app;
pub mod data;
pub mod driver;
pub mod theme;
pub mod ui;
pub mod util;
