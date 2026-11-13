//! eframe shell: viewport setup, global keyboard handling, panel
//! composition. Everything here is UI plumbing; the logic lives in
//! `app`/`data` (see DESIGN.md). Split from main.rs so the library can
//! be driven by tests and tools.

use eframe::egui;
use egui::Key;

use crate::app::App;
use crate::ui;

/// Window title carries the build stamp: a stale exe is recognizable at
/// a glance (bit us once — dist/ had a pre-fix build for hours).
pub fn build_title() -> String {
    format!(
        "pqeditor {} (build {})",
        env!("CARGO_PKG_VERSION"),
        build_stamp()
    )
}

/// Set by build.rs; falls back to "dev" when unset (plain `cargo test`).
fn build_stamp() -> &'static str {
    match option_env!("PQEDITOR_BUILD_STAMP") {
        Some(s) => s,
        None => "dev",
    }
}

/// Panic hook: write panics to a file next to the exe. The Windows GUI
/// build has no console/stderr, so panics were previously invisible.
pub fn install_panic_log() {
    std::panic::set_hook(Box::new(|info| {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let text = format!("=== panic at unix {} ===\n{info}\n", stamp);
        let file = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("pqeditor-panic.log")));
        if let Some(path) = file {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        // Also keep the default behavior (stderr) when there is one.
        eprintln!("{text}");
    }));
}

/// 32x32 RGBA icon: dark square with an amber border. Cheap and in-theme.
pub fn load_icon() -> egui::IconData {
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
        rgba,
    }
}

impl eframe::App for App {
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

        // Unsaved-edits guard for the window close button: cancel the close
        // and ask. A deliberate close (confirm dialog said discard) is not
        // cancelled again.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested
            && self.dirty()
            && !self.close_requested_flag
            && self.confirm != Some(crate::app::Confirm::Quit)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(crate::app::Confirm::Quit);
            self.pending_open = None;
        }
        if self.close_requested_flag {
            self.close_requested_flag = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Files dragged onto the window (start screen and grid alike).
        let dropped: Vec<_> = ui.input(|i| i.raw.dropped_files.clone());
        if let Some(f) = dropped
            .iter()
            .find(|f| f.path() != std::path::Path::new(""))
        {
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

impl App {
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
        let shift = ctx.input(|i| i.modifiers.shift);
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
            self.move_selection(-1, 0, shift);
        } else if down {
            self.move_selection(1, 0, shift);
        } else if left {
            self.move_selection(0, -1, shift);
        } else if right {
            self.move_selection(0, 1, shift);
        } else if page_up {
            self.move_selection(-20, 0, shift);
        } else if page_down {
            self.move_selection(20, 0, shift);
        } else if home {
            self.move_selection(i64::MIN / 2, 0, shift);
        } else if end {
            self.move_selection(i64::MAX / 2, 0, shift);
        }

        let (f2, enter, del) = ctx.input(|i| {
            (
                i.key_pressed(Key::F2),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Delete),
            )
        });
        if f2 || enter {
            self.begin_edit();
        } else if del && !ctrl {
            // Whole-row selection: delete the rows. Cell selection: clear
            // the selected cells to NULL.
            let whole_rows = self
                .file
                .as_ref()
                .and_then(|f| f.selection.as_ref())
                .is_some_and(|s| s.is_whole_rows(self.file.as_ref().unwrap().grid.schema.len()));
            if whole_rows {
                self.delete_selected_rows();
            } else {
                self.set_cell_null();
            }
        } else if ctrl && del {
            self.set_cell_null();
        }

        // Ctrl+C / Ctrl+X / Ctrl+V never arrive as key events: egui-winit
        // converts them to Copy / Cut / Paste events before egui sees them.
        // React to those, but only when no text field has focus (a focused
        // TextEdit handles the clipboard natively).
        if focus_free && !dialog_open {
            let (copy, paste) = ctx.input(|i| {
                let mut copy = false;
                let mut paste = None;
                for event in &i.raw.events {
                    match event {
                        egui::Event::Copy | egui::Event::Cut => copy = true,
                        egui::Event::Paste(text) => paste = Some(text.clone()),
                        _ => {}
                    }
                }
                (copy, paste)
            });
            if copy {
                self.copy_selection(&ctx);
            } else if let Some(text) = paste
                && !text.is_empty()
            {
                self.paste_tsv(&text);
            }
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
        let text = self.file.as_mut().and_then(|f| {
            f.selection
                .as_ref()
                .and_then(|sel| f.grid.copy_rect(sel, 1_000_000))
        });
        if let Some(text) = text {
            // egui's clipboard goes through eframe's own arboard instance;
            // the Paste event reads the same clipboard back.
            ctx.copy_text(text);
            self.status = "copied".into();
        }
    }
}

impl App {
    /// Any background work pending that needs repaints.
    fn busy(&self) -> bool {
        self.saving
            || self.exporting
            || self.file.as_ref().is_some_and(|f| {
                f.count_pending || f.pending_filter.is_some() || f.grid.cache_has_pending()
            })
    }

    /// eframe calls this periodically and on exit.
    fn save_state(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, crate::app::STORAGE_KEY, &self.recents);
    }
}

/// Launch the UI. Split from main() so tests and tools can link the
/// library without dragging eframe's windowing into them.
pub fn run_native(open_path: Option<std::path::PathBuf>) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(build_title())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 420.0])
            .with_icon(load_icon()),
        // OpenGL renderer: broader driver/device compatibility than wgpu
        // (which needs Vulkan or D3D12).
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "pqeditor",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            if let Some(path) = open_path {
                app.open_path(path);
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
    .expect("pqeditor crashed");
}
