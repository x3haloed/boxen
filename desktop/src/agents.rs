//! Read-only resident/status projections of the existing authenticated APIs.
use crate::{
    Action, Data,
    model::{Destination, ThreadKey},
};
use eframe::egui::{self, Color32, RichText};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    Healthy,
    Running,
    Failed,
    Unknown,
}
impl Signal {
    fn color(self) -> Color32 {
        match self {
            Self::Healthy => Color32::from_rgb(102, 192, 137),
            Self::Running => Color32::from_rgb(105, 176, 236),
            Self::Failed => Color32::from_rgb(235, 112, 110),
            Self::Unknown => Color32::GRAY,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Healthy => "Available",
            Self::Running => "Running",
            Self::Failed => "Failed",
            Self::Unknown => "Status unavailable",
        }
    }
}
#[derive(Clone)]
struct Activity {
    id: String,
    status: String,
    at: u64,
}
#[derive(Default)]
struct Peer {
    residents: Vec<Value>,
    definitions: HashMap<String, String>,
    incidents: Vec<Value>,
    latest: HashMap<String, Activity>,
    histories: HashMap<String, Vec<Value>>,
    metadata_error: Option<String>,
    runs_error: Option<String>,
    loaded: bool,
}
#[derive(Default)]
pub struct State {
    peers: HashMap<String, Peer>,
}
impl State {
    pub fn resident(&self, key: &ThreadKey) -> Option<&Value> {
        self.peers
            .get(&key.peer)?
            .residents
            .iter()
            .find(|r| r["id"] == key.id)
    }
    pub fn history(&self, key: &ThreadKey) -> Option<&[Value]> {
        let p = self.peers.get(&key.peer)?;
        let thread = p
            .residents
            .iter()
            .find(|r| r["id"] == key.id)
            .and_then(|r| r["thread_id"].as_str())
            .unwrap_or(&key.id);
        if !p.loaded {
            return None;
        }
        Some(
            p.histories
                .get(thread)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
    }
    pub fn error(&self, key: &ThreadKey) -> Option<&str> {
        let p = self.peers.get(&key.peer)?;
        p.runs_error.as_deref().or(p.metadata_error.as_deref())
    }

    pub fn metadata(
        &mut self,
        peer: &str,
        residents: &Result<Vec<Value>, String>,
        definitions: Result<Value, String>,
        health: Result<Value, String>,
    ) {
        let p = self.peers.entry(peer.into()).or_default();
        match residents {
            Ok(rows) => {
                p.residents = rows.clone();
                p.metadata_error = None;
            }
            Err(e) => p.metadata_error = Some(e.clone()),
        }
        if let Ok(v) = definitions {
            p.definitions = v["definitions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some((r["id"].as_str()?.into(), r["name"].as_str()?.into())))
                .collect();
        }
        // Optional enrichment: older daemons may not expose inference health.
        p.incidents = health
            .ok()
            .and_then(|v| v["incidents"].as_array().cloned())
            .unwrap_or_default();
    }
    pub fn activity(&mut self, peer: &str, result: &Result<Vec<Value>, String>) {
        let p = self.peers.entry(peer.into()).or_default();
        let runs = match result {
            Ok(v) => v,
            Err(e) => {
                p.runs_error = Some(e.clone());
                return;
            }
        };
        p.loaded = true;
        p.runs_error = None;
        p.latest.clear();
        p.histories.clear();
        let identities: std::collections::HashSet<&str> = runs
            .iter()
            .filter(|r| {
                !r["record"]["resident"].is_null() || r["record"]["spec"]["resident_id"].is_string()
            })
            .filter_map(|r| r["record"]["spec"]["thread_id"].as_str())
            .chain(
                p.residents
                    .iter()
                    .filter_map(|r| r["thread_id"].as_str().or_else(|| r["id"].as_str())),
            )
            .collect();
        for r in runs {
            if let Some(thread) = r["record"]["spec"]["thread_id"]
                .as_str()
                .filter(|t| identities.contains(t))
            {
                // Catalog metadata only; original input and output arrive through retained-event replay.
                let compact = serde_json::json!({"record":{"spec":{"id":r["record"]["spec"]["id"],"thread_id":thread,"backend":r["record"]["spec"]["backend"]},"created_at":r["record"]["created_at"],"resident_activation":!r["record"]["resident"].is_null() || r["record"]["spec"]["resident_id"].is_string(),"mailbox":{"reason":r["record"]["mailbox"]["reason"]},"transcript_import":r["record"]["transcript_import"],"imported":r["record"]["imported"]},"status":r["status"],"last_seq":r["last_seq"]});
                p.histories.entry(thread.into()).or_default().push(compact);
            }
        }
        for history in p.histories.values_mut() {
            history.sort_by_key(|r| {
                (
                    r["record"]["created_at"].as_u64().unwrap_or(0),
                    r["record"]["spec"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            });
        }

        for r in runs {
            if !r["record"]["transcript_import"].is_null() || !r["record"]["imported"].is_null() {
                continue;
            }
            let Some(id) = r["record"]["spec"]["resident_id"]
                .as_str()
                .or_else(|| r["record"]["resident"]["id"].as_str())
            else {
                continue;
            };
            let a = Activity {
                id: r["record"]["spec"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                status: r["status"].as_str().unwrap_or("unknown").into(),
                at: r["record"]["created_at"].as_u64().unwrap_or(0),
            };
            let replace = p
                .latest
                .get(id)
                .is_none_or(|old| (a.at, &a.id) > (old.at, &old.id));
            if replace {
                p.latest.insert(id.into(), a);
            }
        }
    }
}
fn status(p: &Peer, id: &str) -> (Signal, String, String) {
    if let Some(error) = p.runs_error.as_ref().or(p.metadata_error.as_ref()) {
        return (Signal::Unknown, "Machine unavailable".into(), error.clone());
    }
    if !p.loaded {
        return (Signal::Unknown, "Checking status…".into(), String::new());
    }
    let Some(a) = p.latest.get(id) else {
        return (
            Signal::Healthy,
            "Ready · no activations yet".into(),
            "No accepted activation has been observed.".into(),
        );
    };
    let (signal, text) = match a.status.as_str() {
        "completed" => (Signal::Healthy, "Idle"),
        "cancelled" => (Signal::Healthy, "Idle · cancelled"),
        "failed" => (Signal::Failed, "Failed"),
        "interrupted" => (Signal::Failed, "Interrupted"),
        "starting" => (Signal::Running, "Starting…"),
        "running" => (Signal::Running, "Running"),
        "cancelling" => (Signal::Running, "Cancelling…"),
        _ => (Signal::Unknown, "Status unavailable"),
    };
    let incident = p
        .incidents
        .iter()
        .find(|v| v["last_run_id"] == a.id && v["active"] == true);
    let text = if signal == Signal::Failed {
        incident
            .and_then(|i| i["http_status"].as_u64())
            .map(|code| format!("Interrupted · HTTP {code}"))
            .unwrap_or(text.into())
    } else {
        text.into()
    };
    let detail = format!(
        "Run: {}\nLast activation: {}\n{}",
        a.id,
        a.status,
        incident
            .and_then(|i| i["description"].as_str())
            .unwrap_or(text.as_str())
    );
    (signal, text, detail)
}
struct Group<'a> {
    name: String,
    definition: Option<String>,
    residents: Vec<&'a Value>,
}
fn groups(p: &Peer) -> Vec<Group<'_>> {
    let mut classes: BTreeMap<String, Group<'_>> = BTreeMap::new();
    let mut out = vec![];
    for r in &p.residents {
        if let Some(id) = r["definition"]["id"].as_str() {
            classes
                .entry(id.into())
                .or_insert_with(|| Group {
                    name: p.definitions.get(id).cloned().unwrap_or_else(|| {
                        format!("Class {}", id.chars().take(8).collect::<String>())
                    }),
                    definition: Some(id.into()),
                    residents: vec![],
                })
                .residents
                .push(r);
        } else {
            out.push(Group {
                name: r["name"].as_str().unwrap_or("Resident").into(),
                definition: None,
                residents: vec![r],
            });
        }
    }
    out.extend(classes.into_values());
    for g in &mut out {
        g.residents.sort_by_key(|r| {
            (
                r["name"].as_str().unwrap_or_default(),
                r["id"].as_str().unwrap_or_default(),
            )
        });
    }
    out.sort_by_key(|g| (g.name.to_lowercase(), g.definition.clone()));
    out
}
fn aggregate(signals: &[Signal]) -> Signal {
    if signals.contains(&Signal::Failed) {
        Signal::Failed
    } else if signals.contains(&Signal::Unknown) {
        Signal::Unknown
    } else if signals.contains(&Signal::Running) {
        Signal::Running
    } else {
        Signal::Healthy
    }
}
fn row(
    ui: &mut egui::Ui,
    name: &str,
    line: &str,
    signal: Signal,
    detail: &str,
    interactive: bool,
) -> (bool, bool) {
    let mut clicked = false;
    let mut double_clicked = false;
    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(10., 10.), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 3.5, signal.color());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, signal.label())
        });
        response.on_hover_text(signal.label());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.;
            ui.set_max_width((ui.available_width() - 65.).max(1.));
            let sense = if interactive {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let response = ui.add(egui::Label::new(name).truncate().sense(sense));
            if interactive {
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name)
                });
            }
            clicked |= response.clicked();
            double_clicked |= response.double_clicked();
            response.on_hover_text(detail);
            let response = ui.add(
                egui::Label::new(RichText::new(line).small().weak())
                    .truncate()
                    .sense(sense),
            );
            clicked |= response.clicked();
            double_clicked |= response.double_clicked();
            response.on_hover_text(detail);
        });
    });
    (clicked, double_clicked)
}
pub fn panel(ui: &mut egui::Ui, data: &Data, actions: &mut Vec<Action>) {
    egui::ScrollArea::vertical()
        .id_salt("agents-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut any = false;
            for destination in &data.saved.destinations {
                let Some(p) = data.agents.peers.get(&destination.key) else {
                    ui.weak(format!("{} · connecting…", destination.label));
                    continue;
                };
                let list = groups(p);
                if list.is_empty() {
                    if let Some(e) = p.metadata_error.as_ref().or(p.runs_error.as_ref()) {
                        ui.weak(format!("{} · unavailable", destination.label)).on_hover_text(e);
                    }
                    continue;
                }
                any = true;
                for g in list {
                    if let Some(definition) = g.definition {
                        let signals: Vec<_> = g.residents.iter()
                            .map(|r| status(p, r["id"].as_str().unwrap_or_default()).0).collect();
                        let running = signals.iter().filter(|s| **s == Signal::Running).count();
                        let failed = signals.iter().filter(|s| **s == Signal::Failed).count();
                        let unknown = signals.iter().filter(|s| **s == Signal::Unknown).count();
                        let line = if unknown == signals.len() {
                            format!("{} instances · unavailable", signals.len())
                        } else {
                            let mut text = format!("{} instances · {running} running · {failed} failed", signals.len());
                            if unknown > 0 {text.push_str(&format!(" · {unknown} unavailable"));}
                            text
                        };
                        let mut clicked = false;
                        let mut header = egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(), egui::Id::new(("agent-class", &destination.key, &definition)), false,
                        ).show_header(ui, |ui| {
                            let detail = format!("{}\nClass: {definition}\n{unknown} instances with unavailable status", destination.label);
                            clicked = row(ui, &g.name, &line, aggregate(&signals), &detail,true).0;
                        });
                        if clicked {header.toggle();}
                        header.body(|ui| {
                            for r in g.residents {resident_row(ui, p, r, destination,actions);}
                        });
                    } else {
                        resident_row(ui, p, g.residents[0], destination,actions);
                    }
                }
            }
            if !any {ui.weak("No residents discovered yet.");}
        });
}
fn resident_row(
    ui: &mut egui::Ui,
    p: &Peer,
    r: &Value,
    dest: &Destination,
    actions: &mut Vec<Action>,
) {
    let id = r["id"].as_str().unwrap_or_default();
    let (signal, line, detail) = status(p, id);
    let detail = format!(
        "{}\nResident: {id}\n{} · {}\n{detail}",
        dest.label,
        r["backend"].as_str().unwrap_or_default(),
        r["model"].as_str().unwrap_or_default()
    );
    let key = ThreadKey {
        peer: dest.key.clone(),
        id: id.into(),
    };
    ui.horizontal(|ui| {
        let (clicked, pin) = row(
            ui,
            r["name"].as_str().unwrap_or("Resident"),
            &format!("{line} · {}", dest.label),
            signal,
            &detail,
            true,
        );
        if clicked || pin {
            actions.push(Action::OpenChat(key.clone(), pin));
        }
        let hover_id = ui.id().with(("activity-hover", id));
        let was_hovered = ui
            .ctx()
            .data_mut(|d| d.get_temp::<bool>(hover_id).unwrap_or(false));
        let button = ui.add_visible(was_hovered, egui::Button::new("Activity").small());
        if button.clicked() {
            actions.push(Action::OpenActivity(key.clone(), button.double_clicked()));
        }
        let hovered = ui.rect_contains_pointer(ui.min_rect());
        ui.ctx().data_mut(|d| d.insert_temp(hover_id, hovered));
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn run(id: &str, resident: &str, at: u64, status: &str) -> Value {
        json!({"record":{"spec":{"id":id,"resident_id":resident},"created_at":at},"status":status})
    }
    #[test]
    fn states_recover_and_disconnections_never_show_healthy() {
        let mut s = State::default();
        s.activity("p", &Ok(vec![run("a", "r", 1, "interrupted")]));
        assert_eq!(status(&s.peers["p"], "r").0, Signal::Failed);
        s.activity(
            "p",
            &Ok(vec![
                run("b", "r", 2, "running"),
                run("a", "r", 1, "interrupted"),
            ]),
        );
        assert_eq!(status(&s.peers["p"], "r").0, Signal::Running);
        s.activity("p", &Err("offline".into()));
        assert_eq!(status(&s.peers["p"], "r").0, Signal::Unknown);
        s.activity("p", &Ok(vec![run("c", "r", 3, "completed")]));
        assert_eq!(status(&s.peers["p"], "r").0, Signal::Healthy);
    }
    #[test]
    fn classes_group_by_identity_and_imports_are_not_work() {
        let mut s = State::default();
        s.metadata("p",&Ok(vec![json!({"id":"one","name":"Worker 1","definition":{"id":"class-a"}}),json!({"id":"two","name":"Worker 2","definition":{"id":"class-a"}}),json!({"id":"three","name":"Solo"}),json!({"id":"four","name":"Other","definition":{"id":"class-b"}})]),Ok(json!({"definitions":[{"id":"class-a","name":"Workers"},{"id":"class-b","name":"Workers"}]})),Ok(json!({"incidents":[]})));
        let p = &s.peers["p"];
        let gs = groups(p);
        assert_eq!(gs.len(), 3);
        assert_eq!(gs.iter().filter(|g| g.name == "Workers").count(), 2);
        assert_eq!(
            gs.iter()
                .find(|g| g.definition.as_deref() == Some("class-a"))
                .unwrap()
                .residents
                .len(),
            2
        );
        let mut imported = run("i", "three", 1, "completed");
        imported["record"]["transcript_import"] = json!({});
        s.activity("p", &Ok(vec![imported]));
        assert!(status(&s.peers["p"], "three").1.contains("no activations"));
        assert_eq!(
            aggregate(&[Signal::Failed, Signal::Running]),
            Signal::Failed
        );
    }
    #[test]
    fn resident_click_opens_preview_and_double_click_pins() {
        use egui::{Event, Modifiers, PointerButton, pos2, vec2};
        let mut data = Data::default();
        data.saved.destinations.push(Destination {
            key: "p".into(),
            label: "Host".into(),
            address: "127.0.0.1:1".into(),
        });
        data.agents.metadata(
            "p",
            &Ok(vec![json!({"id":"resident","name":"A resident"})]),
            Ok(json!({"definitions":[]})),
            Ok(json!({"incidents":[]})),
        );
        let ctx = egui::Context::default();
        let mut time = 0.;
        let mut frame = |events| {
            time += 1. / 60.;
            let mut actions = vec![];
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(pos2(0., 0.), vec2(400., 500.))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ui| panel(ui, &data, &mut actions),
            );
            (out, actions)
        };
        let (out, _) = frame(vec![]);
        let pos = out
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::epaint::Shape::Text(t) = &s.shape {
                    (t.galley.job.text == "A resident").then_some(t.pos + t.galley.size() / 2.)
                } else {
                    None
                }
            })
            .unwrap();
        for pinned in [false, true] {
            frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ]);
            let (_, actions) = frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            assert!(
                matches!(&actions[..],[Action::OpenChat(k,p)] if k.peer=="p" && k.id=="resident" && *p==pinned)
            );
        }
        let (out, _) = frame(vec![Event::PointerMoved(pos)]);
        let activity = out
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::epaint::Shape::Text(t) = &s.shape {
                    (t.galley.job.text == "Activity").then_some(t.pos + t.galley.size() / 2.)
                } else {
                    None
                }
            })
            .expect("Hover reveals Activity");
        frame(vec![
            Event::PointerMoved(activity),
            Event::PointerButton {
                pos: activity,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        let (_, actions) = frame(vec![Event::PointerButton {
            pos: activity,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert!(matches!(&actions[..], [Action::OpenActivity(k, _)] if k.id == "resident"));
    }
    #[test]
    fn class_name_click_expands_instances() {
        use egui::{Event, Modifiers, PointerButton, pos2, vec2};
        let mut data = Data::default();
        data.saved.destinations.push(Destination {
            key: "p".into(),
            label: "Host".into(),
            address: "127.0.0.1:1".into(),
        });
        data.agents.metadata(
            "p",
            &Ok(vec![
                json!({"id":"r","name":"An instance","definition":{"id":"class"}}),
            ]),
            Ok(json!({"definitions":[{"id":"class","name":"A class"}]})),
            Ok(json!({"incidents":[]})),
        );
        data.agents.activity("p", &Ok(vec![]));
        let ctx = egui::Context::default();
        let mut time = 0.;
        let mut frame = |events| {
            time += 1. / 60.;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(pos2(0., 0.), vec2(400., 500.))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ui| panel(ui, &data, &mut vec![]),
            )
        };
        let out = frame(vec![]);
        let pos = out
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::epaint::Shape::Text(t) = &s.shape {
                    (t.galley.job.text == "A class").then_some(t.pos + t.galley.size() / 2.)
                } else {
                    None
                }
            })
            .unwrap();
        frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        frame(vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        for _ in 0..15 {
            frame(vec![]);
        }
        let out = frame(vec![]);
        assert!(out.shapes.iter().any(
            |s| matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text=="An instance")
        ));
    }
    #[test]
    #[ignore = "scripts/desktop-smoke.py supplies private paired daemons"]
    fn real_daemon_boundary_agents() {
        use crate::client::{Bus, Client, Update};
        use crate::correspondence::MailUpdate;
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::Duration,
        };
        let home = std::path::PathBuf::from(std::env::var_os("BOXEN_GUI_TEST_HOME").unwrap());
        let client = Client::new(std::env::var("BOXEN_GUI_TEST_URL").unwrap(), &home).unwrap();
        let dest: Destination =
            serde_json::from_str(&std::env::var("BOXEN_GUI_TEST_DEST").unwrap()).unwrap();
        let definition = std::env::var("BOXEN_GUI_TEST_CLASS").unwrap();
        let active = std::env::var("BOXEN_GUI_TEST_ACTIVE_AGENT").unwrap();
        let key = client.status().unwrap()["identity"]
            .as_str()
            .unwrap()
            .to_owned();
        let bus = Bus::new(egui::Context::default());
        let stop = Arc::new(AtomicBool::new(false));
        client.mail_poll(
            dest.clone(),
            key,
            bus.publisher(),
            stop.clone(),
            Default::default(),
        );
        let mut state = State::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            assert!(std::time::Instant::now() < deadline);
            if let Update::Mail(MailUpdate::Page {
                residents,
                definitions,
                health,
                ..
            }) = bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            {
                state.metadata(&dest.key, &residents, definitions, health);
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        let rows = client.operation(&dest, json!({"op":"list"})).unwrap();
        state.activity(&dest.key, &Ok(rows.as_array().unwrap().clone()));
        let peer = &state.peers[&dest.key];
        let grouped = groups(peer);
        let class = grouped
            .iter()
            .find(|g| g.definition.as_deref() == Some(definition.as_str()))
            .unwrap();
        assert_eq!(class.name, "Fixture workers");
        assert_eq!(class.residents.len(), 3);
        assert!(
            class
                .residents
                .iter()
                .any(|r| status(peer, r["id"].as_str().unwrap()).0 == Signal::Failed)
        );
        assert!(class.residents.iter().any(|r| {
            status(peer, r["id"].as_str().unwrap())
                .1
                .contains("no activations")
        }));
        let accepted=client.operation(&dest,json!({"op":"resident_activate","id":active,"activation_id":crate::new_id(),"directions":"SLOW"})).unwrap();
        let run_id = accepted["record"]["spec"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let rows = client.operation(&dest, json!({"op":"list"})).unwrap();
        state.activity(&dest.key, &Ok(rows.as_array().unwrap().clone()));
        assert_eq!(status(&state.peers[&dest.key], &active).0, Signal::Running);
        let history_key = ThreadKey {
            peer: dest.key.clone(),
            id: active.clone(),
        };
        assert_eq!(state.history(&history_key).unwrap().len(), 1);
        let watching = Arc::new(AtomicBool::new(false));
        client.watch(
            dest.clone(),
            run_id.clone(),
            0,
            900,
            bus.publisher(),
            watching.clone(),
        );
        let mut transcript = crate::model::Transcript::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut early = false;
        loop {
            assert!(std::time::Instant::now() < deadline);
            if let Update::Batch(_, 900, batch) =
                bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            {
                for event in batch["events"].as_array().unwrap() {
                    transcript.apply(event).unwrap();
                }
                if batch["done"] != true {
                    early = true;
                }
                if batch["done"] == true {
                    break;
                }
            }
        }
        assert!(early, "Resident activity streams before completion");
        for label in ["Thinking", "Command", "Agent", "Activation input"] {
            assert!(
                transcript.rows.iter().any(|r| r.label == label),
                "Missing {label}"
            );
        }
        let snapshot = client
            .operation(&dest, json!({"op":"get","id":run_id}))
            .unwrap();
        assert_eq!(transcript.seq, snapshot["last_seq"].as_u64().unwrap());
        let inbox = client
            .operation(
                &dest,
                json!({"op":"client_inbox","client_id":client.status().unwrap()["identity"]}),
            )
            .unwrap();
        assert!(
            !inbox["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["sender"]["id"] == active),
            "Observation delivered private narration"
        );
        let before = transcript.rows.len();
        client.watch(
            dest.clone(),
            run_id,
            transcript.seq,
            901,
            bus.publisher(),
            Arc::new(AtomicBool::new(false)),
        );
        loop {
            if let Update::Batch(_, 901, batch) =
                bus.rx.recv_timeout(Duration::from_secs(12)).unwrap()
            {
                for event in batch["events"].as_array().unwrap() {
                    transcript.apply(event).unwrap();
                }
                if batch["done"] == true {
                    break;
                }
            }
        }
        assert_eq!(
            transcript.rows.len(),
            before,
            "Cursor replay duplicated activity"
        );
        state.activity(&dest.key, &Err("Disconnected fixture".into()));
        assert_eq!(status(&state.peers[&dest.key], &active).0, Signal::Unknown);
        println!(
            "Agents: real remote discovery, class metadata/instances, idle, failed credential activation, running and disconnected projections passed"
        );
    }
}
