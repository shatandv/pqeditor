//! Popovers and modal dialogs: column filter, export, go-to-row.
//!
//! The export and go-to-row dialogs use `egui::Modal` (scrim + centered
//! window). The filter popover is a plain floating `egui::Window`.
#![allow(deprecated)] // egui::Id::new is the stable-id constructor we want

use egui::{Key, RichText, TextEdit};

use crate::app::{App, ExportFormat, ExportScope, NULL_SENTINEL};
use crate::data::types::ColType;
use crate::theme;

/// Draw all open dialogs; returns true if a dialog is open (so grid
/// keyboard shortcuts should not fire this frame).
pub fn draw(ctx: &egui::Context, app: &mut App) -> bool {
    let mut captured = false;
    captured |= confirm_dialog(ctx, app);
    captured |= filter_popover(ctx, app);
    captured |= export_dialog(ctx, app);
    captured |= goto_row_dialog(ctx, app);
    captured
}

/// Unsaved-changes confirmation (opening another file or quitting).
fn confirm_dialog(ctx: &egui::Context, app: &mut App) -> bool {
    let Some(confirm) = app.confirm else {
        return false;
    };
    let mut action: Option<u8> = None; // 0 = save, 1 = discard, 2 = cancel

    egui::Modal::new(egui::Id::new("pq-confirm")).show(ctx, |ui| {
        ui.label(
            RichText::new("Unsaved changes")
                .color(theme::TEXT_HEADER)
                .size(14.0),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(match confirm {
                crate::app::Confirm::OpenFile => {
                    "Opening another file will discard the unsaved edits."
                }
                crate::app::Confirm::Quit => "Quit and discard the unsaved edits?",
            })
            .color(theme::TEXT),
        );
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(
                    RichText::new("Save").color(theme::ACCENT_TEXT),
                ))
                .clicked()
            {
                action = Some(0);
            }
            if ui.button("discard").clicked() {
                action = Some(1);
            }
            if ui.button("cancel").clicked() {
                action = Some(2);
            }
        });
    });

    match action {
        Some(0) => {
            // Save first; the pending open / quit continues after the save
            // job finishes.
            if matches!(confirm, crate::app::Confirm::Quit) {
                app.quit_after_save = true;
            }
            app.confirm = None;
            app.save();
        }
        Some(1) => {
            if let Some(f) = app.file.as_mut() {
                f.grid.overlay.clear();
                f.editing = None;
                f.selection = None;
            }
            // Deleted rows were excluded from the view; restore it.
            app.engine
                .send(crate::data::engine::Job::SetDeletedRows { rows: vec![] });
            app.refresh_count();
            match confirm {
                crate::app::Confirm::OpenFile => {
                    app.confirm = None;
                    if let Some(p) = app.pending_open.take() {
                        app.open_path(p);
                    }
                }
                crate::app::Confirm::Quit => {
                    app.confirm = None;
                    app.close_requested_flag = true;
                }
            }
        }
        Some(_) => {
            // Cancel: stay in the current file.
            app.confirm = None;
            app.pending_open = None;
        }
        None => {}
    }
    app.confirm.is_some()
}

fn filter_popover(ctx: &egui::Context, app: &mut App) -> bool {
    // Pre-read the metadata (immutable borrows end before the mutable ones).
    let (col, name, ty) = match app.file.as_ref().and_then(|f| f.popover.as_ref()) {
        Some(pop) => (
            pop.col,
            app.file
                .as_ref()
                .and_then(|f| f.grid.schema.get(pop.col).map(|c| c.name.clone()))
                .unwrap_or_default(),
            app.file
                .as_ref()
                .and_then(|f| f.grid.schema.get(pop.col).map(|c| c.ty)),
        ),
        None => return false,
    };

    let mut close_requested = false;
    let mut apply_requested = false;
    let mut clear_requested = false;

    egui::Window::new(format!("filter · {name}"))
        .open(&mut close_requested)
        .resizable(false)
        .collapsible(false)
        .default_width(280.0)
        .default_pos([200.0, 120.0])
        .show(ctx, |ui| {
            let Some(pop) = app
                .file
                .as_mut()
                .and_then(|f| f.popover.as_mut())
                .filter(|p| p.col == col)
            else {
                return;
            };

            ui.add(
                TextEdit::singleline(&mut pop.search)
                    .hint_text(RichText::new("search values\u{2026}").color(theme::TEXT_DIM))
                    .desired_width(ui.available_width()),
            );

            if pop.loading {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("loading values\u{2026}").color(theme::TEXT_DIM));
                });
                ui.add_space(6.0);
                return;
            }

            ui.add_space(4.0);
            let needle = pop.search.to_lowercase();
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    for (value, count) in &pop.values {
                        if !needle.is_empty() && !value.to_lowercase().contains(&needle) {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            let mut checked = pop.checked.contains(value);
                            if ui.checkbox(&mut checked, value).changed() {
                                if checked {
                                    pop.checked.insert(value.clone());
                                } else {
                                    pop.checked.remove(value);
                                }
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(crate::util::fmt_count(*count))
                                            .color(theme::TEXT_DIM)
                                            .size(11.0),
                                    );
                                },
                            );
                        });
                    }
                    if ty.is_some_and(|t| t != ColType::Other) {
                        let mut checked = pop.checked.contains(NULL_SENTINEL);
                        if ui
                            .checkbox(
                                &mut checked,
                                RichText::new(NULL_SENTINEL).color(theme::TEXT_DIM),
                            )
                            .changed()
                        {
                            if checked {
                                pop.checked.insert(NULL_SENTINEL.into());
                            } else {
                                pop.checked.remove(NULL_SENTINEL);
                            }
                        }
                    }
                });
            if pop.truncated {
                ui.label(
                    RichText::new("showing top 200 values by frequency")
                        .color(theme::TEXT_DIM)
                        .size(10.5),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("clear").clicked() {
                    clear_requested = true;
                }
                if ui
                    .add(egui::Button::new(
                        RichText::new("apply").color(theme::ACCENT_TEXT),
                    ))
                    .clicked()
                {
                    apply_requested = true;
                }
            });
        });

    if close_requested && let Some(f) = app.file.as_mut() {
        f.popover = None;
    }
    if apply_requested {
        app.apply_popover();
    }
    if clear_requested {
        if let Some(f) = app.file.as_mut() {
            f.popover = None;
        }
        app.clear_filter();
    }
    app.file.as_ref().is_some_and(|f| f.popover.is_some())
}

fn export_dialog(ctx: &egui::Context, app: &mut App) -> bool {
    // Snapshot dialog state (immutable borrows end here).
    let Some(dialog) = app.file.as_ref().and_then(|f| f.export_dialog.as_ref()) else {
        return false;
    };
    let mut dialog = dialog.clone();
    let filter_active = app.file.as_ref().is_some_and(|f| f.grid.filter.is_some());
    let mut cancel = false;
    let mut browse = false;
    let mut submit = false;

    // Fixed width: the path TextEdit must not push the modal wider every
    // frame (runaway auto-sizing on any input). Modal sizes to content, so
    // pin the content width.
    egui::Modal::new(egui::Id::new("pq-export")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new("Export").color(theme::TEXT_HEADER).size(14.0));
        ui.add_space(4.0);

        ui.label(RichText::new("scope").color(theme::TEXT_DIM).size(11.0));
        ui.horizontal(|ui| {
            if ui
                .selectable_label(dialog.scope == ExportScope::Filtered && filter_active, "filtered")
                .clicked()
            {
                dialog.scope = ExportScope::Filtered;
            }
            if ui.selectable_label(dialog.scope == ExportScope::All, "all rows").clicked() {
                dialog.scope = ExportScope::All;
            }
            if dialog.scope == ExportScope::Filtered && !filter_active {
                ui.label(RichText::new("(no filter active \u{2192} everything)").color(theme::TEXT_DIM).size(10.5));
            }
        });

        ui.add_space(6.0);
        ui.label(RichText::new("format").color(theme::TEXT_DIM).size(11.0));
        ui.horizontal(|ui| {
            if ui.selectable_label(dialog.format == ExportFormat::Csv, "csv").clicked() {
                dialog.format = ExportFormat::Csv;
            }
            if ui.selectable_label(dialog.format == ExportFormat::Xlsx, "xlsx").clicked() {
                dialog.format = ExportFormat::Xlsx;
            }
        });

        if dialog.format == ExportFormat::Csv {
            ui.add_space(6.0);
            ui.label(RichText::new("delimiter").color(theme::TEXT_DIM).size(11.0));
            ui.horizontal(|ui| {
                for (ch, label) in [(';', ";"), (',', ","), ('\t', "tab"), ('|', "|")] {
                    if ui.selectable_label(dialog.delimiter == ch, label).clicked() {
                        dialog.delimiter = ch;
                    }
                }
            });
        }

        ui.add_space(6.0);
        ui.label(RichText::new("destination").color(theme::TEXT_DIM).size(11.0));
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width() - 80.0, 22.0],
                TextEdit::singleline(&mut dialog.path),
            );
            if ui.button("browse\u{2026}").clicked() {
                browse = true;
            }
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new(
                match dialog.format {
                    ExportFormat::Csv => "csv: utf-8 (BOM) · edits included · runs in background",
                    ExportFormat::Xlsx => "xlsx: bold frozen header · autofilter · autofit columns · typed cells · Excel row limit",
                },
            )
            .color(theme::TEXT_DIM)
            .size(10.5),
        );

        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(RichText::new("export").color(theme::ACCENT_TEXT)))
                .clicked()
            {
                submit = true;
            }
            if ui.button("cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                cancel = true;
            }
        });
    });

    if browse {
        let ext = match dialog.format {
            ExportFormat::Csv => "csv",
            ExportFormat::Xlsx => "xlsx",
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(ext.to_uppercase(), &[ext])
            .save_file()
        {
            dialog.path = path.to_string_lossy().into_owned();
        }
    }
    if submit {
        // Write the (possibly edited) dialog state back, then start.
        if let Some(f) = app.file.as_mut() {
            f.export_dialog = Some(dialog.clone());
        }
        app.start_export();
        return false; // dialog closes itself in start_export
    }
    if cancel {
        if let Some(f) = app.file.as_mut() {
            f.export_dialog = None;
        }
        return false;
    }
    // Persist any edits made this frame.
    if let Some(current) = app.file.as_mut().and_then(|f| f.export_dialog.as_mut()) {
        *current = dialog;
    }
    true
}

fn goto_row_dialog(ctx: &egui::Context, app: &mut App) -> bool {
    let Some(draft) = app.file.as_ref().and_then(|f| f.goto_row.clone()) else {
        return false;
    };
    let total = app.file.as_ref().map(|f| f.grid.shown_rows).unwrap_or(0);
    let mut go: Option<u64> = None;
    let mut draft = draft;

    egui::Modal::new(egui::Id::new("pq-goto")).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label("row");
            let resp = ui.add_sized(
                [140.0, 22.0],
                TextEdit::singleline(&mut draft).desired_width(140.0),
            );
            resp.request_focus();
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                go = draft.trim().replace(',', "").parse::<u64>().ok();
            }
            ui.label(
                RichText::new(format!("of {}", crate::util::fmt_count(total)))
                    .color(theme::TEXT_DIM),
            );
        });
    });

    if let Some(row) = go {
        let row = row.min(total.saturating_sub(1));
        if let Some(f) = app.file.as_mut() {
            f.selection = Some(crate::app::Sel::single(row, 0));
            f.scroll_to_row = Some(crate::app::ScrollRequest::Center(row));
            f.goto_row = None;
        }
        return false;
    }
    // Store the draft back (Esc closes via Modal's default close behavior;
    // keep draft in state until closed).
    if let Some(f) = app.file.as_mut() {
        f.goto_row = Some(draft);
    }
    true
}
