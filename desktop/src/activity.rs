//! An observer of the existing run journals; opening it never activates a resident.
use crate::{
    Data,
    client::run_key,
    model::{ThreadKey, terminal},
};
use eframe::egui::{self, RichText};
use serde_json::Value;
#[derive(Default)]
pub struct Window {
    pub first: Option<String>,
}
pub fn title(data: &Data, key: &ThreadKey) -> String {
    data.agents
        .resident(key)
        .and_then(|r| r["name"].as_str())
        .unwrap_or("Agent")
        .to_owned()
}
pub fn runs(data: &Data, key: &ThreadKey) -> Vec<Value> {
    let all = data.agents.history(key).unwrap_or_default();
    let start = data
        .activity
        .get(key)
        .and_then(|w| w.first.as_ref())
        .and_then(|id| all.iter().position(|r| r["record"]["spec"]["id"] == *id))
        .unwrap_or_else(|| all.len().saturating_sub(10));
    all[start..].to_vec()
}
pub fn panel(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey) {
    let name = title(data, key);
    let all = data.agents.history(key);
    let loaded = all.is_some();
    let total = all.map_or(0, <[Value]>::len);
    let runs = runs(data, key);
    let machine = data
        .saved
        .destinations
        .iter()
        .find(|d| d.key == key.peer)
        .map(|d| d.label.as_str())
        .unwrap_or("Machine");
    ui.horizontal_wrapped(|ui| {
        ui.heading(format!("{name} · Activity"));
        ui.weak(machine);
    });
    ui.weak("Private execution narration · messages are sent explicitly");
    if let Some(error) = data.agents.error(key) {
        ui.colored_label(
            egui::Color32::LIGHT_RED,
            format!("Observation unavailable · {error}"),
        );
    }
    if !loaded {
        ui.weak("Waiting for the activity catalog…");
        return;
    }
    if total == 0 {
        ui.weak("No activations yet.");
        return;
    }
    let start = total.saturating_sub(runs.len());
    let first = runs
        .first()
        .and_then(|r| r["record"]["spec"]["id"].as_str())
        .map(str::to_owned);
    let previous = all
        .and_then(|rows| rows.get(start.saturating_sub(10)))
        .and_then(|r| r["record"]["spec"]["id"].as_str())
        .map(str::to_owned);
    data.activity.entry(key.clone()).or_insert(Window { first });
    if runs.len() < total {
        ui.horizontal(|ui| {
            if ui.button("Load earlier activity").clicked() {
                let w = data.activity.entry(key.clone()).or_default();
                w.first = previous.clone();
            }
            ui.weak(format!(
                "Showing the latest {} of {total} retained runs",
                runs.len()
            ));
        });
    }
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt(("agent-activity", &key.peer, &key.id))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for run in &runs {
                let id = run["record"]["spec"]["id"].as_str().unwrap_or_default();
                let tag = run_key(&key.peer, id);
                let state = run["status"].as_str().unwrap_or("unknown");
                let imported = !run["record"]["transcript_import"].is_null()
                    || !run["record"]["imported"].is_null();
                let label = if imported {
                    "Imported history"
                } else if run["record"]["resident_activation"] == false {
                    "Earlier thread execution"
                } else {
                    "Activation"
                };
                let reason = run["record"]["mailbox"]["reason"]
                    .as_str()
                    .unwrap_or_default();
                ui.push_id((&key.peer, id), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{label} · {} · {state}{}",
                            id.chars().take(8).collect::<String>(),
                            if reason.is_empty() {
                                String::new()
                            } else {
                                format!(" · {reason}")
                            }
                        ))
                        .strong(),
                    )
                    .on_hover_text(format!(
                        "Run: {id}\nAccepted at Unix milliseconds: {}\nBackend: {}",
                        run["record"]["created_at"],
                        run["record"]["spec"]["backend"]
                            .as_str()
                            .unwrap_or_default()
                    ));
                    if let Some(w) = data.watches.get(&tag)
                        && let Some(error) = &w.error
                    {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("Reconnecting observation · {error}"),
                        );
                    }
                    match data.transcripts.get(&tag) {
                        None => {
                            ui.weak("Loading retained activity…");
                        }
                        Some(t) => {
                            for row in &t.rows {
                                ui.push_id(&row.id, |ui| {
                                    if matches!(
                                        row.label.as_str(),
                                        "Tool"
                                            | "Tool result"
                                            | "Tool output"
                                            | "Command"
                                            | "Thinking"
                                            | "Activation input"
                                            | "Imported history"
                                            | "Context"
                                            | "Schedule"
                                    ) {
                                        egui::CollapsingHeader::new(&row.label)
                                            .default_open(row.label == "Thinking" && !terminal(run))
                                            .show(ui, |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&row.text).monospace(),
                                                    )
                                                    .wrap()
                                                    .selectable(true),
                                                );
                                            });
                                    } else {
                                        ui.label(
                                            RichText::new(if row.label == "Agent" {
                                                "Private narration"
                                            } else {
                                                &row.label
                                            })
                                            .small()
                                            .weak(),
                                        );
                                        ui.add(egui::Label::new(&row.text).wrap().selectable(true));
                                    }
                                    ui.add_space(5.);
                                });
                            }
                            if t.seq < run["last_seq"].as_u64().unwrap_or(0) {
                                ui.weak("Replaying retained activity…");
                            }
                        }
                    }
                    ui.separator();
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn pagination_keeps_earlier_activity_when_new_runs_arrive() {
        let mut data = Data::default();
        let key = ThreadKey {
            peer: "p".into(),
            id: "r".into(),
        };
        let mut records:Vec<_>=(0..20).map(|n|json!({"record":{"spec":{"id":format!("run-{n:02}"),"thread_id":"r","resident_id":"r"},"created_at":n},"status":"completed"})).collect();
        data.agents.activity("p", &Ok(records.clone()));
        assert_eq!(runs(&data, &key).len(), 10);
        data.activity.insert(
            key.clone(),
            Window {
                first: Some("run-10".into()),
            },
        );
        records.push(json!({"record":{"spec":{"id":"run-20","thread_id":"r","resident_id":"r"},"created_at":20},"status":"running"}));
        data.agents.activity("p", &Ok(records));
        assert_eq!(runs(&data, &key).len(), 11);
        data.activity.get_mut(&key).unwrap().first = Some("run-00".into());
        assert_eq!(runs(&data, &key).len(), 21);
    }
}
