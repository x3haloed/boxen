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
    Work,
    Agents,
    Activity(ThreadKey),
    Chat(ThreadKey),
    Threads,
    Correspondence,
    Message(ThreadKey),
    Compose(String),
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
            let prompt = run["title"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    run["record"]["spec"]["prompt"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                })
                .or_else(|| run["record"]["spec"]["attachments"][0]["name"].as_str())
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
    pub attachments: Vec<crate::attachments::FileRef>,
    pub request: Option<Value>,
    pub update_id: Option<String>,
    pub tool: Option<ToolDetail>,
}
#[derive(Clone, Debug, Default)]
pub struct ToolDetail {
    pub output: String,
    pub result: Option<String>,
    pub failed: bool,
}
#[derive(Default)]
pub struct Transcript {
    pub seq: u64,
    pub rows: Vec<Row>,
    pub rejected_updates: HashSet<String>,
    positions: BTreeMap<String, usize>,
    imported: bool,
}
impl Transcript {
    fn put(&mut self, id: String, label: &str, text: String, append: bool) {
        let index = *self.positions.entry(id.clone()).or_insert_with(|| {
            self.rows.push(Row {
                id,
                label: label.into(),
                text: String::new(),
                attachments: vec![],
                request: None,
                update_id: None,
                tool: None,
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
            "activation_input" => self.put(format!("event-{seq}"),"Activation input",text("text"),false),
            "transcript_imported" => {self.imported=true;self.put(format!("event-{seq}"),"Imported history",display(data),false);}
            "inference_message" if self.imported => self.put(format!("event-{seq}"),if data["role"]=="user" {"Imported user"} else {"Imported assistant"},display(&data["content"]),false),
            "scheduled_event" => self.put(format!("event-{seq}"),"Schedule",display(data),false),
            "context_boundary" => self.put(format!("event-{seq}"),"Context",format!("Policy: {} · {} of {} source entries selected · {} observation bodies masked. Original history remains retained.",data["settings"]["mode"].as_str().unwrap_or("unknown"),data["selected"].as_array().map_or(0,Vec::len),data["source_count"],data["masked"].as_array().map_or(0,Vec::len)),false),
            "inference_frame" if data["event"]["type"]=="response_headers" && data["event"]["status"].as_u64().is_some_and(|s|s>=400) => self.put(format!("event-{seq}"),"Incident",format!("Inference provider response: HTTP {}\nGeneration: {}",data["event"]["status"],data["event"]["generation_id"].as_str().unwrap_or("not supplied")),false),
            "user_message" => {
                let id = format!("event-{seq}");
                self.put(id.clone(), "You", text("text"), false);
                let row = &mut self.rows[self.positions[&id]];
                row.attachments = crate::attachments::FileRef::parse(&data["attachments"]);
                row.update_id = data["update_id"].as_str().map(str::to_owned);
            },
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
            "tool_started" | "tool_output" | "tool_result" | "resident_tool_result" => {
                let call = text("call_id");
                let id = if call.is_empty() {format!("event-{seq}")} else {format!("tool-{call}")};
                if kind == "tool_started" || !self.positions.contains_key(&id) {
                    let name = text("tool");
                    self.put(id.clone(),"Tool",format!("{name}\n{}",display(&data["arguments"])),false);
                }
                let details = self.rows[self.positions[&id]].tool.get_or_insert_with(ToolDetail::default);
                if kind == "tool_output" {
                    details.output.push_str(&text("text"));
                } else if matches!(kind,"tool_result" | "resident_tool_result") {
                    details.result = Some(display(&data["result"]));
                    details.failed = data["success"] == false || data["result"]["exit_code"].as_i64().is_some_and(|n|n!=0);
                }
            }
            "control" if data["type"] == "steer" => self.put(
                format!("event-{seq}"),
                "You · steering",
                text("text"),
                false,
            ),
            "backend" => {
                let method = data["method"].as_str().unwrap_or("");
                let p = &data["params"];
                if method == "item/tool/call" && let Some(call) = p["callId"].as_str() {
                    self.put(format!("tool-{call}"),"Tool",format!("{}\n{}",display(&p["tool"]),display(&p["arguments"])),false);
                } else if method == "item/started" || method == "item/completed" {
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
                            let id = item["id"].as_str().unwrap_or(ty).to_owned();
                            self.put(id.clone(),label,body,false);
                            if ty == "commandExecution" && method == "item/completed" {
                                self.rows[self.positions[&id]].tool = Some(ToolDetail {
                                    result:item["exitCode"].as_i64().map(|n| format!("Exit code {n}")),
                                    failed:matches!(item["status"].as_str(),Some("failed"|"declined")) || item["exitCode"].as_i64().is_some_and(|n|n!=0),
                                    ..Default::default()
                                });
                            }
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
                } else if !data["id"].is_null() && (method.contains("requestApproval") || method.contains("requestUserInput")) {
                    self.put(
                        format!("event-{seq}"),
                        "Backend input requested",
                        display(data),
                        false,
                    );
                    self.rows[self.positions[&format!("event-{seq}")]].request = Some(data.clone());
                } else if matches!(method, "turn/plan/updated" | "turn/diff/updated" | "error") {
                    self.put(format!("event-{seq}"), method, display(p), false);
                }
            }
            "request_answered" => {
                for row in &mut self.rows {
                    if row.request.as_ref().is_some_and(|r|r["id"] == data["request_id"]) {
                        row.request = None;
                        row.label = "Backend input answered".into();
                    }
                }
            }
            "control" => self.put(format!("event-{seq}"),"Control",display(data),false),
            "status" if data["status"] != "completed" => self.put(
                format!("event-{seq}"),
                "Status",
                if text("detail").is_empty() || text("detail") == "null" {
                    text("status")
                } else {
                    format!("{} · {}", text("status"), text("detail"))
                },
                false,
            ),
            "control_error" => {
                if let Some(id) = data["update"]["update_id"].as_str() {
                    self.rejected_updates.insert(id.into());
                }
                self.put(format!("event-{seq}"), "Incident", display(data), false);
            }
            "tool_outcome_unknown"
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
    fn activation_and_import_roles_remain_distinct_from_delivered_mail() {
        let mut t = Transcript::default();
        t.apply(
            &json!({"seq":1,"kind":"activation_input","data":{"text":"Authored incoming batch"}}),
        )
        .unwrap();
        t.apply(&json!({"seq":2,"kind":"inference_message","data":{"role":"assistant","content":"Not a second rendered copy"}})).unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].label, "Activation input");
        let mut imported = Transcript::default();
        imported.apply(&json!({"seq":1,"kind":"transcript_imported","data":{"source":{"provider":"example"}}})).unwrap();
        imported.apply(&json!({"seq":2,"kind":"inference_message","data":{"role":"assistant","content":"Original source"}})).unwrap();
        assert_eq!(imported.rows[1].label, "Imported assistant");
        assert_eq!(imported.rows[1].text, "Original source");
    }
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
