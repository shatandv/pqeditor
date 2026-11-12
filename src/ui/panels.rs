//! Toolbar, stats strip, status bar, and the start screen.

use egui::{Align, RichText, TextEdit, Ui};

use crate::app::App;
use crate::data::types::{ColumnStats, NumericStats};
use crate::theme;
use crate::util::fmt_count;

pub const TOOLBAR_HEIGHT: f32 = 38.0;
pub const STATUS_HEIGHT: f32 = 24.0;
pub const STATS_ROW_HEIGHT: f32 = 24.0;

/// Actions a panel can request from the app.
pub enum PanelAction {
    Open,
    Save,
    Export,
    SubmitFilter,
    ClearFilter,
    DropSort(usize),
    CloseStats,
    OpenRecent(usize),
}

/// Debug tracing, enabled with PQEDITOR_DEBUG=1. Goes to stderr; on
/// Windows launch with stderr redirected (GUI app: no console).
pub fn dbg(msg: &str) {
    if std::env::var_os("PQEDITOR_DEBUG").is_some() {
        eprintln!("pqeditor: {msg}");
    }
}

pub fn toolbar(ui: &mut Ui, app: &mut App) -> Vec<PanelAction> {
    let mut actions = Vec::new();
    dbg("toolbar: enter");

    egui::Panel::top("pq-toolbar")
        .exact_size(TOOLBAR_HEIGHT)
        .frame(egui::Frame::NONE.fill(theme::BG_WINDOW).inner_margin(egui::Margin::symmetric(8, 4)))
        .show(ui, |ui| {
            // Panels default to a top-down layout; the toolbar needs a
            // horizontal one, or everything after the first button stacks
            // below the 38px panel edge and gets clipped.
            ui.horizontal(|ui| {
            let file_open = app.file.is_some();

            if ui
                .add_enabled(true, egui::Button::new(RichText::new("Open").color(theme::TEXT_HEADER)))
                .clicked()
            {
                actions.push(PanelAction::Open);
            }
            dbg("toolbar: open drawn");
            let save_resp = ui.add_enabled(
                file_open,
                egui::Button::new(
                    RichText::new("Save").color(if app.dirty() { theme::ACCENT_TEXT } else { theme::TEXT_HEADER }),
                ),
            );
            dbg(&format!("toolbar: save drawn rect={:?} visible={}", save_resp.rect, ui.is_rect_visible(save_resp.rect)));
            if save_resp.clicked()
            {
                actions.push(PanelAction::Save);
            }
            if ui
                .add_enabled(file_open, egui::Button::new(RichText::new("Export\u{2026}").color(theme::TEXT_HEADER)))
                .clicked()
            {
                actions.push(PanelAction::Export);
            }

            dbg("toolbar: export drawn");
            ui.separator();

            // Filter box.
            let Some(file) = app.file.as_mut() else {
                dbg("toolbar: nofile branch");
                // No file: keep the layout breathing room only.
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("choose a file (Ctrl+O)").clicked() {
                        actions.push(PanelAction::Open);
                    }
                });
                return;
            };
            dbg("toolbar: filter box drawing");
            let resp = ui.add_sized(
                [ui.available_width() - 24.0, 24.0],
                TextEdit::singleline(&mut file.filter_draft)
                    .hint_text(RichText::new("filter · SQL WHERE fragment · Ctrl+L").color(theme::TEXT_DIM))
                    .desired_width(f32::INFINITY)
                    .frame(egui::Frame::NONE),
            );
            if app.focus_filter_next {
                resp.request_focus();
                app.focus_filter_next = false;
            }
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if resp.lost_focus() && enter {
                actions.push(PanelAction::SubmitFilter);
            }
            if resp.has_focus() && esc {
                file.filter_draft.clear();
            }

            dbg(&format!("toolbar: filter drawn rect={:?}", resp.rect));
            // Clear filter button.
            let has_filter = file.grid.filter.is_some();
            if ui
                .add_enabled(has_filter, egui::Button::new(RichText::new("\u{2715}").color(theme::TEXT_DIM).size(11.0)))
                .clicked()
            {
                actions.push(PanelAction::ClearFilter);
            }

            // Sort chips.
            for (i, key) in file.grid.sort.iter().enumerate().rev() {
                let name = file
                    .grid
                    .schema
                    .get(key.col)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let arrow = if key.ascending { "\u{25b2}" } else { "\u{25bc}" };
                let chip = egui::Button::new(
                    RichText::new(format!("{name} {arrow}")).color(theme::ACCENT_TEXT).size(11.0),
                )
                .fill(theme::ACCENT_SOFT)
                .stroke(egui::Stroke::new(1.0, theme::ACCENT_BORDER));
                let resp = ui.add(chip);
                if resp.clicked() {
                    actions.push(PanelAction::DropSort(key.col));
                } else if resp.secondary_clicked() {
                    actions.push(PanelAction::DropSort(key.col));
                }
                let _ = i;
            }
            // Focus request from Ctrl+L is consumed inside; nothing here.
            });
        });
    actions
}

pub fn stats_strip(ui: &mut Ui, app: &mut App) -> Vec<PanelAction> {
    let mut actions = Vec::new();
    let Some(file) = app.file.as_ref() else { return actions };
    if file.stats.is_empty() {
        return actions;
    }

    egui::Panel::bottom("pq-stats")
        .exact_size(STATS_ROW_HEIGHT * (file.stats.len() as f32 + 0.4).min(6.0).max(1.0))
        .frame(egui::Frame::NONE.fill(theme::BG_PANEL).inner_margin(egui::Margin::symmetric(10, 4)))
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(STATS_ROW_HEIGHT * 5.0).show(ui, |ui| {
                for stat in &file.stats {
                    ui.horizontal(|ui| {
                        let name = app
                            .file
                            .as_ref()
                            .and_then(|f| f.grid.schema.get(stat.col).map(|c| c.name.clone()))
                            .unwrap_or_default();
                        ui.label(RichText::new(name).color(theme::TEXT_HEADER).size(12.0));
                        ui.label(RichText::new(stat_line(stat)).color(theme::TEXT_DIM).size(12.0));
                        if ui
                            .small_button(RichText::new("\u{2715}").color(theme::TEXT_DIM).size(10.0))
                            .clicked()
                        {
                            actions.push(PanelAction::CloseStats);
                        }
                    });
                }
            });
        });
    actions
}

fn stat_line(stat: &ColumnStats) -> String {
    let mut parts = vec![
        format!("n {}", fmt_count(stat.rows)),
        format!("nulls {}", fmt_count(stat.nulls)),
    ];
    if let Some(d) = stat.distinct {
        parts.push(format!("distinct {}", fmt_count(d)));
    }
    if let Some(num) = stat.numeric {
        let NumericStats { min, max, avg, stddev } = num;
        parts.push(format!("min {}", crate::data::types::format_float(min)));
        parts.push(format!("max {}", crate::data::types::format_float(max)));
        parts.push(format!("avg {}", crate::data::types::format_float(avg)));
        parts.push(format!("\u{03c3} {}", crate::data::types::format_float(stddev)));
    }
    for (value, count) in stat.top.iter().take(3) {
        let text = value.display();
        let text = if text.chars().count() > 20 {
            format!("{}\u{2026}", text.chars().take(20).collect::<String>())
        } else {
            text
        };
        parts.push(format!("{} ({})", text, fmt_count(*count)));
    }
    parts.join(" · ")
}

pub fn status_bar(ui: &mut Ui, app: &mut App) {
    egui::Panel::bottom("pq-status")
        .exact_size(STATUS_HEIGHT)
        .frame(egui::Frame::NONE.fill(theme::BG_STATUS).inner_margin(egui::Margin::symmetric(10, 4)))
        .show(ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                let (rows, shown, edits, busy) = match app.file.as_ref() {
                    Some(f) => (
                        f.total_rows,
                        f.shown_rows,
                        f.grid.overlay.len(),
                        f.count_pending,
                    ),
                    None => (0, 0, 0, false),
                };
                if app.file.is_some() {
                    if busy {
                        ui.label(RichText::new("counting\u{2026}").color(theme::TEXT_DIM));
                    } else {
                        ui.label(
                            RichText::new(format!("{} rows", fmt_count(rows)))
                                .color(theme::TEXT_DIM),
                        );
                        if app.file.as_ref().is_some_and(|f| f.grid.filter.is_some()) {
                            ui.label(
                                RichText::new(format!("{} shown", fmt_count(shown)))
                                    .color(theme::TEXT),
                            );
                        }
                    }
                    if edits > 0 {
                        ui.label(
                            RichText::new(format!("{} unsaved edits", edits))
                                .color(theme::ACCENT_TEXT),
                        );
                    }
                    if app.saving {
                        ui.label(RichText::new("saving\u{2026}").color(theme::ACCENT_TEXT));
                    } else if app.exporting {
                        ui.label(RichText::new("exporting\u{2026}").color(theme::ACCENT_TEXT));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if let Some(err) = app.error.as_ref() {
                        ui.label(RichText::new(err).color(theme::DANGER).size(11.5));
                    } else {
                        let commit_err = app
                            .file
                            .as_ref()
                            .and_then(|f| f.commit_error.clone());
                        if let Some(err) = commit_err {
                            ui.label(RichText::new(err).color(theme::DANGER).size(11.5));
                        } else {
                            ui.label(
                                RichText::new(app.status.clone())
                                    .color(theme::TEXT_DIM)
                                    .size(11.5),
                            );
                        }
                    }
                });
            });
        });
}

pub fn start_screen(ui: &mut Ui, app: &mut App) -> Vec<PanelAction> {
    let mut actions = Vec::new();
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(theme::BG_WINDOW))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.18);

                // Drop zone.
                let (rect, _) = ui.allocate_exact_size(egui::vec2(420.0, 130.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_stroke(rect, 10.0, egui::Stroke::new(1.5, theme::LINE_STRONG), egui::StrokeKind::Outside);
                painter.text(
                    rect.center() - egui::vec2(0.0, 10.0),
                    egui::Align2::CENTER_CENTER,
                    "Drop a .parquet file",
                    egui::FontId::proportional(15.0),
                    theme::TEXT_HEADER,
                );
                painter.text(
                    rect.center() + egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    "or press Ctrl+O to browse",
                    egui::FontId::proportional(12.0),
                    theme::TEXT_DIM,
                );

                ui.add_space(24.0);
                if !app.recents.is_empty() {
                    ui.label(
                        RichText::new("RECENT")
                            .color(theme::TEXT_DIM)
                            .size(10.0),
                    );
                    ui.add_space(4.0);
                    for (i, recent) in app.recents.clone().iter().enumerate() {
                        let label = format!(
                            "{}   {}",
                            file_name(&recent.path),
                            fmt_count(recent.rows)
                        );
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label).color(theme::TEXT).size(12.0),
                                )
                                .fill(theme::BG_PANEL)
                                .min_size(egui::vec2(420.0, 24.0)),
                            )
                            .clicked()
                        {
                            actions.push(PanelAction::OpenRecent(i));
                        }
                    }
                }
            });
        });
    actions
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}
