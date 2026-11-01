use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadKey {
    pub peer: String,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pane {
    Threads,
    Welcome,
    Thread(ThreadKey),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    pub key: String,
    pub label: String,
    pub address: String,
}
#[derive(Clone, Debug)]
pub struct Conversation {
    pub key: ThreadKey,
    pub title: String,
    pub runs: Vec<Value>,
}
pub fn catalog(peer: &str, runs: &[Value]) -> Vec<Conversation> {
    // Any identity attachment excludes the whole conversation, including older runs.
    let identities: HashSet<&str> = runs
        .iter()
        .filter(|r| {
            !r["record"]["resident"].is_null() || !r["record"]["spec"]["resident_id"].is_null()
        })
        .filter_map(|r| r["record"]["spec"]["thread_id"].as_str())
        .collect();
    let mut out: Vec<Conversation> = vec![];
    for run in runs {
        let Some(id) = run["record"]["spec"]["thread_id"].as_str() else {
            continue;
        };
        if identities.contains(id) {
            continue;
        }
        if let Some(thread) = out.iter_mut().find(|t| t.key.id == id) {
            thread.runs.push(run.clone());
        } else {
            let prompt = run["record"]["spec"]["prompt"]
                .as_str()
                .unwrap_or("Untitled thread");
            out.push(Conversation {
                key: ThreadKey {
                    peer: peer.into(),
                    id: id.into(),
                },
                title: prompt
                    .lines()
                    .next()
                    .unwrap_or("Untitled thread")
                    .chars()
                    .take(72)
                    .collect(),
                runs: vec![run.clone()],
            });
        }
    }
    out.reverse();
    out
}
pub fn terminal(snapshot: &Value) -> bool {
    matches!(
        snapshot["status"].as_str(),
        Some("completed" | "failed" | "cancelled" | "interrupted")
    )
}
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub label: String,
    pub text: String,
}
#[derive(Default)]
pub struct Transcript {
    pub seq: u64,
    pub rows: Vec<Row>,
    positions: BTreeMap<String, usize>,
}
impl Transcript {
    fn put(&mut self, id: String, label: &str, text: String, append: bool) {
        let index = *self.positions.entry(id.clone()).or_insert_with(|| {
            self.rows.push(Row {
                id,
                label: label.into(),
                text: String::new(),
            });
            self.rows.len() - 1
        });
        let row = &mut self.rows[index];
        row.label = label.into();
        if append {
            row.text.push_str(&text)
        } else {
            row.text = text
        }
    }
    pub fn apply(&mut self, event: &Value) -> Result<(), String> {
        let seq = event["seq"].as_u64().ok_or("Event has no sequence")?;
        if seq <= self.seq {
            return Ok(());
        }
        if seq != self.seq + 1 {
            return Err(format!(
                "History gap: expected {}, received {seq}",
                self.seq + 1
            ));
        }
        let data = &event["data"];
        let kind = event["kind"].as_str().unwrap_or("");
        let text = |key: &str| data[key].as_str().unwrap_or("").to_owned();
        match kind {
            "user_message" => self.put(format!("event-{seq}"), "You", text("text"), false),
            "assistant_delta" | "reasoning_delta" | "assistant_message" => self.put(
                text("id"),
                if kind == "reasoning_delta" {
                    "Thinking"
                } else {
                    "Agent"
                },
                text("text"),
                kind != "assistant_message",
            ),
            "tool_started" => self.put(
                format!("event-{seq}"),
                "Tool",
                format!("{}\n{}", text("tool"), display(&data["arguments"])),
                false,
            ),
            "tool_output" => self.put(
                format!("tool-{}", text("call_id")),
                "Tool output",
                text("text"),
                true,
            ),
            "tool_result" => self.put(
                format!("event-{seq}"),
                "Tool result",
                display(&data["result"]),
                false,
            ),
            "control" if data["type"] == "steer" => self.put(
                format!("event-{seq}"),
                "You · steering",
                text("text"),
                false,
            ),
            "backend" => {
                let method = data["method"].as_str().unwrap_or("");
                let p = &data["params"];
                if method == "item/started" || method == "item/completed" {
                    let item = &p["item"];
                    let ty = item["type"].as_str().unwrap_or("Activity");
                    if ty != "userMessage" {
                        let label = match ty {
                            "agentMessage" => "Agent",
                            "reasoning" => "Thinking",
                            "commandExecution" => "Command",
                            _ => ty,
                        };
                        let body = match ty {
                            "agentMessage" => item["text"].as_str().unwrap_or("").into(),
                            "reasoning" => item["summary"]
                                .as_array()
                                .map(|a| a.iter().map(display).collect::<Vec<_>>().join("\n"))
                                .unwrap_or_default(),
                            "commandExecution" => [
                                item["command"].as_str().unwrap_or(""),
                                item["aggregatedOutput"].as_str().unwrap_or(""),
                            ]
                            .join("\n"),
                            _ => display(item),
                        };
                        if !body.is_empty() || method == "item/completed" {
                            self.put(item["id"].as_str().unwrap_or(ty).into(), label, body, false)
                        }
                    }
                } else if method.ends_with("Delta") || method.ends_with("/delta") {
                    let label = if method.contains("reasoning") {
                        "Thinking"
                    } else if method.contains("agentMessage") {
                        "Agent"
                    } else {
                        "Command"
                    };
                    self.put(
                        p["itemId"].as_str().unwrap_or(method).into(),
                        label,
                        p["delta"].as_str().unwrap_or("").into(),
                        true,
                    );
                } else if !data["id"].is_null() {
                    self.put(
                        format!("event-{seq}"),
                        "Backend input requested",
                        display(data),
                        false,
                    );
                } else if matches!(method, "turn/plan/updated" | "turn/diff/updated" | "error") {
                    self.put(format!("event-{seq}"), method, display(p), false);
                }
            }
            "status" => self.put(
                format!("event-{seq}"),
                "Status",
                if text("detail").is_empty() || text("detail") == "null" {
                    text("status")
                } else {
                    format!("{} · {}", text("status"), text("detail"))
                },
                false,
            ),
            "control_error"
            | "tool_outcome_unknown"
            | "recovery_notice"
            | "backend_session_rebuilt"
            | "context_summary_failed"
            | "observation_incident" => {
                self.put(format!("event-{seq}"), "Incident", display(data), false)
            }
            _ => {}
        }
        self.seq = seq;
        Ok(())
    }
}
pub fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn identity_excludes_whole_conversation() {
        let runs = vec![
            json!({"record":{"spec":{"thread_id":"a","prompt":"one"}}}),
            json!({"record":{"spec":{"thread_id":"b","prompt":"two"}}}),
            json!({"record":{"spec":{"thread_id":"a"},"resident":{"id":"resident"}}}),
        ];
        let c = catalog("peer", &runs);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].key.id, "b");
    }
    #[test]
    fn replay_deduplicates_and_completion_replaces_deltas() {
        let mut t = Transcript::default();
        let e = json!({"seq":1,"kind":"assistant_delta","data":{"id":"m","text":"part"}});
        t.apply(&e).unwrap();
        t.apply(&e).unwrap();
        t.apply(&json!({"seq":2,"kind":"assistant_message","data":{"id":"m","text":"whole"}}))
            .unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].text, "whole");
        assert!(t.apply(&json!({"seq":4})).is_err());
        assert_eq!(t.seq, 2);
    }
}
