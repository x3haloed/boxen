//! Bounded arrival-order projection of the journal observers.
use crate::{Action, Data, conversation, model::ThreadKey};
use eframe::egui::{self, RichText};
use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 160;

#[derive(Clone, Debug)]
pub struct Visible {
    pub at: u64,
    pub label: &'static str,
}

pub fn visible(event: &Value) -> Option<Visible> {
    let data = &event["data"];
    let label = match event["kind"].as_str()? {
        "user_message" => "message",
        "assistant_message" => "reply",
        "input_requested" => "input",
        "request_answered" => "answered",
        "activation_input" => "activation",
        "tool_started" => "tool",
        "tool_result" => "result",
        "tool_outcome_unknown" | "control_error" | "observation_incident" => "incident",
        "backend_compaction" => "compaction",
        "context_shortage" => "context shortage",
        "context_boundary"
            if data["masked"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
                || data["source_count"]
                    .as_u64()
                    .zip(data["selected"].as_array())
                    .is_some_and(|(count, selected)| count > selected.len() as u64) =>
        {
            "context selection"
        }
        "status" => match data["status"].as_str()? {
            "completed" => "completed",
            "interrupted" => "interrupted",
            "cancelled" => "cancelled",
            "failed" => "failed",
            _ => return None,
        },
        "backend" => match data["method"].as_str()? {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                "approval"
            }
            "item/tool/requestUserInput" | "mcpServer/elicitation/request" => "input",
            "serverRequest/resolved" => "resolved",
            "item/started" if data["params"]["item"]["type"] == "commandExecution" => "command",
            "item/completed" => match data["params"]["item"]["type"].as_str()? {
                "commandExecution"
                    if data["params"]["item"]["exitCode"]
                        .as_i64()
                        .is_some_and(|n| n != 0) =>
                {
                    "command failed"
                }
                "commandExecution" => "command done",
                "agentMessage" => "reply",
                "contextCompaction" => "compaction",
                "fileChange" => "file change",
                "reasoning" => "thinking",
                _ => "activity",
            },
            _ => return None,
        },
        _ => return None,
    };
    Some(Visible {
        at: event["at"].as_u64()?,
        label,
    })
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn age(at: u64) -> String {
    let now = u128::from(now());
    let at = u128::from(at);
    if at > now + 1000 {
        return "skew".into();
    }
    let seconds = now.saturating_sub(at) / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

enum Entry {
    Event {
        key: ThreadKey,
        event: Visible,
    },
    Gap {
        tag: String,
        key: ThreadKey,
        detected: u64,
        recovered: usize,
        ended: Option<u64>,
    },
}

#[derive(Default)]
pub struct Pulse {
    entries: VecDeque<Entry>,
}

impl Pulse {
    fn append(&mut self, entry: Entry) {
        if self.entries.len() == LIMIT {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn gap(&mut self, tag: &str, key: &ThreadKey) {
        if self.entries.iter().any(|entry| matches!(entry, Entry::Gap { tag: previous, ended:None, .. } if previous == tag)) {
            return;
        }
        self.append(Entry::Gap {
            tag: tag.into(),
            key: key.clone(),
            detected: now(),
            recovered: 0,
            ended: None,
        });
    }

    pub fn live(&mut self, key: &ThreadKey, event: Visible) {
        self.append(Entry::Event {
            key: key.clone(),
            event,
        });
    }

    pub fn replayed(&mut self, tag: &str, count: usize, reached_frontier: bool) {
        if let Some(Entry::Gap {
            recovered, ended, ..
        }) = self.entries.iter_mut().rev().find(
            |entry| matches!(entry, Entry::Gap { tag: prior, ended:None, .. } if prior == tag),
        ) {
            *recovered += count;
            if reached_frontier {
                *ended = Some(now());
            }
        }
    }
}

pub fn panel(ui: &mut egui::Ui, data: &Data, actions: &mut Vec<Action>) {
    let coverage = crate::needs::coverage(data);
    ui.horizontal_wrapped(|ui| {
        for dest in &data.saved.destinations {
            let count = coverage.get(dest.key.as_str());
            let state = if data.errors.contains_key(&dest.key) {
                "down"
            } else if !data.catalogs.contains_key(&dest.key) {
                "connecting"
            } else if count.is_some_and(|count| count.observed < count.total) {
                "gap"
            } else {
                "live"
            };
            let label = count.map_or_else(
                || format!("{} · {state}", dest.label),
                |count| {
                    format!(
                        "{} · {state} {}/{}",
                        dest.label, count.observed, count.total
                    )
                },
            );
            ui.label(RichText::new(label).monospace().size(10.5));
        }
    });
    ui.separator();
    let height = (ui.available_height() - 25.).max(24.);
    egui::ScrollArea::vertical()
        .id_salt("live-pulse")
        .max_height(height)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if data.pulse.entries.is_empty() {
                ui.weak("Waiting for live activity");
            }
            for entry in &data.pulse.entries {
                let key = match entry {
                    Entry::Event { key, .. } | Entry::Gap { key, .. } => key,
                };
                let machine = conversation::destination(data, &key.peer);
                let resident = data.agents.resident(key).is_some();
                let title = crate::find_thread(data, key)
                    .map(|thread| thread.title.as_str())
                    .or_else(|| {
                        data.agents
                            .resident(key)
                            .and_then(|resident| resident["name"].as_str())
                    })
                    .unwrap_or("Conversation");
                ui.horizontal_wrapped(|ui| {
                    match entry {
                        Entry::Event { event, .. } => {
                            conversation::journal_time(ui, Some(event.at));
                            ui.weak(&machine.label);
                            ui.label(RichText::new(event.label).monospace());
                        }
                        Entry::Gap {
                            detected,
                            recovered,
                            ended,
                            ..
                        } => {
                            ui.weak(&machine.label);
                            let from = conversation::utc_minute(*detected);
                            let gap = ended.map_or_else(
                                || format!("Coverage gap since {from}"),
                                |end| {
                                    format!(
                                        "Coverage gap {from}–{} · {recovered} replayed into",
                                        conversation::utc_minute(end),
                                    )
                                },
                            );
                            ui.colored_label(egui::Color32::from_rgb(216, 162, 58), gap)
                                .on_hover_text("Client observation time (UTC)");
                        }
                    }
                    if ui.link(title).clicked() {
                        if resident {
                            actions.push(Action::OpenActivity(key.clone(), false));
                        } else {
                            actions.push(Action::Open(key.clone(), false));
                        }
                    }
                });
            }
        });
    ui.weak("Live only · received order · open a thread to replay it");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replay_updates_the_gap_and_live_rows_stay_in_arrival_order() {
        let mut pulse = Pulse::default();
        assert!(visible(&json!({"at":10,"kind":"context_budget"})).is_none());
        let key = ThreadKey {
            peer: "peer".into(),
            id: "thread".into(),
        };
        pulse.live(
            &key,
            Visible {
                at: 12,
                label: "tool",
            },
        );
        pulse.gap("peer:run", &key);
        pulse.gap("peer:run", &key);
        pulse.replayed("peer:run", 3, false);
        pulse.replayed("peer:run", 2, true);
        pulse.live(
            &key,
            Visible {
                at: 7,
                label: "reply",
            },
        );
        assert_eq!(pulse.entries.len(), 3);
        assert!(matches!(
            pulse.entries[1],
            Entry::Gap {
                recovered: 5,
                ended: Some(_),
                ..
            }
        ));
        assert!(matches!(
            pulse.entries[2],
            Entry::Event {
                event: Visible { at: 7, .. },
                ..
            }
        ));
    }
}
