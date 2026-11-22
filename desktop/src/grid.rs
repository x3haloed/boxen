use crate::{Action, Desktop, Viewer, model::Pane};
use eframe::egui::{self, RichText};
use egui_dock::TabViewer;
use serde::{Deserialize, Serialize};

pub const MIN_COLUMN_WIDTH: f32 = 400.;
pub const SIDEBAR_WIDTH: f32 = 285.;
const ROWS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub column: usize,
    pub row: usize,
    pub width: usize,
    pub height: usize,
}

impl Cell {
    fn column(column: usize) -> Self {
        Self {
            column,
            row: 0,
            width: 1,
            height: ROWS,
        }
    }

    pub fn fits(self, columns: usize) -> bool {
        self.column + self.width <= columns
    }

    pub fn contains(self, column: usize, row: usize) -> bool {
        column >= self.column
            && column < self.column + self.width
            && row >= self.row
            && row < self.row + self.height
    }

    fn overlaps(self, other: Self) -> bool {
        self.column < other.column + other.width
            && other.column < self.column + self.width
            && self.row < other.row + other.height
            && other.row < self.row + self.height
    }
}

struct SnapPlan {
    incoming: Cell,
    change: Option<(usize, Option<Cell>)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Placement {
    pub pane: Pane,
    pub home: Cell,
}

#[derive(Serialize, Deserialize)]
pub struct Grid {
    pub slots: Vec<Placement>,
    pub sidebar: Pane,
    #[serde(skip)]
    pub peek: Option<Pane>,
    #[serde(skip)]
    pub placing: Option<Pane>,
    #[serde(skip)]
    pub open_here: Option<Cell>,
    #[serde(skip)]
    pub focus: Option<Pane>,
    #[serde(skip, default = "initial_columns")]
    pub columns: usize,
}

fn initial_columns() -> usize {
    3
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            sidebar: Pane::Threads,
            peek: None,
            placing: None,
            open_here: None,
            focus: None,
            columns: initial_columns(),
        }
    }
}

impl Grid {
    pub fn columns_for(width: f32) -> usize {
        ((width - SIDEBAR_WIDTH - 1.) / MIN_COLUMN_WIDTH)
            .floor()
            .max(1.) as usize
    }

    pub fn migrate(&mut self, pane: Pane) {
        if !self.slots.iter().any(|slot| slot.pane == pane) {
            self.slots.push(Placement {
                pane,
                home: Cell::column(self.slots.len()),
            });
        }
    }

    pub fn open(&mut self, pane: Pane, place: bool) {
        let requested = if place { self.open_here.take() } else { None };
        if let Some(slot) = self.slots.iter().find(|s| s.pane == pane)
            && slot.home.fits(self.columns)
        {
            self.focus = Some(pane);
            self.peek = None;
            self.placing = None;
            return;
        }
        if !place {
            self.peek = Some(pane);
            self.placing = None;
            return;
        }
        let requested = requested.filter(|cell| cell.fits(self.columns) && self.free(*cell, &pane));
        if let Some(cell) = requested.or_else(|| {
            (0..self.columns)
                .map(Cell::column)
                .find(|cell| self.free(*cell, &pane))
        }) {
            self.install(pane, cell);
        } else {
            self.peek = None;
            self.placing = Some(pane);
        }
    }

    pub fn free(&self, cell: Cell, pane: &Pane) -> bool {
        self.slots
            .iter()
            .all(|slot| slot.pane == *pane || !slot.home.overlaps(cell))
    }

    pub fn install(&mut self, pane: Pane, home: Cell) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.pane == pane) {
            slot.home = home;
        } else {
            self.slots.push(Placement {
                pane: pane.clone(),
                home,
            });
        }
        self.focus = Some(pane);
        self.peek = None;
        self.placing = None;
    }

    pub fn close(&mut self, pane: &Pane) {
        self.slots.retain(|slot| &slot.pane != pane);
        if self.focus.as_ref() == Some(pane) {
            self.focus = None;
        }
    }

    pub fn snap_region(&self, column: usize, row: usize, replace: bool) -> Option<Cell> {
        Some(self.snap_plan(column, row, replace)?.incoming)
    }

    fn snap_plan(&self, column: usize, row: usize, replace: bool) -> Option<SnapPlan> {
        let pane = self.placing.as_ref()?;
        if column >= self.columns || row >= ROWS {
            return None;
        }
        let unit = Cell {
            column,
            row,
            width: 1,
            height: 1,
        };
        let Some((index, slot)) = self
            .slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.pane != *pane && slot.home.contains(column, row))
        else {
            let full = Cell::column(column);
            return Some(SnapPlan {
                incoming: if self.free(full, pane) { full } else { unit },
                change: None,
            });
        };
        let cell = slot.home;
        if !cell.fits(self.columns) {
            return None;
        }
        if replace {
            return Some(SnapPlan {
                incoming: cell,
                change: Some((index, None)),
            });
        }
        let (incoming, previous) = if cell.width > 1 {
            if column == cell.column {
                (
                    Cell { width: 1, ..cell },
                    Cell {
                        column: column + 1,
                        width: cell.width - 1,
                        ..cell
                    },
                )
            } else {
                let offset = column - cell.column;
                (
                    Cell {
                        column,
                        width: cell.width - offset,
                        ..cell
                    },
                    Cell {
                        width: offset,
                        ..cell
                    },
                )
            }
        } else if cell.height == ROWS {
            (
                unit,
                Cell {
                    row: 1 - row,
                    height: 1,
                    ..cell
                },
            )
        } else {
            return None;
        };
        Some(SnapPlan {
            incoming,
            change: Some((index, Some(previous))),
        })
    }

    pub fn snap(&mut self, column: usize, row: usize, replace: bool) {
        let Some(SnapPlan { incoming, change }) = self.snap_plan(column, row, replace) else {
            return;
        };
        let pane = self.placing.clone().unwrap();
        if let Some((index, previous)) = change {
            match previous {
                Some(previous) => self.slots[index].home = previous,
                None => {
                    self.slots.remove(index);
                }
            }
        }
        self.install(pane, incoming);
    }

    pub fn holes(&self) -> Vec<Cell> {
        let mut holes = Vec::new();
        for column in 0..self.columns {
            let full = Cell::column(column);
            if self.slots.iter().all(|slot| !slot.home.overlaps(full)) {
                holes.push(full);
            } else {
                for row in 0..ROWS {
                    if self
                        .slots
                        .iter()
                        .all(|slot| !slot.home.contains(column, row))
                    {
                        holes.push(Cell {
                            column,
                            row,
                            width: 1,
                            height: 1,
                        });
                    }
                }
            }
        }
        holes
    }

    pub fn neighbor(&self, hole: Cell) -> Option<(Pane, Cell)> {
        self.slots
            .iter()
            .filter(|slot| slot.home.fits(self.columns))
            .find_map(|slot| {
                let cell = slot.home;
                let expanded = if cell.row == hole.row
                    && cell.height == hole.height
                    && (cell.column + cell.width == hole.column
                        || hole.column + hole.width == cell.column)
                {
                    Cell {
                        column: cell.column.min(hole.column),
                        width: cell.width + hole.width,
                        ..cell
                    }
                } else if cell.column == hole.column
                    && cell.width == hole.width
                    && (cell.row + cell.height == hole.row || hole.row + hole.height == cell.row)
                {
                    Cell {
                        row: cell.row.min(hole.row),
                        height: cell.height + hole.height,
                        ..cell
                    }
                } else {
                    return None;
                };
                self.free(expanded, &slot.pane)
                    .then(|| (slot.pane.clone(), expanded))
            })
    }

    pub fn tidy(&self, even: bool) -> Option<Vec<Placement>> {
        let (visible, parked): (Vec<_>, Vec<_>) = self
            .slots
            .iter()
            .cloned()
            .partition(|slot| slot.home.fits(self.columns));
        let mut visible = visible;
        visible.sort_by_key(|slot| (slot.home.column, slot.home.row));
        let even_height = if visible.len() <= self.columns {
            ROWS
        } else {
            1
        };
        let mut arranged = parked;
        let mut changed = false;
        for mut slot in visible {
            let (width, height) = if even {
                (1, even_height)
            } else {
                (slot.home.width, slot.home.height)
            };
            let cell = (0..=self.columns.checked_sub(width)?).find_map(|column| {
                (0..=ROWS.checked_sub(height)?).find_map(|row| {
                    let candidate = Cell {
                        column,
                        row,
                        width,
                        height,
                    };
                    arranged
                        .iter()
                        .all(|other| !other.home.overlaps(candidate))
                        .then_some(candidate)
                })
            })?;
            changed |= cell != slot.home;
            slot.home = cell;
            arranged.push(slot);
        }
        changed.then_some(arranged)
    }

    pub fn span(&mut self, pane: &Pane, width: usize, height: usize) {
        let Some(slot) = self.slots.iter().find(|s| s.pane == *pane) else {
            return;
        };
        let candidate = Cell {
            width,
            height,
            ..slot.home
        };
        if candidate.fits(self.columns)
            && candidate.row + height <= ROWS
            && self.free(candidate, pane)
        {
            self.install(pane.clone(), candidate);
        }
    }
}

impl Desktop {
    fn grid(&self) -> &Grid {
        self.data
            .saved
            .workspace
            .grid
            .as_ref()
            .expect("desktop grid is initialized")
    }

    fn grid_mut(&mut self) -> &mut Grid {
        self.data
            .saved
            .workspace
            .grid
            .as_mut()
            .expect("desktop grid is initialized")
    }

    pub(super) fn grid_workspace(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let width = ui.available_width();
        let columns = Grid::columns_for(width);
        let Some(layout) = self.data.saved.workspace.grid.as_mut() else {
            return;
        };
        layout.columns = columns;
        let parked: Vec<_> = layout
            .slots
            .iter()
            .filter(|slot| !slot.home.fits(columns))
            .cloned()
            .collect();
        let visible: Vec<_> = layout
            .slots
            .iter()
            .filter(|slot| slot.home.fits(columns))
            .cloned()
            .collect();
        let mut sidebar = layout.sidebar.clone();
        let has_holes = !layout.holes().is_empty();
        let mut tidy_preview = None;
        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.,
            crate::console_style::CANVAS,
        );

        ui.horizontal(|ui| {
            if ui.button("Find…   ⌘K").clicked() {
                self.open_palette();
            }
            let coverage = crate::needs::coverage(&self.data);
            for dest in &self.data.saved.destinations {
                let count = coverage.get(dest.key.as_str());
                let (observed, total) = count.map_or((0, 0), |c| (c.observed, c.total));
                let down = self.data.errors.contains_key(&dest.key);
                let catalogued = self.data.catalogs.contains_key(&dest.key);
                let (label, color) = if down && !catalogued {
                    (format!("{} down", dest.label), egui::Color32::LIGHT_RED)
                } else if down {
                    (
                        format!("{} 0/{total}", dest.label),
                        egui::Color32::LIGHT_RED,
                    )
                } else if !catalogued {
                    (format!("{} …", dest.label), egui::Color32::GRAY)
                } else if observed != total {
                    (
                        format!("{} {observed}/{total}", dest.label),
                        egui::Color32::from_rgb(216, 162, 58),
                    )
                } else {
                    (
                        format!("{} {observed}/{total}", dest.label),
                        ui.visuals().text_color(),
                    )
                };
                let dot = if catalogued && !down && observed == total {
                    crate::console_style::OK
                } else {
                    color
                };
                crate::console_style::dot(ui, dot);
                ui.label(RichText::new(label).monospace().size(10.5).color(color))
                    .on_hover_text(if down {
                        "Machine unreachable · active total last observed"
                    } else {
                        "Observed active runs / active runs in the latest catalog"
                    });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Settings").clicked() {
                    self.data.tool_approvals.open();
                }
                ui.weak(format!("{columns} × 2"));
                crate::needs::toolbar(ui, &self.data, actions);
                ui.menu_button("Receipts", receipt_key);
                if ui.small_button("Live pulse").clicked() {
                    self.grid_mut().open(Pane::LivePulse, true);
                }
                if has_holes && !visible.is_empty() {
                    ui.menu_button("Tidy", |ui| {
                        for (name, even) in [("Fill holes", false), ("Even grid", true)] {
                            let plan = self.grid().tidy(even);
                            let response = ui.add_enabled(plan.is_some(), egui::Button::new(name));
                            if let Some(plan) = plan {
                                if response.clicked() {
                                    self.grid_mut().slots = plan;
                                    ui.close();
                                } else if response.hovered() {
                                    tidy_preview = Some(plan);
                                }
                            }
                        }
                    });
                }

                if !parked.is_empty() {
                    ui.menu_button(format!("Parked ({})", parked.len()), |ui| {
                        for slot in &parked {
                            let mut pane = slot.pane.clone();
                            let title = Viewer {
                                data: &mut self.data,
                                actions,
                            }
                            .title(&mut pane);
                            ui.label(title);
                            ui.weak(format!("Home column {}", slot.home.column + 1));
                            ui.horizontal(|ui| {
                                let layout = self.grid_mut();
                                if ui.small_button("Peek").clicked() {
                                    layout.open(pane.clone(), false);
                                    ui.close();
                                }
                                if ui.small_button("Place on grid…").clicked() {
                                    layout.open(pane.clone(), true);
                                    ui.close();
                                }
                            });
                        }
                    });
                }
            });
        });
        ui.separator();

        let workspace = ui.available_rect_before_wrap();
        let sidebar_rect = egui::Rect::from_min_max(
            workspace.min,
            egui::pos2(workspace.left() + SIDEBAR_WIDTH, workspace.bottom()),
        );
        let board_left = sidebar_rect.right() + 1.;
        let width = (workspace.right() - board_left).max(1.) / columns as f32;
        let board = egui::Rect::from_min_max(
            egui::pos2(board_left, workspace.top()),
            egui::pos2(board_left + width * columns as f32, workspace.bottom()),
        );
        let height = board.height() / 2.;
        let rect = |cell: Cell| {
            egui::Rect::from_min_size(
                board.min + egui::vec2(cell.column as f32 * width, cell.row as f32 * height),
                egui::vec2(cell.width as f32 * width, cell.height as f32 * height),
            )
        };
        ui.allocate_rect(workspace, egui::Sense::hover());
        ui.painter()
            .rect_filled(sidebar_rect, 0., crate::console_style::PANEL);
        ui.painter().vline(
            sidebar_rect.right(),
            sidebar_rect.y_range(),
            egui::Stroke::new(1., crate::console_style::RULE),
        );
        let mut sidebar_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("watchlist")
                .max_rect(sidebar_rect.shrink(6.)),
        );
        sidebar_ui.set_clip_rect(sidebar_rect);
        sidebar_ui.horizontal(|ui| {
            ui.strong("Watchlist");
            for (pane, label) in [
                (Pane::Threads, "Workspace"),
                (Pane::Agents, "Agents"),
                (Pane::Correspondence, "Mail"),
            ] {
                if ui.selectable_label(sidebar == pane, label).clicked() {
                    sidebar = pane;
                }
            }
        });
        sidebar_ui.separator();
        self.grid_mut().sidebar = sidebar.clone();
        Viewer {
            data: &mut self.data,
            actions,
        }
        .ui(&mut sidebar_ui, &mut sidebar);

        let holes = self.grid().holes();
        for hole in holes {
            let bounds = rect(hole).shrink(2.);
            crate::console_style::dashed_rect(ui, bounds, crate::console_style::RULE);
            let mut empty = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("empty", hole.column, hole.row))
                    .max_rect(bounds.shrink(6.)),
            );
            empty.set_clip_rect(bounds);
            empty.add_space((bounds.height() / 2. - 26.).max(0.));
            empty.vertical_centered(|ui| {
                if ui.small_button("+ Open here").clicked() {
                    self.open_palette();
                    self.grid_mut().open_here = Some(hole);
                }
                let layout = self.grid_mut();
                if let Some((pane, expanded)) = layout.neighbor(hole)
                    && ui.small_button("Expand neighbor").clicked()
                {
                    layout.install(pane, expanded);
                }
            });
        }

        let placing = self.grid().placing.clone();
        for mut slot in visible {
            let bounds = rect(slot.home);
            let id = Viewer {
                data: &mut self.data,
                actions,
            }
            .id(&mut slot.pane);
            let focus = self.grid().focus.as_ref() == Some(&slot.pane);
            ui.painter().rect_stroke(
                bounds.shrink(0.5),
                0.,
                egui::Stroke::new(
                    1.,
                    if focus {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().widgets.noninteractive.bg_stroke.color
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let title_strip =
                egui::Rect::from_min_size(bounds.min, egui::vec2(bounds.width(), 29.));
            ui.painter()
                .rect_filled(title_strip.shrink(1.), 0., crate::console_style::PANEL);
            if focus {
                ui.painter().hline(
                    bounds.x_range(),
                    bounds.top(),
                    egui::Stroke::new(2., crate::console_style::ACCENT),
                );
            }
            let inner = bounds.shrink(5.);
            let mut pane_ui = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(inner));
            pane_ui.set_clip_rect(inner);
            let mut close = false;
            pane_ui.horizontal(|ui| {
                let title = Viewer {
                    data: &mut self.data,
                    actions,
                }
                .title(&mut slot.pane);
                ui.dnd_drag_source(
                    id.with("drag"),
                    slot.pane.clone(),
                    crate::console_style::drag_grip,
                );
                ui.weak(format!(
                    "{}",
                    slot.home.column + 1 + slot.home.row * columns
                ));
                let name = match &slot.pane {
                    Pane::Thread(key) | Pane::Chat(key) | Pane::Activity(key) => {
                        let host = crate::conversation::destination(&self.data, &key.peer);
                        format!("{} / {}", host.label, title.text())
                    }
                    _ => title.text().to_owned(),
                };
                let width = (ui.available_width() - 75.).max(0.);
                if ui
                    .allocate_ui_with_layout(
                        egui::vec2(width, 20.),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(name).monospace().size(10.5))
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            )
                        },
                    )
                    .inner
                    .clicked()
                {
                    self.grid_mut().focus = Some(slot.pane.clone());
                    match &slot.pane {
                        Pane::Thread(key) | Pane::Chat(key) | Pane::Activity(key) => {
                            self.data.selected_thread = Some(key.clone());
                        }
                        _ => {}
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.small_button("×").on_hover_text("Close pane").clicked();
                    ui.menu_button(format!("{}×{}", slot.home.width, slot.home.height), |ui| {
                        let layout = self.grid_mut();
                        for span in 1..=columns - slot.home.column {
                            for rows in 1..=2 - slot.home.row {
                                let candidate = Cell {
                                    width: span,
                                    height: rows,
                                    ..slot.home
                                };
                                if ui
                                    .add_enabled(
                                        layout.free(candidate, &slot.pane),
                                        egui::Button::new(format!("{span}×{rows}")),
                                    )
                                    .clicked()
                                {
                                    layout.span(&slot.pane, span, rows);
                                    ui.close();
                                }
                            }
                        }
                    });
                });
            });
            if close {
                self.grid_mut().close(&slot.pane);
            } else {
                pane_ui.separator();
                Viewer {
                    data: &mut self.data,
                    actions,
                }
                .ui(&mut pane_ui, &mut slot.pane);
            }
        }

        if let Some(plan) = tidy_preview {
            for slot in plan.iter().filter(|slot| slot.home.fits(columns)) {
                ui.painter().rect_stroke(
                    rect(slot.home).shrink(3.),
                    0.,
                    egui::Stroke::new(2., ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Inside,
                );
            }
        }

        if let Some(pane) = egui::DragAndDrop::payload::<Pane>(ui.ctx())
            && let Some(position) = ui.ctx().input(|input| input.pointer.hover_pos())
            && board.contains(position)
        {
            let layout = self.grid_mut();
            if let Some(slot) = layout.slots.iter().find(|slot| slot.pane == *pane) {
                let home = Cell {
                    column: ((position.x - board.left()) / width) as usize,
                    row: if slot.home.height == 2 || position.y < board.center().y {
                        0
                    } else {
                        1
                    },
                    ..slot.home
                };
                if home.fits(columns) && layout.free(home, &pane) {
                    ui.painter().rect_stroke(
                        rect(home).shrink(2.),
                        0.,
                        egui::Stroke::new(2., ui.visuals().selection.stroke.color),
                        egui::StrokeKind::Inside,
                    );
                    if ui.ctx().input(|input| input.pointer.any_released()) {
                        layout.install((*pane).clone(), home);
                    }
                }
            }
        }

        if placing.is_some() {
            let cancel = ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let replace = ui.ctx().input(|i| i.modifiers.shift);
            let mut choices = Vec::new();
            let layout = self.grid();
            for column in 0..columns {
                for row in [1, 0] {
                    if let Some(region) = layout.snap_region(column, row, replace) {
                        choices.push((column, row, region));
                        break;
                    }
                }
            }
            for (column, row, region) in choices {
                let target = rect(Cell {
                    column,
                    row,
                    width: 1,
                    height: 1,
                })
                .shrink(2.);
                let response = ui.interact(
                    target,
                    egui::Id::new(("grid-snap", column)),
                    egui::Sense::click(),
                );
                ui.painter().rect_filled(
                    target,
                    0.,
                    egui::Color32::from_rgba_unmultiplied(76, 158, 255, 40),
                );
                if response.hovered() {
                    ui.painter().rect_filled(
                        rect(region).shrink(2.),
                        0.,
                        egui::Color32::from_rgba_unmultiplied(76, 158, 255, 53),
                    );
                }
                ui.painter().text(
                    target.center(),
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{} {}",
                        if replace {
                            "Replace in column"
                        } else {
                            "Place in column"
                        },
                        column + 1
                    ),
                    egui::FontId::proportional(13.),
                    ui.visuals().text_color(),
                );
                let key = [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                ]
                .get(column)
                .copied();
                let pressed = key.is_some_and(|key| {
                    ui.ctx().input_mut(|i| {
                        i.consume_key(
                            if replace {
                                egui::Modifiers::SHIFT
                            } else {
                                egui::Modifiers::NONE
                            },
                            key,
                        )
                    })
                });
                if response.clicked() || pressed {
                    self.grid_mut().snap(column, row, replace);
                }
            }
            if ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
                && let Some(pane) = placing.clone()
            {
                self.grid_mut().open(pane, false);
            }
            if cancel {
                self.grid_mut().placing = None;
            }
        }

        let peek = self.grid().peek.clone();
        if let Some(mut pane) = peek {
            let mut viewer = Viewer {
                data: &mut self.data,
                actions,
            };
            let id = viewer.id(&mut pane);
            let title = Viewer {
                data: &mut self.data,
                actions,
            }
            .title(&mut pane);
            let mut open = true;
            egui::Window::new(title.text())
                .id(egui::Id::new(("peek", id)))
                .default_pos(board.min + egui::vec2(8., 12.))
                .default_size([440., 360.])
                .max_height(board.height() * 0.85)
                .collapsible(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    if ui.small_button("Open on grid").clicked() {
                        self.grid_mut().open(pane.clone(), true);
                    }
                    Viewer {
                        data: &mut self.data,
                        actions,
                    }
                    .ui(ui, &mut pane);
                });
            if !open
                || (placing.is_none()
                    && ui
                        .ctx()
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
            {
                self.grid_mut().peek = None;
            }
        }
    }
}

fn receipt_key(ui: &mut egui::Ui) {
    let groups: [(&str, &[(&str, &str)]); 4] = [
        (
            "Thread message",
            &[
                (
                    "Steered into run",
                    "The runtime took the message into the active turn.",
                ),
                ("Queued for run", "OpenRouter retained the queued update."),
                (
                    "Incorporated into run",
                    "The run recorded the queued update as a user message.",
                ),
                (
                    "Refused → started run",
                    "Steering was refused and the daemon admitted a new run.",
                ),
                (
                    "Refused · back to draft",
                    "The attempted follow-up went back to the draft.",
                ),
                (
                    "Not confirmed · draft kept",
                    "Delivery remains uncertain. Boxen leaves the draft for you.",
                ),
            ],
        ),
        (
            "Held follow-up · Claude Code",
            &[
                (
                    "Held after run",
                    "The daemon retained one hold for the predecessor.",
                ),
                (
                    "Appended",
                    "An additional contribution joined the same hold.",
                ),
                (
                    "Started run",
                    "The predecessor completed and the daemon admitted the held follow-up.",
                ),
                (
                    "Back to draft",
                    "The hold was released or the follow-up could not start.",
                ),
                (
                    "Edit kept as draft",
                    "The hold changed before the edit was saved.",
                ),
            ],
        ),
        (
            "Gate answer",
            &[
                (
                    "Your answer queued",
                    "The journal retained your attempt ID and decision.",
                ),
                (
                    "Your answer written",
                    "The adapter recorded the write under your attempt ID.",
                ),
                (
                    "Answered by another client",
                    "The journal attributes the written decision to their attempt.",
                ),
                (
                    "Closed · no answer recorded",
                    "The request ended with no recorded answer.",
                ),
                (
                    "Write uncertain",
                    "Replay could not settle whether the write occurred.",
                ),
            ],
        ),
        (
            "Resident mail",
            &[
                (
                    "Saved in outbox",
                    "The sending daemon retained the message for delivery.",
                ),
                (
                    "Accepted by mailbox",
                    "The destination retained the message. It can wait for activation.",
                ),
                (
                    "Presented in activation",
                    "The message appeared in the recorded activation input batch.",
                ),
                (
                    "Auto-activation off",
                    "Queued mail waits for an activation, such as a manual one.",
                ),
                (
                    "Reading and replies",
                    "Reading, understanding and answering require separate evidence.",
                ),
            ],
        ),
    ];
    let width = ui.ctx().content_rect().width();
    let columns = if width >= 970. { 4 } else { 2 };
    ui.set_width((width - 48.).clamp(250., 940.));
    ui.strong("What each receipt establishes");
    ui.separator();
    egui::ScrollArea::vertical()
        .max_height(410.)
        .show(ui, |ui| {
            for row in groups.chunks(columns) {
                ui.columns(columns, |uis| {
                    for (column, (heading, receipts)) in uis.iter_mut().zip(row) {
                        column.label(RichText::new(*heading).monospace().size(10.5).strong());
                        for (label, explanation) in *receipts {
                            column.label(RichText::new(*label).strong());
                            column.weak(*explanation);
                            column.add_space(4.);
                        }
                    }
                });
                ui.add_space(8.);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThreadKey;

    fn pane(id: &str) -> Pane {
        Pane::Thread(ThreadKey {
            peer: "fixture-peer".into(),
            id: id.into(),
        })
    }

    #[test]
    fn receipt_key_uses_four_columns_when_the_viewport_can_fit_them() {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200., 750.),
                )),
                ..Default::default()
            },
            receipt_key,
        );
        let positions: Vec<_> = [
            "Thread message",
            "Held follow-up · Claude Code",
            "Gate answer",
            "Resident mail",
        ]
        .map(|heading| {
            output
                .shapes
                .iter()
                .find_map(|shape| {
                    let egui::Shape::Text(text) = &shape.shape else {
                        return None;
                    };
                    (text.galley.job.text == heading && shape.clip_rect.contains(text.pos))
                        .then_some(text.pos)
                })
                .unwrap()
        })
        .into();
        assert!(
            positions
                .windows(2)
                .all(|pair| pair[0].x < pair[1].x && pair[0].y == pair[1].y)
        );
    }

    #[test]
    fn peeking_placement_splitting_and_closing_keep_unaffected_homes() {
        let mut grid = Grid::default();
        for id in ["a", "b", "c"] {
            grid.open(pane(id), true);
        }
        let first = grid.slots[0].home;
        let second = grid.slots[1].home;
        grid.open(pane("d"), false);
        assert_eq!(grid.peek, Some(pane("d")));
        grid.open(pane("d"), true);
        assert_eq!(grid.slots.len(), 3);
        grid.snap(2, 1, false);
        assert_eq!(grid.slots[2].home.height, 1);
        assert_eq!(grid.slots[3].home.row, 1);
        grid.close(&pane("d"));
        assert_eq!(grid.slots[0].home, first);
        assert_eq!(grid.slots[1].home, second);
        assert_eq!(grid.slots[2].home.height, 1);
    }

    #[test]
    fn wide_placement_and_tidy_reserve_the_parked_home() {
        assert_eq!(Grid::columns_for(1600.), 3);
        assert_eq!(Grid::columns_for(2040.), 4);
        let mut grid = Grid::default();
        grid.open(pane("wide"), true);
        grid.span(&pane("wide"), 3, 2);
        grid.open(pane("new"), true);
        assert_eq!(grid.snap_region(1, 1, false).unwrap().width, 2);
        grid.snap(1, 1, false);
        assert_eq!(grid.slots[0].home.width, 1);
        assert_eq!(grid.slots[1].home.width, 2);
        grid.columns = 4;
        grid.open(pane("parked"), true);
        grid.columns = 3;
        let parked = grid.slots[2].home;
        let plan = grid.tidy(true).unwrap();
        assert_eq!(
            plan.iter()
                .find(|slot| slot.pane == pane("parked"))
                .unwrap()
                .home,
            parked
        );
        assert_eq!(grid.slots[1].home.width, 2);
        assert_eq!(
            plan.iter()
                .find(|slot| slot.pane == pane("new"))
                .unwrap()
                .home
                .width,
            1
        );
    }

    #[test]
    fn width_round_trip_parks_only_the_fourth_column() {
        let mut grid = Grid {
            columns: Grid::columns_for(2040.),
            ..Default::default()
        };
        let names = ["first", "second", "third", "fourth"];
        for name in names {
            grid.open(pane(name), true);
        }
        let original: Vec<_> = grid.slots.iter().map(|slot| slot.home).collect();
        let mut data = crate::Data::default();
        data.saved.workspace.grid = Some(grid);

        let ctx = egui::Context::default();
        crate::console_style::install(&ctx);
        let mut desktop = crate::fixture_desktop(
            ctx.clone(),
            data,
            std::path::PathBuf::new(),
            String::new(),
            None,
        );
        for (width, columns, shown) in [(2040., 4, 4), (1600., 3, 3), (2040., 4, 4)] {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1000.),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let mut actions = Vec::new();
                    desktop.grid_workspace(ui, &mut actions);
                    assert!(actions.is_empty());
                },
            );
            let visible_text = |label: &str| {
                output.shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text)
                        if text.galley.job.text == label && shape.clip_rect.contains(text.pos))
                })
            };
            assert_eq!(desktop.grid().columns, columns);
            assert_eq!(visible_text("Parked (1)"), shown == 3);
            for (index, name) in names.iter().enumerate() {
                assert_eq!(visible_text(&format!("Machine / {name}")), index < shown);
            }
            assert_eq!(
                desktop
                    .grid()
                    .slots
                    .iter()
                    .map(|slot| slot.home)
                    .collect::<Vec<_>>(),
                original,
            );
        }
    }

    #[test]
    fn explicit_placement_clears_a_parked_home() {
        let mut grid = Grid {
            columns: 4,
            ..Default::default()
        };
        for id in ["a", "b", "c", "d"] {
            grid.open(pane(id), true);
        }
        grid.columns = 3;
        assert!(!grid.slots[3].home.fits(grid.columns));
        grid.close(&pane("b"));
        grid.open(pane("d"), true);
        grid.columns = 4;
        assert_eq!(
            grid.slots.iter().position(|slot| slot.pane == pane("d")),
            Some(2)
        );
        assert_eq!(grid.slots[2].home.column, 1);
    }
}
