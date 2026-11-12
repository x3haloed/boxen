//! An observer of the existing run journals; opening it never activates a resident.
use crate::{
    Data,
    client::run_key,
    conversation,
    model::{ThreadKey, terminal},
};
use eframe::egui;
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
pub fn earlier(ui: &mut egui::Ui, data: &mut Data, key: &ThreadKey) {
    if let Some(error) = data.agents.error(key) {
        ui.label(format!("Observation unavailable · {error}"));
    }
    let Some(all) = data.agents.history(key) else {
        ui.weak("Waiting for the activity catalog…");
        return;
    };
    let runs = runs(data, key);
    let start = all.len().saturating_sub(runs.len());
    let first = runs
        .first()
        .and_then(|r| r["record"]["spec"]["id"].as_str())
        .map(str::to_owned);
    let previous = all
        .get(start.saturating_sub(10))
        .and_then(|r| r["record"]["spec"]["id"].as_str())
        .map(str::to_owned);
    let window = data.activity.entry(key.clone()).or_insert(Window { first });
    if start > 0 && ui.small_button("Load earlier activity").clicked() {
        window.first = previous;
    }
}
pub fn run(ui: &mut egui::Ui, data: &Data, key: &ThreadKey, run: &Value) {
    let id = run["record"]["spec"]["id"].as_str().unwrap_or_default();
    let tag = run_key(&key.peer, id);
    let imported =
        !run["record"]["transcript_import"].is_null() || !run["record"]["imported"].is_null();
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
    let status = run["status"].as_str().unwrap_or("");
    let suffix = if status == "completed" {
        String::new()
    } else {
        format!(" · {status}")
    };
    let title = if reason.is_empty() {
        format!("{label}{suffix}")
    } else {
        format!("{label} · {reason}{suffix}")
    };
    ui.push_id(&tag, |ui| {
        if let Some(error) = data.watches.get(&tag).and_then(|w| w.error.as_ref()) {
            ui.label(format!("Reconnecting observation · {error}"));
        }
        let details = egui::CollapsingHeader::new(title)
            .default_open(!terminal(run) || matches!(status, "failed" | "interrupted"))
            .show(ui, |ui| {
                if let Some(t) = data.transcripts.get(&tag) {
                    for row in &t.rows {
                        conversation::activity_row(ui, row);
                    }
                    if t.seq < run["last_seq"].as_u64().unwrap_or(0) {
                        ui.weak("Replaying retained activity…");
                    }
                } else {
                    ui.weak("Loading retained activity…");
                }
            });
        let collapsed = details.fully_closed();
        details.header_response.on_hover_text(format!("Run {id}"));
        if collapsed
            && let Some(note) = data.transcripts.get(&tag).and_then(|t| {
                t.rows
                    .iter()
                    .rev()
                    .find(|row| row.label == "Agent" && !row.text.is_empty())
            })
        {
            ui.weak("Private execution note");
            let brief: String = note.text.chars().take(240).collect();
            ui.add(
                egui::Label::new(if brief.len() < note.text.len() {
                    format!("{brief}…")
                } else {
                    brief
                })
                .wrap()
                .selectable(true),
            );
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
