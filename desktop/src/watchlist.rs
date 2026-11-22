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
        name: cell(name),
        mark: cell(MARK),
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
        egui::Label::new(RichText::new(text).monospace().size(10.).color(color)).truncate(),
    )
}

pub fn header(ui: &mut egui::Ui) {
    let c = columns(ui);
    let weak = ui.visuals().weak_text_color();
    label(ui, c.name, "NAME", weak);
    label(ui, c.state, "STATE", weak).on_hover_text("Current input owner or latest activation");
    label(ui, c.last, "LAST", weak).on_hover_text("Latest event observed by this client");
    label(ui, c.age, "AGE", weak).on_hover_text("Time since the last observed event");
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
}

impl Row<'_> {
    pub fn show(self, ui: &mut egui::Ui, data: &Data) -> egui::Response {
        let c = columns(ui);
        if self.selected {
            ui.painter()
                .rect_filled(c.name.union(c.age), 0., ui.visuals().selection.bg_fill);
        }
        let title = RichText::new(self.title).color(if self.muted {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().text_color()
        });
        let response = cell(
            ui,
            c.name,
            egui::Label::new(title)
                .truncate()
                .halign(egui::Align::Min)
                .sense(egui::Sense::click()),
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, self.title));
        if let Some((key, runs)) = self.context {
            let mut mark =
                ui.new_child(egui::UiBuilder::new().max_rect(c.mark.shrink2(egui::vec2(1., 4.))));
            conversation::context_mark(&mut mark, data, key, runs);
        }
        label(ui, c.state, self.state, self.color).on_hover_text(self.state);
        let weak = ui.visuals().weak_text_color();
        label(ui, c.last, self.last, weak).on_hover_text(self.last);
        label(ui, c.age, self.age, weak);
        response
    }
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

pub fn latest_visible<'a>(data: &'a Data, key: &ThreadKey, runs: &[Value]) -> Option<&'a Visible> {
    runs.iter()
        .filter_map(|run| {
            let id = run["record"]["spec"]["id"].as_str()?;
            data.transcripts
                .get(&run_key(&key.peer, id))?
                .latest_visible
                .as_ref()
                .map(|(_, event)| event)
        })
        .max_by_key(|event| event.at)
}

pub fn event_age(event: Option<&Visible>) -> String {
    event.map_or_else(|| "—".into(), |event| pulse::age(event.at))
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
        ui.label(RichText::new("REPO").monospace().small().weak());
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
    ui.label(RichText::new(title).monospace().small().weak());
    ui.add(egui::Label::new(detail).truncate())
        .on_hover_text(detail);
    if let Some(source) = source {
        ui.add(egui::Label::new(RichText::new(source).monospace().small().weak()).truncate())
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
            .small()
            .weak(),
    );
    let state = if data.errors.contains_key(&key.peer) {
        "Machine unreachable"
    } else {
        run.and_then(|run| run["status"].as_str())
            .unwrap_or("Status unknown")
    };
    ui.horizontal(|ui| {
        ui.label(state);
        if let Some(event) = latest {
            ui.weak(format!("{} · {}", event.label, pulse::age(event.at)));
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
    ui.columns(2, |columns| {
        columns[0].label(RichText::new("CONTEXT").monospace().small().weak());
        columns[0].horizontal_wrapped(|ui| conversation::context_label(ui, data, key, runs));
        if let Some(usage) = transcript.and_then(|transcript| transcript.exchange_usage) {
            let summary = usage.label();
            let source = usage.source();
            field(&mut columns[1], "LAST EXCHANGE", &summary, Some(&source));
        } else {
            field(&mut columns[1], "LAST EXCHANGE", "Unknown", None);
        }
    });
    if let Some(cwd) = transcript
        .and_then(|transcript| transcript.execution.as_ref())
        .and_then(|execution| execution["cwd"].as_str())
    {
        ui.label(RichText::new("DIRECTORY").monospace().small().weak());
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
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Open on grid").clicked() {
            actions.push(if resident.is_some() {
                Action::OpenChat(key.clone(), true)
            } else {
                Action::Open(key.clone(), true)
            });
        }
        if let (Some(run), Some(tag), Some(transcript)) = (run, &tag, transcript)
            && crate::needs::replay_current(data, tag, run)
            && let Some(row) = transcript.rows.iter().find(|row| row.request.is_some())
            && let Some(id) = run["record"]["spec"]["id"].as_str()
            && ui.small_button("Pending request").clicked()
        {
            actions.push(Action::OpenGate(
                key.clone(),
                resident.is_some(),
                format!("{id}:{}", row.id),
            ));
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
    }
}
