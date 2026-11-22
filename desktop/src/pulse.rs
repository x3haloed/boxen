//! Bounded arrival-order projection of the journal observers.
use crate::{Data, conversation, model::ThreadKey};
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
        "context_boundary" => "context",
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

pub fn age(at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
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
        tag: String,
        thread: String,
        event: Visible,
    },
    Gap {
        tag: String,
        recovered: usize,
        closed: bool,
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

    pub fn gap(&mut self, tag: &str) {
        if self.entries.iter().any(|entry| matches!(entry, Entry::Gap { tag: previous, closed:false, .. } if previous == tag)) {
            return;
        }
        self.append(Entry::Gap {
            tag: tag.into(),
            recovered: 0,
            closed: false,
        });
    }

    pub fn live(&mut self, tag: &str, thread: &str, event: Visible) {
        self.append(Entry::Event {
            tag: tag.into(),
            thread: thread.into(),
            event,
        });
    }

    pub fn replayed(&mut self, tag: &str, count: usize, reached_frontier: bool) {
        if let Some(Entry::Gap {
            recovered, closed, ..
        }) = self.entries.iter_mut().rev().find(
            |entry| matches!(entry, Entry::Gap { tag: prior, closed:false, .. } if prior == tag),
        ) {
            *recovered += count;
            *closed = reached_frontier;
        }
    }
}

pub fn panel(ui: &mut egui::Ui, data: &Data) {
    ui.weak("Live only · received order · open a thread to replay it");
    egui::ScrollArea::vertical()
        .id_salt("live-pulse")
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if data.pulse.entries.is_empty() {
                ui.weak("Waiting for live activity");
            }
            for entry in &data.pulse.entries {
                let tag = match entry {
                    Entry::Event { tag, .. } | Entry::Gap { tag, .. } => tag,
                };
                let (peer, run) = tag.split_once(':').unwrap_or(("", tag));
                let machine = conversation::destination(data, peer);
                ui.horizontal_wrapped(|ui| match entry {
                    Entry::Event { thread, event, .. } => {
                        let key = ThreadKey {
                            peer: peer.into(),
                            id: thread.into(),
                        };
                        let name = crate::find_thread(data, &key)
                            .map(|t| t.title.as_str())
                            .unwrap_or_else(|| if run.len() >= 8 { &run[..8] } else { run });
                        ui.weak(age(event.at));
                        ui.label(&machine.label);
                        ui.label(name);
                        ui.label(RichText::new(event.label).monospace());
                    }
                    Entry::Gap {
                        recovered, closed, ..
                    } => {
                        ui.weak(&machine.label);
                        ui.colored_label(
                            egui::Color32::from_rgb(216, 162, 58),
                            if *closed {
                                format!("Coverage gap · {recovered} events replayed into the run")
                            } else {
                                "Coverage gap · waiting for observation".into()
                            },
                        );
                    }
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replay_updates_the_gap_and_live_rows_stay_in_arrival_order() {
        let mut pulse = Pulse::default();
        assert!(visible(&json!({"at":10,"kind":"context_budget"})).is_none());
        pulse.live(
            "peer:run",
            "thread",
            Visible {
                at: 12,
                label: "tool",
            },
        );
        pulse.gap("peer:run");
        pulse.gap("peer:run");
        pulse.replayed("peer:run", 3, false);
        pulse.replayed("peer:run", 2, true);
        pulse.live(
            "peer:run",
            "thread",
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
                closed: true,
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
