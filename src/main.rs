//! pqeditor: a barebones spreadsheet-style editor for parquet files.
//!
//! Module map (see AGENTS.md for the full architecture):
//! - `data::engine`: duckdb worker thread + job protocol
//! - `data::model`: windowed row cache + edit overlay (UI thread)
//! - `data::types`: value types crossing the worker boundary
//! - `app`: application state and actions
//! - `ui`: drawing code per region
//! - `theme`: the design system

#![warn(clippy::all)]
#![allow(clippy::too_many_arguments)]
// No console window in release builds on Windows.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod data;
mod theme;
mod ui;
mod util;

use eframe::egui;
use egui::Key;

pub const STORAGE_KEY: &str = "pqeditor/recents";

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("pqeditor")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 420.0])
            .with_icon(load_icon()),
        // OpenGL renderer: broader driver/device compatibility than wgpu
        // (which needs Vulkan or D3D12).
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native("pqeditor", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
        .expect("pqeditor crashed");
}

/// 32x32 RGBA icon: dark square with an amber border. Cheap and in-theme.
fn load_icon() -> egui::IconData {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let edge = x < 3 || y < 3 || x >= S - 3 || y >= S - 3;
            let i = (y * S + x) * 4;
            if edge {
                rgba[i..i + 3].copy_from_slice(&[0xe8, 0xa1, 0x3c]);
            } else {
                rgba[i..i + 3].copy_from_slice(&[0x1d, 0x20, 0x27]);
            }
            rgba[i + 3] = 255;
        }
    }
    egui::IconData {
        width: S as u32,
        height: S as u32,
        rgba: rgba.into(),
    }
}

impl eframe::App for app::App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.save_state(storage);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // No UI here: drain the worker and keep repaints coming.
        self.poll();
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Files dragged onto the window (start screen and grid alike).
        let dropped: Vec<_> = ui.input(|i| i.raw.dropped_files.clone());
        if let Some(f) = dropped.iter().find(|f| f.path() != std::path::Path::new("")) {
            self.open_path(f.path().to_path_buf());
        }

        self.handle_keys(ui);

        // Panels; the grid (or start screen) fills what remains.
        let mut actions = ui::panels::toolbar(ui, self);
        ui::panels::status_bar(ui, self);
        actions.extend(ui::panels::stats_strip(ui, self).into_iter().rev());

        if self.file.is_some() {
            ui::grid::draw(ui, self);
        } else {
            actions.extend(ui::panels::start_screen(ui, self));
        }

        ui::dialogs::draw(&ctx, self);

        for action in actions {
            match action {
                ui::panels::PanelAction::Open => self.open_dialog(),
                ui::panels::PanelAction::Save => self.save(),
                ui::panels::PanelAction::Export => self.open_export_dialog(),
                ui::panels::PanelAction::SubmitFilter => self.submit_filter(),
                ui::panels::PanelAction::ClearFilter => self.clear_filter(),
                ui::panels::PanelAction::DropSort(col) => self.cycle_sort(col, false),
                ui::panels::PanelAction::CloseStats => {
                    if let Some(f) = self.file.as_mut() {
                        f.stats.clear();
                    }
                }
                ui::panels::PanelAction::OpenRecent(i) => {
                    if let Some(r) = self.recents.get(i) {
                        let path = std::path::PathBuf::from(r.path.clone());
                        self.open_path(path);
                    }
                }
            }
        }

        // Persist recents: done in App::save (called by eframe on exit and
        // on the auto-save interval).
    }
}

impl app::App {
    /// Global keyboard shortcuts. Suppressed while any text field has focus
    /// (except Ctrl+O / Ctrl+S) and while a modal dialog is open.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let focus_free = ctx.memory(|m| m.focused().is_none());
        let dialog_open = self.file.as_ref().is_some_and(|f| {
            f.popover.is_some() || f.export_dialog.is_some() || f.goto_row.is_some()
        });

        // Ctrl+O / Ctrl+S work even while typing in the filter box.
        let ctrl = ctx.input(|i| i.modifiers.ctrl);
        if ctrl && ctx.input(|i| i.key_pressed(Key::O)) {
            self.open_dialog();
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::S)) {
            self.save();
            return;
        }
        if !focus_free || dialog_open {
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::L)) {
            self.focus_filter_next = true;
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::E)) {
            self.open_export_dialog();
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::G)) {
            if let Some(f) = self.file.as_mut() {
                f.goto_row = Some(String::new());
            }
            return;
        }
        if self.file.is_none() {
            return;
        }

        // Selection navigation (no borrow held while acting).
        let (up, down, left, right) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
            )
        });
        let (page_up, page_down) =
            ctx.input(|i| (i.key_pressed(Key::PageUp), i.key_pressed(Key::PageDown)));
        let (home, end) = ctx.input(|i| (i.key_pressed(Key::Home), i.key_pressed(Key::End)));
        if up {
            self.move_selection(-1, 0);
        } else if down {
            self.move_selection(1, 0);
        } else if left {
            self.move_selection(0, -1);
        } else if right {
            self.move_selection(0, 1);
        } else if page_up {
            self.move_selection(-20, 0);
        } else if page_down {
            self.move_selection(20, 0);
        } else if home {
            self.move_selection(i64::MIN / 2, 0);
        } else if end {
            self.move_selection(i64::MAX / 2, 0);
        }

        let (f2, enter, del, c) = ctx.input(|i| {
            (
                i.key_pressed(Key::F2),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::C),
            )
        });
        if f2 || enter {
            self.begin_edit();
        } else if ctrl && del {
            self.set_cell_null();
        } else if ctrl && c {
            self.copy_selection(&ctx);
        }

        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        if esc {
            // Close, in priority order: popover, goto, stats, edit.
            if let Some(file) = self.file.as_mut() {
                if file.popover.is_some() {
                    file.popover = None;
                } else if file.goto_row.is_some() {
                    file.goto_row = None;
                } else if !file.stats.is_empty() {
                    file.stats.clear();
                } else if file.editing.is_some() {
                    file.editing = None;
                } else {
                    file.selection = None;
                }
            }
        }
    }

    fn copy_selection(&mut self, ctx: &egui::Context) {
        if let Some(f) = self.file.as_mut() {
            if let Some(sel) = f.selection {
                if let Some(text) = f.grid.copy_text(sel.rowid, sel.col) {
                    ctx.copy_text(text);
                }
            }
        }
    }
}

impl app::App {
    /// Any background work pending that needs repaints.
    fn busy(&self) -> bool {
        self.saving
            || self.exporting
            || self
                .file
                .as_ref()
                .is_some_and(|f| {
                    f.count_pending || f.pending_filter.is_some() || f.grid.cache_has_pending()
                })
    }

    /// eframe calls this periodically and on exit.
    fn save_state(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.recents);
    }
}
