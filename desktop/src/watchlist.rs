//! Compact, fixed-column rows for the Grid workspace Watchlist.
use crate::{
    Action, Data,
    client::run_key,
    conversation,
    model::ThreadKey,
    pulse::{self, Visible},
};
use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

const HEIGHT: f32 = 20.;
const MARK: f32 = 12.;
const STATE: f32 = 68.;
const LAST: f32 = 46.;
const AGE: f32 = 30.;

struct Columns {
    name: egui::Rect,
    mark: egui::Rect,
    state: egui::Rect,
    last: egui::Rect,
    age: egui::Rect,
}

fn columns(ui: &mut egui::Ui) -> Columns {
    let width = (ui.clip_rect().right() - ui.next_widget_position().x - 8.).max(0.);
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::hover());
    let fixed = MARK + STATE + LAST + AGE;
    let name = (width - fixed).max(1.);
    let mut x = row.left();
    let mut cell = |w| {
        let rect = egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(w, HEIGHT));
        x += w;
        rect
    };
    Columns {
        mark: cell(MARK + 4.),
        name: cell(name - 4.),
        state: cell(STATE),
        last: cell(LAST),
        age: cell(AGE),
    }
}

fn cell(ui: &mut egui::Ui, rect: egui::Rect, widget: impl egui::Widget) -> egui::Response {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    child.add(widget)
}

fn label(ui: &mut egui::Ui, rect: egui::Rect, text: &str, color: Color32) -> egui::Response {
    cell(
        ui,
        rect,
        egui::Label::new(RichText::new(text).monospace().size(10.).color(color))
            .truncate()
            .selectable(false),
    )
}

pub fn header(ui: &mut egui::Ui) {
    let c = columns(ui);
    let weak = ui.visuals().weak_text_color();
    label(ui, c.name, "NAME", weak);
    label(ui, c.state, "STATE", weak).on_hover_text("Current input owner or latest activation");
    label(ui, c.last, "LAST", weak).on_hover_text("Latest event observed by this client");
    label(ui, c.age, "AGE", weak).on_hover_text(
        "Time since the last observed event · ~ marks time since the latest run was accepted, for rows this client has not observed",
    );
}

pub struct Row<'a> {
    pub title: &'a str,
    pub state: &'a str,
    pub color: Color32,
    pub last: &'a str,
    pub age: &'a str,
    pub selected: bool,
    pub muted: bool,
    pub context: Option<(&'a ThreadKey, &'a [Value])>,
    /// A thread's machine glyph or an agent's sigil, in place of a host label.
    pub identity: Option<crate::marks::Identity<'a>>,
}

impl Row<'_> {
    pub fn show(self, ui: &mut egui::Ui, data: &Data) -> egui::Response {
        self.show_with_disclosure(ui, data, None)
    }

    pub fn show_with_disclosure(
        self,
        ui: &mut egui::Ui,
        data: &Data,
        disclosure: Option<bool>,
    ) -> egui::Response {
        let c = columns(ui);
        if self.selected {
            ui.painter()
                .rect_filled(c.mark.union(c.age), 0., ui.visuals().selection.bg_fill);
        }
        if let Some((key, _)) = self.context
            && on_grid(data, key)
        {
            let tick = egui::Rect::from_min_size(
                c.mark.left_top() - egui::vec2(4., -2.),
                egui::vec2(2., HEIGHT - 4.),
            );
            ui.painter()
                .rect_filled(tick, 0., crate::console_style::ACCENT);
        }
        let title = RichText::new(self.title).color(if self.muted {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().text_color()
        });
        // The whole row, not only the name's glyphs, is the click target.
        let response = ui.interact(
            c.mark.union(c.age),
            ui.next_auto_id().with(("watchlist-row", self.title)),
            egui::Sense::click(),
        );
        response.widget_info(|| {
            let kind = if disclosure.is_some() {
                egui::WidgetType::CollapsingHeader
            } else {
                egui::WidgetType::Button
            };
            egui::WidgetInfo::labeled(kind, true, self.title)
        });
        if let Some(open) = disclosure {
            ui.ctx().accesskit_node_builder(response.id, |node| node.set_expanded(open));
        }
        cell(
            ui,
            c.name,
            egui::Label::new(title)
                .truncate()
                .halign(egui::Align::Min)
                .selectable(false),
        );
        let mark = egui::Rect::from_center_size(
            c.mark.left_center() + egui::vec2(5., 0.),
            egui::vec2(10., 10.),
        );
        if let Some(open) = disclosure {
            egui::collapsing_header::paint_default_icon(
                ui,
                if open { 1. } else { 0. },
                &response.clone().with_new_rect(mark),
            );
        } else if let Some(identity) = self.identity {
            let background = if self.selected {
                ui.visuals().selection.bg_fill
            } else {
                crate::console_style::PANEL
            };
            identity.paint(ui.painter(), mark, background);
        }
        label(ui, c.state, self.state, self.color).on_hover_text(self.state);
        let weak = ui.visuals().weak_text_color();
        label(ui, c.last, self.last, weak).on_hover_text(self.last);
        label(ui, c.age, self.age, weak);
        response
    }
}

/// Whether this conversation currently occupies a visible grid cell.
fn on_grid(data: &Data, key: &ThreadKey) -> bool {
    data.saved.workspace.grid.as_ref().is_some_and(|grid| {
        grid.slots.iter().any(|slot| {
            slot.home.fits(grid.columns)
                && matches!(&slot.pane,
                    crate::model::Pane::Thread(k) | crate::model::Pane::Chat(k) | crate::model::Pane::Activity(k)
                        if k == key)
        })
    })
}

pub fn opens(ui: &egui::Ui, response: &egui::Response) -> Option<bool> {
    if response.clicked() {
        response.request_focus();
    }
    if response.double_clicked()
        || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
    {
        Some(true)
    } else {
        response.clicked().then_some(false)
    }
}

/// LAST names the latest activity (a status change only when nothing else
/// was observed); AGE counts from the latest visible event, status included.
pub fn latest_visible(data: &Data, key: &ThreadKey, runs: &[Value]) -> Option<Visible> {
    let transcripts: Vec<_> = runs
        .iter()
        .filter_map(|run| {
            let id = run["record"]["spec"]["id"].as_str()?;
            data.transcripts.get(&run_key(&key.peer, id))
        })
        .collect();
    let latest = transcripts
        .iter()
        .filter_map(|t| t.latest_visible.as_ref().map(|(_, event)| event))
        .max_by_key(|event| event.at)?;
    let label = transcripts
        .iter()
        .filter_map(|t| t.latest_activity.as_ref())
        .max_by_key(|event| event.at)
        .map_or(latest.label, |event| event.label);
    Some(Visible {
        at: latest.at,
        label,
    })
}

/// AGE for rows this client has not observed: time since the latest run was
/// accepted. Callers prefix it with `~`.
pub fn accepted_age(runs: &[Value]) -> Option<String> {
    runs.iter()
        .filter_map(|run| run["record"]["created_at"].as_u64())
        .max()
        .map(pulse::age)
}

pub struct RepoObservation {
    pub run: String,
    pub result: Option<Result<Value, String>>,
}

fn repo_fields(
    ui: &mut egui::Ui,
    data: &Data,
    key: &ThreadKey,
    id: Option<&str>,
    actions: &mut Vec<Action>,
) {
    let sample = data
        .repo_observations
        .get(key)
        .filter(|value| Some(value.run.as_str()) == id);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("REPO").monospace().size(10.5).weak());
        if let Some(Some(Ok(facts))) = sample.map(|value| &value.result) {
            let at = facts["at"]
                .as_u64()
                .map(conversation::utc_minute)
                .unwrap_or_default();
            let path = facts["cwd"].as_str().unwrap_or("Unknown directory");
            ui.weak(format!("probed {at} UTC")).on_hover_text(path);
        }
        if let Some(id) = id {
            let ready = !data.errors.contains_key(&key.peer)
                && sample.is_none_or(|sample| sample.result.is_some());
            let button = if sample.is_some() { "Refresh" } else { "Probe" };
            if ui
                .add_enabled(ready, egui::Button::new(button).small())
                .clicked()
            {
                actions.push(Action::ProbeRepo(key.clone(), id.into()));
            }
        }
    });
    match sample.map(|value| &value.result) {
        Some(Some(Ok(facts))) => {
            let tree = if facts["dirty"] == true {
                "dirty"
            } else {
                "clean"
            };
            let mut tree = tree.to_owned();
            if let Some(ahead) = facts["ahead"].as_u64().filter(|ahead| *ahead > 0) {
                tree.push_str(&format!(" · ahead {ahead}"));
            }
            if let Some(behind) = facts["behind"].as_u64().filter(|behind| *behind > 0) {
                tree.push_str(&format!(" · behind {behind}"));
            }
            ui.columns(2, |columns| {
                field(
                    &mut columns[0],
                    "BRANCH",
                    facts["branch"].as_str().unwrap_or("Unknown"),
                    None,
                );
                let head = facts["head"].as_str();
                let short_head = head.map(|head| head.chars().take(8).collect::<String>());
                field(
                    &mut columns[1],
                    "HEAD",
                    short_head.as_deref().unwrap_or("Unknown"),
                    None,
                );
            });
            field(ui, "TREE", &tree, facts["scope"].as_str());
        }
        Some(Some(Err(error))) => {
            ui.weak("Repository unavailable").on_hover_text(error);
        }
        Some(None) => {
            ui.weak("Probing repository…");
        }
        None => {
            ui.weak("Repository unknown");
        }
    }
}

fn field(ui: &mut egui::Ui, title: &str, detail: &str, source: Option<&str>) {
    ui.label(RichText::new(title).monospace().size(10.5).weak());
    ui.add(egui::Label::new(detail).truncate())
        .on_hover_text(detail);
    if let Some(source) = source {
        ui.add(egui::Label::new(RichText::new(source).monospace().size(10.5).weak()).truncate())
            .on_hover_text(source);
    }
}

pub fn inspector(ui: &mut egui::Ui, data: &Data, key: &ThreadKey, actions: &mut Vec<Action>) {
    let thread = crate::find_thread(data, key);
    let resident = data.agents.resident(key);
    let Some(title) = thread
        .map(|thread| thread.title.as_str())
        .or_else(|| resident.and_then(|entry| entry["name"].as_str()))
    else {
        return;
    };
    let runs = thread
        .map(|thread| thread.runs.as_slice())
        .or_else(|| data.agents.history(key))
        .unwrap_or_default();
    let run = runs.last();
    let dest = conversation::destination(data, &key.peer);
    let latest = latest_visible(data, key, runs);
    let latest = latest.as_ref();
    let tag = run
        .and_then(|run| run["record"]["spec"]["id"].as_str())
        .map(|id| run_key(&key.peer, id));
    let transcript = tag.as_ref().and_then(|tag| data.transcripts.get(tag));
    let backend = run
        .and_then(|run| run["record"]["spec"]["backend"].as_str())
        .or_else(|| resident.and_then(|entry| entry["backend"].as_str()));
    let model = transcript
        .and_then(|transcript| transcript.backend_model.as_deref())
        .or_else(|| run.and_then(|run| run["record"]["spec"]["model"].as_str()))
        .filter(|value| !value.is_empty());
    let short_id: String = key.id.chars().take(8).collect();

    ui.separator();
    ui.strong(title);
    ui.label(
        RichText::new(format!("{short_id} · {}", dest.label))
            .monospace()
            .size(10.5)
            .weak(),
    );
    let (state, color) = if data.errors.contains_key(&key.peer) {
        ("machine unreachable", crate::console_style::ERROR)
    } else {
        match run.and_then(|run| run["status"].as_str()) {
            Some("waiting" | "blocked") => ("awaiting input", crate::console_style::WARN),
            Some("starting" | "running") => ("running", crate::console_style::ACCENT),
            Some("failed") => ("failed", crate::console_style::ERROR),
            Some("interrupted") => ("interrupted", crate::console_style::ERROR),
            Some("completed") => ("done", crate::console_style::OK),
            Some(other) => (other, ui.visuals().weak_text_color()),
            None => ("status unknown", ui.visuals().weak_text_color()),
        }
    };
    let pending = match (run, &tag, transcript) {
        (Some(run), Some(tag), Some(transcript))
            if crate::needs::replay_current(data, tag, run) =>
        {
            transcript
                .rows
                .iter()
                .find(|row| row.request.is_some())
                .zip(run["record"]["spec"]["id"].as_str())
                .map(|(row, id)| format!("{id}:{}", row.id))
        }
        _ => None,
    };
    let (state, color) = if pending.is_some() {
        ("awaiting your answer", crate::console_style::WARN)
    } else {
        (state, color)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(state).strong().color(color));
        if let Some(event) = latest {
            ui.weak(pulse::age(event.at))
                .on_hover_text(format!("Latest observed event · {}", event.label));
        }
    });
    ui.columns(2, |columns| {
        field(
            &mut columns[0],
            "RUNTIME",
            backend.unwrap_or("Unknown"),
            backend.map(|_| {
                if run.is_some() {
                    "run record"
                } else {
                    "resident record"
                }
            }),
        );
        let observed = transcript
            .and_then(|transcript| transcript.backend_model.as_ref())
            .is_some();
        field(
            &mut columns[1],
            "MODEL",
            model.unwrap_or("Unknown"),
            model.map(|_| {
                if observed {
                    "runtime reported"
                } else {
                    "run record"
                }
            }),
        );
    });
    // Context fullness is not shown: compaction keeps it from being the user's concern.
    if let Some(usage) = transcript.and_then(|transcript| transcript.exchange_usage) {
        let summary = usage.label();
        let source = usage.source();
        field(ui, "LAST EXCHANGE", &summary, Some(&source));
    } else {
        field(ui, "LAST EXCHANGE", "Unknown", None);
    }
    if let Some(cwd) = transcript
        .and_then(|transcript| transcript.execution.as_ref())
        .and_then(|execution| execution["cwd"].as_str())
    {
        ui.label(RichText::new("DIRECTORY").monospace().size(10.5).weak());
        ui.add(egui::Label::new(cwd).truncate()).on_hover_text(cwd);
    }

    repo_fields(
        ui,
        data,
        key,
        run.and_then(|run| run["record"]["spec"]["id"].as_str()),
        actions,
    );
    let tests = runs
        .iter()
        .rev()
        .filter_map(|run| {
            let id = run["record"]["spec"]["id"].as_str()?;
            data.transcripts
                .get(&run_key(&key.peer, id))?
                .observed_tests
                .as_ref()
        })
        .max_by_key(|tests| tests.at);
    if let Some(tests) = tests {
        let summary = format!("{}/{}", tests.passed, tests.total);
        let source = format!(
            "{} · {} UTC",
            tests.command,
            conversation::utc_minute(tests.at)
        );
        field(ui, "TESTS", &summary, Some(&source));
    } else {
        field(ui, "TESTS", "Unknown", None);
    }
    ui.add_space(6.);
    ui.horizontal_wrapped(|ui| {
        let primary = |label: &str| {
            egui::Button::new(RichText::new(label).color(Color32::WHITE))
                .fill(crate::console_style::ACCENT)
        };
        let open = || {
            if resident.is_some() {
                Action::OpenChat(key.clone(), true)
            } else {
                Action::Open(key.clone(), true)
            }
        };
        if let Some(anchor) = &pending {
            if ui.add(primary("Go to request ↵")).clicked() {
                actions.push(Action::OpenGate(
                    key.clone(),
                    resident.is_some(),
                    anchor.clone(),
                ));
            }
            if ui.button("Open on grid").clicked() {
                actions.push(open());
            }
        } else if ui
            .add(primary("Open ↵"))
            .on_hover_text("Place on the grid")
            .clicked()
        {
            actions.push(open());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_a_row_peeks_and_enter_places_it() {
        let ctx = egui::Context::default();
        let data = Data::default();
        let frame = |events| {
            let mut clicked = None;
            let mut center = egui::Pos2::ZERO;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(285., 85.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = Row {
                        title: "Fixture",
                        state: "idle",
                        color: Color32::GRAY,
                        last: "—",
                        age: "—",
                        selected: false,
                        muted: false,
                        context: None,
                        identity: None,
                    }
                    .show(ui, &data);
                    center = response.rect.center();
                    clicked = opens(ui, &response);
                },
            );
            (center, clicked)
        };
        let (pos, _) = frame(vec![]);
        let pointer = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(pos), pointer(true)]);
        assert_eq!(frame(vec![pointer(false)]).1, Some(false));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert_eq!(frame(vec![enter]).1, Some(true));
        // The AGE column, far from the name's glyphs, is part of the target.
        let edge = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(270., pos.y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(egui::pos2(270., pos.y)), edge(true)]);
        assert_eq!(frame(vec![edge(false)]).1, Some(false));
    }
}
