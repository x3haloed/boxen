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
    /// Holes present when Tidy was dismissed; Tidy stays quiet until they change.
    #[serde(skip)]
    pub tidy_quiet: Option<Vec<Cell>>,
    /// Recently closed panes, so a hole can say what it held.
    #[serde(skip)]
    pub closed: Vec<(Cell, String, u64)>,
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
            tidy_quiet: None,
            closed: Vec::new(),
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

    pub fn close_titled(&mut self, pane: &Pane, title: String) {
        if let Some(slot) = self.slots.iter().find(|slot| &slot.pane == pane) {
            let cell = slot.home;
            self.closed.retain(|(c, _, _)| !c.overlaps(cell));
            self.closed.push((cell, title, crate::pulse::now()));
            self.closed.truncate(16);
        }
        self.close(pane);
    }

    pub fn close(&mut self, pane: &Pane) {
        self.slots.retain(|slot| &slot.pane != pane);
        if self.focus.as_ref() == Some(pane) {
            self.focus = None;
        }
    }

    /// What happens to the pane already there: `None` = nothing displaced,
    /// `Some((pane, None))` = replaced, `Some((pane, Some(cell)))` = it moves to `cell`.
    pub fn snap_consequence(
        &self,
        column: usize,
        row: usize,
        replace: bool,
    ) -> Option<Option<(Pane, Option<Cell>)>> {
        let plan = self.snap_plan(column, row, replace)?;
        Some(
            plan.change
                .map(|(index, cell)| (self.slots[index].pane.clone(), cell)),
        )
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

    /// Grow panes into adjacent holes without moving anything.
    fn grow_into_holes(&self) -> Option<Vec<Placement>> {
        let mut grown = Grid {
            slots: self.slots.clone(),
            columns: self.columns,
            ..Default::default()
        };
        let mut changed = false;
        while let Some((pane, cell)) = grown
            .holes()
            .into_iter()
            .find_map(|hole| grown.neighbor(hole))
        {
            if let Some(slot) = grown.slots.iter_mut().find(|s| s.pane == pane) {
                slot.home = cell;
                changed = true;
            }
        }
        changed.then_some(grown.slots)
    }

    pub fn tidy(&self, even: bool) -> Option<Vec<Placement>> {
        if !even && let Some(grown) = self.grow_into_holes() {
            return Some(grown);
        }
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
        if !even {
            // Fill holes: after compaction, grow neighbors into what is left.
            let compacted = Grid {
                slots: arranged.clone(),
                columns: self.columns,
                ..Default::default()
            };
            if let Some(grown) = compacted.grow_into_holes() {
                arranged = grown;
                changed = true;
            }
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

    /// Widening into occupied cells: each overlapped pane keeps the
    /// rectangle the wider span leaves it. `None` when a neighbor would be
    /// left with nothing or a non-rectangular remainder.
    pub fn span_plan(&self, pane: &Pane, width: usize, height: usize) -> Option<Vec<(Pane, Cell)>> {
        let home = self.slots.iter().find(|s| s.pane == *pane)?.home;
        let candidate = Cell {
            width,
            height,
            ..home
        };
        if !candidate.fits(self.columns) || candidate.row + height > ROWS {
            return None;
        }
        self.slots
            .iter()
            .filter(|slot| slot.pane != *pane && slot.home.overlaps(candidate))
            .map(|slot| Some((slot.pane.clone(), remainder(slot.home, candidate)?)))
            .collect()
    }

    pub fn span_displacing(&mut self, pane: &Pane, width: usize, height: usize) {
        let Some(plan) = self.span_plan(pane, width, height) else {
            return;
        };
        for (other, cell) in plan {
            if let Some(slot) = self.slots.iter_mut().find(|s| s.pane == other) {
                slot.home = cell;
            }
        }
        self.span(pane, width, height);
    }
}

/// `cell` minus `taken`, when what is left is one rectangle.
fn remainder(cell: Cell, taken: Cell) -> Option<Cell> {
    let covers_rows = taken.row <= cell.row && taken.row + taken.height >= cell.row + cell.height;
    let covers_columns =
        taken.column <= cell.column && taken.column + taken.width >= cell.column + cell.width;
    if covers_rows {
        let left = taken.column.saturating_sub(cell.column).min(cell.width);
        let right_start = (taken.column + taken.width).max(cell.column);
        let right = (cell.column + cell.width).saturating_sub(right_start);
        return match (left, right) {
            (0, 0) => None,
            (left, 0) => Some(Cell {
                width: left,
                ..cell
            }),
            (0, right) => Some(Cell {
                column: right_start,
                width: right,
                ..cell
            }),
            _ => None,
        };
    }
    if covers_columns {
        let top = taken.row.saturating_sub(cell.row).min(cell.height);
        let bottom_start = (taken.row + taken.height).max(cell.row);
        let bottom = (cell.row + cell.height).saturating_sub(bottom_start);
        return match (top, bottom) {
            (0, 0) => None,
            (top, 0) => Some(Cell {
                height: top,
                ..cell
            }),
            (0, bottom) => Some(Cell {
                row: bottom_start,
                height: bottom,
                ..cell
            }),
            _ => None,
        };
    }
    None
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

        let holes_now = layout.holes();
        let quiet = layout.tidy_quiet.as_ref() == Some(&holes_now);
        // Titles for menus that cannot borrow the viewer while the grid is borrowed.
        let mut titles: Vec<(Pane, String, String)> = Vec::new();
        let mut panes: Vec<Placement> = layout.slots.clone();
        if let Some(placing) = layout.placing.clone()
            && !panes.iter().any(|slot| slot.pane == placing)
        {
            panes.push(Placement {
                pane: placing,
                home: Cell::column(0),
            });
        }
        for slot in panes {
            let mut pane = slot.pane.clone();
            let title = Viewer {
                data: &mut self.data,
                actions,
            }
            .title(&mut pane)
            .text()
            .to_owned();
            let detail = match &slot.pane {
                Pane::Thread(key) | Pane::Chat(key) | Pane::Activity(key) => {
                    let host = crate::conversation::destination(&self.data, &key.peer).label;
                    let (_, run) = pane_heading(&self.data, &slot.pane, &title);
                    match run {
                        Some(run) => format!("{host} · {run}"),
                        None => host,
                    }
                }
                _ => String::new(),
            };
            titles.push((slot.pane, title, detail));
        }
        let title_of = |pane: &Pane| {
            titles
                .iter()
                .find(|(p, _, _)| p == pane)
                .map_or_else(|| "Pane".to_owned(), |(_, title, _)| title.clone())
        };

        let bar = egui::Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            egui::vec2(ui.available_width(), 24.),
        );
        ui.allocate_rect(bar, egui::Sense::hover());
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("toolbar-right")
                .max_rect(bar)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        {
            let ui = &mut right;
            ui.spacing_mut().item_spacing.x = 10.;
            egui::containers::menu::MenuButton::from_button(
                crate::console_style::icon_button(RichText::new("⚙").size(13.)),
            )
            .ui(ui, |ui| {
                ui.menu_button("Settings", |ui| {
                    if ui.button("Machine connections…").clicked() {
                        self.connections.open = true;
                        ui.close();
                    }
                    if ui.button("Tool approvals…").clicked() {
                        self.data.tool_approvals.open();
                        ui.close();
                    }
                });
                if ui.button("Live pulse").clicked() {
                    self.grid_mut().open(Pane::LivePulse, true);
                    ui.close();
                }
                ui.menu_button("Receipt key", receipt_key);
            })
            .0
            .on_hover_text("Settings, Live pulse and the receipt key");
            ui.label(
                RichText::new(format!("grid {columns} × 2"))
                    .monospace()
                    .size(10.5)
                    .weak(),
            );
            crate::needs::toolbar(ui, &self.data, actions);
            if has_holes && !visible.is_empty() && !quiet {
                let count = holes_now.len();
                let chip = egui::Button::new(
                    RichText::new(format!(
                        "◇ Tidy · {count} {}",
                        if count == 1 { "hole" } else { "holes" }
                    ))
                    .monospace()
                    .size(10.5)
                    .color(crate::console_style::WARN),
                )
                .fill(egui::Color32::from_rgb(34, 27, 14))
                .stroke(egui::Stroke::new(1., egui::Color32::from_rgb(90, 67, 24)));
                egui::containers::menu::MenuButton::from_button(chip).ui(ui, |ui| {
                    ui.set_width(300.);
                    ui.strong("Tidy layout");
                    ui.weak("Suggestions only; nothing changes until you pick one.");
                    ui.separator();
                    for (name, even) in [("Fill holes", false), ("Even grid", true)] {
                        let plan = self.grid().tidy(even);
                        let summary = plan.as_ref().map(|plan| {
                            describe_plan(&self.grid().slots, plan, columns, &title_of)
                        });
                        let response = ui.add_enabled(plan.is_some(), egui::Button::new(name));
                        ui.weak(summary.clone().unwrap_or_else(|| {
                            if even {
                                "Already even".into()
                            } else {
                                "No pane can grow into the holes".into()
                            }
                        }));
                        ui.add_space(4.);
                        if let Some(plan) = plan {
                            if response.clicked() {
                                self.grid_mut().slots = plan;
                                ui.close();
                            } else if response.hovered() {
                                tidy_preview = Some(plan);
                            }
                        }
                    }
                    ui.separator();
                    if ui
                        .button("Dismiss")
                        .on_hover_text("The holes stay and Tidy stays quiet until they change")
                        .clicked()
                    {
                        self.grid_mut().tidy_quiet = Some(holes_now.clone());
                        ui.close();
                    }
                });
            }
            if !parked.is_empty() {
                let chip = egui::Button::new(
                    RichText::new(format!("Parked ({})", parked.len()))
                        .monospace()
                        .size(10.5),
                );
                egui::containers::menu::MenuButton::from_button(chip).ui(ui, |ui| {
                    ui.set_min_width(260.);
                    for slot in &parked {
                        let pane = slot.pane.clone();
                        let (title, detail) = titles
                            .iter()
                            .find(|(p, _, _)| *p == pane)
                            .map(|(_, t, d)| (t.clone(), d.clone()))
                            .unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(&title).strong()).truncate())
                            .on_hover_text(&title);
                        ui.weak(if detail.is_empty() {
                            format!("home column {}", slot.home.column + 1)
                        } else {
                            format!("{detail} · home column {}", slot.home.column + 1)
                        });
                        ui.horizontal(|ui| {
                            let layout = self.grid_mut();
                            if ui.button("Peek").clicked() {
                                layout.open(pane.clone(), false);
                                ui.close();
                            }
                            if ui.button("Place on grid…").clicked() {
                                layout.open(pane.clone(), true);
                                ui.close();
                            }
                        });
                        ui.add_space(4.);
                    }
                });
            }
        }
        let left_rect = egui::Rect::from_min_max(
            bar.min,
            egui::pos2(
                (right.min_rect().left() - 12.).max(bar.left()),
                bar.bottom(),
            ),
        );
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("toolbar-left")
                .max_rect(left_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        {
            let ui = &mut left;
            let search_width = (ui.available_width() * 0.45).clamp(60., 240.);
            let (search, response) =
                ui.allocate_exact_size(egui::vec2(search_width, 20.), egui::Sense::click());
            let painter = ui.painter();
            painter.rect(
                search,
                3.,
                crate::console_style::FIELD,
                egui::Stroke::new(
                    1.,
                    if response.hovered() {
                        crate::console_style::ACCENT
                    } else {
                        crate::console_style::EDGE
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let weak = ui.visuals().weak_text_color();
            painter.with_clip_rect(search.shrink(2.)).text(
                search.left_center() + egui::vec2(7., 0.),
                egui::Align2::LEFT_CENTER,
                "thr: res: host: run: …",
                egui::FontId::monospace(10.5),
                weak,
            );
            if search_width > 180. {
                painter.text(
                    search.right_center() - egui::vec2(7., 0.),
                    egui::Align2::RIGHT_CENTER,
                    "⌘K",
                    egui::FontId::monospace(10.),
                    weak,
                );
            }
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Find…"));
            if response
                .on_hover_text("Find threads, residents, hosts and runs · ⌘K")
                .clicked()
            {
                self.open_palette();
            }
            ui.add_space(12.);
            let coverage = crate::needs::coverage(&self.data);
            let destinations = self.data.saved.destinations.clone();
            for (index, dest) in destinations.iter().enumerate() {
                let count = coverage.get(dest.key.as_str());
                let (observed, total) = count.map_or((0, 0), |c| (c.observed, c.total));
                let down = self.data.errors.contains_key(&dest.key);
                let catalogued = self.data.catalogs.contains_key(&dest.key);
                // A hollow glyph and a red name say the machine is unreachable.
                let (label, color) = if down {
                    (dest.label.clone(), crate::console_style::ERROR)
                } else if !catalogued {
                    (format!("{} …", dest.label), egui::Color32::GRAY)
                } else if observed != total {
                    (
                        format!("{} {observed}/{total}", dest.label),
                        crate::console_style::WARN,
                    )
                } else {
                    (dest.label.clone(), ui.visuals().text_color())
                };
                let needed = label.chars().count() as f32 * 6.6 + 22.;
                if ui.available_width() < needed {
                    let rest = destinations.len() - index;
                    ui.label(
                        RichText::new(format!("+{rest}"))
                            .monospace()
                            .size(10.5)
                            .weak(),
                    )
                    .on_hover_text(
                        destinations[index..]
                            .iter()
                            .map(|d| d.label.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    break;
                }
                crate::marks::Identity::Machine {
                    key: &dest.key,
                    reachable: !down,
                }
                .show(ui, 12., crate::console_style::CANVAS);
                ui.label(RichText::new(label).monospace().size(10.5).color(color))
                    .on_hover_text(if down {
                        "Machine unreachable"
                    } else if total > 0 {
                        "Observed active runs / active runs in the latest catalog"
                    } else {
                        "Reachable · no active runs"
                    });
                ui.add_space(8.);
            }
        }
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
            if sidebar == Pane::Threads {
                ui.strong("Watchlist");
            } else {
                if ui.button("← Watchlist").clicked() {
                    sidebar = Pane::Threads;
                }
                ui.strong(match sidebar {
                    Pane::Agents => "Agents",
                    Pane::Correspondence => "Mail",
                    _ => "",
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::containers::menu::MenuButton::from_button(
                    crate::console_style::icon_button("···"),
                )
                .ui(ui, |ui| {
                    for (pane, label) in [
                        (Pane::Threads, "Watchlist"),
                        (Pane::Agents, "Agents and definitions"),
                        (Pane::Correspondence, "Mail"),
                    ] {
                        if ui.selectable_label(sidebar == pane, label).clicked() {
                            sidebar = pane;
                            ui.close();
                        }
                    }
                })
                .0
                .on_hover_text("Sidebar views");
                if ui
                    .add(crate::console_style::icon_button("+"))
                    .on_hover_text("New thread")
                    .clicked()
                {
                    actions.push(Action::NewConversation);
                }
            });
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
            empty.add_space((bounds.height() / 2. - 34.).max(0.));
            let closed = self
                .grid()
                .closed
                .iter()
                .rev()
                .find(|(cell, _, _)| cell.overlaps(hole))
                .cloned();
            empty.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!(
                        "empty · col {}{}",
                        hole.column + 1,
                        if hole.height == 1 {
                            if hole.row == 0 { " top" } else { " bottom" }
                        } else {
                            ""
                        }
                    ))
                    .monospace()
                    .size(10.5)
                    .weak(),
                );
                ui.add_space(4.);
                if ui.button("+ Open here").clicked() {
                    self.open_palette();
                    self.grid_mut().open_here = Some(hole);
                }
                let layout = self.grid_mut();
                if let Some((pane, expanded)) = layout.neighbor(hole)
                    && ui.button("Expand neighbor").clicked()
                {
                    layout.install(pane, expanded);
                }
                if let Some((_, title, at)) = &closed {
                    ui.add_space(4.);
                    let short: String = title.chars().take(32).collect();
                    ui.label(
                        RichText::new(format!(
                            "closed {short} · {} UTC",
                            crate::conversation::utc_minute(*at)
                        ))
                        .monospace()
                        .size(10.)
                        .weak(),
                    );
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
                egui::Stroke::new(1., ui.visuals().widgets.noninteractive.bg_stroke.color),
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
                if let Some(identity) = pane_identity(&self.data, &slot.pane) {
                    identity.show(ui, 12., crate::console_style::PANEL);
                }
                // The state dot, runtime (and for threads the ··· menu) sit at
                // the right end of the title bar; the title takes the rest.
                let status = match &slot.pane {
                    Pane::Thread(_) => 110.,
                    Pane::Chat(_) | Pane::Activity(_) => 80.,
                    _ => 0.,
                };
                let width = (ui.available_width() - 75. - status).max(0.);
                let heading = ui.allocate_ui_with_layout(
                    egui::vec2(width, 20.),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(title.text()).strong())
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                    },
                );
                // The heading's full width focuses the pane, not only its glyphs.
                if heading.inner.clicked()
                    || heading.response.interact(egui::Sense::click()).clicked()
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
                    close = ui.add(crate::console_style::icon_button("×")).on_hover_text("Close pane").clicked();
                    let pane = slot.pane.clone();
                    ui.menu_button(format!("{}×{}", slot.home.width, slot.home.height), |ui| {
                        ui.set_min_width(240.);
                        ui.weak("Span · neighbors keep what is left");
                        for span in 1..=columns - slot.home.column {
                            for rows in 1..=2 - slot.home.row {
                                if span == slot.home.width && rows == slot.home.height {
                                    continue;
                                }
                                let candidate = Cell {
                                    width: span,
                                    height: rows,
                                    ..slot.home
                                };
                                let free = self.grid().free(candidate, &pane);
                                let plan = if free {
                                    Some(Vec::new())
                                } else {
                                    self.grid().span_plan(&pane, span, rows)
                                };
                                let effect = match &plan {
                                    Some(changes)
                                        if changes.is_empty()
                                            && span * rows < slot.home.width * slot.home.height =>
                                    {
                                        "leaves a hole".to_owned()
                                    }
                                    Some(changes) if changes.is_empty() => {
                                        "into empty cells".to_owned()
                                    }
                                    Some(changes) => changes
                                        .iter()
                                        .map(|(other, cell)| {
                                            format!(
                                                "{} → {}×{}",
                                                title_of(other)
                                                    .chars()
                                                    .take(18)
                                                    .collect::<String>(),
                                                cell.width,
                                                cell.height
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    None => "a neighbor would have no room".to_owned(),
                                };
                                let response = ui.add_enabled(
                                    plan.is_some(),
                                    egui::Button::new(format!("{span}×{rows}  ·  {effect}")),
                                );
                                if response.clicked() {
                                    self.grid_mut().span_displacing(&pane, span, rows);
                                    ui.close();
                                }
                            }
                        }
                    });
                    match &slot.pane {
                        Pane::Thread(key) => {
                            crate::thread_view::title_status(ui, &mut self.data, key, actions);
                        }
                        Pane::Chat(key) | Pane::Activity(key) => {
                            crate::correspondence::title_status(ui, &self.data, key);
                        }
                        _ => {}
                    }
                });
            });
            if close {
                let title = title_of(&slot.pane);
                self.grid_mut().close_titled(&slot.pane, title);
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
            let current = self.grid().slots.clone();
            for slot in plan.iter().filter(|slot| slot.home.fits(columns)) {
                let before = current.iter().find(|c| c.pane == slot.pane).map(|c| c.home);
                if before == Some(slot.home) {
                    continue;
                }
                let area = rect(slot.home).shrink(3.);
                crate::console_style::dashed_rect(ui, area, crate::console_style::ACCENT);
                let label = format!(
                    "preview · {} → {}×{}",
                    title_of(&slot.pane).chars().take(26).collect::<String>(),
                    slot.home.width,
                    slot.home.height
                );
                let galley = ui.painter().layout_no_wrap(
                    label,
                    egui::FontId::monospace(10.5),
                    crate::console_style::ACCENT,
                );
                let tag = egui::Rect::from_center_size(
                    egui::pos2(area.center().x, area.top() + 40.),
                    galley.size() + egui::vec2(16., 8.),
                );
                ui.painter().rect(
                    tag,
                    3.,
                    crate::console_style::PANEL,
                    egui::Stroke::new(1., crate::console_style::ACCENT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().galley(
                    tag.center() - galley.size() / 2.,
                    galley,
                    crate::console_style::ACCENT,
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
            let placing_title = placing.as_ref().map(&title_of).unwrap_or_default();
            for (column, row, region) in choices {
                let consequence = layout_consequence(self.grid(), column, row, replace);
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
                let fill = if response.hovered() {
                    egui::Color32::from_rgb(22, 34, 52)
                } else {
                    egui::Color32::from_rgb(15, 20, 28)
                };
                ui.painter().rect_filled(target, 0., fill);
                if response.hovered()
                    && region
                        != (Cell {
                            column,
                            row,
                            width: 1,
                            height: 1,
                        })
                {
                    crate::console_style::dashed_rect(
                        ui,
                        rect(region).shrink(3.),
                        crate::console_style::ACCENT,
                    );
                }
                crate::console_style::dashed_rect(ui, target, crate::console_style::ACCENT);
                let chip = egui::Rect::from_center_size(
                    target.center() - egui::vec2(0., 30.),
                    egui::vec2(24., 24.),
                );
                ui.painter().rect(
                    chip,
                    3.,
                    if response.hovered() {
                        crate::console_style::ACCENT
                    } else {
                        crate::console_style::FIELD
                    },
                    egui::Stroke::new(1., crate::console_style::ACCENT),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    chip.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", column + 1),
                    egui::FontId::monospace(12.),
                    if response.hovered() {
                        egui::Color32::WHITE
                    } else {
                        crate::console_style::ACCENT
                    },
                );
                let heading = match (replace, region.height, row) {
                    (true, _, _) => format!("Replace in col {}", column + 1),
                    (false, 2, _) => format!("Column {}", column + 1),
                    (false, _, 0) => format!("Top of col {}", column + 1),
                    _ => format!("Bottom of col {}", column + 1),
                };
                ui.painter().text(
                    target.center() + egui::vec2(0., 2.),
                    egui::Align2::CENTER_CENTER,
                    heading,
                    egui::FontId::proportional(12.5),
                    ui.visuals().text_color(),
                );
                let effect = match &consequence {
                    Some(Some((pane, Some(cell)))) => format!(
                        "{} → {} {}×{}",
                        title_of(pane).chars().take(24).collect::<String>(),
                        if cell.height == 2 {
                            "full".to_owned()
                        } else if cell.row == 0 {
                            "top".to_owned()
                        } else {
                            "bottom".to_owned()
                        },
                        cell.width,
                        cell.height
                    ),
                    Some(Some((pane, None))) => format!(
                        "{} closes",
                        title_of(pane).chars().take(24).collect::<String>()
                    ),
                    _ => "empty cell · nothing moves".into(),
                };
                ui.painter().text(
                    target.center() + egui::vec2(0., 20.),
                    egui::Align2::CENTER_CENTER,
                    effect,
                    egui::FontId::monospace(10.),
                    ui.visuals().weak_text_color(),
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
            let hint = format!(
                "Place {}   1–{} {}   ⇧1–{} replace   Space peek   Esc cancel",
                placing_title.chars().take(28).collect::<String>(),
                columns.min(4),
                if replace { "replace" } else { "choose" },
                columns.min(4),
            );
            let galley = ui.painter().layout_no_wrap(
                hint,
                egui::FontId::monospace(10.5),
                ui.visuals().text_color(),
            );
            let bar = egui::Rect::from_center_size(
                egui::pos2(board.center().x, board.bottom() - 34.),
                galley.size() + egui::vec2(24., 12.),
            );
            ui.painter().rect(
                bar,
                4.,
                crate::console_style::PANEL,
                egui::Stroke::new(1., crate::console_style::ACCENT),
                egui::StrokeKind::Inside,
            );
            ui.painter().galley(
                bar.center() - galley.size() / 2.,
                galley,
                ui.visuals().text_color(),
            );
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
            let (heading, run) = pane_heading(&self.data, &pane, title.text());
            let heading = match run {
                Some(run) => format!("{heading}  {run}"),
                None => heading,
            };
            let size = egui::vec2(
                (board.width() * 0.55).clamp(380., 620.),
                (board.height() * 0.72).max(300.),
            );
            egui::Window::new(RichText::new(heading).monospace().size(10.5))
                .id(egui::Id::new(("peek", id)))
                .fixed_pos(board.min + egui::vec2(8., 8.))
                .fixed_size(size)
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    let footer = 26.;
                    let body = egui::vec2(ui.available_width(), (size.y - footer).max(120.));
                    ui.allocate_ui(body, |ui| {
                        ui.set_min_size(body);
                        Viewer {
                            data: &mut self.data,
                            actions,
                        }
                        .ui(ui, &mut pane);
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Open on grid ↗").clicked() {
                            self.grid_mut().open(pane.clone(), true);
                        }
                        ui.label(
                            RichText::new("Esc closes · the grid is not changed")
                                .monospace()
                                .size(10.)
                                .weak(),
                        );
                    });
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
                    "No line",
                    "The message appeared in a recorded activation input batch.",
                ),
                (
                    "Input unconfirmed",
                    "An activation was assigned, but its input batch was not recorded.",
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

fn layout_consequence(
    grid: &Grid,
    column: usize,
    row: usize,
    replace: bool,
) -> Option<Option<(Pane, Option<Cell>)>> {
    grid.snap_consequence(column, row, replace)
}

fn describe_plan(
    current: &[Placement],
    plan: &[Placement],
    columns: usize,
    title_of: &dyn Fn(&Pane) -> String,
) -> String {
    let changes: Vec<String> = plan
        .iter()
        .filter(|slot| slot.home.fits(columns))
        .filter_map(|slot| {
            let before = current.iter().find(|c| c.pane == slot.pane)?.home;
            (before != slot.home).then(|| {
                let size = format!("{}×{}", slot.home.width, slot.home.height);
                let title = title_of(&slot.pane);
                let short: String = title.chars().take(22).collect();
                if before.width != slot.home.width || before.height != slot.home.height {
                    if before.column == slot.home.column && before.row == slot.home.row {
                        format!("{short} grows to {size}")
                    } else {
                        format!("{short} → col {} · {size}", slot.home.column + 1)
                    }
                } else {
                    format!("{short} → col {}", slot.home.column + 1)
                }
            })
        })
        .collect();
    changes.join("; ")
}

/// The mark that leads a pane's title: a thread shows the machine it lives on,
/// an agent its own sigil, which follows it from machine to machine.
pub fn pane_identity<'a>(data: &'a crate::Data, pane: &'a Pane) -> Option<crate::marks::Identity<'a>> {
    match pane {
        Pane::Thread(key) => Some(crate::marks::Identity::Machine {
            key: &key.peer,
            reachable: !data.errors.contains_key(&key.peer),
        }),
        Pane::Chat(key) | Pane::Activity(key) => Some(crate::marks::Identity::Agent {
            resident: &key.id,
            definition: data
                .agents
                .resident(key)
                .and_then(|resident| resident["definition"]["id"].as_str()),
        }),
        _ => None,
    }
}

/// `THIS MACHINE · LOCAL DEV ENVIRONMENT` plus `run 42` / `act 14`.
pub fn pane_heading(data: &crate::Data, pane: &Pane, title: &str) -> (String, Option<String>) {
    match pane {
        Pane::Thread(key) => {
            let host = crate::conversation::destination(data, &key.peer);
            let runs = crate::find_thread(data, key).map_or(0, |t| t.runs.len());
            (
                format!("{} · {}", host.label, title).to_uppercase(),
                (runs > 0).then(|| format!("run {runs}")),
            )
        }
        Pane::Chat(key) | Pane::Activity(key) => {
            let host = crate::conversation::destination(data, &key.peer);
            let acts = data.agents.history(key).map_or(0, |runs| runs.len());
            (
                format!("{} · {}", host.label, title).to_uppercase(),
                (acts > 0).then(|| format!("act {acts}")),
            )
        }
        _ => (title.to_uppercase(), None),
    }
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
    fn fill_holes_grows_a_neighbor_and_span_keeps_neighbors_rectangular() {
        let mut grid = Grid {
            columns: 2,
            ..Default::default()
        };
        grid.open(pane("left"), true);
        grid.open(pane("right"), true);
        grid.span(&pane("left"), 1, 1);
        assert_eq!(grid.holes().len(), 1);
        let plan = grid.tidy(false).expect("fill holes has a plan");
        let left = plan.iter().find(|slot| slot.pane == pane("left")).unwrap();
        assert_eq!((left.home.width, left.home.height), (1, 2));

        let mut grid = Grid {
            columns: 3,
            ..Default::default()
        };
        for name in ["a", "b", "c"] {
            grid.open(pane(name), true);
        }
        let plan = grid
            .span_plan(&pane("a"), 2, 1)
            .expect("b keeps its bottom half");
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].1,
            Cell {
                column: 1,
                row: 1,
                width: 1,
                height: 1
            }
        );
        assert!(
            grid.span_plan(&pane("a"), 2, 2).is_none(),
            "b would have nothing left"
        );
        grid.span_displacing(&pane("a"), 2, 1);
        assert_eq!(
            grid.slots[0].home,
            Cell {
                column: 0,
                row: 0,
                width: 2,
                height: 1
            }
        );
        assert_eq!(
            grid.slots[1].home,
            Cell {
                column: 1,
                row: 1,
                width: 1,
                height: 1
            }
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
                assert_eq!(
                    visible_text(name),
                    index < shown
                );
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
