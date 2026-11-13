//! The data grid: header with sort controls, windowed rows, inline editing,
//! selection, context menus.
//!
//! Renders only visible rows (egui_extras `TableBuilder::body.rows`), reads
//! through the `RowCache`, applies the edit overlay. Missing pages are
//! requested from the engine after the draw pass.
//!
//! Header events are deferred: the header closure runs while the body
//! closure holds the mutable `FileState`, so header interactions record
//! `HeaderEvent`s and are applied after the table is drawn.

use egui::{Align, Color32, Key, Label, RichText, Sense, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};

use crate::app::{App, ScrollRequest};
use crate::data::engine::Job;
use crate::data::model::PAGE_SIZE;
use crate::data::types::{CellValue, ColType, SortKey};
use crate::theme;
use crate::util::fmt_count;

const HEADER_HEIGHT: f32 = 26.0;
const ROWNUM_WIDTH: f32 = 64.0;
const COL_DEFAULT_WIDTH: f32 = 130.0;

/// Deferred header interactions.
enum HeaderEvent {
    Sort { col: usize, append: bool },
    SortDesc { col: usize },
    SortAdd { col: usize, ascending: bool },
    SortRemove { col: usize },
    Filter { col: usize },
    Stats { col: usize },
    CopyName { col: usize },
    SelectCol { col: usize },
    DropCol { col: usize },
}

/// Cell geometry captured during the table draw. Used afterwards to
/// claim the body's column-edge strips: egui_extras registers column
/// resize handles over the FULL table height and draws them last, so
/// they win hit-testing at every column edge — eating clicks in the
/// body. Our strips (registered after the table) take those clicks back
/// for cell selection; the header keeps working resize handles.
#[derive(Default)]
struct GridGeom {
    initialized: bool,
    /// Top of the first visible row's cell.
    top: f32,
    bottom: f32,
    /// View position of the first drawn row.
    first_pos: u64,
    /// Right edge (plus half the column spacing) of each visible column,
    /// from the first drawn row. Boundary i is between visible column i
    /// and i+1; visible column 0 is the row-number column.
    boundaries: Vec<f32>,
}

pub fn draw(ui: &mut Ui, app: &mut App) {
    let App { engine, file, .. } = app;
    let Some(file) = file else { return };
    let engine = &*engine;

    let total = file.grid.cache.total() as usize;
    let ncols = file.grid.schema.len();

    // Deleted columns are hidden client-side; indices stay stable because
    // save uses EXCLUDE instead of rewriting the schema in place.
    let visible_cols: Vec<usize> = (0..ncols)
        .filter(|c| !file.grid.overlay.is_col_deleted(*c))
        .collect();
    let ncols_visible = visible_cols.len();

    let schema = file.grid.schema.clone();
    let sort_state = file.grid.sort.clone();
    let filter_text = file.grid.filter.clone();
    let scroll_to = file.scroll_to_row.take();
    let sel_rect = file.selection.map(|s| s.rect(total as u64, ncols));
    let sel_head = file.selection.map(|s| s.head);

    let mut header_events: Vec<HeaderEvent> = Vec::new();
    // Visible row range, filled during the body pass for page requests.
    let mut seen: Option<(usize, usize)> = None;
    let geom = std::rc::Rc::new(std::cell::RefCell::new(GridGeom::default()));

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(theme::BG_WINDOW))
        .show(ui, |ui| {
            if total == 0 || ncols_visible == 0 {
                empty_state(ui, file);
                return;
            }

            let mut table = TableBuilder::new(ui)
                .id_salt("pq-grid")
                .striped(false)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(Align::Center))
                .column(Column::exact(ROWNUM_WIDTH))
                .vscroll(true)
                .animate_scrolling(false)
                .auto_shrink([false, false]);
            for _ in 0..ncols_visible {
                table = table.column(
                    Column::initial(COL_DEFAULT_WIDTH)
                        .resizable(true)
                        .clip(true),
                );
            }
            let table = match scroll_to {
                Some(ScrollRequest::Center(idx)) => {
                    table.scroll_to_row(idx as usize, Some(Align::Center))
                }
                Some(ScrollRequest::Edge(idx, down)) => table.scroll_to_row(
                    idx as usize,
                    Some(if down { Align::Max } else { Align::Min }),
                ),
                None => table,
            };

            table
                .header(HEADER_HEIGHT, {
                    let events = &mut header_events;
                    let visible = &visible_cols;
                    move |mut header| {
                        header.col(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new("\u{25b2}#").color(theme::TEXT_DIM).size(11.0),
                                );
                            });
                        });
                        for &ci in visible.iter() {
                            let col = &schema[ci];
                            header.col(|ui| {
                                draw_header_cell(
                                    ui,
                                    events,
                                    ci,
                                    &col.name,
                                    &sort_state,
                                    filter_text.as_ref(),
                                );
                            });
                        }
                    }
                })
                .body(|body| {
                    body.rows(theme::ROW_HEIGHT, total, |mut row| {
                        let row_idx = row.index();
                        // View position: addresses the cache; the true file
                        // rowid comes from the fetched page.
                        let pos = row_idx as u64;
                        seen = Some(match seen {
                            Some((lo, hi)) => (lo.min(row_idx), hi.max(row_idx)),
                            None => (row_idx, row_idx),
                        });
                        // Row tint: any row inside the selection rect; the
                        // head cell gets its own frame (see draw_cell).
                        let row_selected =
                            sel_rect.is_some_and(|(r0, r1, _, _)| pos >= r0 && pos <= r1);
                        row.col(|ui| {
                            draw_rownum_cell(ui, file, &geom, pos, row_selected);
                        });
                        for &col in visible_cols.iter() {
                            row.col(|ui| {
                                draw_cell(ui, file, &geom, pos, col, sel_rect, sel_head);
                            });
                        }
                    });
                });
            // Body column-edge strips: egui_extras' resize handles span the full
            // table height and are drawn last, so at every column edge they won
            // hit-testing — clicks in the body near an edge did nothing (and the
            // cursor became a resize dragger). Register our own click strips
            // after the table: egui's tie-break is "last registered", so these
            // win in the body rows, mapping clicks to the adjacent cell. The
            // header keeps egui's resize handles.
            {
                let g = geom.borrow();
                if g.initialized {
                    eprintln!(
                        "dbg geom top={} bottom={} first_pos={} boundaries={:?} slot={}",
                        g.top,
                        g.bottom,
                        g.first_pos,
                        g.boundaries,
                        theme::ROW_HEIGHT + ui.spacing().item_spacing.y
                    );
                }
            }
            let (initialized, top, bottom, first_pos, boundaries) = {
                let g = geom.borrow();
                (
                    g.initialized,
                    g.top,
                    g.bottom,
                    g.first_pos,
                    g.boundaries.clone(),
                )
            };
            if initialized && boundaries.len() > 1 {
                let slot = theme::ROW_HEIGHT + ui.spacing().item_spacing.y;
                for (i, &b) in boundaries.iter().enumerate() {
                    let strip = egui::Rect::from_min_max(
                        egui::pos2(b - 6.0, top),
                        egui::pos2(b + 6.0, bottom),
                    );
                    let resp =
                        ui.interact(strip, ui.scope_id().with(("pq-edge", i)), Sense::click());
                    if resp.clicked()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let shift = ui.input(|i| i.modifiers.shift);
                        // Nearest row: a click in the spacing gap between
                        // rows belongs to the row it is closest to.
                        let row = (((p.y - top) / slot).round().max(0.0)) as u64;
                        let lo = first_pos;
                        let pos = lo.saturating_add(row);
                        // Left of the boundary = the column that ends here,
                        // right = the next one. Visible column 0 is the row
                        // number column.
                        if p.x <= b {
                            if i == 0 {
                                file.click_rownum(pos, shift);
                            } else {
                                file.click_cell(pos, i - 1, shift);
                            }
                        } else if i + 1 < boundaries.len() {
                            file.click_cell(pos, i, shift);
                        }
                    }
                }
            }
        });

    // Track the rendered row range for Excel-style edge scrolling.
    file.visible_rows = seen.map(|(lo, hi)| (lo as u64, hi as u64));

    // Deferred header actions (needs &mut file, which the closures released).
    let mut sorted = false;
    for event in header_events {
        match event {
            HeaderEvent::Sort { col, append } => {
                file.cycle_sort(col, append);
                sorted = true;
            }
            HeaderEvent::SortDesc { col } => {
                file.set_sort_desc(col);
                sorted = true;
            }
            HeaderEvent::SortAdd { col, ascending } => {
                file.add_sort(col, ascending);
                sorted = true;
            }
            HeaderEvent::SortRemove { col } => {
                file.remove_sort_key(col);
                sorted = true;
            }
            HeaderEvent::Filter { col } => {
                if file.grid.schema.get(col).is_some() {
                    let pop = crate::app::FilterPopover::new(col);
                    file.popover = Some(pop);
                    engine.send(Job::TopValues {
                        col,
                        limit: 200,
                        filter: file.grid.filter.clone(),
                    });
                }
            }
            HeaderEvent::Stats { col } => {
                if file.grid.schema.get(col).is_some() {
                    engine.send(Job::Stats {
                        col,
                        filter: file.grid.filter.clone(),
                    });
                }
            }
            HeaderEvent::CopyName { col } => {
                if let Some(c) = file.grid.schema.get(col) {
                    ui.ctx().copy_text(c.name.clone());
                }
            }
            HeaderEvent::SelectCol { col } => {
                let total = file.grid.cache.total();
                if total > 0 {
                    file.selection = Some(crate::app::Sel {
                        anchor: crate::app::Cell { pos: 0, col },
                        head: crate::app::Cell {
                            pos: total - 1,
                            col,
                        },
                    });
                }
            }
            HeaderEvent::DropCol { col } => file.drop_col(col),
        }
    }
    // A sort requery reset the cache total to 0; the grid would render "no
    // rows" until a count answers. FileState::cycle_sort does not send one
    // (it has no engine), so send it here.
    if sorted {
        let epoch = file.grid.cache.epoch();
        let filter = file.grid.filter.clone();
        engine.send(Job::Count { epoch, filter });
    }

    // Any pointer release ends a drag-select. A drag's release never lands
    // as `clicked()` on the release cell (press started elsewhere), so the
    // per-cell path cannot clear it.
    if ui.input(|i| i.pointer.any_released()) {
        file.pointer_release();
    }

    // Request pages the visible rows need (plus one page ahead).
    // missing_pages marks pages in-flight and dedups.
    if let Some((min_seen, max_seen)) = seen {
        let mut pages = file
            .grid
            .cache
            .missing_pages(min_seen as u64, (max_seen + 1) as u64);
        // Prefetch two pages ahead so fast scrolling rarely waits on the
        // worker (blank-row chunks while a page is in flight).
        let ahead = ((max_seen + 1) as u64) / PAGE_SIZE as u64;
        for next in [ahead, ahead + 1] {
            pages.extend(
                file.grid
                    .cache
                    .missing_pages(next * PAGE_SIZE as u64, next * PAGE_SIZE as u64 + 1),
            );
        }
        for page in pages {
            engine.send(Job::Rows {
                epoch: file.grid.cache.epoch(),
                page,
                sort: file.grid.sort.clone(),
                filter: file.grid.filter.clone(),
            });
        }
    }
}

fn draw_header_cell(
    ui: &mut Ui,
    events: &mut Vec<HeaderEvent>,
    col: usize,
    name: &str,
    sort: &[SortKey],
    filter: Option<&String>,
) {
    ui.spacing_mut().item_spacing.x = 4.0;
    let mut label = RichText::new(name).color(theme::TEXT_HEADER).size(12.5);
    let mut rank_label: Option<RichText> = None;
    if let Some(pos) = sort.iter().position(|k| k.col == col) {
        let key = sort[pos];
        let arrow = if key.ascending {
            "\u{25b2}"
        } else {
            "\u{25bc}"
        };
        label = RichText::new(format!("{name} {arrow}"))
            .color(theme::ACCENT_TEXT)
            .size(12.5);
        if sort.len() > 1 {
            ui.spacing_mut().item_spacing.x = 2.0;
            rank_label = Some(
                RichText::new((pos + 1).to_string())
                    .color(theme::ACCENT_TEXT)
                    .size(9.0)
                    .background_color(theme::ACCENT_SOFT),
            );
        }
    }
    // Non-selectable labels: selectable text would swallow the header's
    // clicks (same root cause as the dead cell clicks).
    ui.add(Label::new(label).selectable(false).sense(Sense::hover()));
    if let Some(rank) = rank_label {
        ui.add(Label::new(rank).selectable(false).sense(Sense::hover()));
    }
    if filter.is_some_and(|f| f.contains(name)) {
        ui.add(
            Label::new(RichText::new("\u{25be}").color(theme::ACCENT).size(10.0))
                .selectable(false)
                .sense(Sense::hover()),
        );
    }

    // Extend the hit area to the full header cell and track clicks.
    let resp = ui.interact(
        ui.max_rect(),
        ui.scope_id().with(("pq-hdr", col)),
        Sense::click(),
    );
    if resp.clicked() {
        events.push(HeaderEvent::Sort {
            col,
            append: ui.input(|i| i.modifiers.shift),
        });
    }
    resp.context_menu(|ui| {
        if ui.button("sort ascending").clicked() {
            events.push(HeaderEvent::Sort { col, append: false });
            ui.close();
        }
        if ui.button("sort descending").clicked() {
            events.push(HeaderEvent::SortDesc { col });
            ui.close();
        }
        if ui.button("add to sort ascending").clicked() {
            events.push(HeaderEvent::SortAdd {
                col,
                ascending: true,
            });
            ui.close();
        }
        if ui.button("add to sort descending").clicked() {
            events.push(HeaderEvent::SortAdd {
                col,
                ascending: false,
            });
            ui.close();
        }
        if sort.iter().any(|k| k.col == col) && ui.button("remove from sort").clicked() {
            events.push(HeaderEvent::SortRemove { col });
            ui.close();
        }
        if ui.button("filter values\u{2026}").clicked() {
            events.push(HeaderEvent::Filter { col });
            ui.close();
        }
        if ui.button("column statistics").clicked() {
            events.push(HeaderEvent::Stats { col });
            ui.close();
        }
        if ui.button("select column").clicked() {
            events.push(HeaderEvent::SelectCol { col });
            ui.close();
        }
        if ui.button("drop column").clicked() {
            events.push(HeaderEvent::DropCol { col });
            ui.close();
        }
        if ui.button("copy column name").clicked() {
            events.push(HeaderEvent::CopyName { col });
            ui.close();
        }
    });
}

fn draw_rownum_cell(
    ui: &mut Ui,
    file: &mut crate::app::FileState,
    geom: &std::rc::Rc<std::cell::RefCell<GridGeom>>,
    pos: u64,
    selected: bool,
) {
    if selected {
        paint_row_bg(ui, theme::BG_SELECT);
    }
    {
        let mut g = geom.borrow_mut();
        let rect = ui.max_rect();
        let spacing = ui.spacing().item_spacing.x;
        if !g.initialized {
            g.initialized = true;
            g.top = rect.top();
            g.bottom = rect.bottom();
            g.first_pos = pos;
            // First boundary: the row-number column's right edge, so its
            // edge strip is claimed too (clicks left of it = row select).
            g.boundaries.push(rect.right() + spacing * 0.5);
        }
        g.bottom = g.bottom.max(rect.bottom());
    }
    // Clicking the row number selects the whole row (Excel-style); the
    // hit box covers the full cell plus the surrounding gap half.
    let hit = ui.max_rect().expand2(0.5 * ui.spacing().item_spacing);
    let resp = ui.interact(hit, ui.scope_id().with(("pq-rownum", pos)), Sense::click());
    if resp.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        file.click_rownum(pos, shift);
    }
    // Show the row's true file rowid when the page is fetched (stable under
    // sort); fall back to the view position while loading.
    let shown = file.grid.rowid_at(pos).unwrap_or(pos);
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        ui.add(
            Label::new(
                RichText::new(fmt_count(shown))
                    .color(if selected {
                        theme::TEXT
                    } else {
                        theme::TEXT_DIM
                    })
                    .size(11.0),
            )
            .selectable(false)
            .sense(Sense::hover()),
        );
    });
}

fn draw_cell(
    ui: &mut Ui,
    file: &mut crate::app::FileState,
    geom: &std::rc::Rc<std::cell::RefCell<GridGeom>>,
    pos: u64,
    col: usize,
    sel_rect: Option<(u64, u64, usize, usize)>,
    sel_head: Option<crate::app::Cell>,
) {
    let (r0, r1, c0, c1) = sel_rect.unwrap_or((u64::MAX, 0, usize::MAX, 0));
    let in_rect = pos >= r0 && pos <= r1 && col >= c0 && col <= c1;
    let head_here = sel_head.is_some_and(|h| h.pos == pos && h.col == col);
    if in_rect {
        paint_row_bg(ui, theme::BG_SELECT);
    }

    if file
        .editing
        .as_ref()
        .is_some_and(|e| e.pos == pos && e.col == col)
    {
        draw_editor(ui, file);
        return;
    }

    let Some(cell) = file.grid.cell(pos, col) else {
        ui.label(RichText::new("\u{2026}").color(theme::NULL_DOT).size(11.0));
        return;
    };

    let ty = file.grid.schema.get(col).map(|s| s.ty);
    let numeric = matches!(ty, Some(ColType::Integer) | Some(ColType::Float));

    // Hit box: the full cell area PLUS half the spacing around it. The
    // visible gaps between cells are margins and row spacing, and a click
    // there used to select nothing. (Adjacent hit boxes overlap in the
    // middle of a gap; egui's tie-break is "last registered", so the
    // lower/right cell wins — every click lands in a cell.)
    let spacing = ui.spacing().item_spacing;
    let hit = ui.max_rect().expand2(0.5 * spacing);
    // The painted area stays snug around the value.
    let rect = ui.available_rect_before_wrap().shrink(1.0);
    {
        let mut g = geom.borrow_mut();
        if g.initialized && pos == g.first_pos {
            // First drawn row: record where each column ends, for the
            // body edge-strip claim after the table.
            g.boundaries.push(ui.max_rect().right() + spacing.x * 0.5);
        }
    }
    let resp = ui.interact(
        hit,
        ui.scope_id().with(("pq-cell", pos, col)),
        Sense::click(),
    );

    // Backgrounds: edited marker, selection fill.
    if cell.edited {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
            0.0,
            theme::ACCENT,
        );
    }
    if in_rect {
        ui.painter().rect_filled(rect, 0.0, theme::BG_SELECT);
    }
    if head_here {
        ui.painter().rect_filled(rect, 0.0, theme::BG_SELECT_CELL);
    }

    // Text as a real widget (AT-SPI/screen-reader visible), placed into the
    // exact cell rect; right-aligned for numbers. NOT selectable: text
    // selection would swallow clicks and drag-select.
    let text = match cell.value {
        CellValue::Null => RichText::new("·").color(theme::NULL_DOT).size(13.0),
        ref v => RichText::new(v.display()).size(13.0).color(if cell.edited {
            theme::ACCENT_TEXT
        } else {
            theme::TEXT
        }),
    };
    let label = Label::new(text)
        .selectable(false)
        .sense(Sense::hover())
        .halign(if numeric { Align::Max } else { Align::Min });
    ui.put(rect, label);
    if head_here {
        ui.painter().rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.5, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }

    // Drag select: while the primary button is held, every cell the pointer
    // passes over extends the rect. (is_pointer_button_down_on is only true
    // for the press-origin widget, which never extends.)
    let dragging = ui.input(|i| i.pointer.primary_down()) && ui.rect_contains_pointer(hit);
    if dragging {
        let shift = ui.input(|i| i.modifiers.shift);
        file.pointer_on_cell(pos, col, shift);
    }

    if resp.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        file.click_cell(pos, col, shift);
    }
    if resp.double_clicked() {
        file.double_click_cell(pos, col);
    }
    if resp.secondary_clicked() {
        file.secondary_click_cell(pos, col);
    }
    resp.context_menu(|ui| {
        let value_text = file
            .grid
            .cell(pos, col)
            .map(|c| c.value.display())
            .unwrap_or_default();
        if ui.button("edit cell").clicked() {
            file.begin_edit_at(pos, col);
            ui.close();
        }
        if ui.button("set NULL").clicked() {
            if let Some(rowid) = file.grid.rowid_at(pos) {
                file.grid
                    .overlay
                    .set(rowid, col, crate::data::types::EditValue::Null);
            }
            ui.close();
        }
        if ui.button("copy value").clicked() {
            ui.ctx().copy_text(value_text);
            ui.close();
        }
    });
}

fn draw_editor(ui: &mut Ui, file: &mut crate::app::FileState) {
    let Some(edit) = file.editing.as_mut() else {
        return;
    };
    let width = ui.available_width().max(60.0);
    // Stable id: the true rowid does not shift as rows scroll or the view
    // is re-sorted.
    let editor = TextEdit::singleline(&mut edit.text)
        .id_salt(("pq-edit", edit.rowid, edit.col))
        .desired_width(width)
        // Center the text like the cell's own value.
        .align(egui::Align2::LEFT_CENTER);
    // Put the box into the exact cell rect: laid out top-down it sat
    // below the row's text line and bled into the next row (the user's
    // "cell looks cut off when editing").
    let rect = ui.max_rect().shrink(1.0);
    let resp = ui.put(rect, editor);
    resp.request_focus();
    if resp.has_focus() {
        edit.had_focus = true;
    }
    if edit.created_frame == u64::MAX {
        edit.created_frame = ui.ctx().cumulative_frame_nr();
    }
    let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
    // Commit on Enter only after the creation frame: the Enter that STARTED
    // the edit is still in this frame's input state.
    let past_creation = ui.ctx().cumulative_frame_nr() > edit.created_frame;
    if resp.has_focus() && enter && past_creation {
        file.commit_editing();
    } else if resp.has_focus() && esc {
        file.editing = None;
    } else if edit.had_focus && resp.lost_focus() {
        // Clicked elsewhere / focus moved: commit like Excel does. Only
        // after the editor actually held focus at least once — a focus
        // race must not silently commit a half-open editor. A no-change
        // commit is a no-op; an invalid value keeps the editor.
        file.commit_editing();
    }
}

fn paint_row_bg(ui: &mut Ui, color: Color32) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, color);
}

fn empty_state(ui: &mut Ui, file: &mut crate::app::FileState) {
    ui.centered_and_justified(|ui| {
        let msg = if file.grid.filter.is_some() {
            "No rows match the filter."
        } else {
            "This file has no rows."
        };
        ui.label(RichText::new(msg).color(theme::TEXT_DIM).size(14.0));
    });
}
